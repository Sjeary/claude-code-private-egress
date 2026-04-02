set -euo pipefail

echo '  [guest] Installing Rust via rustup...'
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y --default-toolchain stable
cp /root/.cargo/bin/* /usr/local/bin/ 2>/dev/null || true
