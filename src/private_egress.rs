//! Transparent, fail-closed egress for macOS/Lima guests.
//!
//! The agent VM never receives proxy settings. A separate Lima VM owns the
//! Mihomo subscription and TUN device; the agent VM gets only an L3 next hop.

use std::io::ErrorKind;

#[cfg(target_os = "macos")]
use anyhow::ensure;
use anyhow::{Context, Result};

use crate::backend::SshSession;
use crate::config::CoopConfig;
use crate::remote_command::RemoteCommand;

#[cfg(target_os = "macos")]
pub const GATEWAY_NAME: &str = "coop-egress";
pub(crate) const OPENAI_PROXY_ACTIVE_ENV: &str = "COOP_OPENAI_PROXY_ACTIVE";
pub(crate) const PROXY_ENV_NAMES: [&str; 8] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
];
const INSTANCE_MODE_MARKER: &str = "private-egress-mode";
const INSTANCE_MODE_VERSION: &str = "version=1\n";

pub(crate) fn is_proxy_env_name(name: &str) -> bool {
    PROXY_ENV_NAMES.contains(&name)
}

#[cfg(target_os = "macos")]
mod platform {
    use std::fs::{self, File};
    use std::io::Read as _;
    use std::net::Ipv4Addr;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::str::FromStr as _;

    use anyhow::{Context, Result, ensure};
    use serde_json::Value as JsonValue;
    use serde_yaml_ng::{Mapping, Value};
    use url::Url;

    use super::{GATEWAY_NAME, PROXY_ENV_NAMES};
    use crate::cmd::Cmd;
    use crate::config::{CoopConfig, PrivateEgressConfig};
    use crate::sha256_hash::Sha256Hash;

    const MAX_SUBSCRIPTION_BYTES: usize = 16 * 1024 * 1024;
    const GATEWAY_CONFIG_VERSION: u32 = 2;
    type HardenedConfig = (Vec<u8>, String, Vec<(String, String)>);

    fn lima_cmd() -> Cmd {
        let mut command = Cmd::new("limactl");
        for name in PROXY_ENV_NAMES {
            command = command.env_remove(name);
        }
        command
    }

    fn host_curl_cmd() -> Cmd {
        let mut command = Cmd::new("curl");
        for name in PROXY_ENV_NAMES {
            command = command.env_remove(name);
        }
        command
    }

    fn host_https_curl_cmd() -> Cmd {
        host_curl_cmd().args([
            "--proto",
            "=https",
            "--proto-redir",
            "=https",
            "--retry",
            "3",
            "--retry-all-errors",
            "--retry-delay",
            "1",
        ])
    }

    pub fn ensure_gateway(cfg: &CoopConfig) -> Result<()> {
        let Some(egress) = &cfg.private_egress else {
            return Ok(());
        };
        ensure!(
            crate::cmd::command_exists("limactl"),
            "private_egress requires Lima (limactl) on macOS"
        );

        // Config installation restarts a shared gateway and rotates its
        // controller secret. Serialize the complete operation so concurrent
        // `coop` processes cannot invalidate each other's selector state.
        let gateway_state = cfg.data_dir.join("private-egress").join("gateway-state");
        let _gateway_lock = crate::fs_util::lock_sibling(&gateway_state)?;

        ensure_gateway_vm(cfg)?;
        ensure_mihomo_binary()?;

        let raw_subscription = egress
            .subscription
            .resolve()
            .context("Failed to resolve private_egress.subscription")?;
        let expected_egress = egress
            .expected_egress_ip
            .resolve()
            .context("Failed to resolve private_egress.expected_egress_ip")?;
        expected_egress
            .parse::<Ipv4Addr>()
            .context("Expected egress value is not a single IPv4 address")?;
        let deployment =
            gateway_deployment_fingerprint(egress, &raw_subscription, &expected_egress);
        let deployment_stamp = cfg
            .data_dir
            .join("private-egress")
            .join("gateway-deployment.stamp");
        if fs::read_to_string(&deployment_stamp).is_ok_and(|current| current.trim() == deployment) {
            if gateway_service_healthy() {
                return Ok(());
            }
            // A reboot or transient service failure should recover from the
            // already-hardened on-disk config without requiring the remote
            // subscription endpoint to be available.
            let _ = lima_cmd()
                .args([
                    "shell",
                    GATEWAY_NAME,
                    "--",
                    "sudo",
                    "systemctl",
                    "restart",
                    "mihomo.service",
                ])
                .run();
            if gateway_service_healthy() {
                return Ok(());
            }
        }

        let bootstrap_dns = gateway_bootstrap_dns()?;
        let subscription = fetch_subscription(&raw_subscription)?;
        let (config, controller_secret, selections) =
            harden_config(subscription, egress, bootstrap_dns)?;
        let selector_state = selector_payload(&controller_secret, &selections)?;
        install_gateway_config(&config, &selector_state, bootstrap_dns)?;
        verify_gateway_connectivity()?;
        crate::fs_util::atomic_write_with_mode(&deployment_stamp, &deployment, 0o600)?;
        Ok(())
    }

    fn gateway_deployment_fingerprint(
        config: &PrivateEgressConfig,
        subscription: &str,
        expected_egress: &str,
    ) -> String {
        let source = format!(
            "version={GATEWAY_CONFIG_VERSION}\nsubscription={}\nexpected={}\nentry_group={}\nentry_choice={}\nexit_group={}\nexit_prefix={}\nexit_suffix={}\n",
            Sha256Hash::of(subscription.as_bytes()),
            Sha256Hash::of(expected_egress.as_bytes()),
            config.entry_group,
            config.entry_choice,
            config.exit_group,
            config.exit_choice_prefix,
            config.exit_choice_suffix,
        );
        Sha256Hash::of(source.as_bytes()).to_string()
    }

    fn gateway_service_healthy() -> bool {
        lima_cmd()
            .args([
                "shell",
                GATEWAY_NAME,
                "--",
                "sudo",
                "sh",
                "-c",
                "systemctl is-active --quiet mihomo.service && ip link show coop-egress >/dev/null && nft list chain inet coop_gateway_killswitch forward | grep -q 'oifname \\\"coop-egress\\\" accept'",
            ])
            .status_ok()
    }

    fn ensure_gateway_vm(cfg: &CoopConfig) -> Result<()> {
        if gateway_shell_ok() {
            return Ok(());
        }

        let names = lima_cmd()
            .args(["list", "--format", "{{.Name}}"])
            .capture()
            .context("Failed to list Lima instances")?;
        if names.lines().any(|name| name == GATEWAY_NAME) {
            lima_cmd()
                .args(["start", GATEWAY_NAME])
                .run()
                .context("Failed to start the private egress gateway VM")?;
        } else {
            let dir = cfg.data_dir.join("private-egress");
            fs::create_dir_all(&dir)
                .with_context(|| format!("Failed to create {}", dir.display()))?;
            let template = dir.join("gateway.yaml");
            crate::fs_util::atomic_write_with_mode(&template, gateway_template(), 0o600)?;
            lima_cmd()
                .arg("start")
                .arg(&template)
                .arg(format!("--name={GATEWAY_NAME}"))
                .arg("--tty=false")
                .run()
                .context("Failed to create the private egress gateway VM")?;
        }
        ensure!(
            gateway_shell_ok(),
            "private egress gateway did not become ready"
        );
        Ok(())
    }

    fn gateway_shell_ok() -> bool {
        lima_cmd()
            .args(["shell", GATEWAY_NAME, "--", "true"])
            .status_ok()
    }

