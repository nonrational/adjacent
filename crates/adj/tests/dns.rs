//! Integration coverage for the offline DNS path: the daemon's UDP server on its configurable
//! port, and the install/uninstall-resolver printers.

use std::net::UdpSocket;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::process::{Child, Command};

fn adj_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}

/// Bind :0, read the assigned port, close. Same accepted race as the TCP variant in the other
/// integration tests — fine for localhost test traffic.
fn pick_udp_port() -> u16 {
    let s = UdpSocket::bind("127.0.0.1:0").expect("bind udp :0");
    s.local_addr().expect("local_addr").port()
}

fn pick_tcp_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind tcp :0");
    l.local_addr().expect("local_addr").port()
}

struct Sandbox {
    _home: TempDir,
    home_path: PathBuf,
    dns_port: u16,
    daemon: Option<Child>,
}

impl Sandbox {
    fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let home_path = home.path().to_path_buf();
        Self {
            _home: home,
            home_path,
            dns_port: pick_udp_port(),
            daemon: None,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(adj_bin());
        c.env("ADJACENT_HOME", &self.home_path);
        // Random TCP ports so this suite can't collide with the other integration tests'
        // daemons (or a real one) running in parallel.
        c.env("ADJACENT_PROXY_PORT", pick_tcp_port().to_string());
        c.env("ADJACENT_HTTPS_PORT", pick_tcp_port().to_string());
        c.env("ADJACENT_DNS_PORT", self.dns_port.to_string());
        c.env("RUST_LOG", "warn");
        c
    }

    fn start_daemon(&mut self) {
        let mut c = self.cmd();
        c.arg("daemon");
        c.stdout(Stdio::null());
        c.stderr(Stdio::null());
        self.daemon = Some(c.spawn().expect("spawn daemon"));
    }

    async fn stop_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

fn a_query(id: u16, name: &str) -> Vec<u8> {
    let mut q = Vec::new();
    q.extend_from_slice(&id.to_be_bytes());
    q.extend_from_slice(&[0x01, 0x00]); // RD
    q.extend_from_slice(&[0, 1, 0, 0, 0, 0, 0, 0]); // QDCOUNT=1
    for label in name.split('.') {
        q.push(label.len() as u8);
        q.extend_from_slice(label.as_bytes());
    }
    q.push(0);
    q.extend_from_slice(&[0, 1, 0, 1]); // A, IN
    q
}

/// Resend the query until the daemon's DNS task answers or the deadline passes. Retrying the
/// send (not just the recv) doubles as boot synchronization — UDP has no connection refused we
/// could key off reliably.
fn query_until_answer(dns_port: u16, packet: &[u8]) -> Vec<u8> {
    let sock = UdpSocket::bind("127.0.0.1:0").expect("bind query socket");
    sock.set_read_timeout(Some(Duration::from_millis(250)))
        .expect("set timeout");
    let addr = format!("127.0.0.1:{dns_port}");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut buf = [0u8; 512];
    while Instant::now() < deadline {
        sock.send_to(packet, &addr).expect("send query");
        if let Ok((n, _)) = sock.recv_from(&mut buf) {
            return buf[..n].to_vec();
        }
    }
    panic!("no DNS answer from daemon within 5s");
}

#[tokio::test]
async fn daemon_answers_adj_ac_queries_on_configured_port() {
    let mut sb = Sandbox::new();
    sb.start_daemon();

    let query = a_query(0x4242, "foo.adj.ac");
    let resp = query_until_answer(sb.dns_port, &query);

    assert_eq!(&resp[0..2], &0x4242u16.to_be_bytes(), "id echoed");
    assert!(resp[2] & 0x80 != 0, "QR bit set");
    assert_eq!(resp[3] & 0x0F, 0, "NOERROR");
    assert_eq!(u16::from_be_bytes([resp[6], resp[7]]), 1, "one answer");
    assert!(
        resp.ends_with(&[0, 4, 127, 0, 0, 1]),
        "A record is 127.0.0.1, got {resp:?}"
    );

    sb.stop_daemon().await;
}

#[tokio::test]
async fn install_resolver_prints_file_and_sudo_command() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .arg("install-resolver")
        .output()
        .await
        .expect("run install-resolver");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("/etc/resolver/adj.ac"), "{stdout}");
    assert!(stdout.contains("nameserver 127.0.0.1"), "{stdout}");
    // The printed resolver file must carry the configured (sandboxed) port, not the default.
    assert!(
        stdout.contains(&format!("port {}", sb.dns_port)),
        "{stdout}"
    );
    assert!(
        stdout.contains("sudo sh -c 'mkdir -p /etc/resolver && cat > /etc/resolver/adj.ac"),
        "{stdout}"
    );
}

#[tokio::test]
async fn uninstall_resolver_prints_removal_command() {
    let sb = Sandbox::new();
    let out = sb
        .cmd()
        .arg("uninstall-resolver")
        .output()
        .await
        .expect("run uninstall-resolver");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("sudo rm -f /etc/resolver/adj.ac"),
        "{stdout}"
    );
}
