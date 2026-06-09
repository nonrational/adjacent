use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::process::{Child, Command};

fn adj_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}

/// Bind :0, read the assigned port, close. The kernel may reissue this number to anyone before
/// the daemon claims it, but for a localhost test that's rare enough to accept.
fn pick_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    l.local_addr().expect("local_addr").port()
}

struct Sandbox {
    _home: TempDir,
    home_path: PathBuf,
    proxy_port: u16,
    daemon: Option<Child>,
}

impl Sandbox {
    async fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let home_path = home.path().to_path_buf();
        Self {
            _home: home,
            home_path,
            proxy_port: pick_port(),
            daemon: None,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(adj_bin());
        c.env("ADJACENT_HOME", &self.home_path);
        c.env("ADJACENT_PROXY_PORT", self.proxy_port.to_string());
        c.env("RUST_LOG", "warn");
        c.env_remove("PORT");
        c.env_remove("BIND_PORT");
        c
    }

    async fn start_daemon(&mut self) {
        let mut c = self.cmd();
        c.arg("daemon");
        c.stdout(Stdio::null());
        c.stderr(Stdio::null());
        let child = c.spawn().expect("spawn daemon");
        self.daemon = Some(child);

        // Wait for both the control socket and the proxy port to be live.
        let deadline = Instant::now() + Duration::from_secs(5);
        let sock = self.home_path.join("sock");
        let proxy_addr = format!("127.0.0.1:{}", self.proxy_port);
        let mut sock_ready = false;
        let mut proxy_ready = false;
        while Instant::now() < deadline {
            if !sock_ready && sock.exists() {
                let out = self
                    .cmd()
                    .arg("status")
                    .arg("__probe__")
                    .output()
                    .await
                    .expect("probe");
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stderr.contains("daemon not reachable") {
                    sock_ready = true;
                }
            }
            if !proxy_ready && TcpStream::connect(&proxy_addr).is_ok() {
                proxy_ready = true;
            }
            if sock_ready && proxy_ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("daemon did not come up within 5s (sock={sock_ready}, proxy={proxy_ready})");
    }

    async fn stop_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

async fn write_app(dir: &Path, name: &str, cmd: &str) {
    let manifest = dir.join("adjacent.toml");
    let body = format!("name = \"{name}\"\ncmd = \"{cmd}\"\n");
    tokio::fs::write(manifest, body).await.expect("write toml");
}

async fn write_app_with_boot_timeout(dir: &Path, name: &str, cmd: &str, boot_timeout: u64) {
    let manifest = dir.join("adjacent.toml");
    let body = format!(
        "name = \"{name}\"\ncmd = \"{cmd}\"\nboot_timeout = {boot_timeout}\n"
    );
    tokio::fs::write(manifest, body).await.expect("write toml");
}

/// Write a tiny multi-connection HTTP echo to `dir/server.py` and return the shell command
/// that runs it. A real (threaded) server is required because the proxy's tcp-ready probe plus
/// the actual forward connect are at least two accepts; concurrent forwards add more. BSD
/// netcat's single-accept-then-exit loop races with all of that.
async fn write_echo_server(dir: &Path, marker: &str) -> String {
    let py = format!(
        r#"import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"{marker}"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a, **kw):
        pass
ThreadingHTTPServer(("127.0.0.1", int(os.environ["PORT"])), H).serve_forever()
"#
    );
    let script = dir.join("server.py");
    tokio::fs::write(&script, py).await.expect("write server.py");
    format!("exec /usr/bin/python3 {}", script.display())
}

/// Send an HTTP GET to the proxy with the given Host header, return (status_line, body).
fn http_get(proxy_port: u16, host: &str, path: &str) -> Result<(String, String), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", proxy_port))
        .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(70)))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    let req = format!(
        "GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n"
    );
    stream.write_all(req.as_bytes()).map_err(|e| format!("write: {e}"))?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut parts = text.splitn(2, "\r\n");
    let status_line = parts.next().unwrap_or("").to_string();
    let rest = parts.next().unwrap_or("");
    // Skip headers up to \r\n\r\n.
    let body = if let Some(idx) = rest.find("\r\n\r\n") {
        rest[idx + 4..].to_string()
    } else {
        String::new()
    };
    Ok((status_line, body))
}

#[tokio::test]
async fn proxy_lazy_boots_app_and_forwards_response() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_echo_server(app_dir.path(), "hello-from-echo").await;
    write_app(app_dir.path(), "echo", &cmd).await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    let proxy_port = sandbox.proxy_port;
    let (status_line, body) = tokio::task::spawn_blocking(move || {
        http_get(proxy_port, "echo.adj.ac", "/")
    })
    .await
    .expect("join")
    .expect("http_get");

    assert!(
        status_line.contains(" 200 "),
        "expected 200 OK, got: {status_line}"
    );
    assert!(body.contains("hello-from-echo"), "body: {body}");

    // Status should report the app as running after the lazy-boot.
    let status = sandbox
        .cmd()
        .arg("status")
        .arg("echo")
        .output()
        .await
        .expect("status");
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_out.contains("running"),
        "status output: {status_out}"
    );

    let _ = sandbox.cmd().arg("down").arg("echo").output().await;
    sandbox.stop_daemon().await;
}

