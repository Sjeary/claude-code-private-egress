#!/usr/bin/env bash
set -euo pipefail

# Fetch a prebuilt Firecracker-compatible Linux kernel from AWS.
#
# Usage: fetch-kernel.sh <output-path>
#
# Downloads the kernel binary to the specified path. Uses the latest
# Firecracker CI kernel (x86_64, Linux 6.1).

OUTPUT="${1:-.coop/vmlinux}"
OUTPUT_DIR="$(dirname "$OUTPUT")"

# Firecracker CI kernel — stable, tested, minimal config
KERNEL_URL="https://s3.amazonaws.com/spec.ccfc.min/ci-artifacts/kernels/x86_64/vmlinux-6.1.102"
KERNEL_SHA256="24c9a94bbeef2b09d49e067ba586e1d2d3c0f23a4fb6f63db25c8e3cdcce0ecb"

ARCH="$(uname -m)"
if [ "$ARCH" = "aarch64" ] || [ "$ARCH" = "arm64" ]; then
    KERNEL_URL="https://s3.amazonaws.com/spec.ccfc.min/ci-artifacts/kernels/aarch64/vmlinux-6.1.102"
    # SHA for aarch64 differs; verify manually on first use
    KERNEL_SHA256=""
fi

mkdir -p "$OUTPUT_DIR"

if [ -f "$OUTPUT" ]; then
    echo "Kernel already exists at $OUTPUT"
    exit 0
fi

echo "Downloading Firecracker kernel..."
echo "  URL: $KERNEL_URL"
echo "  Output: $OUTPUT"

curl -fSL -o "$OUTPUT" "$KERNEL_URL"

if [ -n "$KERNEL_SHA256" ]; then
    echo "Verifying checksum..."
    ACTUAL_SHA256="$(sha256sum "$OUTPUT" | awk '{print $1}')"
    if [ "$ACTUAL_SHA256" != "$KERNEL_SHA256" ]; then
        echo "ERROR: Checksum mismatch!"
        echo "  Expected: $KERNEL_SHA256"
        echo "  Actual:   $ACTUAL_SHA256"
        rm -f "$OUTPUT"
        exit 1
    fi
    echo "Checksum verified"
else
    echo "WARNING: No checksum available for $ARCH — verify manually"
fi

echo "Kernel downloaded to $OUTPUT"
