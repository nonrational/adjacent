//! Opt-in offline DNS for `*.adj.ac`.
//!
//! By default the public authoritative DNS for `adj.ac` resolves everything to 127.0.0.1 —
//! which fails offline. The daemon therefore runs a tiny UDP DNS server that answers the same
//! way locally, and `adj install-resolver` prints the commands to point a macOS resolver hook
//! (`/etc/resolver/adj.ac`) at it. Hand-rolled rather than a DNS crate: the server speaks just
//! enough RFC 1035 to answer one fixed zone with one fixed address.

use std::net::Ipv4Addr;

use anyhow::{Context, Result};
use tokio::net::UdpSocket;

const DNS_PORT_ENV: &str = "ADJACENT_DNS_PORT";
const DEFAULT_DNS_PORT: u16 = 1053;

/// The zone we are authoritative for. The apex and every subdomain resolve to loopback.
const ZONE: &str = "adj.ac";
const LOOPBACK: Ipv4Addr = Ipv4Addr::new(127, 0, 0, 1);

/// Short TTL: answers are free (localhost UDP) and a stale cache after a config change would
/// be confusing for no win.
const TTL: u32 = 10;

const TYPE_A: u16 = 1;
const CLASS_IN: u16 = 1;
const RCODE_FORMERR: u16 = 1;
const RCODE_NXDOMAIN: u16 = 3;

pub fn dns_port() -> u16 {
    std::env::var(DNS_PORT_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_DNS_PORT)
}

/// Absolute path of the macOS resolver hook. The filename **is** the routing rule: macOS sends
/// queries for `<file name>` and its subdomains to the nameserver listed inside.
pub fn resolver_path() -> String {
    format!("/etc/resolver/{ZONE}")
}

pub fn resolver_file_body(port: u16) -> String {
    format!("nameserver 127.0.0.1\nport {port}\n")
}

/// Serve UDP DNS on 127.0.0.1. Loopback-only on purpose: this server exists solely for the
/// local resolver hook, and binding wider would advertise a wildcard-answering resolver to
/// the LAN.
pub async fn run() -> Result<()> {
    let port = dns_port();
    let socket = UdpSocket::bind(("127.0.0.1", port))
        .await
        .with_context(|| format!("binding udp 127.0.0.1:{port} for dns"))?;
    tracing::info!("dns listener on 127.0.0.1:{port} (udp)");
    // 512 bytes is the classic UDP DNS limit; our answers are far smaller and queries that
    // overflow it would be malformed for our purposes anyway.
    let mut buf = [0u8; 512];
    loop {
        let (n, peer) = match socket.recv_from(&mut buf).await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!("dns recv failed: {err}");
                continue;
            }
        };
        if let Some(resp) = build_response(&buf[..n]) {
            if let Err(err) = socket.send_to(&resp, peer).await {
                tracing::warn!("dns send to {peer} failed: {err}");
            }
        }
    }
}

/// Answer a single DNS query datagram, or `None` for packets not worth replying to.
///
/// - A/IN for the zone or any subdomain → one A record, 127.0.0.1.
/// - any other type in-zone (AAAA, HTTPS, …) → NOERROR with zero answers (NODATA), which tells
///   the resolver "this name exists but has no such record" instead of making it wait or retry.
/// - out of zone → NXDOMAIN (shouldn't happen — the resolver hook scopes queries to the zone).
fn build_response(query: &[u8]) -> Option<Vec<u8>> {
    if query.len() < 12 {
        return None;
    }
    let flags = u16::from_be_bytes([query[2], query[3]]);
    // QR bit set means this is itself a response; replying would invite reflection loops.
    if flags & 0x8000 != 0 {
        return None;
    }
    let qdcount = u16::from_be_bytes([query[4], query[5]]);
    let parsed = if qdcount == 1 { parse_question(query) } else { None };
    let Some((name, qtype, qclass, question_end)) = parsed else {
        return Some(header_only_response(query, flags, RCODE_FORMERR));
    };

    let in_zone = name == ZONE || name.ends_with(&format!(".{ZONE}"));
    let answer = in_zone && qtype == TYPE_A && qclass == CLASS_IN;
    let rcode = if in_zone { 0 } else { RCODE_NXDOMAIN };

    let mut resp = Vec::with_capacity(question_end + 16);
    resp.extend_from_slice(&query[0..2]); // id
    resp.extend_from_slice(&response_flags(flags, rcode).to_be_bytes());
    resp.extend_from_slice(&1u16.to_be_bytes()); // QDCOUNT
    resp.extend_from_slice(&(answer as u16).to_be_bytes()); // ANCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // NSCOUNT
    resp.extend_from_slice(&0u16.to_be_bytes()); // ARCOUNT
    resp.extend_from_slice(&query[12..question_end]); // echo the question
    if answer {
        // 0xC00C is a compression pointer to offset 12 — the question name we just echoed.
        resp.extend_from_slice(&[0xC0, 0x0C]);
        resp.extend_from_slice(&TYPE_A.to_be_bytes());
        resp.extend_from_slice(&CLASS_IN.to_be_bytes());
        resp.extend_from_slice(&TTL.to_be_bytes());
        resp.extend_from_slice(&4u16.to_be_bytes()); // RDLENGTH
        resp.extend_from_slice(&LOOPBACK.octets());
    }
    Some(resp)
}