    fn gateway_template() -> &'static str {
        if cfg!(target_arch = "aarch64") {
            r#"# Generated by coop. Contains no subscription credentials.
vmType: "vz"
propagateProxyEnv: false
images:
- location: "https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-arm64.img"
  arch: "aarch64"
cpus: 1
memory: "1GiB"
disk: "6GiB"
networks:
- lima: user-v2
mounts: []
containerd:
  system: false
  user: false
provision:
- mode: system
  script: |
    apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl nftables python3
"#
        } else {
            r#"# Generated by coop. Contains no subscription credentials.
vmType: "vz"
propagateProxyEnv: false
images:
- location: "https://cloud-images.ubuntu.com/releases/24.04/release/ubuntu-24.04-server-cloudimg-amd64.img"
  arch: "x86_64"
cpus: 1
memory: "1GiB"
disk: "6GiB"
networks:
- lima: user-v2
mounts: []
containerd:
  system: false
  user: false
provision:
- mode: system
  script: |
    apt-get update
    DEBIAN_FRONTEND=noninteractive apt-get install -y ca-certificates curl nftables python3
"#
        }
    }

    fn ensure_mihomo_binary() -> Result<()> {
        if lima_cmd()
            .args([
                "shell",
                GATEWAY_NAME,
                "--",
                "test",
                "-x",
                "/usr/local/bin/mihomo",
            ])
            .status_ok()
        {
            return Ok(());
        }

        let release = host_https_curl_cmd()
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "https://api.github.com/repos/MetaCubeX/mihomo/releases/latest",
            ])
            .capture()
            .context("Failed to query the latest Mihomo release")?;
        let release: JsonValue = serde_json::from_str(&release)
            .context("GitHub returned invalid Mihomo release metadata")?;
        let assets = release["assets"]
            .as_array()
            .context("Mihomo release metadata has no assets")?;
        let needle = if cfg!(target_arch = "aarch64") {
            "mihomo-linux-arm64-"
        } else {
            "mihomo-linux-amd64-compatible-"
        };
        let asset = assets
            .iter()
            .find(|asset| {
                asset["name"].as_str().is_some_and(|name| {
                    name.starts_with(needle)
                        && Path::new(name)
                            .extension()
                            .is_some_and(|ext| ext.eq_ignore_ascii_case("gz"))
                })
            })
            .with_context(|| format!("Mihomo release has no {needle}*.gz asset"))?;
        let url = asset["browser_download_url"]
            .as_str()
            .context("Mihomo asset has no download URL")?;
        let digest = asset["digest"]
            .as_str()
            .and_then(|value| value.strip_prefix("sha256:"))
            .context("Mihomo asset has no GitHub SHA-256 digest")?;
        let expected = Sha256Hash::from_str(digest).context("Invalid Mihomo asset digest")?;

        let dir = tempfile::tempdir().context("Failed to create Mihomo download directory")?;
        let archive = dir.path().join("mihomo.gz");
        host_https_curl_cmd()
            .args(["--fail", "--silent", "--show-error", "--location"])
            .arg(url)
            .arg("--output")
            .arg(&archive)
            .run()
            .context("Failed to download Mihomo")?;
        let bytes = fs::read(&archive).context("Failed to read downloaded Mihomo archive")?;
        ensure!(
            Sha256Hash::of(&bytes) == expected,
            "Mihomo download failed SHA-256 verification"
        );

        let binary = dir.path().join("mihomo");
        gunzip(&archive, &binary)?;
        let binary = fs::read(&binary).context("Failed to read decompressed Mihomo binary")?;
        lima_cmd()
            .args([
                "shell",
                GATEWAY_NAME,
                "--",
                "sudo",
                "sh",
                "-c",
                "umask 022; cat > /usr/local/bin/mihomo && chmod 0755 /usr/local/bin/mihomo",
            ])
            .stdin_input(binary)
            .run()
            .context("Failed to install Mihomo in gateway VM")
    }

    fn gunzip(archive: &Path, output: &Path) -> Result<()> {
        let file = File::create(output)
            .with_context(|| format!("Failed to create {}", output.display()))?;
        let status = Command::new("gzip")
            .args(["-dc"])
            .arg(archive)
            .stdout(Stdio::from(file))
            .status()
            .context("Failed to run gzip")?;
        ensure!(status.success(), "gzip failed while unpacking Mihomo");
        Ok(())
    }

    fn fetch_subscription(raw_url: &str) -> Result<Value> {
        let url = Url::parse(raw_url).context("Subscription secret is not a valid URL")?;
        ensure!(url.scheme() == "https", "Subscription URL must use HTTPS");
        ensure!(
            !url.cannot_be_a_base() && url.host_str().is_some(),
            "Subscription URL must have a host"
        );
        ensure!(
            url.username().is_empty() && url.password().is_none() && url.fragment().is_none(),
            "Subscription URL must not contain userinfo or a fragment"
        );
        ensure!(
            !raw_url
                .chars()
                .any(|c| c.is_control() || matches!(c, '"' | '\\')),
            "Subscription URL contains characters unsupported by curl config input"
        );

        let curl_config = subscription_curl_config(raw_url);
        let yaml = host_curl_cmd()
            .args(["--config", "-"])
            .stdin_input(curl_config)
            .capture()
            .context("Failed to download the private egress subscription")?;
        ensure!(
            yaml.len() <= MAX_SUBSCRIPTION_BYTES,
            "Subscription exceeds {MAX_SUBSCRIPTION_BYTES} bytes"
        );
        serde_yaml_ng::from_str(&yaml).context("Subscription is not valid YAML")
    }

    fn subscription_curl_config(raw_url: &str) -> String {
        format!(
            "url = \"{raw_url}\"\nproto = \"=https\"\nproto-redir = \"=https\"\nretry = 3\nretry-all-errors\nretry-delay = 1\nfail\nsilent\nshow-error\nlocation\nmax-time = 30\nmax-filesize = {MAX_SUBSCRIPTION_BYTES}\n"
        )
    }

    fn harden_config(
        mut root: Value,
        config: &PrivateEgressConfig,
        bootstrap_dns: Ipv4Addr,
    ) -> Result<HardenedConfig> {
        let map = root
            .as_mapping_mut()
            .context("Subscription YAML root must be a mapping")?;
        for key in [
            "port",
            "socks-port",
            "redir-port",
            "tproxy-port",
            "mixed-port",
            "listeners",
            "authentication",
            "skip-auth-prefixes",
            "lan-allowed-ips",
            "lan-disallowed-ips",
            "external-ui",
            "external-ui-url",
            "external-ui-name",
        ] {
            map.remove(Value::String(key.into()));
        }
        insert(map, "mode", Value::String("global".into()));
        insert(map, "ipv6", Value::Bool(false));
        insert(map, "allow-lan", Value::Bool(false));
        insert(map, "bind-address", Value::String("127.0.0.1".into()));
        insert(map, "log-level", Value::String("warning".into()));
        insert(
            map,
            "external-controller",
            Value::String("127.0.0.1:9090".into()),
        );

        let secret = random_secret()?;
        insert(map, "secret", Value::String(secret.clone()));
        insert(map, "tun", hardened_tun());
        harden_dns(map, &config.exit_group, bootstrap_dns);
        insert(map, "profile", profile_config());
        map.remove(Value::String("rule-providers".into()));
        insert(
            map,
            "rules",
            Value::Sequence(vec![Value::String("MATCH,DIRECT".into())]),
        );

        let groups = map
            .get_mut(Value::String("proxy-groups".into()))
            .and_then(Value::as_sequence_mut)
            .context("Subscription has no proxy-groups list")?;
        let entry = prioritize_choice(groups, &config.entry_group, |name| {
            name == config.entry_choice
        })?;
        let exit = prioritize_choice(groups, &config.exit_group, |name| {
            name.starts_with(&config.exit_choice_prefix)
                && name.ends_with(&config.exit_choice_suffix)
        })?;
        let selections = vec![
            (config.entry_group.clone(), entry),
            (config.exit_group.clone(), exit),
            ("GLOBAL".to_string(), config.exit_group.clone()),
        ];

        let mut output = Vec::new();
        serde_yaml_ng::to_writer(&mut output, &root)
            .context("Failed to serialize hardened Mihomo config")?;
        Ok((output, secret, selections))
    }

    fn insert(map: &mut Mapping, key: &str, value: Value) {
        map.insert(Value::String(key.into()), value);
    }

    fn hardened_tun() -> Value {
        let mut tun = Mapping::new();
        insert(&mut tun, "enable", Value::Bool(true));
        insert(&mut tun, "device", Value::String("coop-egress".into()));
        insert(&mut tun, "stack", Value::String("mixed".into()));
        insert(&mut tun, "auto-route", Value::Bool(true));
        insert(&mut tun, "auto-redirect", Value::Bool(true));
        insert(&mut tun, "auto-detect-interface", Value::Bool(true));
        insert(&mut tun, "strict-route", Value::Bool(true));
        insert(
            &mut tun,
            "dns-hijack",
            Value::Sequence(vec![
                Value::String("any:53".into()),
                Value::String("tcp://any:53".into()),
            ]),
        );
        Value::Mapping(tun)
    }

    fn harden_dns(root: &mut Mapping, exit_group: &str, bootstrap_dns: Ipv4Addr) {
        let key = Value::String("dns".into());
        let dns = root
            .entry(key)
            .or_insert_with(|| Value::Mapping(Mapping::new()));
        if !dns.is_mapping() {
            *dns = Value::Mapping(Mapping::new());
        }
        if let Some(map) = dns.as_mapping_mut() {
            for key in [
                "nameserver-policy",
                "fallback",
                "fallback-filter",
                "direct-nameserver",
                "direct-nameserver-follow-policy",
                "proxy-server-nameserver-policy",
            ] {
                map.remove(Value::String(key.into()));
            }
            insert(map, "enable", Value::Bool(true));
            insert(map, "ipv6", Value::Bool(false));
            insert(map, "listen", Value::String("0.0.0.0:53".into()));
            insert(map, "enhanced-mode", Value::String("redir-host".into()));
            insert(map, "use-system-hosts", Value::Bool(false));
            insert(map, "respect-rules", Value::Bool(true));
            insert(
                map,
                "default-nameserver",
                Value::Sequence(vec![Value::String(bootstrap_dns.to_string())]),
            );
            insert(
                map,
                "nameserver",
                Value::Sequence(vec![Value::String(format!(
                    "https://cloudflare-dns.com/dns-query#{exit_group}"
                ))]),
            );
            insert(
                map,
                "proxy-server-nameserver",
                Value::Sequence(vec![Value::String(format!("{bootstrap_dns}#DIRECT"))]),
            );
        }
    }

    fn profile_config() -> Value {
        let mut profile = Mapping::new();
        insert(&mut profile, "store-selected", Value::Bool(true));
        insert(&mut profile, "store-fake-ip", Value::Bool(false));
        Value::Mapping(profile)
    }

    fn prioritize_choice<F>(groups: &mut [Value], group_name: &str, matches: F) -> Result<String>
    where
        F: Fn(&str) -> bool,
    {
        let group = groups
            .iter_mut()
            .find(|group| {
                group
                    .as_mapping()
                    .and_then(|map| map.get(Value::String("name".into())))
                    .and_then(Value::as_str)
                    == Some(group_name)
            })
            .with_context(|| format!("Subscription has no proxy group '{group_name}'"))?;
        let proxies = group
            .as_mapping_mut()
            .and_then(|map| map.get_mut(Value::String("proxies".into())))
            .and_then(Value::as_sequence_mut)
            .with_context(|| format!("Proxy group '{group_name}' has no proxies list"))?;
        let index = proxies
            .iter()
            .position(|value| value.as_str().is_some_and(&matches))
            .with_context(|| format!("Proxy group '{group_name}' has no requested choice"))?;
        let selected = proxies[index]
            .as_str()
            .context("Selected proxy name is not a string")?
            .to_string();
        let value = proxies.remove(index);
        proxies.insert(0, value);
        Ok(selected)
    }

    fn random_secret() -> Result<String> {
        let mut bytes = [0_u8; 32];
        File::open("/dev/urandom")
            .context("Failed to open host random source")?
            .read_exact(&mut bytes)
            .context("Failed to read host random source")?;
        Ok(hex::encode(bytes))
    }

    fn gateway_bootstrap_dns() -> Result<Ipv4Addr> {
        let output = lima_cmd()
            .args([
                "shell",
                GATEWAY_NAME,
                "--",
                "sh",
                "-c",
                "ip -4 route show default | awk 'NR == 1 { print $3; exit }'",
            ])
            .capture()
            .context("Failed to discover the gateway VM bootstrap resolver")?;
        output
            .trim()
            .parse()
            .context("Gateway VM default route did not provide an IPv4 bootstrap resolver")
    }

    fn selector_payload(secret: &str, selections: &[(String, String)]) -> Result<Vec<u8>> {
        serde_json::to_vec(&serde_json::json!({
            "secret": secret,
            "selections": selections,
        }))
        .context("Failed to encode Mihomo selector state")
    }

    fn install_gateway_config(
        config: &[u8],
        selector_state: &[u8],
        bootstrap_dns: Ipv4Addr,
    ) -> Result<()> {
        lima_cmd()
            .args([
                "shell",
                GATEWAY_NAME,
                "--",
                "sudo",
                "sh",
                "-c",
                "umask 077; install -d -m 0700 /etc/mihomo; cat > /etc/mihomo/config.yaml.tmp && mv /etc/mihomo/config.yaml.tmp /etc/mihomo/config.yaml",
            ])
            .stdin_input(config.to_vec())
            .run()
            .context("Failed to transfer private egress config")?;
        lima_cmd()
            .args([
                "shell",
                GATEWAY_NAME,
                "--",
                "sudo",
                "sh",
                "-c",
                "umask 077; cat > /etc/mihomo/selector-state.json.tmp && mv /etc/mihomo/selector-state.json.tmp /etc/mihomo/selector-state.json",
            ])
            .stdin_input(selector_state.to_vec())
            .run()
            .context("Failed to transfer private egress selector state")?;
        lima_cmd()
            .args(["shell", GATEWAY_NAME, "--", "sudo", "sh", "-s", "--"])
            .arg(bootstrap_dns.to_string())
            .stdin_input(GATEWAY_SETUP_SCRIPT.as_bytes().to_vec())
            .run()
            .context("Failed to configure the private egress service")
    }

    fn verify_gateway_connectivity() -> Result<()> {
        let actual = lima_cmd()
            .args([
                "shell",
                GATEWAY_NAME,
                "--",
                "curl",
                "--fail",
                "--silent",
                "--show-error",
                "--max-time",
                "15",
                "https://api.ipify.org",
            ])
            .capture()
            .context("Private egress gateway cannot reach the IP verification service")?;
        let actual: Ipv4Addr = actual
            .trim()
            .parse()
            .context("IP verification service returned a non-IPv4 response")?;
        tracing::info!(
            %actual,
            "Private egress gateway is active; the agent VM performs the authoritative exit check"
        );
        Ok(())
    }

    pub fn gateway_ipv4() -> Result<Ipv4Addr> {
        let output = lima_cmd()
            .args([
                "shell",
                GATEWAY_NAME,
                "--",
                "sh",
                "-c",
                "iface=$(ip -4 route show default | awk 'NR == 1 { for (i=1; i<=NF; i++) if ($i == \"dev\") { print $(i+1); exit } }'); test -n \"$iface\"; ip -4 -o address show dev \"$iface\" scope global | awk 'NR == 1 { split($4, addr, \"/\"); print addr[1]; exit }'",
            ])
            .capture()
            .context("Failed to discover the private egress gateway address")?;
        output
            .trim()
            .parse()
            .context("Private egress gateway returned an invalid IPv4 address")
    }

    const GATEWAY_SETUP_SCRIPT: &str = r#"set -eu
