set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
export DPKG_OPTIONS='--force-confnew'
APT_OPTS=(-o Dpkg::Options::=--force-confnew)

mkdir -p /var/cache/apt/archives/partial /var/lib/dpkg/updates /var/lib/dpkg/info /var/log/apt
touch /var/lib/dpkg/status 2>/dev/null || true

cat > /usr/sbin/policy-rc.d <<'POLICY'
#!/bin/sh
exit 101
POLICY
chmod +x /usr/sbin/policy-rc.d
dpkg-divert --local --rename --add /sbin/initctl 2>/dev/null || true
ln -sf /bin/true /sbin/initctl 2>/dev/null || true

echo '  [guest] Updating package lists...'
apt-get update -qq