/// QR=1, opcode and RD echoed from the query, AA=1 (we are the authority for the zone), RA=0.
fn response_flags(query_flags: u16, rcode: u16) -> u16 {
    0x8000 | (query_flags & 0x7800) | 0x0400 | (query_flags & 0x0100) | rcode
}

fn header_only_response(query: &[u8], flags: u16, rcode: u16) -> Vec<u8> {
    let mut resp = Vec::with_capacity(12);
    resp.extend_from_slice(&query[0..2]);
    resp.extend_from_slice(&response_flags(flags, rcode).to_be_bytes());
    resp.extend_from_slice(&[0u8; 8]); // all section counts zero
    resp
}

/// Parse the single question at offset 12: lowercased dotted name, qtype, qclass, and the
/// offset just past the question. Compression pointers are rejected — real stub resolvers
/// never compress the question, so one in a query marks the packet as garbage.
fn parse_question(packet: &[u8]) -> Option<(String, u16, u16, usize)> {
    let mut pos = 12;
    let mut name = String::new();
    loop {
        let len = *packet.get(pos)? as usize;
        if len == 0 {
            pos += 1;
            break;
        }
        if len & 0xC0 != 0 {
            return None;
        }
        let label = packet.get(pos + 1..pos + 1 + len)?;
        if !name.is_empty() {
            name.push('.');
        }
        // DNS names are case-insensitive; normalize so the zone suffix check is too.
        name.extend(label.iter().map(|b| (*b as char).to_ascii_lowercase()));
        if name.len() > 255 {
            return None;
        }
        pos += 1 + len;
    }
    let qtype = u16::from_be_bytes([*packet.get(pos)?, *packet.get(pos + 1)?]);
    let qclass = u16::from_be_bytes([*packet.get(pos + 2)?, *packet.get(pos + 3)?]);
    Some((name, qtype, qclass, pos + 4))
}

/// Print the resolver file content and the exact sudo invocation that installs it. The daemon
/// never escalates: the user reviews and runs the printed commands themselves.
pub fn install_resolver() -> Result<()> {
    let port = dns_port();
    let path = resolver_path();
    let body = resolver_file_body(port);

    println!("# Adjacent resolver installer");
    println!("#");
    println!("# *.{ZONE} normally resolves via public DNS, which fails offline. This resolver hook");
    println!("# routes {ZONE} queries to the daemon's local DNS server on 127.0.0.1:{port} instead.");
    println!("# Adjacent never escalates — review and run these manually.");
    println!();
    println!("# 1. Resolver file ({path}):");
    println!("# ---8<--- begin resolver ---8<---");
    // Comment-prefix the doc copy so the output stays a valid shell script when piped
    // to a file and executed. The heredoc copy below (inside `sudo sh -c …`) is raw —
    // heredoc treats it as data, not commands.
    for line in body.lines() {
        println!("# {line}");
    }
    println!("# ---8<--- end resolver ---8<---");
    println!();
    println!("# 2. Write the resolver file (run as root; macOS picks it up automatically):");
    println!();
    println!("sudo sh -c 'mkdir -p /etc/resolver && cat > {path} <<EOF");
    print!("{body}");
    println!("EOF'");
    println!();
    println!("# 3. Verify (with the daemon running — works offline):");
    println!("#   dscacheutil -q host -a name foo.{ZONE}");
    Ok(())
}