/// `adj doctor` probes the proxy by asking for Host `__adj_verify__.adj.ac`. The marker handler
/// must short-circuit BEFORE the boot gate runs — otherwise a doctor probe on a fresh install
/// (no apps registered) would surface as a 404 NotRegistered, and a doctor probe on an install
/// where someone registered `__adj_verify__` would spawn that app. Neither is acceptable.
#[tokio::test]
async fn verify_marker_short_circuits_before_boot_gate() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let proxy_port = sandbox.proxy_port;
    let (status_line, body) = tokio::task::spawn_blocking(move || {
        http_get(proxy_port, "__adj_verify__.adj.ac", "/")
    })
    .await
    .expect("join")
    .expect("http_get");

    assert!(
        status_line.contains(" 200 "),
        "expected 200 OK, got: {status_line}"
    );
    assert_eq!(
        body, "adj-port-forward-ok\n",
        "marker body must be the fixed `adj-port-forward-ok\\n` so the doctor can match by equality"
    );

    // No apps registered → registry is empty. If the marker had fallen through to the boot
    // gate we'd see `[]` either way, but the 200/marker-body assertion above plus this
    // sanity-check guard against a future "if name == reserved { ...load registry... }" rewrite.
    let list = sandbox
        .cmd()
        .arg("list")
        .arg("--json")
        .output()
        .await
        .expect("list");
    let stdout = String::from_utf8_lossy(&list.stdout);
    assert_eq!(stdout.trim(), "[]", "no apps should have been registered");

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn proxy_single_flights_concurrent_first_requests() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let spawn_counter = app_dir.path().join("spawn-count");
    let server = write_echo_server(app_dir.path(), "ok").await;
    // Each spawn appends a line to the counter file before serving. Three concurrent first
    // requests must produce exactly one line: one spawn, three forwards from the same instance.
    let cmd = format!(
        "echo spawn >> {counter}; {server}",
        counter = spawn_counter.display(),
    );
    write_app(app_dir.path(), "flight", &cmd).await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    let proxy_port = sandbox.proxy_port;
    let host = Arc::new("flight.adj.ac".to_string());
    let mut handles = Vec::new();
    for _ in 0..3 {
        let host = host.clone();
        handles.push(tokio::task::spawn_blocking(move || {
            http_get(proxy_port, &host, "/")
        }));
    }
    for h in handles {
        let (status_line, body) = h.await.expect("join").expect("http_get");
        assert!(status_line.contains(" 200 "), "status: {status_line}");
        assert!(body.contains("ok"), "body: {body}");
    }

    // Allow filesystem to settle, then count spawn lines.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let spawns = std::fs::read_to_string(&spawn_counter)
        .expect("read spawn counter")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
    assert_eq!(spawns, 1, "expected single boot, saw {spawns} spawns");

    let _ = sandbox.cmd().arg("down").arg("flight").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn proxy_returns_504_when_boot_times_out() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // Process runs but never binds $PORT — readiness probe never succeeds. boot_timeout=1s makes
    // the test deterministic and fast.
    write_app_with_boot_timeout(app_dir.path(), "slowboot", "sleep 30", 1).await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    let proxy_port = sandbox.proxy_port;
    let start = Instant::now();
    let (status_line, _body) = tokio::task::spawn_blocking(move || {
        http_get(proxy_port, "slowboot.adj.ac", "/")
    })
    .await
    .expect("join")
    .expect("http_get");
    let elapsed = start.elapsed();

    assert!(
        status_line.contains(" 504 "),
        "expected 504, got: {status_line}"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "504 took too long ({elapsed:?}) — boot_timeout override may not be wired"
    );

    let _ = sandbox.cmd().arg("down").arg("slowboot").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn install_port_forward_prints_pf_anchor_and_sudo_commands() {
    let sandbox = Sandbox::new().await;
    let out = sandbox
        .cmd()
        .arg("install-port-forward")
        .output()
        .await
        .expect("install-port-forward");
    assert!(
        out.status.success(),
        "install-port-forward failed: {:?}",
        out
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    // Anchor rule shape.
    assert!(
        stdout.contains("rdr") && stdout.contains("port 80") && stdout.contains("127.0.0.1"),
        "missing rdr/loopback in output: {stdout}"
    );
    // Sudo command shape.
    assert!(stdout.contains("sudo pfctl"), "missing sudo pfctl: {stdout}");
    // Target proxy port matches the sandbox override.
    assert!(
        stdout.contains(&format!("port {}", sandbox.proxy_port)),
        "stdout doesn't mention proxy port {}: {stdout}",
        sandbox.proxy_port
    );
}
