#!/usr/bin/env bash
set -euo pipefail

repo_root=$(cd "$(dirname "$0")/.." && pwd)
fixture=$(mktemp -d)
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/bin" "$fixture/config"

cat >"$fixture/bin/uname" <<'SCRIPT'
#!/bin/sh
printf '%s\n' Darwin
SCRIPT
cat >"$fixture/bin/security" <<'SCRIPT'
#!/bin/sh
printf '%s\n' "$*" >>"$SECURITY_CALLS"
SCRIPT
cat >"$fixture/bin/stty" <<'SCRIPT'
#!/bin/sh
exit 0
SCRIPT
chmod +x "$fixture/bin/uname" "$fixture/bin/security" "$fixture/bin/stty"

run_helper() {
  env \
    USER=fixture-user \
    HOME="$fixture/home" \
    COOP_CONFIG_DIR="$fixture/config" \
    SECURITY_CALLS="$fixture/security.calls" \
    PATH="$fixture/bin:$PATH" \
    bash "$repo_root/scripts/setup-private-egress-keychain.sh"
}

printf '%s\n' \
  'https://provider.invalid/secret-one' \
  '203.0.113.10' \
  'entry-group' \
  'us-entry' \
  'exit-group' \
  'los-angeles-exit' \
  '' | run_helper >/dev/null

config="$fixture/config/config.toml"
grep -Fq 'guest_timezone = "America/Los_Angeles"' "$config"
grep -Fq 'exit_choice = "los-angeles-exit"' "$config"
grep -Fq -- "-a 'fixture-user' -s coop-mihomo-subscription -w" "$config"
! grep -Fq 'provider.invalid/secret-one' "$config"
! grep -Fq 'exit_choice_prefix' "$config"

printf '\n[claude]\nenv_forward = ["CUSTOM_TOKEN"]\n' >>"$config"
printf '%s\n' \
  'https://provider.invalid/secret-two' \
  '198.51.100.20' \
  'new-entry-group' \
  'new-entry' \
  'new-exit-group' \
  'new-los-angeles-exit' \
  'UTC' | run_helper >/dev/null

test "$(grep -Fc '[private_egress]' "$config")" -eq 1
test "$(grep -Fc 'guest_timezone =' "$config")" -eq 1
grep -Fq 'guest_timezone = "UTC"' "$config"
grep -Fq 'exit_choice = "new-los-angeles-exit"' "$config"
grep -Fq 'env_forward = ["CUSTOM_TOKEN"]' "$config"
! grep -Fq 'exit_choice = "los-angeles-exit"' "$config"
! grep -Fq 'provider.invalid/secret-two' "$config"
if [[ -n ${COOP_TEST_BIN:-} ]]; then
  "$COOP_TEST_BIN" --config "$config" validate >/dev/null
fi

calls_before=$(wc -l <"$fixture/security.calls")
if printf '%s\n' \
  'https://provider.invalid/secret-three' \
  '999.51.100.20' \
  'entry-group' \
  'entry' \
  'exit-group' \
  'exit' \
  'UTC' | run_helper >/dev/null 2>&1; then
  echo 'invalid IPv4 unexpectedly succeeded' >&2
  exit 1
fi
test "$(wc -l <"$fixture/security.calls")" -eq "$calls_before"
