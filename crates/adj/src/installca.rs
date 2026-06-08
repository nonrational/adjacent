use anyhow::Result;

use crate::tls;

/// Print the local CA install banner. Generates the CA on disk if missing. Adjacent never
/// escalates: the user reviews and runs the printed sudo command themselves.
pub fn install() -> Result<()> {
    let ca_cert = tls::ca_cert_path()?;
    let ca_key = tls::ca_key_path()?;

    let just_generated = if !tls::ca_exists()? {
        tls::generate_ca()?;
        true
    } else {
        false
    };

    println!("# Adjacent local CA installer");
    println!("#");
    println!("# Adjacent serves HTTPS with a wildcard cert (`*.adj.ac`, `adj.ac`) signed by a");
    println!("# local root CA. To make browsers and curl trust it, install the CA cert into the");
    println!("# system trust store. Adjacent never escalates — review and run the command below.");
    println!();
    if just_generated {
        println!("# 1. CA generated at:");
    } else {
        println!("# 1. Existing CA at:");
    }
    println!("#    {}", ca_cert.display());
    println!("#    {} (mode 0600)", ca_key.display());
    println!();
    println!("# 2. Install into the macOS system keychain (run as root):");
    println!();
    println!(
        "sudo security add-trusted-cert -d -r trustRoot -k /Library/Keychains/System.keychain {}",
        ca_cert.display()
    );
    println!();
    println!("# 3. To remove later:");
    println!();
    println!(
        "sudo security delete-certificate -c 'Adjacent local' /Library/Keychains/System.keychain"
    );
    Ok(())
}