BOOTSTRAP_DNS=$1
UPLINK=$(ip -4 route show default | awk 'NR == 1 { for (i=1; i<=NF; i++) if ($i == "dev") { print $(i+1); exit } }')
case "$UPLINK" in
  ''|*[!A-Za-z0-9_.:-]*) echo "invalid gateway uplink interface" >&2; exit 1 ;;
esac
install -d -o root -g root -m 0755 /usr/local/libexec
cat >/usr/local/libexec/mihomo-restore-selectors <<'PYTHON'
#!/usr/bin/python3
import json, pathlib, time, urllib.parse, urllib.request

state_path = pathlib.Path("/etc/mihomo/selector-state.json")
if not state_path.exists():
    raise SystemExit("selector state is missing; refusing to open forwarding")
payload = json.loads(state_path.read_text())
headers = {"Authorization": "Bearer " + payload["secret"], "Content-Type": "application/json"}
for group, choice in payload["selections"]:
    url = "http://127.0.0.1:9090/proxies/" + urllib.parse.quote(group, safe="")
    body = json.dumps({"name": choice}).encode()
    last = None
    for _ in range(30):
        try:
            request = urllib.request.Request(url, data=body, headers=headers, method="PUT")
            with urllib.request.urlopen(request, timeout=2) as response:
                if response.status in (200, 204):
                    last = None
                    break
        except Exception as exc:
            last = exc
            time.sleep(1)
    if last is not None:
        raise last
PYTHON
chmod 0755 /usr/local/libexec/mihomo-restore-selectors
cat >/etc/systemd/system/mihomo.service <<'UNIT'
[Unit]
Description=Coop private egress (Mihomo TUN)
After=network-online.target
Wants=network-online.target

[Service]
Type=simple
User=root
ExecStartPre=/usr/sbin/nft -f /etc/mihomo/killswitch-closed.nft
ExecStartPre=/usr/local/bin/mihomo -t -d /etc/mihomo
ExecStart=/usr/local/bin/mihomo -d /etc/mihomo
ExecStartPost=/usr/local/libexec/mihomo-restore-selectors
ExecStartPost=/usr/sbin/nft -f /etc/mihomo/killswitch-open.nft
ExecStopPost=/usr/sbin/nft -f /etc/mihomo/killswitch-closed.nft
Restart=always
RestartSec=2
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
ReadWritePaths=/etc/mihomo
PrivateTmp=true
RestrictAddressFamilies=AF_INET AF_NETLINK AF_UNIX
CapabilityBoundingSet=CAP_NET_ADMIN CAP_NET_BIND_SERVICE CAP_NET_RAW
AmbientCapabilities=CAP_NET_ADMIN CAP_NET_BIND_SERVICE CAP_NET_RAW

[Install]
WantedBy=multi-user.target
UNIT
cat >/etc/mihomo/killswitch-closed.nft <<'NFT'
destroy table inet coop_gateway_killswitch
table inet coop_gateway_killswitch {
  chain forward {
    type filter hook forward priority 100; policy drop;
  }
}
NFT
cat >/etc/mihomo/killswitch-open.nft <<NFT
destroy table inet coop_gateway_killswitch
table inet coop_gateway_killswitch {
  chain forward {
    type filter hook forward priority 100; policy drop;
    iifname "$UPLINK" oifname "coop-egress" accept
    iifname "coop-egress" oifname "$UPLINK" accept
  }
}
NFT
chmod 0600 /etc/mihomo/killswitch-closed.nft /etc/mihomo/killswitch-open.nft
cat >/etc/sysctl.d/90-coop-egress.conf <<'SYSCTL'
net.ipv4.ip_forward=1
net.ipv6.conf.all.disable_ipv6=1
net.ipv6.conf.default.disable_ipv6=1
SYSCTL
sysctl --system >/dev/null
systemctl disable --now systemd-resolved.service >/dev/null 2>&1 || true
rm -f /etc/resolv.conf
cat >/etc/resolv.conf <<RESOLV
nameserver $BOOTSTRAP_DNS
options timeout:2 attempts:3
RESOLV
grep -qE '^[^#]*[[:space:]]lima-coop-egress([[:space:]]|$)' /etc/hosts || \
  printf '%s\n' '127.0.1.1 lima-coop-egress' >>/etc/hosts
systemctl daemon-reload
systemctl enable mihomo.service >/dev/null
systemctl restart mihomo.service
for _ in $(seq 1 30); do
  systemctl is-active --quiet mihomo.service && exit 0
  sleep 1
