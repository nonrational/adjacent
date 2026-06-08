use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::process::{Child, Command};

fn adj_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}

fn pick_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    l.local_addr().expect("local_addr").port()
}

fn curl_available() -> bool {
    std::process::Command::new("which")
        .arg("curl")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

struct TlsSandbox {
    _home: TempDir,
    home_path: PathBuf,
    proxy_port: u16,
    https_port: u16,
    daemon: Option<Child>,
}

impl TlsSandbox {
    async fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let home_path = home.path().to_path_buf();
        Self {
            _home: home,
            home_path,
            proxy_port: pick_port(),
            https_port: pick_port(),
            daemon: None,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(adj_bin());
        c.env("ADJACENT_HOME", &self.home_path);
        c.env("ADJACENT_PROXY_PORT", self.proxy_port.to_string());
        c.env("ADJACENT_HTTPS_PORT", self.https_port.to_string());
        c.env("RUST_LOG", "warn");
        c.env_remove("PORT");
        c.env_remove("BIND_PORT");
        c
    }

    async fn install_ca(&self) {
        let out = self
            .cmd()
            .arg("install-ca")
            .output()
            .await
            .expect("install-ca");
        assert!(out.status.success(), "install-ca failed: {:?}", out);
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("security add-trusted-cert"), "banner missing security command: {stdout}");
        assert!(self.home_path.join("ca.crt").exists(), "ca.crt not created");
        assert!(self.home_path.join("ca.key").exists(), "ca.key not created");
    }

    async fn start_daemon(&mut self) {
        let mut c = self.cmd();
        c.arg("daemon");
        c.stdout(Stdio::null());
        c.stderr(Stdio::null());
        let child = c.spawn().expect("spawn daemon");
        self.daemon = Some(child);

        // Wait for the control socket, the HTTP proxy port, AND the HTTPS port to bind.
        let deadline = Instant::now() + Duration::from_secs(5);
        let sock = self.home_path.join("sock");
        let http_addr = format!("127.0.0.1:{}", self.proxy_port);
        let https_addr = format!("127.0.0.1:{}", self.https_port);
        let mut sock_ready = false;
        let mut http_ready = false;
        let mut https_ready = false;
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
            if !http_ready && TcpStream::connect(&http_addr).is_ok() {
                http_ready = true;
            }
            if !https_ready && TcpStream::connect(&https_addr).is_ok() {
                https_ready = true;
            }
            if sock_ready && http_ready && https_ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "daemon did not come up within 5s (sock={sock_ready}, http={http_ready}, https={https_ready})"
        );
    }

    async fn start_daemon_http_only(&mut self) {
        let mut c = self.cmd();
        c.arg("daemon");
        c.stdout(Stdio::null());
        c.stderr(Stdio::null());
        let child = c.spawn().expect("spawn daemon");
        self.daemon = Some(child);

        let deadline = Instant::now() + Duration::from_secs(5);
        let sock = self.home_path.join("sock");
        let http_addr = format!("127.0.0.1:{}", self.proxy_port);
        while Instant::now() < deadline {
            let sock_ready = sock.exists();
            let http_ready = TcpStream::connect(&http_addr).is_ok();
            if sock_ready && http_ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("http-only daemon did not come up within 5s");
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

#[tokio::test]
async fn install_ca_generates_files_and_prints_macos_command() {
    let sandbox = TlsSandbox::new().await;
    sandbox.install_ca().await;
    // Second invocation must succeed without regenerating (idempotent UX) — but we don't assert
    // the file contents are identical because timestamp re-runs are fine; we only assert success.
    let again = sandbox
        .cmd()
        .arg("install-ca")
        .output()
        .await
        .expect("install-ca rerun");
    assert!(again.status.success());
    let stdout = String::from_utf8_lossy(&again.stdout);
    assert!(stdout.contains("Existing CA"), "second run should report existing CA: {stdout}");
}

#[tokio::test]
async fn install_port_forward_emits_both_http_and_https_rules() {
    let sandbox = TlsSandbox::new().await;
    let out = sandbox
        .cmd()
        .arg("install-port-forward")
        .output()
        .await
        .expect("install-port-forward");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("port 80"), "missing :80 rule: {stdout}");
    assert!(stdout.contains("port 443"), "missing :443 rule: {stdout}");
    assert!(
        stdout.contains(&format!("port {}", sandbox.proxy_port)),
        "missing http target port: {stdout}"
    );
    assert!(
        stdout.contains(&format!("port {}", sandbox.https_port)),
        "missing https target port: {stdout}"
    );
}

#[tokio::test]
async fn https_proxy_forwards_request_through_tls_termination() {
    if !curl_available() {
        eprintln!("curl not on PATH — skipping TLS forward test");
        return;
    }

    let mut sandbox = TlsSandbox::new().await;
    sandbox.install_ca().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_echo_server(app_dir.path(), "hello-over-tls").await;
    write_app(app_dir.path(), "echo", &cmd).await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    let ca_path = sandbox.home_path.join("ca.crt");
    let https_port = sandbox.https_port;
    let resolve = format!("echo.adj.ac:{https_port}:127.0.0.1");
    let output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("curl")
            .arg("-s")
            .arg("-o")
            .arg("/dev/null")
            .arg("-w")
            .arg("%{http_code}|%{stderr}")
            .arg("--cacert")
            .arg(&ca_path)
            .arg("--resolve")
            .arg(&resolve)
            .arg(format!("https://echo.adj.ac:{https_port}/"))
            .output()
            .expect("curl")
    })
    .await
    .expect("join");

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.starts_with("200"),
        "expected 200 status, got `{stdout}` (stderr: {stderr})"
    );

    // Also fetch the body to confirm we proxied through to the real upstream, not a TLS-only
    // dummy response. Use a fresh invocation so we don't have to juggle stdout redirection.
    let ca_path = sandbox.home_path.join("ca.crt");
    let resolve = format!("echo.adj.ac:{https_port}:127.0.0.1");
    let body_output = tokio::task::spawn_blocking(move || {
        std::process::Command::new("curl")
            .arg("-s")
            .arg("--cacert")
            .arg(&ca_path)
            .arg("--resolve")
            .arg(&resolve)
            .arg(format!("https://echo.adj.ac:{https_port}/"))
            .output()
            .expect("curl body")
    })
    .await
    .expect("join body");
    let body = String::from_utf8_lossy(&body_output.stdout);
    assert!(body.contains("hello-over-tls"), "body was `{body}`");

    let _ = sandbox.cmd().arg("down").arg("echo").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn https_listener_is_best_effort_when_ca_missing() {
    // No install-ca: the HTTPS listener task should log + exit, but the daemon must keep
    // running. We confirm by issuing a plain HTTP request through the proxy and getting a
    // response, while the HTTPS port stays unreachable.
    let mut sandbox = TlsSandbox::new().await;
    sandbox.start_daemon_http_only().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_echo_server(app_dir.path(), "http-still-works").await;
    write_app(app_dir.path(), "echo", &cmd).await;
    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    // HTTP proxy should answer normally.
    let proxy_port = sandbox.proxy_port;
    let (status_line, body) = tokio::task::spawn_blocking(move || http_get(proxy_port, "echo.adj.ac"))
        .await
        .expect("join")
        .expect("http_get");
    assert!(status_line.contains(" 200 "), "status: {status_line}");
    assert!(body.contains("http-still-works"), "body: {body}");

    // HTTPS port should NOT be bound — the listener task exited at startup because the CA is
    // missing. Give it a beat to actually exit and then probe.
    tokio::time::sleep(Duration::from_millis(300)).await;
    let https_addr = format!("127.0.0.1:{}", sandbox.https_port);
    assert!(
        TcpStream::connect_timeout(&https_addr.parse().unwrap(), Duration::from_millis(200))
            .is_err(),
        "https port should not be bound when CA is missing"
    );

    let _ = sandbox.cmd().arg("down").arg("echo").output().await;
    sandbox.stop_daemon().await;
}

fn http_get(proxy_port: u16, host: &str) -> Result<(String, String), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", proxy_port))
        .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(70)))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    let req = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| format!("write: {e}"))?;
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).map_err(|e| format!("read: {e}"))?;
    let text = String::from_utf8_lossy(&buf).to_string();
    let mut parts = text.splitn(2, "\r\n");
    let status_line = parts.next().unwrap_or("").to_string();
    let rest = parts.next().unwrap_or("");
    let body = if let Some(idx) = rest.find("\r\n\r\n") {
        rest[idx + 4..].to_string()
    } else {
        String::new()
    };
    Ok((status_line, body))
}
