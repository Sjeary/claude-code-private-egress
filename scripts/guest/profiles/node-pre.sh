set -euo pipefail

echo '  [guest] Adding NodeSource 22.x repository...'
curl -fsSL https://deb.nodesource.com/setup_22.x | bash -
