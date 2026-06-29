#!/usr/bin/env bash
# Install mise + pinned python via shims. Echoes PATH additions for the shell context.
set -euo pipefail
# mise's python-build-standalone attestation verification fails in clean CI
# environments (no GitHub OIDC token / network path to attestation endpoint).
export MISE_PYTHON_GITHUB_ATTESTATIONS=false
HERE="$(cd "$(dirname "$0")" && pwd)"

if ! command -v mise >/dev/null 2>&1; then
  curl -fsSL https://mise.run | sh
fi
export PATH="$HOME/.local/bin:$PATH"
mise install python@3.11.9
mise reshim

# The shim dir on PATH is mise's non-activation resolution path.
echo "SHELL_PATH_ADD=$HOME/.local/share/mise/shims:$HOME/.local/bin"
# launchd context: nothing extra — a bare PATH must fall back.
echo "LAUNCHD_EXTRA_PATH="
