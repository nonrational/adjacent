#!/usr/bin/env bash
# Install rbenv + ruby-build + pinned ruby (compiles; slow, cache ~/.rbenv).
set -euo pipefail
export RBENV_ROOT="$HOME/.rbenv"
if [ ! -d "$RBENV_ROOT" ]; then
  git clone --depth 1 https://github.com/rbenv/rbenv.git "$RBENV_ROOT"
  git clone --depth 1 https://github.com/rbenv/ruby-build.git "$RBENV_ROOT/plugins/ruby-build"
fi
export PATH="$RBENV_ROOT/bin:$RBENV_ROOT/shims:$PATH"
eval "$(rbenv init - bash)"
rbenv install -s 3.3.6
rbenv rehash
echo "SHELL_PATH_ADD=$RBENV_ROOT/shims:$RBENV_ROOT/bin"
echo "LAUNCHD_EXTRA_PATH="
