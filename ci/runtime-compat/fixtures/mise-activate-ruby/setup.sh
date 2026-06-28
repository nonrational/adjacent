#!/usr/bin/env bash
# Berkopec profile: mise wired via the activation hook, no shims exported.
set -euo pipefail
if ! command -v mise >/dev/null 2>&1; then
  curl -fsSL https://mise.run | sh
fi
export PATH="$HOME/.local/bin:$PATH"
mise install ruby@3.3.6
# Activation hook only: emulate `mise activate` from a NON-fixture dir.
# We intentionally do NOT add mise shims to SHELL_PATH_ADD.
eval "$(mise activate bash)"
echo "SHELL_PATH_ADD=$HOME/.local/bin"
echo "LAUNCHD_EXTRA_PATH="
