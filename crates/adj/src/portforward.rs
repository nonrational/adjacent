use anyhow::Result;

use crate::proxy::proxy_port;

/// Anchor name registered with pfctl. Lowercased + dotless for readability in `pfctl -s nat`.
const ANCHOR_NAME: &str = "adjacent";

/// Print the pf anchor content and the exact sudo invocation that installs it. The daemon never
/// escalates: the user reviews and runs the printed commands themselves.
pub fn install() -> Result<()> {
    let port = proxy_port();
    let anchor_path = format!("/etc/pf.anchors/{ANCHOR_NAME}");
    let anchor_body = anchor_rule(port);

    println!("# Adjacent port-forward installer");
    println!("#");
    println!("# Adjacent listens on :{port} (high port, unprivileged). To make :80 reach it,");
    println!("# install a pf NAT rule. Adjacent never escalates — review and run these manually.");
    println!();
    println!("# 1. Anchor file ({anchor_path}):");
    println!("# ---8<--- begin anchor ---8<---");
    print!("{anchor_body}");
    println!("# ---8<--- end anchor ---8<---");
    println!();
    println!("# 2. Write the anchor and load it (run as root):");
    println!();
    println!("sudo sh -c 'cat > {anchor_path} <<EOF");
    print!("{anchor_body}");
    println!("EOF'");
    println!("sudo pfctl -a {ANCHOR_NAME} -f {anchor_path}");
    println!("(echo 'rdr-anchor \"{ANCHOR_NAME}\" all'; sudo pfctl -sn 2>/dev/null) | sudo pfctl -f -");
    println!("sudo pfctl -E");
    Ok(())
}

fn anchor_rule(proxy_port: u16) -> String {
    format!("rdr pass on lo0 inet proto tcp from any to any port 80 -> 127.0.0.1 port {proxy_port}\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_rule_targets_loopback_and_port() {
        let rule = anchor_rule(8080);
        assert!(rule.contains("port 80"));
        assert!(rule.contains("127.0.0.1 port 8080"));
        assert!(rule.contains("rdr"));
    }
}
