#!/usr/bin/env bash
set -euo pipefail

# Set up host networking for Firecracker guest: TAP device + NAT.
#
# Usage: setup-network.sh [tap-device] [host-ip] [host-iface]
#
# Defaults:
#   tap-device: tap0
#   host-ip:    172.16.0.1/24
#   host-iface: eth0 (auto-detected if possible)
#
# Requires root privileges.

TAP_DEV="${1:-tap0}"
HOST_IP="${2:-172.16.0.1/24}"
HOST_IFACE="${3:-}"

if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: This script must be run as root."
    exit 1
fi

# Auto-detect the default route interface if not specified
if [ -z "$HOST_IFACE" ]; then
    HOST_IFACE="$(ip route show default | awk '/default/ {print $5}' | head -1)"
    if [ -z "$HOST_IFACE" ]; then
        echo "ERROR: Could not auto-detect host network interface."
        echo "Please specify it as the third argument."
        exit 1
    fi
    echo "Auto-detected host interface: $HOST_IFACE"
fi

echo "=== Setting up TAP device ==="
echo "  TAP device:      $TAP_DEV"
echo "  Host IP:         $HOST_IP"
echo "  Host interface:  $HOST_IFACE"

# Remove existing TAP device if present
if ip link show "$TAP_DEV" &>/dev/null; then
    echo "  Removing existing $TAP_DEV..."
    ip link del "$TAP_DEV"
fi

ip tuntap add "$TAP_DEV" mode tap
ip addr add "$HOST_IP" dev "$TAP_DEV"
ip link set "$TAP_DEV" up

echo "=== Enabling IP forwarding ==="
sysctl -w net.ipv4.ip_forward=1 >/dev/null

echo "=== Configuring NAT ==="
# Add rules idempotently (check before adding)
if ! iptables -t nat -C POSTROUTING -o "$HOST_IFACE" -j MASQUERADE 2>/dev/null; then
    iptables -t nat -A POSTROUTING -o "$HOST_IFACE" -j MASQUERADE
fi

if ! iptables -C FORWARD -i "$TAP_DEV" -o "$HOST_IFACE" -j ACCEPT 2>/dev/null; then
    iptables -A FORWARD -i "$TAP_DEV" -o "$HOST_IFACE" -j ACCEPT
fi

if ! iptables -C FORWARD -i "$HOST_IFACE" -o "$TAP_DEV" \
    -m state --state RELATED,ESTABLISHED -j ACCEPT 2>/dev/null; then
    iptables -A FORWARD -i "$HOST_IFACE" -o "$TAP_DEV" \
        -m state --state RELATED,ESTABLISHED -j ACCEPT
fi

echo "=== Network setup complete ==="
echo "  Guest should use: IP=172.16.0.2/24, Gateway=172.16.0.1"
