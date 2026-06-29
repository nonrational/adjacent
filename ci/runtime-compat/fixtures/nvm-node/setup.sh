#!/usr/bin/env bash
# nvm is a shell function: no shim, no per-dir resolution under sh -c.
set -euo pipefail
export NVM_DIR="$HOME/.nvm"
if [ ! -s "$NVM_DIR/nvm.sh" ]; then
  git clone --depth 1 https://github.com/nvm-sh/nvm.git "$NVM_DIR"
fi
# shellcheck source=/dev/null
. "$NVM_DIR/nvm.sh"
nvm install 22 >/dev/null
nvm install 18.20.5 >/dev/null
nvm alias default 22 >/dev/null
# Export the default node's bin dir (NOT a per-dir shim) for the shell context.
echo "SHELL_PATH_ADD=$(dirname "$(nvm which default)")"
echo "LAUNCHD_EXTRA_PATH="
