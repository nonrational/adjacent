daemon:
  cargo run -- daemon

help:
  cargo run -- --help

build:
  cargo build

test:
  cargo test

serve:
  npx live-server

# Rebuild + re-create the local CA so the macOS keychain ACL trusts the new binary.
# A fresh `cargo build` changes the cdhash, which the CA key's ACL refuses (issue #44).
# Drops the Keychain entry, untrusts the old root, regenerates, re-trusts. One sudo prompt.
reset-ca:
  sudo -v
  cargo run -- install-ca --reset
  -sudo security delete-certificate -c 'Adjacent local' /Library/Keychains/System.keychain
  cargo run -- install-ca
  sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain ~/.adjacent/ca.crt