/// Print the inverse of `install_resolver`. Removal is also a sudo op, so same posture: print,
/// never run.
pub fn uninstall_resolver() -> Result<()> {
    let path = resolver_path();
    println!("# Remove the Adjacent resolver hook ({path}).");
    println!("# macOS notices the deletion automatically; {ZONE} goes back to public DNS.");
    println!();
    println!("sudo rm -f {path}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a_query(id: u16, name: &str, qtype: u16) -> Vec<u8> {
        let mut q = Vec::new();
        q.extend_from_slice(&id.to_be_bytes());
        q.extend_from_slice(&[0x01, 0x00]); // RD set, everything else clear
        q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // QDCOUNT=1
        for label in name.split('.') {
            q.push(label.len() as u8);
            q.extend_from_slice(label.as_bytes());
        }
        q.push(0);
        q.extend_from_slice(&qtype.to_be_bytes());
        q.extend_from_slice(&CLASS_IN.to_be_bytes());
        q
    }

    fn ancount(resp: &[u8]) -> u16 {
        u16::from_be_bytes([resp[6], resp[7]])
    }

    fn rcode(resp: &[u8]) -> u16 {
        u16::from_be_bytes([resp[2], resp[3]]) & 0x000F
    }

    #[test]
    fn subdomain_a_query_gets_loopback() {
        let resp = build_response(&a_query(0xBEEF, "foo.adj.ac", TYPE_A)).expect("response");
        assert_eq!(&resp[0..2], &0xBEEFu16.to_be_bytes());
        assert!(resp[2] & 0x80 != 0, "QR bit set");
        assert_eq!(rcode(&resp), 0);
        assert_eq!(ancount(&resp), 1);
        assert!(resp.ends_with(&[0, 4, 127, 0, 0, 1]), "A rdata is 127.0.0.1");
    }

    #[test]
    fn zone_apex_resolves_too() {
        let resp = build_response(&a_query(1, "adj.ac", TYPE_A)).expect("response");
        assert_eq!(ancount(&resp), 1);
        assert!(resp.ends_with(&[0, 4, 127, 0, 0, 1]));
    }

    #[test]
    fn name_matching_is_case_insensitive() {
        let resp = build_response(&a_query(1, "Foo.ADJ.ac", TYPE_A)).expect("response");
        assert_eq!(ancount(&resp), 1);
    }

    #[test]
    fn aaaa_in_zone_is_nodata_not_error() {
        let resp = build_response(&a_query(1, "foo.adj.ac", 28)).expect("response");
        assert_eq!(rcode(&resp), 0);
        assert_eq!(ancount(&resp), 0);
    }

    #[test]
    fn out_of_zone_is_nxdomain() {
        // "xadj.ac" exercises the suffix check: it contains "adj.ac" but is not under the zone.
        for name in ["example.com", "xadj.ac"] {
            let resp = build_response(&a_query(1, name, TYPE_A)).expect("response");
            assert_eq!(rcode(&resp), RCODE_NXDOMAIN, "{name}");
            assert_eq!(ancount(&resp), 0, "{name}");
        }
    }

    #[test]
    fn truncated_packets_get_no_reply_and_bad_questions_get_formerr() {
        assert!(build_response(&[0u8; 5]).is_none());
        // Valid header claiming one question, but no question bytes follow.
        let mut q = vec![0u8; 12];
        q[5] = 1;
        let resp = build_response(&q).expect("formerr response");
        assert_eq!(rcode(&resp), RCODE_FORMERR);
        assert_eq!(ancount(&resp), 0);
    }

    #[test]
    fn responses_are_ignored() {
        let mut q = a_query(1, "foo.adj.ac", TYPE_A);
        q[2] |= 0x80; // QR bit: this packet is a response
        assert!(build_response(&q).is_none());
    }

    #[test]
    fn resolver_file_points_at_loopback_and_port() {
        let body = resolver_file_body(1053);
        assert_eq!(body, "nameserver 127.0.0.1\nport 1053\n");
        assert_eq!(resolver_path(), "/etc/resolver/adj.ac");
    }
}
