//! `adj doctor` — verify the two pieces of Adjacent that the user explicitly installs:
//! the pf port-forward rule and the local CA. Every check is rootless. Network probes shell
//! out to /usr/bin/curl (ships with macOS, already a runtime dep for the integration tests)
//! and rely on a reserved `__adj_verify__.adj.ac` marker handler the proxy short-circuits to
//! before the boot gate — so a probe never accidentally spawns a registered app.
//!
//! Test ergonomics: the "public" ports the doctor probes (defaults 80/443) are overridable via
//! `ADJACENT_DOCTOR_HTTP_PORT` / `ADJACENT_DOCTOR_HTTPS_PORT` so the integration test can point
//! them at the high random ports the sandbox binds the daemon to. In a real install the user
//! never sets these — pf is the whole point of :80/:443.
use std::fs;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;

use crate::portforward;
use crate::proxy::{
    https_port as daemon_https_port, proxy_port as daemon_http_port, VERIFY_BODY, VERIFY_HOST,
};
use crate::tls;

const DOCTOR_HTTP_PORT_ENV: &str = "ADJACENT_DOCTOR_HTTP_PORT";
const DOCTOR_HTTPS_PORT_ENV: &str = "ADJACENT_DOCTOR_HTTPS_PORT";

fn doctor_http_port() -> u16 {
    std::env::var(DOCTOR_HTTP_PORT_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(80)
}

fn doctor_https_port() -> u16 {
    std::env::var(DOCTOR_HTTPS_PORT_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(443)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Status {
    Pass,
    Fail,
    Skip,
}

struct Check {
    name: String,
    status: Status,
    detail: String,
}

impl Check {
    fn pass(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Pass,
            detail: detail.into(),
        }
    }
    fn fail(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Fail,
            detail: detail.into(),
        }
    }
    fn skip(name: impl Into<String>, detail: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            status: Status::Skip,
            detail: detail.into(),
        }
    }
}

pub fn run() -> Result<()> {
    let port_forward = port_forward_checks();
    let ca = ca_checks();
    print_section("Port Forwarding", &port_forward);
    println!();
    print_section("TLS Certificate Authority", &ca);
    let failed = port_forward
        .iter()
        .chain(ca.iter())
        .any(|c| c.status == Status::Fail);
    if failed {
        // 2 distinguishes "the doctor ran and found problems" from the generic adj-error 1.
        std::process::exit(2);
    }
    Ok(())
}

fn print_section(title: &str, checks: &[Check]) {
    println!("{title}");
    for c in checks {
        let tag = match c.status {
            Status::Pass => "*GOOD",
            Status::Fail => "!FAIL",
            Status::Skip => "-SKIP",
        };
        if c.detail.is_empty() {
            println!("  {tag} {}", c.name);
        } else {
            println!("  {tag} {} — {}", c.name, c.detail);
        }
    }
}

fn port_forward_checks() -> Vec<Check> {
    vec![
        check_anchor_file(),
        check_marker(
            "http",
            doctor_http_port(),
            CaTrust::None,
            format!("HTTP :{} → daemon", doctor_http_port()),
        ),
        check_marker(
            "https",
            doctor_https_port(),
            CaTrust::Insecure,
            format!("HTTPS :{} → daemon", doctor_https_port()),
        ),
    ]
}

fn ca_checks() -> Vec<Check> {
    vec![
        check_ca_cert(),
        check_ca_keychain(),
        check_ca_sign_canary(),
        check_system_trust(),
    ]
}

fn check_anchor_file() -> Check {
    let path = PathBuf::from(portforward::anchor_path());
    let name = format!("pf anchor file {}", path.display());
    if !path.exists() {
        return Check::fail(
            name,
            "not installed — run `adj install-port-forward` and follow the printed sudo steps",
        );
    }
    let body = match fs::read_to_string(&path) {
        Ok(b) => b,
        Err(e) => return Check::fail(name, format!("unreadable: {e}")),
    };
    // The user might have run install-port-forward, then later changed ADJACENT_PROXY_PORT /
    // ADJACENT_HTTPS_PORT — the file on disk would then route to stale ports. Compare against
    // what we'd write today.
    let expected = portforward::expected_anchor_body(daemon_http_port(), daemon_https_port());
    if body.trim() == expected.trim() {
        Check::pass(name, "matches expected rdr rules")
    } else {
        Check::fail(
            name,
            "contents differ from expected `rdr` rules — re-run `adj install-port-forward`",
        )
    }
}

#[derive(Clone, Copy)]
enum CaTrust {
    /// No TLS in play (HTTP probe).
    None,
    /// Skip verification entirely (`-k`). Use when we only care that something speaks TLS on
    /// the port — the marker handler proves the responder is the daemon regardless of trust.
    Insecure,
    /// Default system trust store. Passes only when the CA has been added with
    /// `security add-trusted-cert` to a system or login keychain.
    System,
}

