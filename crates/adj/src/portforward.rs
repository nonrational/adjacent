use anyhow::Result;

use crate::proxy::{https_port, proxy_port};

/// Anchor name registered with pfctl. Lowercased + dotless for readability in `pfctl -s nat`.
const ANCHOR_NAME: &str = "adjacent";

/// Absolute path to the pf anchor file on disk. Exposed so `adj doctor` can verify the file
/// exists with the expected rule set without shelling out to pfctl (which would require root).
pub fn anchor_path() -> String {
    format!("/etc/pf.anchors/{ANCHOR_NAME}")
}

/// Expected anchor body for a given pair of daemon ports. The doctor reads
/// `/etc/pf.anchors/adjacent` and compares against this — if the daemon's ports were changed
/// (via env vars) after the rule was installed, the file would be stale and the doctor flags it.
pub fn expected_anchor_body(http_port: u16, https_port: u16) -> String {
    anchor_rules(http_port, https_port)
}

/// Print the pf anchor content and the exact sudo invocation that installs it. The daemon never
/// escalates: the user reviews and runs the printed commands themselves.
pub fn install() -> Result<()> {
    let http = proxy_port();
    let https = https_port();
    let anchor_path = format!("/etc/pf.anchors/{ANCHOR_NAME}");
    let anchor_body = anchor_rules(http, https);

    println!("# Adjacent port-forward installer");
    println!("#");
    println!("# Adjacent listens on :{http} (HTTP) and :{https} (HTTPS) — both high, unprivileged");
    println!("# ports. To make :80/:443 reach them, install a pf NAT rule. Adjacent never escalates");
    println!("# — review and run these manually.");
    println!();
    println!("# 1. Anchor file ({anchor_path}):");
    println!("# ---8<--- begin anchor ---8<---");
    // Comment-prefix the doc copy so the output stays a valid shell script when piped
    // to a file and executed. The heredoc copy below (inside `sudo sh -c …`) is raw —
    // heredoc treats it as data, not commands.
    for line in anchor_body.lines() {
        println!("# {line}");
    }
    println!("# ---8<--- end anchor ---8<---");
    println!();
    println!("# 2. Write the anchor and load it (run as root):");
    println!();
    println!("sudo sh -c 'cat > {anchor_path} <<EOF");
    print!("{anchor_body}");
    println!("EOF'");
    println!("sudo pfctl -a {ANCHOR_NAME} -f {anchor_path}");
    println!("# Note: the next command replaces the active NAT ruleset (current rules are");
    println!("# re-read via `pfctl -sn` and reloaded with the anchor prepended). If you manage");
    println!("# pf NAT rules outside /etc/pf.conf, review before running.");
    println!("(echo 'rdr-anchor \"{ANCHOR_NAME}\" all'; sudo pfctl -sn 2>/dev/null) | sudo pfctl -f -");
    println!("sudo pfctl -E");
    Ok(())
}

fn anchor_rules(http_port: u16, https_port: u16) -> String {
    format!(
        "rdr pass on lo0 inet proto tcp from any to any port 80 -> 127.0.0.1 port {http_port}\n\
         rdr pass on lo0 inet proto tcp from any to any port 443 -> 127.0.0.1 port {https_port}\n"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anchor_rules_target_loopback_and_both_ports() {
        let rules = anchor_rules(8080, 8443);
        assert!(rules.contains("port 80"));
        assert!(rules.contains("port 443"));
        assert!(rules.contains("127.0.0.1 port 8080"));
        assert!(rules.contains("127.0.0.1 port 8443"));
        assert_eq!(rules.matches("rdr").count(), 2);
    }
}
