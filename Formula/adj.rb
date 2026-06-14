class Adj < Formula
  desc "Local dev-server harness so a human and an agent share one supervised server"
  homepage "https://adj.ac/ent"
  version "0.1.0-alpha.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/nonrational/adjacent/releases/download/v0.1.0-alpha.1/adj-0.1.0-alpha.1-aarch64-apple-darwin.tar.gz"
      sha256 "0000000000000000000000000000000000000000000000000000000000000000"
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

      HTTPS note: each 'brew upgrade adj' replaces the binary and invalidates the CA
      keychain ACL. Repair it with: adj install-ca --reset && adj install-ca
    EOS
  end

  test do
    assert_match "0.1.0-alpha.1", shell_output("#{bin}/adj --version")
  end
end
