#!/usr/bin/env bash
# Berkopec remedy: drive the app through a mise task.
set -euo pipefail
if ! command -v mise >/dev/null 2>&1; then
  curl -fsSL https://mise.run | sh
fi
export PATH="$HOME/.local/bin:$PATH"
mise install ruby@3.3.6
echo "SHELL_PATH_ADD=$HOME/.local/bin"
echo "LAUNCHD_EXTRA_PATH=$HOME/.local/bin"