done
systemctl --no-pager status mihomo.service >&2
exit 1
"#;

    #[cfg(test)]
    #[expect(clippy::expect_used, reason = "test failures should identify fixtures")]
    mod tests {
        use super::super::{AGENT_LOCKDOWN_SCRIPT, BOOT_GUARD_INSTALL_SCRIPT};
        use super::*;

        fn egress_config() -> PrivateEgressConfig {
            let config: CoopConfig = toml::from_str(
                r#"
[private_egress]
subscription = "cmd:printf subscription"
expected_egress_ip = "cmd:printf 203.0.113.10"
entry_group = "entry-group"
entry_choice = "us-entry"
exit_group = "exit-group"
exit_choice_prefix = "los-angeles-exit"
exit_choice_suffix = ""
"#,
            )
            .expect("valid test config");
            config.private_egress.expect("private egress configured")
        }

        #[test]
        fn deployment_fingerprint_tracks_resolved_secrets_without_exposing_them() {
            let config = egress_config();
            let first = gateway_deployment_fingerprint(
                &config,
                "https://secret.example/sub/token-one",
                "203.0.113.10",
            );
            let second = gateway_deployment_fingerprint(
                &config,
                "https://secret.example/sub/token-two",
                "203.0.113.10",
            );
            assert_ne!(first, second);
            assert_eq!(first.len(), 64);
            assert!(!first.contains("token-one"));
        }

        #[test]
        fn subscription_download_forbids_redirect_downgrades_and_retries() {
            let curl = subscription_curl_config("https://secret.example/sub/token");
            assert!(curl.contains("proto = \"=https\""));
            assert!(curl.contains("proto-redir = \"=https\""));
            assert!(curl.contains("retry-all-errors"));
            assert!(curl.contains("max-filesize = 16777216"));
        }

        #[test]
        fn subscription_is_forced_to_global_strict_tun() {
            let subscription: Value = serde_yaml_ng::from_str(
                r#"
mode: rule
ipv6: true
allow-lan: true
bind-address: "*"
mixed-port: 7890
socks-port: 7891
listeners: [{name: leaked-listener, type: socks, port: 7892}]
dns: false
proxies: []
proxy-groups:
  - name: entry-group
    type: select
    proxies: [other-entry, us-entry]
  - name: exit-group
    type: select
    proxies: [DIRECT, los-angeles-exit]
rules: [MATCH,DIRECT]
"#,
            )
            .expect("valid subscription fixture");
            let (yaml, _secret, selections) =
                harden_config(subscription, &egress_config(), Ipv4Addr::new(192, 0, 2, 53))
                    .expect("harden config");
            let hardened: Value = serde_yaml_ng::from_slice(&yaml).expect("parse hardened config");
            let root = hardened.as_mapping().expect("mapping root");

            assert_eq!(
                root.get(Value::String("mode".into()))
                    .and_then(Value::as_str),
                Some("global")
            );
            assert_eq!(
                root.get(Value::String("ipv6".into())),
                Some(&Value::Bool(false))
            );
            assert_eq!(
                root.get(Value::String("allow-lan".into())),
                Some(&Value::Bool(false))
            );
            assert_eq!(
                root.get(Value::String("bind-address".into()))
                    .and_then(Value::as_str),
                Some("127.0.0.1")
            );
            for key in ["mixed-port", "socks-port", "listeners"] {
                assert!(!root.contains_key(Value::String(key.into())));
            }
            let tun = root
                .get(Value::String("tun".into()))
                .and_then(Value::as_mapping)
                .expect("tun mapping");
            assert_eq!(
                tun.get(Value::String("device".into()))
                    .and_then(Value::as_str),
                Some("coop-egress")
            );
            assert_eq!(
                tun.get(Value::String("strict-route".into())),
                Some(&Value::Bool(true))
            );
            assert!(selections.contains(&("GLOBAL".into(), "exit-group".into())));
            assert!(selections.contains(&("exit-group".into(), "los-angeles-exit".into())));
            assert_eq!(
                root.get(Value::String("rules".into())),
                Some(&Value::Sequence(vec![Value::String("MATCH,DIRECT".into())]))
            );
            assert!(!root.contains_key(Value::String("rule-providers".into())));
            let dns = root
                .get(Value::String("dns".into()))
                .and_then(Value::as_mapping)
                .expect("dns mapping");
            assert_eq!(
                dns.get(Value::String("respect-rules".into())),
                Some(&Value::Bool(true))
            );
            assert_eq!(
                dns.get(Value::String("nameserver".into())),
                Some(&Value::Sequence(vec![Value::String(
                    "https://cloudflare-dns.com/dns-query#exit-group".into()
                )]))
            );
            assert_eq!(
                dns.get(Value::String("default-nameserver".into())),
                Some(&Value::Sequence(vec![Value::String("192.0.2.53".into())]))
            );
            assert_eq!(
                dns.get(Value::String("proxy-server-nameserver".into())),
                Some(&Value::Sequence(vec![Value::String(
                    "192.0.2.53#DIRECT".into()
                )]))
            );
            let profile = root
                .get(Value::String("profile".into()))
                .and_then(Value::as_mapping)
                .expect("profile mapping");
            assert_eq!(
                profile.get(Value::String("store-selected".into())),
                Some(&Value::Bool(true))
            );
        }

        #[test]
        fn agent_scripts_fail_closed_and_start_from_a_clean_environment() {
            assert!(BOOT_GUARD_INSTALL_SCRIPT.contains("Before=network-pre.target"));
            assert!(BOOT_GUARD_INSTALL_SCRIPT.contains("iptables -w -P OUTPUT DROP"));
            assert!(BOOT_GUARD_INSTALL_SCRIPT.contains("--sport 68 --dport 67"));
            assert!(AGENT_LOCKDOWN_SCRIPT.contains("env -i"));
            assert!(AGENT_LOCKDOWN_SCRIPT.contains("for item in .claude .claude.json .codex"));
            assert!(AGENT_LOCKDOWN_SCRIPT.contains("ln -s \"$DEST\" \"$MANAGER_ITEM\""));
            assert!(AGENT_LOCKDOWN_SCRIPT.contains("rm -f /home/developer/.codex/auth.json"));
            assert!(AGENT_LOCKDOWN_SCRIPT.contains("usermod -G developers developer"));
            assert!(
                AGENT_LOCKDOWN_SCRIPT
                    .contains("git config --global --add safe.directory /workspace")
            );
            assert!(AGENT_LOCKDOWN_SCRIPT.contains("mount --bind /dev/null /usr/bin/sudo"));
            assert!(AGENT_LOCKDOWN_SCRIPT.contains("TZ=\"$AGENT_TZ\""));
            assert!(!AGENT_LOCKDOWN_SCRIPT.contains("TZ=America/Los_Angeles"));
            assert!(AGENT_LOCKDOWN_SCRIPT.contains("iptables -w -A \"$NEXT\" -j ACCEPT"));
            assert!(AGENT_LOCKDOWN_SCRIPT.contains("ip -4 route replace table 100 default"));
            assert!(!AGENT_LOCKDOWN_SCRIPT.contains("ip rule del priority 100"));
            assert!(AGENT_LOCKDOWN_SCRIPT.contains("iptables -w -D OUTPUT -j PRIVATE_EGRESS_BOOT"));
            assert!(GATEWAY_SETUP_SCRIPT.contains("destroy table inet coop_gateway_killswitch"));
            assert!(!GATEWAY_SETUP_SCRIPT.contains("nft delete table"));
            let restore = GATEWAY_SETUP_SCRIPT
                .find("ExecStartPost=/usr/local/libexec/mihomo-restore-selectors")
                .expect("selector restore hook");
            let open = GATEWAY_SETUP_SCRIPT
                .find("ExecStartPost=/usr/sbin/nft -f /etc/mihomo/killswitch-open.nft")
                .expect("forward-open hook");
            assert!(
                restore < open,
                "forwarding must open after selector restore"
            );
            assert!(
                GATEWAY_SETUP_SCRIPT
                    .contains("ExecStopPost=/usr/sbin/nft -f /etc/mihomo/killswitch-closed.nft")
            );
            assert!(GATEWAY_SETUP_SCRIPT.contains("iifname \"$UPLINK\""));
            assert!(GATEWAY_SETUP_SCRIPT.contains("refusing to open forwarding"));
            assert!(!GATEWAY_SETUP_SCRIPT.contains("raise SystemExit(0)"));
        }
    }
}

pub fn ensure_gateway(cfg: &CoopConfig) -> Result<()> {
    if cfg.private_egress.is_none() {
        return Ok(());
    }
    #[cfg(target_os = "macos")]
    {
        platform::ensure_gateway(cfg)
    }
    #[cfg(not(target_os = "macos"))]
    {
        anyhow::bail!("private_egress is currently supported only by the macOS Lima backend")
    }
}

/// Persist the network mode used to create an instance. A stopped Lima VM
/// cannot be converted safely after the fact because an ordinary image may
/// emit traffic before Coop can reconnect over SSH and install its guard.
pub(crate) fn record_instance_mode(cfg: &CoopConfig, inst: &crate::config::Instance) -> Result<()> {
    let marker = inst.dir.join(INSTANCE_MODE_MARKER);
    if cfg.private_egress.is_some() {
        crate::fs_util::atomic_write_with_mode(&marker, INSTANCE_MODE_VERSION, 0o600)
            .context("Failed to record private-egress instance mode")?;
    } else if let Err(error) = std::fs::remove_file(&marker)
        && error.kind() != ErrorKind::NotFound
    {
        return Err(error).context("Failed to clear stale private-egress mode marker");
    }
    Ok(())
}

