#!/usr/bin/env bash
set -euo pipefail

if [[ $(uname -s) != Darwin ]]; then
  echo "This helper requires macOS Keychain." >&2
  exit 1
fi

cleanup() {
  if [[ -n ${tmp_config:-} && -e $tmp_config ]]; then
    rm -f -- "$tmp_config"
  fi
  unset subscription expected_ip entry_group entry_choice exit_group exit_choice timezone
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
read -r -p "Exact exit selector choice: " exit_choice
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
for octet in ${expected_ip//./ }; do
  if (( 10#$octet > 255 )); then
    echo "Expected exit must be an IPv4 address." >&2
    exit 1
  fi
done
for required in "$entry_group" "$entry_choice" "$exit_group" "$exit_choice"; do
  if [[ -z $required || $required == *$'\n'* || $required == *$'\r'* ]]; then
    echo "Selector names must be non-empty single-line values." >&2
    exit 1
  fi
done
if [[ ! $timezone =~ ^[A-Za-z0-9_+-]+(/[A-Za-z0-9_+-]+)*$ ||
      ! -e /usr/share/zoneinfo/$timezone ]]; then
  echo "Timezone must name an installed IANA timezone." >&2
  exit 1
fi

toml_string() {
  local value=$1
  value=${value//\\/\\\\}
  value=${value//\"/\\\"}
  printf '"%s"' "$value"
}

shell_quote() {
  local value=$1
  value=${value//\'/\'\\\'\'}
  printf "'%s'" "$value"
}

security add-generic-password -U -a "$USER" \
  -s coop-mihomo-subscription -w "$subscription" >/dev/null
security add-generic-password -U -a "$USER" \
  -s coop-mihomo-egress-ip -w "$expected_ip" >/dev/null

echo "Stored private egress values in macOS Keychain."

config_dir=${COOP_CONFIG_DIR:-"$HOME/.coop"}
config_file="$config_dir/config.toml"
umask 077
mkdir -p "$config_dir"
chmod 0700 "$config_dir"

account=$(shell_quote "$USER")
subscription_cmd="cmd:security find-generic-password -a $account -s coop-mihomo-subscription -w"
expected_ip_cmd="cmd:security find-generic-password -a $account -s coop-mihomo-egress-ip -w"

write_private_egress() {
  printf '%s\n' \
    '[private_egress]' \
    "subscription = $(toml_string "$subscription_cmd")" \
    "expected_egress_ip = $(toml_string "$expected_ip_cmd")" \
    "entry_group = $(toml_string "$entry_group")" \
    "entry_choice = $(toml_string "$entry_choice")" \
    "exit_group = $(toml_string "$exit_group")" \
    "exit_choice = $(toml_string "$exit_choice")"
}

tmp_config=$(mktemp "$config_dir/.config.toml.XXXXXX")
if [[ -e $config_file ]]; then
  {
    printf 'guest_timezone = %s\n' "$(toml_string "$timezone")"
    awk '
      BEGIN { at_root = 1; in_private = 0 }
      {
        if (in_private) {
          if ($0 ~ /^[[:space:]]*\[/) { in_private = 0 } else { next }
        }
        if ($0 ~ /^[[:space:]]*\[private_egress\][[:space:]]*(#.*)?$/) {
          in_private = 1
          at_root = 0
          next
        }
        if (at_root && $0 ~ /^[[:space:]]*guest_timezone[[:space:]]*=/) { next }
        if ($0 ~ /^[[:space:]]*\[/) { at_root = 0 }
        print
      }
    ' "$config_file"
    printf '\n'
    write_private_egress
  } >"$tmp_config"
  mv "$tmp_config" "$config_file"
  chmod 0600 "$config_file"
  echo "Updated private egress settings in $config_file."
else
  {
    printf 'guest_timezone = %s\n\n' "$(toml_string "$timezone")"
    write_private_egress
  } >"$tmp_config"
  mv "$tmp_config" "$config_file"
  chmod 0600 "$config_file"
  echo "Created $config_file with Keychain-backed private egress settings."
fi
