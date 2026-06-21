// End-to-end: drive requests through the proxy at a real app, then assert `adj stats --json`
// reports per-route metrics and (on Linux) a process section.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use tokio::process::{Child, Command};

fn adj_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}

fn read_port_file(path: &Path) -> Option<u16> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

struct Sandbox {
    _home: TempDir,
    home_path: std::path::PathBuf,
    proxy_port: u16,
    daemon: Option<Child>,
}

impl Sandbox {
    fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let home_path = home.path().to_path_buf();
        Self {
            _home: home,
            home_path,
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
        c.arg("daemon").stdout(Stdio::null()).stderr(Stdio::null());
        self.daemon = Some(c.spawn().expect("spawn daemon"));

        let deadline = Instant::now() + Duration::from_secs(5);
        let sock = self.home_path.join("sock");
        let port_file = self.home_path.join("proxy.port");
        loop {
            let sock_ready = sock.exists();
            if self.proxy_port == 0 {
                if let Some(p) = read_port_file(&port_file) {
                    self.proxy_port = p;
                }
            }
            if sock_ready && self.proxy_port != 0 {
                return;
            }
            if Instant::now() >= deadline {
                panic!("daemon did not come up within 5s");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn stop_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

async fn write_echo_server(dir: &Path, name: &str) {
    // A tiny HTTP server that 200s every request. Mirrors tests/proxy.rs: python3 stdlib, so no
    // node/npm dependency — /usr/bin/python3 is present on both the ubuntu-latest and macos-14
    // runners.
    let py = r#"import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"ok"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a, **kw):
        pass
ThreadingHTTPServer(("127.0.0.1", int(os.environ["PORT"])), H).serve_forever()
"#;
    let script = dir.join("server.py");
    tokio::fs::write(&script, py)
        .await
        .expect("write server.py");
    let body = format!(
        "name = \"{name}\"\ncmd = \"exec /usr/bin/python3 {}\"\n",
        script.display()
    );
    tokio::fs::write(dir.join("adjacent.toml"), body)
        .await
        .expect("write toml");
}

fn http_get(proxy_port: u16, host: &str, path: &str) -> Result<u16, String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", proxy_port)).map_err(|e| format!("connect: {e}"))?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut buf = String::new();
    stream
        .read_to_string(&mut buf)
        .map_err(|e| format!("read: {e}"))?;
    let status = buf
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("no status line in: {buf}"))?;
    Ok(status)
}

#[tokio::test]
async fn stats_json_reports_routes_and_process() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_echo_server(app_dir.path(), "st-app").await;
    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");

    // First request lazy-boots the app; drive a handful across two templated routes.
    let proxy_port = sandbox.proxy_port;
    for path in ["/users/1", "/users/2", "/users/3", "/health"] {
        let status =
            tokio::task::spawn_blocking(move || http_get(proxy_port, "st-app.adj.ac", path))
                .await
                .unwrap()
                .expect("http_get");
        assert_eq!(status, 200, "request to {path} should 200");
    }

    // Give the 2s sampler at least one tick so the process section is populated.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let out = sandbox
        .cmd()
        .arg("stats")
        .arg("st-app")
        .arg("--json")
        .output()
        .await
        .expect("stats");
    assert!(out.status.success(), "stats --json failed: {:?}", out);
    let v: Value = serde_json::from_slice(&out.stdout).expect("parse stats json");

    assert_eq!(v["name"], "st-app");
    assert_eq!(v["total_requests"], 4);
    let routes = v["routes"].as_array().expect("routes array");
    let route_names: Vec<&str> = routes
        .iter()
        .map(|r| r["route"].as_str().unwrap())
        .collect();
    assert!(
        route_names.contains(&"GET /users/:id"),
        "templated route missing: {route_names:?}"
    );
    assert!(
        route_names.contains(&"GET /health"),
        "health route missing: {route_names:?}"
    );
    let users = routes
        .iter()
        .find(|r| r["route"] == "GET /users/:id")
        .unwrap();
    assert_eq!(users["count"], 3);
    assert!(users["latency_ms"]["p95"].is_u64());

    // The process section is present on platforms with a sampler (Linux CI included), absent
    // otherwise — assert the running app surfaced one where supported.
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        let proc = v
            .get("process")
            .expect("process present on supported platform");
        assert!(
            proc["rss_bytes"].as_u64().unwrap() > 0,
            "rss should be non-zero"
        );
        assert!(proc["threads"].as_u64().unwrap() >= 1);
    }

    let _ = sandbox
        .cmd()
        .arg("down")
        .arg("st-app")
        .output()
        .await
        .expect("down");
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn stats_json_unknown_app_errors() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let out = sandbox
        .cmd()
        .arg("stats")
        .arg("nope")
        .arg("--json")
        .output()
        .await
        .expect("stats");
    assert!(!out.status.success(), "unknown app must be an error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no app named"), "got: {stderr}");

    sandbox.stop_daemon().await;
}
