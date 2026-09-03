# Downstream Policy

Claude Code Private Egress is an independently maintained downstream of
[Trail of Bits' coop](https://github.com/trailofbits/coop). The Git history is
preserved so the upstream source and each downstream modification remain
auditable.

## Product scope

This repository optimizes the macOS/Lima path for Claude Code with a separate
Mihomo gateway, fail-closed routing, verified exit IP, managed timezone, and a
restricted agent account. These are downstream product capabilities, not
claims about functionality or support provided by Trail of Bits.

The `coop` binary name and shared internals remain compatible with upstream to
keep security and correctness updates practical. Codex compatibility may remain
where removing it would create needless divergence, but it is not this
downstream's product focus.

## Upstream synchronization

Upstream security, correctness, dependency, and platform-compatibility fixes
are reviewed and merged into this repository. Downstream-specific features and
support requests belong here; they are not automatically proposed back to the
upstream project.

## Attribution and license

The upstream project and downstream changes are licensed under Apache License
2.0. The original `LICENSE` is retained. See `NOTICE` for attribution.
