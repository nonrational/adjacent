#!/usr/bin/env bash
# mise exec wrapper: needs the mise binary reachable, not its shims.
set -euo pipefail
if ! command -v mise >/dev/null 2>&1; then
  curl -fsSL https://mise.run | sh
fi
export PATH="$HOME/.local/bin:$PATH"
mise install node@18.20.5
echo "SHELL_PATH_ADD=$HOME/.local/bin"
# launchd: expose ONLY the mise binary dir on the bare PATH.
echo "LAUNCHD_EXTRA_PATH=$HOME/.local/bin"
