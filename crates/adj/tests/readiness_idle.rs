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

async fn write_manifest(dir: &Path, body: &str) {
    let manifest = dir.join("adjacent.toml");
    tokio::fs::write(manifest, body).await.expect("write toml");
}

/// Write an HTTP server that binds $PORT immediately but returns 503 for `delay_seconds` after
/// boot, then flips to 200 for /healthz. This exercises the difference between TCP-open and
/// HTTP-2xx readiness — a TCP-only probe would consider the app ready instantly; the
/// health-check probe must wait for the 200.
async fn write_delayed_healthz_server(dir: &Path, delay_seconds: f64, marker: &str) -> String {
    let py = format!(
        r#"import os, time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
START = time.monotonic()
DELAY = {delay_seconds}
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        if self.path == "/healthz":
            if time.monotonic() - START < DELAY:
                self.send_response(503)
                self.end_headers()
                return
            self.send_response(200)
            self.end_headers()
            return
        body = b"{marker}"
        self.send_response(200)
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

/// Same shape as the proxy tests: a tiny GET that returns 200 + a marker body.
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

fn http_get(proxy_port: u16, host: &str, path: &str) -> Result<(String, String), String> {
    let mut stream = TcpStream::connect(("127.0.0.1", proxy_port))
        .map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(70)))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(req.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    let mut buf = Vec::new();
    stream
        .read_to_end(&mut buf)
        .map_err(|e| format!("read: {e}"))?;
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

#[tokio::test]
async fn health_check_url_waits_for_2xx_not_just_tcp_open() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // Server returns 503 for ~1.5s, then 200. TCP-only readiness would return immediately.
    let cmd = write_delayed_healthz_server(app_dir.path(), 1.5, "ok").await;
    let manifest = format!(
        "name = \"hc\"\ncmd = {cmd:?}\nhealth_check_url = \"/healthz\"\nboot_timeout = 15\n"
    );
    write_manifest(app_dir.path(), &manifest).await;
    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    // Lazy-boot via the proxy. The proxy's boot wait holds the request until the readiness
    // probe succeeds, which for `health_check_url` means a 2xx. A pure TCP probe would return
    // in tens of milliseconds because the server binds $PORT immediately.
    let proxy_port = sandbox.proxy_port;
    let start = Instant::now();
    let (status_line, _body) =
        tokio::task::spawn_blocking(move || http_get(proxy_port, "hc.adj.ac", "/"))
            .await
            .expect("join")
            .expect("http_get");
    let elapsed = start.elapsed();

    assert!(status_line.contains(" 200 "), "expected 200: {status_line}");
    assert!(
        elapsed >= Duration::from_millis(1200),
        "request returned in {elapsed:?} — health check probably didn't wait for 2xx"
    );

    let _ = sandbox.cmd().arg("down").arg("hc").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn health_check_url_that_never_returns_2xx_fails_boot() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // Server binds the port but /healthz always 503s.
    let py = r#"import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        self.send_response(503); self.end_headers()
    def log_message(self,*a,**kw): pass
ThreadingHTTPServer(("127.0.0.1", int(os.environ["PORT"])), H).serve_forever()
"#;
    let script = app_dir.path().join("server.py");
    tokio::fs::write(&script, py).await.expect("write server.py");
    let cmd = format!("exec /usr/bin/python3 {}", script.display());
    let manifest = format!(
        "name = \"never-ready\"\ncmd = {cmd:?}\nhealth_check_url = \"/healthz\"\nboot_timeout = 1\n"
    );
    write_manifest(app_dir.path(), &manifest).await;
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
        http_get(proxy_port, "never-ready.adj.ac", "/")
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
        "504 took too long ({elapsed:?})"
    );

    let _ = sandbox.cmd().arg("down").arg("never-ready").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn wait_ready_blocks_until_app_is_ready() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // Server takes ~1s before /healthz returns 200. `adj up` returns as soon as the process is
    // spawned (it does not wait for readiness), so `wait-ready` immediately after `up` is the
    // exact agent-style usage: kick off a boot, then block until the app reports clean.
    let cmd = write_delayed_healthz_server(app_dir.path(), 1.0, "ok").await;
    let manifest = format!(
        "name = \"waitable\"\ncmd = {cmd:?}\nhealth_check_url = \"/healthz\"\nboot_timeout = 15\n"
    );
    write_manifest(app_dir.path(), &manifest).await;
    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");

    let up = sandbox
        .cmd()
        .arg("up")
        .arg("waitable")
        .output()
        .await
        .expect("up");
    assert!(up.status.success(), "up: {:?}", up);

    let start = Instant::now();
    let out = sandbox
        .cmd()
        .arg("wait-ready")
        .arg("waitable")
        .output()
        .await
        .expect("wait-ready");
    let elapsed = start.elapsed();
    assert!(
        out.status.success(),
        "wait-ready failed: stderr={}",
        String::from_utf8_lossy(&out.stderr)
    );
    // Must take at least most of the 1s 503 window — proves wait-ready is using the same
    // health-check polling as the boot path.
    assert!(
        elapsed >= Duration::from_millis(700),
        "wait-ready returned too fast ({elapsed:?}) — health check probably ignored"
    );
    assert!(
        elapsed < Duration::from_secs(10),
        "wait-ready took too long ({elapsed:?})"
    );

    let _ = sandbox.cmd().arg("down").arg("waitable").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn wait_ready_fails_fast_when_app_not_running() {
    // `adj wait-ready` on a registered-but-never-booted app used to sit out the full
    // boot_timeout (60s default) polling connection-refused. Fail fast instead with a message
    // that points the user at `adj up`.
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_manifest(
        app_dir.path(),
        "name = \"nope\"\ncmd = \"sleep 60\"\nboot_timeout = 60\n",
    )
    .await;
    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");

    let start = Instant::now();
    let out = sandbox
        .cmd()
        .arg("wait-ready")
        .arg("nope")
        .arg("--timeout")
        .arg("5")
        .output()
        .await
        .expect("wait-ready");
    let elapsed = start.elapsed();
    assert!(
        !out.status.success(),
        "wait-ready should fail when app is not booted: stdout={}",
        String::from_utf8_lossy(&out.stdout)
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("is not running") && stderr.contains("adj up"),
        "expected actionable not-running message, got: {stderr}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "wait-ready on a stopped app should return nearly instantly, took {elapsed:?}"
    );

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn idle_timeout_stops_app_after_quiet_period() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_echo_server(app_dir.path(), "ok").await;
    // Tiny idle window so the test is tractable.
    let manifest = format!("name = \"idler\"\ncmd = {cmd:?}\nidle_timeout = \"2s\"\n");
    write_manifest(app_dir.path(), &manifest).await;
    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");

    let proxy_port = sandbox.proxy_port;
    let (status_line, _body) =
        tokio::task::spawn_blocking(move || http_get(proxy_port, "idler.adj.ac", "/"))
            .await
            .expect("join")
            .expect("http_get");
    assert!(status_line.contains(" 200 "), "boot 200: {status_line}");

    // Verify running.
    let status_running = sandbox
        .cmd()
        .arg("status")
        .arg("idler")
        .output()
        .await
        .expect("status");
    assert!(
        String::from_utf8_lossy(&status_running.stdout).contains("running"),
        "should be running after request"
    );

    // Wait long enough for idle_timeout + scan interval + termination grace.
    tokio::time::sleep(Duration::from_secs(4)).await;

    let status_idle = sandbox
        .cmd()
        .arg("status")
        .arg("idler")
        .output()
        .await
        .expect("status");
    let out = String::from_utf8_lossy(&status_idle.stdout);
    assert!(out.contains("stopped"), "expected stopped after idle: {out}");

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn idle_timeout_off_keeps_app_running() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_echo_server(app_dir.path(), "ok").await;
    let manifest = format!("name = \"nostop\"\ncmd = {cmd:?}\nidle_timeout = \"off\"\n");
    write_manifest(app_dir.path(), &manifest).await;
    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");

    let proxy_port = sandbox.proxy_port;
    let _ = tokio::task::spawn_blocking(move || http_get(proxy_port, "nostop.adj.ac", "/"))
        .await
        .expect("join")
        .expect("http_get");

    // Sleep longer than the idle scanner's polling interval, but with idle_timeout off the app
    // must still be running.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let status = sandbox
        .cmd()
        .arg("status")
        .arg("nostop")
        .output()
        .await
        .expect("status");
    let out = String::from_utf8_lossy(&status.stdout);
    assert!(
        out.contains("running"),
        "idle_timeout=off must keep app running, got: {out}"
    );

    let _ = sandbox.cmd().arg("down").arg("nostop").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn idle_shutdown_then_next_request_lazy_boots_again() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_echo_server(app_dir.path(), "reborn").await;
    let manifest = format!("name = \"reborn\"\ncmd = {cmd:?}\nidle_timeout = \"2s\"\n");
    write_manifest(app_dir.path(), &manifest).await;
    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");

    let proxy_port = sandbox.proxy_port;
    let _ = tokio::task::spawn_blocking(move || http_get(proxy_port, "reborn.adj.ac", "/"))
        .await
        .expect("join")
        .expect("http_get");

    tokio::time::sleep(Duration::from_secs(4)).await;
    let status = sandbox
        .cmd()
        .arg("status")
        .arg("reborn")
        .output()
        .await
        .expect("status");
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("stopped"),
        "should be idle-stopped"
    );

    let (status_line, body) =
        tokio::task::spawn_blocking(move || http_get(proxy_port, "reborn.adj.ac", "/"))
            .await
            .expect("join")
            .expect("http_get");
    assert!(status_line.contains(" 200 "), "relaunch: {status_line}");
    assert!(body.contains("reborn"), "relaunch body: {body}");

    let _ = sandbox.cmd().arg("down").arg("reborn").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn idle_countdown_resets_on_proxied_request() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_echo_server(app_dir.path(), "alive").await;
    let manifest = format!("name = \"keepalive\"\ncmd = {cmd:?}\nidle_timeout = \"2s\"\n");
    write_manifest(app_dir.path(), &manifest).await;
    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");

    let proxy_port = sandbox.proxy_port;

    // Boot via first request, then keep poking the app every 1s (less than the 2s idle window).
    let _ = tokio::task::spawn_blocking(move || http_get(proxy_port, "keepalive.adj.ac", "/"))
        .await
        .expect("join")
        .expect("http_get");
    for _ in 0..3 {
        tokio::time::sleep(Duration::from_secs(1)).await;
        let _ = tokio::task::spawn_blocking(move || http_get(proxy_port, "keepalive.adj.ac", "/"))
            .await
            .expect("join")
            .expect("http_get");
    }

    let status = sandbox
        .cmd()
        .arg("status")
        .arg("keepalive")
        .output()
        .await
        .expect("status");
    let out = String::from_utf8_lossy(&status.stdout);
    assert!(
        out.contains("running"),
        "regular traffic should keep app alive, got: {out}"
    );

    let _ = sandbox.cmd().arg("down").arg("keepalive").output().await;
    sandbox.stop_daemon().await;
}
