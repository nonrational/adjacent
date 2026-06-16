#!/usr/bin/env bash
# Render the Homebrew formula for a prebuilt arm64 alpha build to stdout.
# Single source of truth for Formula/adjacent.rb — the release workflow renders this and commits the
# result; run it locally to lint the output before tagging.
#
# Usage: render-formula.sh <version> <sha256> [owner/repo]
#   version    e.g. 0.1.0-alpha.1 (no leading v)
#   sha256     hex digest of the release tarball
#   owner/repo defaults to nonrational/adjacent
set -euo pipefail

version="$1"
sha256="$2"
repo="${3:-nonrational/adjacent}"
tag="v${version}"
url="https://github.com/${repo}/releases/download/${tag}/adj-${version}-aarch64-apple-darwin.tar.gz"

cat <<RUBY
class Adjacent < Formula
  desc "Local dev-server harness so a human and an agent share one supervised server"
  homepage "https://adj.ac/ent"
  version "${version}"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "${url}"
      sha256 "${sha256}"
    end
    on_intel do
      odie "adj alpha ships Apple Silicon (arm64) binaries only for now"
    end
  end

  def install
    bin.install "adj"
  end

  def caveats
    <<~EOS
      adj is an alpha build (unsigned, ad-hoc Apple Silicon binary). Quick start:

        adj daemon                # start the supervisor (or run it at login via launchd)
        cd path/to/app && adj add .

      Optional setup (prints or uses sudo only where required):
        adj install-port-forward  # route :80/:443 to the daemon
        adj install-ca            # opt-in local HTTPS CA; run it from this binary

      HTTPS note: each 'brew upgrade adjacent' replaces the binary and invalidates the CA
      keychain ACL. Repair it with: adj install-ca --reset && adj install-ca
    EOS
  end

  test do
    assert_match "${version}", shell_output("#{bin}/adj --version")
  end
end
RUBY
