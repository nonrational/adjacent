#!/usr/bin/env bash
# Install asdf (classic) + nodejs plugin + pinned node (prebuilt download).
set -euo pipefail
export ASDF_DIR="$HOME/.asdf"
if [ ! -d "$ASDF_DIR" ]; then
  git clone --depth 1 --branch v0.14.1 https://github.com/asdf-vm/asdf.git "$ASDF_DIR"
fi
# shellcheck source=/dev/null
. "$ASDF_DIR/asdf.sh"
asdf plugin add nodejs 2>/dev/null || true
asdf install nodejs 18.20.5
asdf reshim nodejs
echo "SHELL_PATH_ADD=$ASDF_DIR/shims:$ASDF_DIR/bin"
echo "LAUNCHD_EXTRA_PATH="
