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

/// Parse a `<port>\n` file the daemon writes after binding a listener. `None` until the file
/// exists with a complete write.
fn read_port_file(path: &Path) -> Option<u16> {
    let s = std::fs::read_to_string(path).ok()?;
    s.trim().parse().ok()
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
            // 0 = the daemon binds a kernel-assigned port; start_daemon learns the real port
            // from the proxy.port file. Picking a free port here and re-binding it in the
            // daemon raced concurrent test processes drawing from the same ephemeral range,
            // which flaked as "connection reset by peer" against a foreign listener.
            proxy_port: 0,
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

        let deadline = Instant::now() + Duration::from_secs(5);
        let sock = self.home_path.join("sock");
        let port_file = self.home_path.join("proxy.port");
        let mut sock_ready = false;
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
            // proxy.port is written after bind, so a parsed port means the listener is live —
            // and unambiguously ours, unlike a bare TCP connect to a guessed port.
            if self.proxy_port == 0 {
                if let Some(p) = read_port_file(&port_file) {
                    self.proxy_port = p;
                }
            }
            if sock_ready && self.proxy_port != 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!(
            "daemon did not come up within 5s (sock={sock_ready}, proxy_port={})",
            self.proxy_port
        );
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

/// Send an HTTP GET to the proxy with the given Host header, return (status_line, raw_headers, body).
fn http_get(proxy_port: u16, host: &str, path: &str) -> Result<(String, String, String), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", proxy_port))
        .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
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
    let (headers, body) = if let Some(idx) = rest.find("\r\n\r\n") {
        (rest[..idx].to_string(), rest[idx + 4..].to_string())
    } else {
        (rest.to_string(), String::new())
    };
    Ok((status_line, headers, body))
}

#[tokio::test]
async fn status_subdomain_serves_html_dashboard() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let proxy_port = sandbox.proxy_port;
    let (status_line, headers, body) = tokio::task::spawn_blocking(move || {
        http_get(proxy_port, "status.adj.ac", "/")
    })
    .await
    .expect("join")
    .expect("http_get");

    assert!(status_line.contains(" 200 "), "expected 200, got: {status_line}");
    let headers_lower = headers.to_ascii_lowercase();
    assert!(
        headers_lower.contains("content-type: text/html"),
        "expected text/html Content-Type, got headers: {headers}"
    );
    assert!(
        headers_lower.contains("cache-control: no-store"),
        "expected no-store Cache-Control, got headers: {headers}"
    );
    assert!(body.contains("adj.ac"), "wordmark missing: {body}");
    // Confirm the polling script ships with the page (sanity-check we're serving the embedded asset).
    assert!(
        body.contains("/apps.json"),
        "embedded JS should reference /apps.json: {body}"
    );

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn status_apps_json_lists_registered_apps() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // `cmd` only matters if we boot the app; we don't, so anything non-empty is fine.
    write_app(app_dir.path(), "demo", "sleep 30").await;
    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    let proxy_port = sandbox.proxy_port;
    let (status_line, headers, body) = tokio::task::spawn_blocking(move || {
        http_get(proxy_port, "status.adj.ac", "/apps.json")
    })
    .await
    .expect("join")
    .expect("http_get");

    assert!(status_line.contains(" 200 "), "expected 200, got: {status_line}");
    let headers_lower = headers.to_ascii_lowercase();
    assert!(
        headers_lower.contains("content-type: application/json"),
        "expected application/json Content-Type, got headers: {headers}"
    );

    let parsed: serde_json::Value =
        serde_json::from_str(body.trim()).expect("apps.json must be valid JSON");
    let arr = parsed.as_array().expect("apps.json must be an array");
    assert_eq!(arr.len(), 1, "expected one app, got: {body}");
    let entry = &arr[0];
    assert_eq!(entry["name"], "demo");
    assert_eq!(entry["state"], "stopped");
    assert!(
        entry["path"].as_str().is_some_and(|p| !p.is_empty()),
        "path should be a non-empty string: {entry}"
    );

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn status_subdomain_returns_404_for_unknown_path() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let proxy_port = sandbox.proxy_port;
    let (status_line, _headers, _body) = tokio::task::spawn_blocking(move || {
        http_get(proxy_port, "status.adj.ac", "/nope")
    })
    .await
    .expect("join")
    .expect("http_get");

    assert!(status_line.contains(" 404 "), "expected 404, got: {status_line}");

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn status_apps_json_marks_vanished_path_as_missing() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_app(app_dir.path(), "ghost", "sleep 30").await;
    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    // Delete the directory underneath the registry entry. TempDir's destructor recursively
    // removes the directory on drop, so the canonical path the registry holds is now gone.
    drop(app_dir);

    let proxy_port = sandbox.proxy_port;
    let (status_line, _headers, body) = tokio::task::spawn_blocking(move || {
        http_get(proxy_port, "status.adj.ac", "/apps.json")
    })
    .await
    .expect("join")
    .expect("http_get");

    assert!(status_line.contains(" 200 "), "expected 200: {status_line}");
    let parsed: serde_json::Value =
        serde_json::from_str(body.trim()).expect("apps.json must be valid JSON");
    let arr = parsed.as_array().expect("apps.json must be an array");
    let ghost = arr
        .iter()
        .find(|e| e["name"] == "ghost")
        .expect("ghost entry should be present");
    assert_eq!(
        ghost["state"], "missing",
        "expected state=missing for vanished path, got: {ghost}"
    );

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn adj_add_refuses_to_register_reserved_name() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_app(app_dir.path(), "status", "sleep 30").await;

    let out = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(!out.status.success(), "expected add to fail, got success");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("reserved"),
        "stderr should mention reserved, got: {stderr}"
    );

    sandbox.stop_daemon().await;
}
