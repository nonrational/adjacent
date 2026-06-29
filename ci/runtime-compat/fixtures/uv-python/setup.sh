#!/usr/bin/env bash
# Install uv + the managed CPython it will run.
set -euo pipefail
if ! command -v uv >/dev/null 2>&1; then
  curl -fsSL https://astral.sh/uv/install.sh | sh
fi
export PATH="$HOME/.local/bin:$PATH"
uv python install 3.11.9
echo "SHELL_PATH_ADD=$HOME/.local/bin"
echo "LAUNCHD_EXTRA_PATH=$HOME/.local/bin"
