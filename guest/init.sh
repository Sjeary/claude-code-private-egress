#!/usr/bin/env bash
set -euo pipefail

# Moat guest init script.
# Runs once at boot inside the Firecracker VM to finalize configuration.

LOG_TAG="coop-init"
log() { echo "[$LOG_TAG] $*"; }

log "Starting guest initialization"

# --- Network ---
# systemd-networkd handles static IP config via /etc/systemd/network/10-eth0.network.
# Wait for network to be ready.
RETRIES=30
for i in $(seq 1 $RETRIES); do
    if ip addr show eth0 | grep -q "inet "; then
        log "Network is up (attempt $i)"
        break
    fi
    if [ "$i" -eq "$RETRIES" ]; then
        log "WARNING: Network did not come up after $RETRIES attempts"
    fi
    sleep 1
done

# Verify DNS resolution
if ! host -W 2 api.anthropic.com >/dev/null 2>&1; then
    log "WARNING: DNS resolution failed for api.anthropic.com"
fi

# --- Claude Code config ---
# Config tarball is injected via SCP after boot by the host coop.
# This block handles any additional setup if config is already present.
CLAUDE_DIR="/root/.claude"
if [ -f "$CLAUDE_DIR/.credentials.json" ]; then
    log "Claude Code credentials found"
fi

if [ -f "$CLAUDE_DIR/settings.json" ]; then
    log "Claude Code settings found"
fi

# --- Workspace ---
# Mount workspace block device if present (second virtio drive)
WORKSPACE_DEV="/dev/vdb"
WORKSPACE_DIR="/workspace"
if [ -b "$WORKSPACE_DEV" ]; then
    log "Workspace block device detected, mounting at $WORKSPACE_DIR"
    mkdir -p "$WORKSPACE_DIR"
    mount "$WORKSPACE_DEV" "$WORKSPACE_DIR"
    log "Workspace mounted"
fi

# --- Docker readiness ---
RETRIES=30
for i in $(seq 1 $RETRIES); do
    if docker info >/dev/null 2>&1; then
        log "Docker daemon is ready (attempt $i)"
        break
    fi
    if [ "$i" -eq "$RETRIES" ]; then
        log "WARNING: Docker daemon did not start after $RETRIES attempts"
    fi
    sleep 1
done

# --- SSH authorized keys ---
# If coop injected keys via the config tarball, they'll already be in
# /root/.ssh/authorized_keys. Ensure permissions are correct.
if [ -f /root/.ssh/authorized_keys ]; then
    chmod 700 /root/.ssh
    chmod 600 /root/.ssh/authorized_keys
fi

# --- Environment ---
# Source /etc/environment for ANTHROPIC_API_KEY and other vars
if [ -f /etc/environment ]; then
    set -a
    # shellcheck source=/dev/null
    . /etc/environment
    set +a
fi

# --- Headless mode setup ---
# For non-interactive use, ensure Claude Code doesn't prompt for onboarding.
CLAUDE_JSON="/root/.claude.json"
if [ ! -f "$CLAUDE_JSON" ]; then
    cat > "$CLAUDE_JSON" <<CJEOF
{
  "hasTrustDialogAccepted": true,
  "hasCompletedProjectOnboarding": true
}
CJEOF
fi

log "Guest initialization complete"
