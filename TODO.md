# TODO

## Hardening

- Firecracker jailer for production deployments
- Resource limits and monitoring
- Seccomp filters on VMM process
- Per-tap networking (no bridge) for L2 isolation between instances

## Priority: Features

- **ProxyJump for remote VS Code** — When CLI runs on a remote Linux host, VS Code needs a two-hop SSH config with `ProxyJump` through the Linux box to reach the guest

## Mount host directories into guest (Firecracker)

**Done.** `--mount` now works on both backends:
- **Lima:** virtiofs (live bidirectional sync via Apple Virtualization.framework)
- **Firecracker:** rsync one-time sync at start. A warning is shown advising `coop push`/`coop pull` for ongoing sync.

Firecracker has no virtiofs support and no plans to add it (GitHub issue #1180, closed 2020; reconfirmed Oct 2023). The rsync approach reuses existing push/pull infrastructure with no new dependencies. If Firecracker adds vhost-user-fs support on top of their virtio-pci work, we can swap to virtiofs transparently.

## Deferred: Persistent volumes

Researched in detail but deprioritized. VMs are disposable cattle — all work product flows out via git or filesystem sync. The pain of cold caches is lower than expected because:

- Profiles pre-install packages into the golden image (pip/npm/cargo caches warm from build)
- Docker images are mostly task-specific (layers won't be reused across different projects)
- VMs typically live hours/days, not minutes — caches warm naturally during a session

If this becomes a real pain point, the implementation path is clean on both backends:

- **Firecracker:** Second block device (`/dev/vdb`). Guest init already detects and mounts it. Add a `Drive` entry to `vm.rs::build_config()`. Store volume file at `{data_dir}/volumes/{name}/data.ext4`, outside `inst.dir` so `destroy` preserves it.
- **Lima:** `limactl disk create ch-{name} --format raw --size <N>GiB`. Add `additionalDisks` to template YAML. Disk lives at `~/.lima/_disks/`, survives `limactl delete`. Must use raw format (not qcow2) for VZ compatibility.
- **Guest side:** Bind-mount subdirs from persistent volume to `/var/lib/docker`, `~/.cache/pip`, `~/.npm`, etc.
- **Destroy:** Preserve volumes by default, add `--volumes` flag to explicitly delete.

## Deferred: Network allowlisting

Restrict guest egress to a set of allowed hosts (Anthropic API, GitHub, Docker Hub, package registries, etc). Both backends are production targets so the approach must have feature parity.

Best approach: in-guest iptables rules installed via SSH after boot. Filter on `-o eth0` so Docker internal networking (docker0, container-to-container) is unaffected. A custom `CH_EGRESS` chain applied to both OUTPUT (guest processes) and FORWARD (Docker containers reaching the internet). Host resolves hostnames to IPs at start time, SSHs in as root, installs rules. Claude Code runs as non-root `ubuntu` and can't flush the rules.

On Firecracker, host-side iptables can be added as defense-in-depth (closes the boot window, protects against guest root escalation).

Config: `network.egress = "allowlist"` with a default set of hosts. Users can extend with `network.allowed_hosts`.

Limitations: CDN IP rotation (resolve-at-start may go stale for long-lived VMs), DNS tunneling (allowed resolvers could exfiltrate data), brief unrestricted window between boot and rule installation.

## Deferred: Disk shrink

Currently resize is grow-only. Firecracker shrink is feasible: `resize2fs -P` for minimum size, backup rootfs, `resize2fs` to target, `truncate`, verify with `e2fsck`. Needs `--force` flag and pre-shrink backup. Lima shrink is not worth it: requires partition surgery (losetup, resize2fs, parted, truncate, sgdisk GPT repair) plus disabling cloud-init growpart to prevent undo on next boot. Recommend grow-only for Lima, shrink for Firecracker only.

## Done: config file format and location

**Done.** Config moved from `coop.json` (CWD) to `~/.coop/config.toml` (colocated with state). TOML supports comments, matches Rust ecosystem conventions. Files with `.json` extension still parse as JSON for backward compatibility.

## Infrastructure

- **GitHub Actions CI** — Build (both targets), clippy, test. Blocked on pushing repo to GitHub
- **Rename repository** — Directory is still `claude-harness/`, rename to `coop/` when creating GitHub repo