/// Refuse to use an instance under a different network mode from the one it
/// was created with. In particular, enabling private egress must not boot a
/// legacy unguarded image and leave a direct-egress window before SSH is ready.
pub(crate) fn ensure_instance_mode(cfg: &CoopConfig, inst: &crate::config::Instance) -> Result<()> {
    let configured = cfg.private_egress.is_some();
    let marker = inst.dir.join(INSTANCE_MODE_MARKER);
    let recorded = match std::fs::read_to_string(&marker) {
        Ok(contents) if contents == INSTANCE_MODE_VERSION => true,
        Ok(_) => anyhow::bail!(
            "Instance '{}' has an invalid or unsupported private-egress mode marker at {}. \
             Destroy and recreate this instance before using it",
            inst.name,
            marker.display(),
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => false,
        Err(error) => {
            return Err(error).with_context(|| {
                format!("Failed to read instance mode marker {}", marker.display())
            });
        }
    };
    if configured != recorded {
        let configured_mode = if configured {
            "private egress"
        } else {
            "ordinary networking"
        };
        let recorded_mode = if recorded {
            "private egress"
        } else {
            "ordinary networking"
        };
        anyhow::bail!(
            "Instance '{}' was created with {recorded_mode}, but the current config uses \
             {configured_mode}. Destroy and recreate this instance before using it under the \
             new network mode",
            inst.name
        );
    }
    Ok(())
}

/// The restricted account is a security boundary only when it is distinct
/// from the image's passwordless-sudo management account.
pub(crate) fn ensure_management_user(cfg: &CoopConfig, user: &str) -> Result<()> {
    if cfg.private_egress.is_some() && user == "developer" {
        anyhow::bail!(
            "private_egress reserves guest user 'developer' for restricted agent sessions. \
             Rebuild the image with a different --guest-user (for example, 'ubuntu')"
        );
    }
    Ok(())
}

/// Replace the agent guest's network with a policy-routing table whose only
/// default next hop is the separate gateway VM, then create the unprivileged
/// account used for agent processes.
pub fn configure_agent_guest(session: &SshSession, cfg: &CoopConfig) -> Result<()> {
    if cfg.private_egress.is_none() {
        return Ok(());
    }
    ensure_management_user(cfg, session.target.user.as_ref())?;
    #[cfg(target_os = "macos")]
    {
        let gateway = platform::gateway_ipv4()?.to_string();
        let management_user = session.target.user.as_ref();
        let timezone = cfg
            .guest_timezone
            .as_ref()
            .map_or("", crate::config::GuestTimeZone::as_str);
        let drop_codex_auth = if session.env.contains(OPENAI_PROXY_ACTIVE_ENV) {
            "1"
        } else {
            "0"
        };
        session
            .target
            .exec_with_stdin(
                RemoteCommand::new().literal("sudo sh -s"),
                BOOT_GUARD_INSTALL_SCRIPT.as_bytes().to_vec(),
            )
            .context("Failed to install the early-boot egress guard in agent VM")?;
        let command = RemoteCommand::new()
            .literal("peer=${SSH_CONNECTION%% *}; sudo sh -s -- ")
            .arg(&gateway)
            .literal(" ")
            .arg(management_user)
            .literal(" \"$peer\" ")
            .arg(timezone)
            .literal(" ")
            .arg(drop_codex_auth);
        session
            .target
            .exec_with_stdin(command, AGENT_LOCKDOWN_SCRIPT.as_bytes().to_vec())
            .context("Failed to enforce fail-closed networking in agent VM")?;
        verify_agent_route(session, cfg, &gateway)?;
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = session;
        anyhow::bail!("private_egress is currently supported only by the macOS Lima backend")
    }
}

#[cfg(target_os = "macos")]
fn verify_agent_route(session: &SshSession, cfg: &CoopConfig, gateway: &str) -> Result<()> {
    let route = session
        .target
        .capture("ip -4 route get 1.1.1.1")
        .context("Failed to inspect agent VM policy route")?;
    ensure!(
        route.split_whitespace().any(|word| word == gateway),
        "agent VM is not policy-routed through the private gateway"
    );
    session
        .target
        .exec(RemoteCommand::new().literal(
            "test -z \"${HTTP_PROXY-}${HTTPS_PROXY-}${ALL_PROXY-}${http_proxy-}${https_proxy-}${all_proxy-}\"",
        ))
        .context("Proxy environment variables are unexpectedly visible in the agent VM")?;
    let expected = cfg
        .private_egress
        .as_ref()
        .context("Private egress config disappeared during verification")?
        .expected_egress_ip
        .resolve()
        .context("Failed to resolve private_egress.expected_egress_ip")?;
    let expected: std::net::Ipv4Addr = expected
        .parse()
        .context("Expected egress value is not a single IPv4 address")?;
    let actual = session
        .target
        .capture("curl --fail --silent --show-error --max-time 15 https://api.ipify.org")
        .context("Agent VM cannot reach the exit verification service through the gateway")?;
    let actual: std::net::Ipv4Addr = actual
        .trim()
        .parse()
        .context("Agent VM received a non-IPv4 exit verification response")?;
    ensure!(
        actual == expected,
        "agent VM exit verification failed: expected {expected}, observed {actual}"
    );
    Ok(())
}

/// Prefix an agent binary with the root-owned normalized agent view. The
/// launcher builds a private mount/UTS view, then drops to an account with
/// neither sudo nor Docker access. Network namespaces are deliberately shared
/// so the fail-closed route remains authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RestrictedAgent {
    Claude,
    Codex,
}

pub fn restricted_agent_command(
    session: &SshSession,
    agent: RestrictedAgent,
    binary: &str,
    args: Vec<String>,
) -> Vec<String> {
    if session
        .env
        .as_envs()
        .contains_key("COOP_PRIVATE_EGRESS_ACTIVE")
    {
        let manager = session.target.user.as_ref();
        let mut command = vec!["sudo".to_string()];
        if agent == RestrictedAgent::Codex
            && session
                .env
                .contains(crate::model_state::CODEX_LOCAL_ENV_KEY)
        {
            command.push(format!(
                "--preserve-env={}",
                crate::model_state::CODEX_LOCAL_ENV_KEY
            ));
        }
        command.extend([
            "--".to_string(),
            "/usr/local/sbin/dev-session".to_string(),
            manager.to_string(),
            binary.to_string(),
        ]);
        command.extend(args);
        command
    } else {
        let mut command = vec![binary.to_string()];
        command.extend(args);
        command
    }
}

/// Run a project-provided post-start hook inside the same unprivileged view as
/// agents. Devcontainer hooks are guest-controlled input and must not inherit
/// the management user's passwordless sudo in private-egress mode.
pub fn restricted_post_start_command(session: &SshSession, command: &str) -> RemoteCommand {
    if session.env.contains("COOP_PRIVATE_EGRESS_ACTIVE") {
        RemoteCommand::new()
            .literal("sudo -- /usr/local/sbin/dev-session ")
            .arg(session.target.user.as_ref())
            .literal(" /bin/bash -c ")
            .arg(command)
    } else {
        RemoteCommand::new().literal(command)
    }
}

pub(crate) fn boot_guard_install_script() -> &'static [u8] {
    BOOT_GUARD_INSTALL_SCRIPT.as_bytes()
}

/// Installed into the private-egress base image before an agent VM is ever
/// created. On every boot it denies new outbound traffic before networking is
/// configured; DHCP and replies to inbound management SSH remain possible.
const BOOT_GUARD_INSTALL_SCRIPT: &str = r"set -eu
install -d -o root -g root -m 0755 /usr/local/sbin
cat >/usr/local/sbin/private-egress-boot-guard <<'SCRIPT'
#!/bin/sh
set -eu
sysctl -qw net.ipv6.conf.all.disable_ipv6=1
sysctl -qw net.ipv6.conf.default.disable_ipv6=1
iptables -w -N PRIVATE_EGRESS_BOOT 2>/dev/null || true
iptables -w -F PRIVATE_EGRESS_BOOT
iptables -w -A PRIVATE_EGRESS_BOOT -o lo -j ACCEPT
iptables -w -A PRIVATE_EGRESS_BOOT -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -w -A PRIVATE_EGRESS_BOOT -p udp --sport 68 --dport 67 -j ACCEPT
while iptables -w -D OUTPUT -j PRIVATE_EGRESS_BOOT 2>/dev/null; do :; done
iptables -w -I OUTPUT 1 -j PRIVATE_EGRESS_BOOT
iptables -w -P OUTPUT DROP
ip6tables -w -P OUTPUT DROP
SCRIPT
chmod 0755 /usr/local/sbin/private-egress-boot-guard
cat >/etc/systemd/system/private-egress-boot-guard.service <<'UNIT'
[Unit]
Description=Early boot fail-closed egress guard
DefaultDependencies=no
After=systemd-modules-load.service local-fs.target
Before=network-pre.target
Wants=network-pre.target

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/private-egress-boot-guard
RemainAfterExit=yes

[Install]
WantedBy=sysinit.target
UNIT
systemctl daemon-reload
systemctl enable private-egress-boot-guard.service >/dev/null
";

#[cfg(target_os = "macos")]
const AGENT_LOCKDOWN_SCRIPT: &str = r##"set -eu
GATEWAY=$1
MANAGER=$2
SSH_PEER=$3
AGENT_TZ=$4
DROP_CODEX_AUTH=$5
if [ -z "$AGENT_TZ" ]; then
  AGENT_TZ=$(cat /etc/timezone 2>/dev/null || printf '%s\n' UTC)
fi
printf '%s\n' "$AGENT_TZ" >/etc/dev-session-timezone
chmod 0644 /etc/dev-session-timezone
IFACE=$(ip -4 route get "$GATEWAY" | awk '{for(i=1;i<=NF;i++) if($i=="dev") {print $(i+1); exit}}')
test -n "$IFACE"
MANAGER_HOME=$(getent passwd "$MANAGER" | cut -d: -f6)
test -n "$MANAGER_HOME" && test -d "$MANAGER_HOME"

# Lima 2.2 may materialize macOS system-proxy settings in /etc/environment
# even when propagateProxyEnv is false. Remove both its managed block and any
# standalone proxy keys before any untrusted agent session can be launched.
ENV_TMP=$(mktemp)
awk '
  $0 == "#LIMA-START" { lima = 1; next }
  $0 == "#LIMA-END" { lima = 0; next }
  lima { next }
  /^[[:space:]]*(HTTP_PROXY|HTTPS_PROXY|ALL_PROXY|NO_PROXY|http_proxy|https_proxy|all_proxy|no_proxy)=/ { next }
  { print }
