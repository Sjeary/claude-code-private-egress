set -euo pipefail

echo '  [guest] Cleaning up...'
apt-get clean
rm -rf /var/lib/apt/lists/* /tmp/* /var/tmp/*
rm -f /usr/sbin/policy-rc.d
dpkg-divert --local --rename --remove /sbin/initctl 2>/dev/null || true
rm -f /sbin/initctl 2>/dev/null || true

echo '  [guest] Package installation complete'
