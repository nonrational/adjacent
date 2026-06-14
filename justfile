daemon:
  cargo run -- daemon

help:
  cargo run -- --help

build:
  cargo build

test:
  cargo test

# Cut the next alpha by tagging v<workspace-version>-alpha.<N+1> and pushing it. The release
# workflow then builds the arm64 binary, publishes a GitHub prerelease, and updates Formula/adj.rb.
# Pushes as whoever runs it (releasing is a human action) — no auto-increment happens otherwise.
# Tag and push the next v<version>-alpha.N release.
release:
  #!/usr/bin/env bash
  set -euo pipefail
  base=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
  [ -n "$base" ] || { echo "could not read workspace version from Cargo.toml" >&2; exit 1; }
  latest=$(git tag -l "v${base}-alpha.*" | sed "s/^v${base}-alpha\.//" | sort -n | tail -1)
  next=$(( ${latest:-0} + 1 ))
  tag="v${base}-alpha.${next}"
  echo "tagging $tag"
  git tag "$tag"
  git push origin "$tag"

serve:
  npx live-server --port=8081

# Rebuild + re-create the local CA so the macOS keychain ACL trusts the new binary.
# A fresh `cargo build` changes the cdhash, which the CA key's ACL refuses (issue #44).
# Drops the Keychain entry, untrusts the old root, regenerates, re-trusts. One sudo prompt.
reset-ca:
  sudo -v
  cargo run -- install-ca --reset
  -sudo security delete-certificate -c 'Adjacent local' /Library/Keychains/System.keychain
  cargo run -- install-ca
  sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain ~/.adjacent/ca.crt
