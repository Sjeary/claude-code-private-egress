# Claude Code Private Egress

A Claude Code-first downstream of [Trail of Bits' coop](https://github.com/trailofbits/coop).
It runs Claude Code in an isolated Linux VM on macOS and can force the agent
VM's network through a separate, fail-closed Mihomo gateway with a verified
public exit address.

The executable remains `coop` so existing VM state, configuration, and command
habits stay compatible with upstream.

## What this downstream adds

- A separate Lima gateway VM running Mihomo in strict TUN/global mode.
- An agent VM with no direct fallback route: gateway, selector, DNS, or exit-IP
  verification failure leaves it offline.
- Explicit entry and exit selectors for provider-neutral, location-pinned
  routing, plus an expected public IPv4 check.
- A managed guest timezone, defaulting to `America/Los_Angeles` in the setup
  helper.
- A restricted `developer` account for Claude Code and project hooks, without
  `sudo` or Docker-group access and with a reduced VM fingerprint surface.
- Secret subscription and exit-IP values resolved from macOS Keychain rather
  than stored in the repository or agent VM.

Private egress is currently supported on macOS with Apple Silicon and Lima.
The upstream Firecracker/Linux backend remains in the codebase, but it does not
provide this downstream's Mihomo gateway mode.

## Install from source

Install [Rust](https://rustup.rs/) and Lima (`brew install lima`), then:

```shell
git clone https://github.com/Sjeary/claude-code-private-egress.git
cd claude-code-private-egress
cargo build --release --workspace
install -m 0755 target/release/coop "$HOME/.local/bin/coop"
install -m 0755 target/release/coop-proxy "$HOME/.local/bin/coop-proxy"
```

The downstream does not advertise a binary installer until it has published a
signed release. Release builds use the repository-specific installer and
attestation chain already present in this source tree.

## Configure private egress

Run the local helper. Secret prompts do not echo; secrets go to macOS Keychain.
Selector names and timezone go to the owner-only `~/.coop/config.toml`:

```shell
./scripts/setup-private-egress-keychain.sh
coop validate
coop setup
```

The selector names come from your Mihomo subscription. For a fixed exit, enter
its complete node name as `exit_choice`. Prefix/suffix matching remains
available for providers that change a middle component of the node name.
Startup also compares the observed public IPv4 with the expected value you
supplied.

## Use Claude Code

From the host terminal, enter the project you want Claude Code to edit:

```shell
cd /path/to/your/project
coop up . --copy
coop claude
```

`coop up` creates or reconnects the project VM and copies the repository to
`/workspace`. `coop claude` launches Claude Code as the restricted agent user
inside that VM. Stop and resume the same environment with:

```shell
coop stop
coop start
coop claude
```

Use `coop shell` only for VM administration. It opens the management account,
so manually launching Claude from that shell does not provide the restricted
agent view.

## Security boundary

With `--copy` or `--git-repo`, this design isolates the agent from the macOS
host filesystem and removes an agent-VM fallback to the host's ordinary
network. Private-egress mode rejects `--mount`, `--extra-mount`, and
devcontainer host bind mounts because writable Lima virtiofs would cross that
boundary. It is not an anonymity guarantee: the Mihomo provider and destination
services can observe traffic metadata, and kernel-level or timing inspection
can still identify virtualization. See the
[trust model](docs/trust-model.md) and
[private-egress reference](docs/configuration.md#private-egress) for the exact
guarantees and limitations.

## Documentation

- [Getting started](docs/getting-started.md)
- [Command reference](docs/commands.md)
- [Configuration reference](docs/configuration.md)
- [Claude Code integration](docs/claude-integration.md)
- [Architecture](docs/ARCHITECTURE.md)
- [Downstream policy and attribution](DOWNSTREAM.md)
- [Security policy](SECURITY.md)
