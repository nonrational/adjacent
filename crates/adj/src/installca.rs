use anyhow::Result;

use crate::tls;

/// Implements `adj install-ca --reset`. Wipes the Keychain-resident CA key plus on-disk cert
/// and leaf files, then prints the (still user-driven) untrust command. Trust-anchor removal
/// requires sudo, so we never run it ourselves.
pub fn reset() -> Result<()> {
    let label = tls::ca_keychain_label()?;
    tls::delete_ca()?;
    println!("# Adjacent local CA — reset");
    println!("#");
    println!("# Removed:");
    println!("#   - Keychain key (login keychain, label \"{label}\")");
    println!("#   - {}", tls::ca_cert_path()?.display());
    println!("#   - {}", tls::leaf_cert_path()?.display());
    println!("#   - {}", tls::leaf_key_path()?.display());
    println!("#");
    println!("# Still trusted in the system keychain — run this to untrust:");
    println!();
    println!(
        "sudo security delete-certificate -c 'Adjacent local' /Library/Keychains/System.keychain"
    );
    Ok(())
}

/// Print the local CA install banner. Generates the CA on disk + login keychain if missing,
/// detects a pre-Keychain install (private key still sitting at `~/.adjacent/ca.key`) and prints
/// a cleanup script before regenerating. Adjacent never escalates: the user reviews and runs
/// every printed sudo command themselves.
pub fn install() -> Result<()> {
    let ca_cert = tls::ca_cert_path()?;
    let legacy_key = tls::legacy_ca_key_path()?;

    // Old installs left a private key at `~/.adjacent/ca.key`. The new code never reads or
    // writes that file, but leaving it on disk preserves exactly the threat we moved into the
    // Keychain to fix — print a removal script and bail before generating so the user
    // explicitly scrubs it.
    if legacy_key.exists() {
        println!("# Adjacent local CA — migration required");
        println!("#");
        println!("# A legacy on-disk private key is present at:");
        println!("#    {}", legacy_key.display());
        println!("#");
        println!("# The CA now lives in your macOS login keychain (non-extractable: cannot be");
        println!("# exported through Keychain Access) and the cert is name-constrained to");
        println!("# `*.adj.ac`. To migrate, run the cleanup script below, then rerun");
        println!("# `adj install-ca` to generate a fresh, Keychain-backed CA.");
        println!();
        println!("# 1. Untrust the old root in the system keychain:");
        println!();
        println!(
            "sudo security delete-certificate -c 'Adjacent local' /Library/Keychains/System.keychain"
        );
        println!();
        println!("# 2. Remove the old on-disk material:");
        println!();
        println!(
            "rm -f {} {} {} {}",
            ca_cert.display(),
            legacy_key.display(),
            tls::leaf_cert_path()?.display(),
            tls::leaf_key_path()?.display()
        );
        println!();
        println!("# 3. Rerun `adj install-ca` to generate the new CA.");
        return Ok(());
    }

    let just_generated = if !tls::ca_exists()? {
        tls::generate_ca()?;
        true
    } else {
        false
    };
    let label = tls::ca_keychain_label()?;

    println!("# Adjacent local CA installer");
    println!("#");
    println!("# Adjacent serves HTTPS with a wildcard cert (`*.adj.ac`, `adj.ac`) signed by a");
    println!("# local root CA. The CA private key lives in your macOS login keychain and is");
    println!("# marked non-extractable — Keychain Access cannot export it, and `cat`/backup");
    println!("# tools won't find it on disk. The cert carries a critical nameConstraints");
    println!("# extension permitting only `adj.ac`, so even if the CA is misused it cannot");
    println!("# mint trusted certs for other domains. Adjacent never escalates — review and");
    println!("# run the command below.");
    println!();
    if just_generated {
        println!("# 1. CA generated:");
    } else {
        println!("# 1. Existing CA:");
    }
    println!("#    cert: {}", ca_cert.display());
    println!("#    key:  login keychain (label \"{label}\")");
    println!();
    println!("# 2. Install into the macOS system keychain (run as root):");
    println!();
    println!(
        "sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {}",
        ca_cert.display()
    );
    println!();
    println!("# 3. To remove later, run `adj install-ca --reset` and then:");
    println!();
    println!(
        "sudo security delete-certificate -c 'Adjacent local' /Library/Keychains/System.keychain"
    );
    Ok(())
}
