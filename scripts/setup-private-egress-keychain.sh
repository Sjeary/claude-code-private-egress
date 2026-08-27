#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Darwin ]]; then
  echo "This helper requires macOS Keychain." >&2
  exit 1
fi

cleanup() {
  unset subscription expected_ip entry_group entry_choice exit_group exit_prefix exit_suffix timezone
  stty echo 2>/dev/null || true
}
trap cleanup EXIT INT TERM

read -r -s -p "Mihomo subscription URL: " subscription
printf '\n'
read -r -s -p "Expected public IPv4 exit: " expected_ip
printf '\n'

read -r -p "Entry selector group: " entry_group
read -r -p "Entry selector choice: " entry_choice
read -r -p "Exit selector group: " exit_group
read -r -p "Exit choice prefix (use the full name for an exact match): " exit_prefix
read -r -p "Exit choice suffix (may be empty): " exit_suffix
read -r -p "Guest timezone [America/Los_Angeles]: " timezone
timezone=${timezone:-America/Los_Angeles}

if [[ $subscription != https://* ]]; then
  echo "Subscription must use HTTPS." >&2
  exit 1
fi
if [[ ! $expected_ip =~ ^([0-9]{1,3}\.){3}[0-9]{1,3}$ ]]; then
  echo "Expected exit must be an IPv4 address." >&2
  exit 1
fi
for required in "$entry_group" "$entry_choice" "$exit_group" "$exit_prefix"; do
  if [[ -z $required || $required == *$'\n'* || $required == *$'\r'* ]]; then
    echo "Selector names and the exit prefix must be non-empty single-line values." >&2
    exit 1
  fi
done
if [[ $exit_suffix == *$'\n'* || $exit_suffix == *$'\r'* ||
      ! $timezone =~ ^[A-Za-z0-9_+-]+(/[A-Za-z0-9_+-]+)*$ ]]; then
  echo "Exit suffix must be single-line and timezone must be an IANA-style name." >&2
  exit 1
fi

toml_string() {
  local value=$1
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  printf '"%s"' "$value"
}

security add-generic-password -U -a "$USER" \
  -s coop-mihomo-subscription -w "$subscription" >/dev/null
security add-generic-password -U -a "$USER" \
  -s coop-mihomo-egress-ip -w "$expected_ip" >/dev/null

echo "Stored private egress values in macOS Keychain."

config_dir=${COOP_CONFIG_DIR:-"$HOME/.coop"}
config_file="$config_dir/config.toml"
if [[ ! -e $config_file ]]; then
  umask 077
  mkdir -p "$config_dir"
  chmod 0700 "$config_dir"
  printf '%s\n' \
    "guest_timezone = $(toml_string "$timezone")" \
    '' \
    '[private_egress]' \
    'subscription = "cmd:security find-generic-password -s coop-mihomo-subscription -w"' \
    'expected_egress_ip = "cmd:security find-generic-password -s coop-mihomo-egress-ip -w"' \
    "entry_group = $(toml_string "$entry_group")" \
    "entry_choice = $(toml_string "$entry_choice")" \
    "exit_group = $(toml_string "$exit_group")" \
    "exit_choice_prefix = $(toml_string "$exit_prefix")" \
    "exit_choice_suffix = $(toml_string "$exit_suffix")" >"$config_file"
  chmod 0600 "$config_file"
  echo "Created $config_file with Keychain-backed private egress settings."
else
  echo "$config_file already exists; it was not modified."
  echo "Use the [private_egress] cmd: references from config.example.toml."
fi
