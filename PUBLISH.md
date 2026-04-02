# Publish checklist

Tasks to complete before making coop public, ordered by priority.

## Pre-publish (no GitHub required)

- [x] Add LICENSE file (Apache-2.0)
- [x] Write README.md (what coop is, 30-second demo, install, link to docs)
- [x] Add example config (`config.example.toml` with commented defaults)
- [x] Wire `--version` flag in clap (reads from Cargo.toml)
- [x] Move tracing output from stdout to stderr
- [x] Add `coop init` command to generate starter config with comments
- [x] Document `--dangerously-skip-permissions` default in README and `coop claude --help`

## Pre-publish (requires GitHub)

- [ ] Rename repo directory from `claude-harness/` to `coop/`
- [ ] Create GitHub repo and push `main`
- [ ] Add CI workflow (clippy, fmt, test on PR/push)
- [ ] Verify install.sh works against a real release
- [ ] Tag and publish v0.1.0

## Fast follow (post-publish)

- [ ] CHANGELOG.md convention
- [ ] Contributing guide (CONTRIBUTING.md)
- [ ] GitHub issue templates