fn check_marker(scheme: &str, port: u16, ca: CaTrust, label: String) -> Check {
    let url = format!("{scheme}://{VERIFY_HOST}:{port}/");
    let mut cmd = Command::new("/usr/bin/curl");
    cmd.arg("-sS")
        .arg("--max-time")
        .arg("3")
        .arg("--resolve")
        .arg(format!("{VERIFY_HOST}:{port}:127.0.0.1"));
    match ca {
        CaTrust::None | CaTrust::System => {}
        CaTrust::Insecure => {
            cmd.arg("-k");
        }
    }
    cmd.arg(&url);
    let output = match cmd.output() {
        Ok(o) => o,
        Err(e) => return Check::fail(label, format!("could not invoke curl: {e}")),
    };
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Check::fail(label, format!("curl failed: {err}"));
    }
    let body = String::from_utf8_lossy(&output.stdout);
    if body == VERIFY_BODY {
        Check::pass(label, "marker echoed by daemon")
    } else {
        Check::fail(
            label,
            format!("unexpected response body: {:?}", body.trim_end()),
        )
    }
}

fn check_ca_cert() -> Check {
    let name = "ca.crt parses with nameConstraints DNS:adj.ac";
    let path = match tls::ca_cert_path() {
        Ok(p) => p,
        Err(e) => return Check::fail(name, format!("resolving ca.crt path: {e}")),
    };
    if !path.exists() {
        return Check::fail(name, "not generated — run `adj install-ca`");
    }
    let pem = match fs::read(&path) {
        Ok(b) => b,
        Err(e) => return Check::fail(name, format!("reading {}: {e}", path.display())),
    };
    let (_, parsed) = match x509_parser::pem::parse_x509_pem(&pem) {
        Ok(p) => p,
        Err(e) => return Check::fail(name, format!("not a PEM cert: {e}")),
    };
    let cert = match parsed.parse_x509() {
        Ok(c) => c,
        Err(e) => return Check::fail(name, format!("not a valid X.509 cert: {e}")),
    };
    let nc = match cert.name_constraints() {
        Ok(Some(nc)) => nc,
        Ok(None) => return Check::fail(name, "missing nameConstraints extension"),
        Err(e) => return Check::fail(name, format!("parsing nameConstraints: {e}")),
    };
    let permitted = nc.value.permitted_subtrees.as_deref().unwrap_or(&[]);
    let scoped = permitted.iter().any(
        |s| matches!(&s.base, x509_parser::extensions::GeneralName::DNSName(n) if *n == "adj.ac"),
    );
    if !scoped {
        return Check::fail(name, "nameConstraints does not permit adj.ac");
    }
    let expiry = cert.validity().not_after.to_datetime();
    Check::pass(name, format!("expires {expiry}"))
}

#[cfg(target_os = "macos")]
fn check_ca_keychain() -> Check {
    let label = tls::ca_keychain_label().unwrap_or_else(|_| "Adjacent local CA".into());
    let name = format!("login keychain entry `{label}` loadable");
    match tls::load_keychain_ca() {
        Ok(Some(_)) => Check::pass(name, "found and ACL admits the current binary"),
        Ok(None) => Check::fail(name, "no keychain entry — run `adj install-ca`"),
        // The most common failure here is the issue-#44 cdhash drift: the key exists, but the
        // ACL trusts the binary that created it, not the current `cargo build` output. Surface
        // the underlying error so the user sees the actual diagnostic.
        Err(e) => Check::fail(name, format!("load failed: {e:#}")),
    }
}

#[cfg(not(target_os = "macos"))]
fn check_ca_keychain() -> Check {
    Check::skip(
        "login keychain entry loadable",
        "keychain backend is macOS-only",
    )
}

#[cfg(target_os = "macos")]
fn check_ca_sign_canary() -> Check {
    let name = "keychain key signs (cdhash ACL canary)";
    let handle = match tls::load_keychain_ca() {
        Ok(Some(h)) => h,
        Ok(None) => return Check::skip(name, "no keychain entry to test"),
        Err(_) => return Check::skip(name, "keychain entry not loadable (see above)"),
    };
    match handle.sign_canary() {
        Ok(()) => Check::pass(name, "SecKeyCreateSignature succeeded"),
        Err(e) => Check::fail(name, format!("{e:#}")),
    }
}

#[cfg(not(target_os = "macos"))]
fn check_ca_sign_canary() -> Check {
    Check::skip("keychain key signs", "keychain backend is macOS-only")
}

fn check_system_trust() -> Check {
    let port = doctor_https_port();
    // System-default trust store + marker probe: the marker proves the daemon is what we hit,
    // and curl's exit status proves macOS treats the local CA as a root. If you added the trust
    // anchor with `security add-trusted-cert`, this passes; otherwise it fails with curl's
    // certificate-verification error.
    check_marker(
        "https",
        port,
        CaTrust::System,
        format!("HTTPS :{port} validates under system trust"),
    )
}