' /etc/environment >"$ENV_TMP"
install -o root -g root -m 0644 "$ENV_TMP" /etc/environment
rm -f "$ENV_TMP"

getent group developers >/dev/null || groupadd --system developers
id developer >/dev/null 2>&1 || useradd --create-home --shell /bin/bash --gid developers developer
usermod -g developers developer
usermod -G developers developer
passwd -l developer >/dev/null 2>&1 || true
usermod -aG developers "$MANAGER"
install -d -o developer -g developers -m 0750 /home/developer
CLAUDE_BINARY=$(readlink -f /usr/local/bin/claude 2>/dev/null || true)
case "$CLAUDE_BINARY" in
  /home/*/.local/*) TOOL_HOME=${CLAUDE_BINARY%%/.local/*} ;;
  *) TOOL_HOME=/nonexistent ;;
esac
for item in .claude .claude.json .codex; do
  DEST="/home/developer/$item"
  MANAGER_ITEM="$MANAGER_HOME/$item"
  SOURCE=
  for source_home in "$MANAGER_HOME" "$TOOL_HOME"; do
    if [ -e "$source_home/$item" ] || [ -L "$source_home/$item" ]; then
      SOURCE="$source_home/$item"
      break
    fi
  done

  # Migrate pre-shared state once, then make bootstrap and restricted launches
  # use the same files. Directory copies merge so developer-owned histories
  # survive while current manager-side settings win on name collisions.
  if [ -n "$SOURCE" ] && [ "$(readlink -f "$SOURCE" 2>/dev/null || true)" != "$DEST" ]; then
    [ -L "$DEST" ] && rm -f "$DEST"
    if [ -d "$SOURCE" ] && [ -d "$DEST" ]; then
      cp -a "$SOURCE/." "$DEST/"
    elif [ ! -e "$DEST" ]; then
      cp -a "$SOURCE" "$DEST"
    fi
  fi

  if [ ! -e "$DEST" ]; then
    case "$item" in
      .claude.json) printf '%s\n' '{}' >"$DEST" ;;
      *) install -d -o developer -g developers -m 2770 "$DEST" ;;
    esac
  fi
  chown -R developer:developers "$DEST"
  if [ -d "$DEST" ]; then
    chmod -R g+rwX "$DEST"
    find "$DEST" -type d -exec chmod g+s {} +
  else
    chmod 0660 "$DEST"
  fi

  if [ "$(readlink -f "$MANAGER_ITEM" 2>/dev/null || true)" != "$DEST" ]; then
    rm -rf "$MANAGER_ITEM"
    ln -s "$DEST" "$MANAGER_ITEM"
  fi
done
if [ "$DROP_CODEX_AUTH" = 1 ]; then
  rm -f /home/developer/.codex/auth.json
fi
chgrp -R developers /workspace
chmod -R g+rwX /workspace
find /workspace -type d -exec chmod g+s {} +
# Git rejects repositories owned by the management account even when the
# restricted account has intentional group write access. Trust only Coop's
# fixed workspace path in developer's protected global configuration.
if ! runuser -u developer -- env HOME=/home/developer \
  git config --global --get-all safe.directory 2>/dev/null | grep -Fxq /workspace; then
  runuser -u developer -- env HOME=/home/developer \
    git config --global --add safe.directory /workspace
fi

# Give agent processes an ordinary, privacy-normalized Linux view without
# adding PID, user, or network namespaces that would themselves look like a
# container. The real VM hardware view remains available only to root.
VIEW=/usr/local/share/dev-session
install -d -o root -g root -m 0755 "$VIEW/dmi" "$VIEW/empty" /usr/local/libexec
cat >"$VIEW/dmi/sys_vendor" <<'EOF'
GIGA-BYTE TECHNOLOGY CO., LTD.
EOF
cat >"$VIEW/dmi/product_name" <<'EOF'
Development Workstation
EOF
cat >"$VIEW/dmi/product_family" <<'EOF'
Workstation
EOF
cat >"$VIEW/dmi/product_version" <<'EOF'
1.0
EOF
cat >"$VIEW/dmi/bios_vendor" <<'EOF'
American Megatrends International, LLC.
EOF
cat >"$VIEW/dmi/bios_version" <<'EOF'
F31
EOF
cat >"$VIEW/dmi/bios_date" <<'EOF'
01/01/2024
EOF
cat >"$VIEW/dmi/chassis_vendor" <<'EOF'
GIGA-BYTE TECHNOLOGY CO., LTD.
EOF
cat >"$VIEW/dmi/chassis_type" <<'EOF'
3
EOF
cat >"$VIEW/dmi/modalias" <<'EOF'
dmi:bvnAmericanMegatrendsInternationalLLC.:bvrF31:bd01/01/2024:svnGIGA-BYTETECHNOLOGYCO.LTD.:pnDevelopmentWorkstation:pvr1.0:
EOF
cat >"$VIEW/dmi/uevent" <<'EOF'
MODALIAS=dmi:bvnAmericanMegatrendsInternationalLLC.:bvrF31:bd01/01/2024:svnGIGA-BYTETECHNOLOGYCO.LTD.:pnDevelopmentWorkstation:pvr1.0:
EOF
touch "$VIEW/dmi/product_serial" "$VIEW/dmi/product_sku" "$VIEW/dmi/product_uuid"
chmod 0444 "$VIEW"/dmi/*

# Preserve the real feature list and CPU count, but replace the Apple MIDR
# with ARM Neoverse N1 identifiers consistent with a generic ARM workstation.
awk '
  /^CPU implementer[[:space:]]*:/ { print "CPU implementer\t: 0x41"; next }
  /^CPU part[[:space:]]*:/ { print "CPU part\t: 0xd0c"; next }
  { print }
' /proc/cpuinfo >"$VIEW/cpuinfo"
printf '%s\n' 0x00000000410fd0c0 >"$VIEW/midr_el1"
chmod 0444 "$VIEW/cpuinfo" "$VIEW/midr_el1"

grep -vE 'lima|rosetta|cidata|virtiofs' /etc/fstab >"$VIEW/fstab"
grep -vE 'host\.lima\.internal|lima-' /etc/hosts >"$VIEW/hosts"
awk -F: '$1 != "coop-agent"' /etc/passwd >"$VIEW/passwd"
awk -F: '$1 != "coop-workspace"' /etc/group >"$VIEW/group"
printf '%s\n' devbox >"$VIEW/hostname"
cat >"$VIEW/resolv.conf" <<EOF
nameserver $GATEWAY
options timeout:2 attempts:3
EOF
chmod 0444 \
  "$VIEW/fstab" "$VIEW/hosts" "$VIEW/passwd" "$VIEW/group" \
  "$VIEW/hostname" "$VIEW/resolv.conf"

cat >/usr/local/libexec/dev-session-enter <<'SCRIPT'
#!/bin/bash
set -euo pipefail
RUNTIME=$1
TOOL_HOME=$2
AGENT_TZ=$3
shift 3
VIEW=/usr/local/share/dev-session
AGENT_UID=$(id -u developer)
AGENT_GID=$(getent group developers | cut -d: -f3)
CODEX_API_KEY=${COOP_LOCAL_API_KEY-}

mount --make-rprivate /
hostname devbox
mount -t proc proc /proc -o nosuid,nodev,noexec,hidepid=2

# Present a deliberately small sysfs rather than chasing every new transport
# path added by a kernel update. CPU and DMI data needed by common inspection
# tools are copied into the view; hardware control remains root-only outside it.
mount -t tmpfs sysfs-view /sys -o nosuid,nodev,noexec,mode=0755,size=2m
install -d -m 0755 \
  /sys/devices/virtual/dmi \
  /sys/class/dmi \
  /sys/devices/system/cpu \
  /sys/dev/block
cp -a "$VIEW/dmi" /sys/devices/virtual/dmi/id
ln -s ../../devices/virtual/dmi/id /sys/class/dmi/id
CPU_COUNT=$(grep -c '^processor[[:space:]]*:' "$VIEW/cpuinfo")
if [ "$CPU_COUNT" -gt 0 ]; then
  CPU_LAST=$((CPU_COUNT - 1))
  for state in online possible present; do
    printf '0-%s\n' "$CPU_LAST" >"/sys/devices/system/cpu/$state"
  done
  for cpu in $(seq 0 "$CPU_LAST"); do
    TOPOLOGY="/sys/devices/system/cpu/cpu$cpu/topology"
    install -d -m 0755 "$TOPOLOGY"
    CPU_BIT=$((1 << cpu))
    CPU_MASK=$(( (1 << CPU_COUNT) - 1 ))
    printf '%s\n' "$cpu" >"$TOPOLOGY/core_id"
    printf '%s\n' 0 >"$TOPOLOGY/physical_package_id"
    printf '%s\n' 0 >"$TOPOLOGY/cluster_id"
    printf '%x\n' "$CPU_BIT" >"$TOPOLOGY/thread_siblings"
    printf '%s\n' "$cpu" >"$TOPOLOGY/thread_siblings_list"
    printf '%x\n' "$CPU_BIT" >"$TOPOLOGY/core_cpus"
    printf '%s\n' "$cpu" >"$TOPOLOGY/core_cpus_list"
    printf '%x\n' "$CPU_MASK" >"$TOPOLOGY/core_siblings"
    printf '0-%s\n' "$CPU_LAST" >"$TOPOLOGY/core_siblings_list"
    printf '%x\n' "$CPU_MASK" >"$TOPOLOGY/cluster_cpus"
    printf '0-%s\n' "$CPU_LAST" >"$TOPOLOGY/cluster_cpus_list"
    printf '%x\n' "$CPU_MASK" >"$TOPOLOGY/package_cpus"
    printf '0-%s\n' "$CPU_LAST" >"$TOPOLOGY/package_cpus_list"
  done
fi

cp "$VIEW/cpuinfo" "$RUNTIME/cpuinfo"
chmod 0444 "$RUNTIME/cpuinfo"
mount --bind "$RUNTIME/cpuinfo" /proc/cpuinfo

grep -vE 'virtio|vsock' /proc/modules >"$RUNTIME/modules"
grep -vE '[[:space:]]vd[a-z][0-9]*$' /proc/partitions >"$RUNTIME/partitions"
grep -vE '[[:space:]]vd[a-z][0-9]*[[:space:]]' /proc/diskstats >"$RUNTIME/diskstats"
chmod 0444 "$RUNTIME/modules" "$RUNTIME/partitions" "$RUNTIME/diskstats"
mount --bind "$RUNTIME/modules" /proc/modules
mount --bind "$RUNTIME/partitions" /proc/partitions
mount --bind "$RUNTIME/diskstats" /proc/diskstats
[ -d /proc/bus/pci ] && mount -t tmpfs pci-view /proc/bus/pci -o nosuid,nodev,noexec,mode=0555,size=64k

umount -l /mnt/lima-rosetta 2>/dev/null || true
umount -l /mnt/lima-cidata 2>/dev/null || true
cp "$VIEW/fstab" "$RUNTIME/fstab"
cp "$VIEW/hosts" "$RUNTIME/hosts"
cp "$VIEW/hostname" "$RUNTIME/hostname"
cp "$VIEW/passwd" "$RUNTIME/passwd"
cp "$VIEW/group" "$RUNTIME/group"
chmod 0444 \
  "$RUNTIME/fstab" "$RUNTIME/hosts" "$RUNTIME/hostname" \
  "$RUNTIME/passwd" "$RUNTIME/group"
mount --bind "$RUNTIME/fstab" /etc/fstab
mount --bind "$RUNTIME/hosts" /etc/hosts
mount --bind "$RUNTIME/hostname" /etc/hostname
mount --bind "$RUNTIME/passwd" /etc/passwd
mount --bind "$RUNTIME/group" /etc/group
[ -d /run/cloud-init ] && mount -t tmpfs runtime-state /run/cloud-init -o nosuid,nodev,noexec,mode=0755,size=64k
[ -d /var/lib/cloud ] && mount -t tmpfs local-state /var/lib/cloud -o nosuid,nodev,noexec,mode=0755,size=64k
[ -e /run/dbus/system_bus_socket ] && mount --bind /dev/null /run/dbus/system_bus_socket

# Remap the persistent agent home and installed toolchain into neutral paths.
install -d -o root -g root -m 0700 "$RUNTIME/agent-home"
mount --bind /home/developer "$RUNTIME/agent-home"
if [ -d "$TOOL_HOME" ]; then
  install -d -o root -g root -m 0755 /opt/tooling
  mount --bind "$TOOL_HOME" /opt/tooling
fi
mount -t tmpfs home-view /home -o nosuid,nodev,mode=0755,size=64k
install -d -m 0750 /home/developer
mount --move "$RUNTIME/agent-home" /home/developer

# A private minimal /dev removes block/vsock device names while retaining the
# active PTY, cryptographic randomness, and shared memory expected by tools.
install -d -o root -g root -m 0700 "$RUNTIME/devpts"
mount --bind /dev/pts "$RUNTIME/devpts"
mount -t tmpfs tmpfs /dev -o nosuid,noexec,mode=0755,size=16m
install -d -m 0755 /dev/pts
install -d -m 1777 /dev/shm
mount --move "$RUNTIME/devpts" /dev/pts
mount -t tmpfs shm /dev/shm -o nosuid,nodev,noexec,mode=1777,size=64m
mknod -m 0666 /dev/null c 1 3
mknod -m 0666 /dev/zero c 1 5
mknod -m 0666 /dev/full c 1 7
mknod -m 0666 /dev/random c 1 8
mknod -m 0666 /dev/urandom c 1 9
mknod -m 0666 /dev/tty c 5 0
ln -s pts/ptmx /dev/ptmx
ln -s /proc/self/fd /dev/fd
ln -s /proc/self/fd/0 /dev/stdin
ln -s /proc/self/fd/1 /dev/stdout
ln -s /proc/self/fd/2 /dev/stderr
[ -e /usr/bin/sudo ] && mount --bind /dev/null /usr/bin/sudo

# The implementation itself is outside the unprivileged view as well.
install -d -o root -g root -m 0700 \
  "$RUNTIME/local-bin-lower" "$RUNTIME/local-bin-upper" "$RUNTIME/local-bin-work"
chmod 0755 "$RUNTIME/local-bin-upper"
mount --bind /usr/local/bin "$RUNTIME/local-bin-lower"
mount -t overlay local-bin-view /usr/local/bin \
  -o "lowerdir=$RUNTIME/local-bin-lower,upperdir=$RUNTIME/local-bin-upper,workdir=$RUNTIME/local-bin-work"
rm -f /usr/local/bin/lima-guestagent
CLAUDE_VERSION=$(basename "$(readlink /opt/tooling/.local/bin/claude 2>/dev/null || true)")
if [ -n "$CLAUDE_VERSION" ] && [ -x "/opt/tooling/.local/share/claude/versions/$CLAUDE_VERSION" ]; then
  ln -sfn "/opt/tooling/.local/share/claude/versions/$CLAUDE_VERSION" /usr/local/bin/claude
fi
[ -d /usr/local/share/lima ] && mount -t tmpfs local-data /usr/local/share/lima -o nosuid,nodev,noexec,mode=0755,size=64k
mount -t tmpfs service-config /etc/systemd -o nosuid,nodev,noexec,mode=0755,size=64k
mount -t tmpfs kernel-config /etc/sysctl.d -o nosuid,nodev,noexec,mode=0755,size=64k
[ -d /etc/apparmor.d ] && mount -t tmpfs security-config /etc/apparmor.d -o nosuid,nodev,noexec,mode=0755,size=64k
[ -d /etc/cdi ] && mount -t tmpfs device-config /etc/cdi -o nosuid,nodev,noexec,mode=0755,size=64k
mount -t tmpfs runtime-view /run -o nosuid,nodev,noexec,mode=0755,size=2m
install -d -m 0755 /run/systemd/resolve
cp "$VIEW/resolv.conf" /run/systemd/resolve/stub-resolv.conf
chmod 0444 /run/systemd/resolve/stub-resolv.conf
install -d -o "$AGENT_UID" -g "$AGENT_GID" -m 0700 "/run/user/$AGENT_UID"
mount -t tmpfs local-admin /usr/local/sbin -o nosuid,nodev,noexec,mode=0755,size=64k
mount -t tmpfs local-libexec /usr/local/libexec -o nosuid,nodev,noexec,mode=0755,size=64k
mount -t tmpfs local-share /usr/local/share -o nosuid,nodev,noexec,mode=0755,size=64k

if [[ "$TOOL_HOME" != /nonexistent && ${1-} == "$TOOL_HOME/"* ]]; then
  if [[ $1 == "$TOOL_HOME/.local/bin/claude" ]]; then
    TOOL_BINARY=/usr/local/bin/claude
  else
    TOOL_BINARY="/opt/tooling/${1#"$TOOL_HOME/"}"
  fi
  shift
  set -- "$TOOL_BINARY" "$@"
fi

exec setpriv --reuid="$AGENT_UID" --regid="$AGENT_GID" --init-groups \
  env -i \
    HOME=/home/developer USER=developer LOGNAME=developer \
    SHELL=/bin/bash HOSTNAME=devbox TZ="$AGENT_TZ" \
    TERM="${TERM:-xterm-256color}" COLORTERM="${COLORTERM:-truecolor}" \
    LANG="${LANG:-C.UTF-8}" \
    XDG_RUNTIME_DIR="/run/user/$AGENT_UID" \
    PATH="/opt/tooling/.local/bin:/usr/local/bin:/usr/bin:/bin" \
    COOP_LOCAL_API_KEY="$CODEX_API_KEY" \
    "$@"
SCRIPT
chmod 0755 /usr/local/libexec/dev-session-enter

cat >/usr/local/sbin/dev-session <<'SCRIPT'
#!/bin/bash
set -euo pipefail
RUNTIME=$(mktemp -d /run/.session.XXXXXX)
cleanup() { rm -rf "$RUNTIME"; }
trap cleanup EXIT HUP INT TERM
exec 3>&-
MANAGER=$1
shift
MANAGER_HOME=$(getent passwd "$MANAGER" | cut -d: -f6)
test -n "$MANAGER_HOME" && test -d "$MANAGER_HOME"
AGENT_TZ=$(cat /etc/dev-session-timezone)
CLAUDE_BINARY=$(readlink -f /usr/local/bin/claude 2>/dev/null || true)
case "$CLAUDE_BINARY" in
  /home/*/.local/*) TOOL_HOME=${CLAUDE_BINARY%%/.local/*} ;;
  *) TOOL_HOME=/nonexistent ;;
esac
unshare --mount --uts --fork --kill-child=TERM \
  /usr/local/libexec/dev-session-enter "$RUNTIME" "$TOOL_HOME" "$AGENT_TZ" "$@"
SCRIPT
chmod 0755 /usr/local/sbin/dev-session

cat >/etc/network-guard.conf <<EOF
GATEWAY=$GATEWAY
SSH_PEER=$SSH_PEER
IFACE=$IFACE
EOF
chmod 0600 /etc/network-guard.conf
cat >/usr/local/sbin/network-guard <<'SCRIPT'
#!/bin/sh
set -eu
. /etc/network-guard.conf
sysctl -qw net.ipv6.conf.all.disable_ipv6=1
sysctl -qw net.ipv6.conf.default.disable_ipv6=1
ip -4 route replace table 100 "$GATEWAY/32" dev "$IFACE" scope link
if [ -n "$SSH_PEER" ]; then
  ip -4 route replace table 100 "$SSH_PEER/32" dev "$IFACE" scope link
fi
ip -4 route replace table 100 default via "$GATEWAY" dev "$IFACE"
ip rule show priority 100 | grep -q 'lookup 100' || ip rule add priority 100 lookup 100
if command -v resolvectl >/dev/null 2>&1; then
  resolvectl dns "$IFACE" "$GATEWAY"
  resolvectl domain "$IFACE" '~.'
fi
ACTIVE=$(iptables -w -S OUTPUT | awk '/^-A OUTPUT -j COOP_EGRESS_[AB]$/ {print $4; exit}')
if [ "$ACTIVE" = COOP_EGRESS_A ]; then
  NEXT=COOP_EGRESS_B
else
  NEXT=COOP_EGRESS_A
fi
iptables -w -N "$NEXT" 2>/dev/null || true
iptables -w -F "$NEXT"
iptables -w -A "$NEXT" -o lo -j ACCEPT
iptables -w -A "$NEXT" -m conntrack --ctstate ESTABLISHED,RELATED -j ACCEPT
iptables -w -A "$NEXT" -d "$GATEWAY" -p udp --dport 53 -j ACCEPT
iptables -w -A "$NEXT" -d "$GATEWAY" -p tcp --dport 53 -j ACCEPT
for cidr in 10.0.0.0/8 100.64.0.0/10 127.0.0.0/8 169.254.0.0/16 172.16.0.0/12 192.168.0.0/16 224.0.0.0/4; do
  iptables -w -A "$NEXT" -d "$cidr" -j REJECT
done
iptables -w -A "$NEXT" -j ACCEPT
iptables -w -I OUTPUT 1 -j "$NEXT"
if [ -n "$ACTIVE" ]; then
  while iptables -w -D OUTPUT -j "$ACTIVE" 2>/dev/null; do :; done
  iptables -w -F "$ACTIVE"
  iptables -w -X "$ACTIVE"
fi
while iptables -w -D OUTPUT -j PRIVATE_EGRESS_BOOT 2>/dev/null; do :; done
iptables -w -F PRIVATE_EGRESS_BOOT 2>/dev/null || true
iptables -w -X PRIVATE_EGRESS_BOOT 2>/dev/null || true
iptables -w -P OUTPUT DROP
ip6tables -w -P OUTPUT DROP
SCRIPT
chmod 0755 /usr/local/sbin/network-guard
cat >/etc/systemd/system/network-guard.service <<'UNIT'
[Unit]
Description=Fail-closed network policy
After=network-online.target
Wants=network-online.target

[Service]
Type=oneshot
ExecStart=/usr/local/sbin/network-guard

[Install]
WantedBy=multi-user.target
UNIT
cat >/etc/systemd/system/network-guard.timer <<'UNIT'
[Unit]
Description=Reassert fail-closed network policy

[Timer]
OnBootSec=5s
OnUnitActiveSec=10s
Unit=network-guard.service

[Install]
WantedBy=timers.target
UNIT
systemctl disable --now coop-egress-lock.timer coop-egress-lock.service >/dev/null 2>&1 || true
rm -f /etc/systemd/system/coop-egress-lock.timer /etc/systemd/system/coop-egress-lock.service
rm -f /etc/coop-egress-lock.conf /usr/local/sbin/coop-egress-lock
systemctl daemon-reload
systemctl enable network-guard.service network-guard.timer >/dev/null
if ! systemctl restart network-guard.service; then
  systemctl --no-pager --full status network-guard.service >&2 || true
  journalctl --no-pager -u network-guard.service -n 80 >&2 || true
  exit 1
fi
systemctl restart network-guard.timer
"##;

/// Marker added only in memory to agent SSH sessions. It is not a proxy
/// variable and carries no endpoint or credential.
pub fn mark_session(session: &mut SshSession, cfg: &CoopConfig) {
    if cfg.private_egress.is_some() {
        session.env.set("COOP_PRIVATE_EGRESS_ACTIVE", "1");
    }
}

#[cfg(test)]
#[expect(clippy::expect_used, reason = "test failures should identify fixtures")]
mod mode_tests {
    use super::*;
    use crate::backend::{EnvForward, Hostname, SshTarget, SshUser};
    use crate::config::{ImageName, Instance, InstanceIndex, InstanceName};

    fn instance(dir: &std::path::Path) -> Instance {
        Instance {
            name: InstanceName::new("test").expect("valid instance name"),
            index: InstanceIndex::new(0).expect("valid instance index"),
            dir: dir.to_path_buf(),
            image: ImageName::new("default").expect("valid image name"),
        }
    }

    fn private_config() -> CoopConfig {
        toml::from_str(
            r#"
[private_egress]
subscription = "cmd:printf subscription"
expected_egress_ip = "cmd:printf 203.0.113.10"
entry_group = "entry-group"
entry_choice = "us-entry"
exit_group = "exit-group"
exit_choice_prefix = "los-angeles-exit"
exit_choice_suffix = ""
"#,
        )
        .expect("valid private-egress config")
    }

    #[test]
    fn instance_mode_marker_rejects_both_configuration_mismatches() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inst = instance(dir.path());
        let private = private_config();
        let ordinary = CoopConfig::default();

        assert!(ensure_instance_mode(&private, &inst).is_err());
        record_instance_mode(&private, &inst).expect("record private mode");
        ensure_instance_mode(&private, &inst).expect("matching private mode");
        assert!(ensure_instance_mode(&ordinary, &inst).is_err());

        record_instance_mode(&ordinary, &inst).expect("record ordinary mode");
        ensure_instance_mode(&ordinary, &inst).expect("matching ordinary mode");
        assert!(ensure_instance_mode(&private, &inst).is_err());
    }

    #[test]
    fn instance_mode_marker_rejects_unknown_or_corrupt_versions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let inst = instance(dir.path());
        let marker = inst.dir.join(INSTANCE_MODE_MARKER);

        std::fs::write(&marker, "version=2\n").expect("write unknown marker version");
        assert!(ensure_instance_mode(&private_config(), &inst).is_err());
        assert!(ensure_instance_mode(&CoopConfig::default(), &inst).is_err());

        std::fs::write(&marker, "").expect("write corrupt marker");
        assert!(ensure_instance_mode(&private_config(), &inst).is_err());
        assert!(ensure_instance_mode(&CoopConfig::default(), &inst).is_err());
    }

    #[test]
    fn private_egress_reserves_the_restricted_developer_account() {
        let private = private_config();
        assert!(ensure_management_user(&private, "developer").is_err());
        ensure_management_user(&private, "ubuntu").expect("distinct manager is valid");
        ensure_management_user(&CoopConfig::default(), "developer")
            .expect("ordinary networking has no restricted account");
    }

    #[test]
    fn restricted_command_preserves_only_the_managed_codex_key_without_exposing_its_value() {
        let mut env = EnvForward::default();
        env.set("COOP_PRIVATE_EGRESS_ACTIVE", "1");
        env.set("COOP_LOCAL_API_KEY", "secret-capability");
        let session = SshSession {
            target: SshTarget {
                host: Hostname::new("127.0.0.1").expect("valid host"),
                port: std::num::NonZeroU16::new(22).expect("non-zero port"),
                user: SshUser::new("ubuntu").expect("valid user"),
                key_path: std::path::PathBuf::from("/tmp/test-key"),
            },
            env,
        };

        let command = restricted_agent_command(
            &session,
            RestrictedAgent::Codex,
            "/usr/bin/codex",
            vec!["--ask".into()],
        );
        assert_eq!(
            command,
            [
                "sudo",
                "--preserve-env=COOP_LOCAL_API_KEY",
                "--",
                "/usr/local/sbin/dev-session",
                "ubuntu",
                "/usr/bin/codex",
                "--ask",
            ]
        );
        assert!(!command.iter().any(|arg| arg == "secret-capability"));

        let claude = restricted_agent_command(
            &session,
            RestrictedAgent::Claude,
            "/usr/bin/claude",
            Vec::new(),
        );
        assert!(!claude.iter().any(|arg| arg.contains("COOP_LOCAL_API_KEY")));
    }

    #[test]
    fn private_post_start_runs_in_restricted_view_and_quotes_the_hook() {
        let mut env = EnvForward::default();
        env.set("COOP_PRIVATE_EGRESS_ACTIVE", "1");
        let session = SshSession {
            target: SshTarget {
                host: Hostname::new("127.0.0.1").expect("valid host"),
                port: std::num::NonZeroU16::new(22).expect("non-zero port"),
                user: SshUser::new("ubuntu").expect("valid user"),
                key_path: std::path::PathBuf::from("/tmp/test-key"),
            },
            env,
        };

        let rendered = restricted_post_start_command(
            &session,
            "echo ready; sudo iptables -F; touch '$HOME/proof'",
        )
        .into_string();
        assert_eq!(
            rendered,
            "sudo -- /usr/local/sbin/dev-session 'ubuntu' /bin/bash -c \
             'echo ready; sudo iptables -F; touch '\\''$HOME/proof'\\'''"
        );

        let ordinary = SshSession {
            target: session.target,
            env: EnvForward::default(),
        };
        assert_eq!(
            restricted_post_start_command(&ordinary, "echo ready && touch /tmp/proof")
                .into_string(),
            "echo ready && touch /tmp/proof",
        );
    }
}
