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

        // Wait for both the control socket and the proxy port to be live.
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

async fn write_app_with_boot_timeout(dir: &Path, name: &str, cmd: &str, boot_timeout: u64) {
    let manifest = dir.join("adjacent.toml");
    let body = format!("name = \"{name}\"\ncmd = \"{cmd}\"\nboot_timeout = {boot_timeout}\n");
    tokio::fs::write(manifest, body).await.expect("write toml");
}

async fn write_app_with_idle_timeout(dir: &Path, name: &str, cmd: &str, idle_timeout: &str) {
    let manifest = dir.join("adjacent.toml");
    let body = format!("name = \"{name}\"\ncmd = {cmd:?}\nidle_timeout = \"{idle_timeout}\"\n");
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
    tokio::fs::write(&script, py)
        .await
        .expect("write server.py");
    format!("exec /usr/bin/python3 {}", script.display())
}

/// Like `write_echo_server` but binds the IPv6 loopback (`::1`) only — what Node ≥17 does for
/// "localhost" on macOS. Exercises the proxy's dual-family upstream connect: a v6-only app must
/// still be reached by both the readiness probe and the forward path.
async fn write_echo_server_v6(dir: &Path, marker: &str) -> String {
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
class S(ThreadingHTTPServer):
    address_family = __import__("socket").AF_INET6
S(("::1", int(os.environ["PORT"])), H).serve_forever()
"#
    );
    let script = dir.join("server.py");
    tokio::fs::write(&script, py)
        .await
        .expect("write server.py");
    format!("exec /usr/bin/python3 {}", script.display())
}

/// Write a server that echoes back the X-Forwarded-* request headers it received, one
/// `name=value` per line. Lets the test assert what actually crossed the proxy boundary.
async fn write_header_echo_server(dir: &Path) -> String {
    let py = r#"import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        names = ["X-Forwarded-Host", "X-Forwarded-For", "X-Forwarded-Proto"]
        body = "".join(f"{n}={self.headers.get(n)}\n" for n in names).encode()
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
    format!("exec /usr/bin/python3 {}", script.display())
}

/// Write a minimal WebSocket echo server (handshake + unmask + echo) to `dir/ws.py` and return
/// the shell command that runs it. Python because the stdlib has everything needed (sha1,
/// base64, socketserver) — no npm install in the test sandbox. Frames are assumed < 126 bytes
/// and client-masked, which is all the test client sends.
async fn write_ws_echo_server(dir: &Path) -> String {
    let py = r#"import os, base64, hashlib, socketserver

GUID = b"258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

class H(socketserver.StreamRequestHandler):
    def handle(self):
        # Readiness probes TCP-connect and close; an empty first line means no request came.
        if not self.rfile.readline():
            return
        headers = {}
        while True:
            line = self.rfile.readline()
            if line in (b"\r\n", b"\n", b""):
                break
            if b":" in line:
                k, v = line.split(b":", 1)
                headers[k.strip().lower()] = v.strip()
        key = headers.get(b"sec-websocket-key")
        if key is None:
            self.request.sendall(b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n")
            return
        accept = base64.b64encode(hashlib.sha1(key + GUID).digest())
        self.request.sendall(
            b"HTTP/1.1 101 Switching Protocols\r\n"
            b"Upgrade: websocket\r\n"
            b"Connection: Upgrade\r\n"
            b"Sec-WebSocket-Accept: " + accept + b"\r\n\r\n"
        )
        while True:
            hdr = self.rfile.read(2)
            if len(hdr) < 2:
                return
            opcode = hdr[0] & 0x0F
            if opcode == 8:  # close
                return
            length = hdr[1] & 0x7F
            mask = self.rfile.read(4)
            payload = bytearray(self.rfile.read(length))
            for i in range(length):
                payload[i] ^= mask[i % 4]
            self.request.sendall(bytes([0x81, length]) + bytes(payload))

class S(socketserver.ThreadingTCPServer):
    allow_reuse_address = True

S(("127.0.0.1", int(os.environ["PORT"])), H).serve_forever()
"#;
    let script = dir.join("ws.py");
    tokio::fs::write(&script, py).await.expect("write ws.py");
    format!("exec /usr/bin/python3 {}", script.display())
}

/// Send an HTTP GET to the proxy with the given Host header, return (status_line, body).
fn http_get(proxy_port: u16, host: &str, path: &str) -> Result<(String, String), String> {
    http_get_with_headers(proxy_port, host, path, &[])
}

/// Like `http_get` but with extra request headers, e.g. a client-supplied X-Forwarded-For.
fn http_get_with_headers(
    proxy_port: u16,
    host: &str,
    path: &str,
    extra_headers: &[(&str, &str)],
) -> Result<(String, String), String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", proxy_port)).map_err(|e| format!("connect: {e}"))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(70)))
        .map_err(|e| format!("set_read_timeout: {e}"))?;
    let extra: String = extra_headers
        .iter()
        .map(|(k, v)| format!("{k}: {v}\r\n"))
        .collect();
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\n{extra}Connection: close\r\n\r\n");
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
    let (status_line, body) =
        tokio::task::spawn_blocking(move || http_get(proxy_port, "echo.adj.ac", "/"))
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

/// An app that binds the IPv6 loopback only (Node ≥17's default for "localhost" on macOS) must
/// proxy end-to-end. Regression for the v4-only upstream connect that left healthy v6 apps
/// hanging until the boot deadline, then 504'ing.
#[tokio::test]
async fn proxy_reaches_ipv6_only_upstream() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_echo_server_v6(app_dir.path(), "hello-from-v6").await;
    write_app(app_dir.path(), "v6app", &cmd).await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    let proxy_port = sandbox.proxy_port;
    let (status_line, body) =
        tokio::task::spawn_blocking(move || http_get(proxy_port, "v6app.adj.ac", "/"))
            .await
            .expect("join")
            .expect("http_get");

    assert!(
        status_line.contains(" 200 "),
        "expected 200 OK from v6-only upstream, got: {status_line}"
    );
    assert!(body.contains("hello-from-v6"), "body: {body}");

    let _ = sandbox.cmd().arg("down").arg("v6app").output().await;
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
    let (status_line, body) =
        tokio::task::spawn_blocking(move || http_get(proxy_port, "__adj_verify__.adj.ac", "/"))
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

/// The proxy rewrites Host to `127.0.0.1:<port>` for upstream allowlists, so X-Forwarded-* is
/// the only way an app can recover the original request origin (issue #26).
#[tokio::test]
async fn proxy_forwards_x_forwarded_headers_to_upstream() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_header_echo_server(app_dir.path()).await;
    write_app(app_dir.path(), "fwd", &cmd).await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    let proxy_port = sandbox.proxy_port;

    // Clean request: all three headers must be added.
    let (status_line, body) =
        tokio::task::spawn_blocking(move || http_get(proxy_port, "fwd.adj.ac", "/"))
            .await
            .expect("join")
            .expect("http_get");
    assert!(status_line.contains(" 200 "), "status: {status_line}");
    assert!(
        body.contains("X-Forwarded-Host=fwd.adj.ac"),
        "missing/incorrect X-Forwarded-Host in upstream view: {body}"
    );
    assert!(
        body.contains("X-Forwarded-For=127.0.0.1"),
        "missing/incorrect X-Forwarded-For in upstream view: {body}"
    );
    assert!(
        body.contains("X-Forwarded-Proto=http"),
        "missing/incorrect X-Forwarded-Proto in upstream view: {body}"
    );

    // Client-supplied X-Forwarded-For must be appended to, not overwritten.
    let (status_line, body) = tokio::task::spawn_blocking(move || {
        http_get_with_headers(
            proxy_port,
            "fwd.adj.ac",
            "/",
            &[("X-Forwarded-For", "10.0.0.1")],
        )
    })
    .await
    .expect("join")
    .expect("http_get");
    assert!(status_line.contains(" 200 "), "status: {status_line}");
    assert!(
        body.contains("X-Forwarded-For=10.0.0.1, 127.0.0.1"),
        "existing X-Forwarded-For not appended to: {body}"
    );

    // The original Host's port must survive into X-Forwarded-Host — in the default no-pfctl setup
    // the browser sends `Host: fwd.adj.ac:8080`, and an app rebuilds a routable origin from it.
    let (status_line, body) =
        tokio::task::spawn_blocking(move || http_get(proxy_port, "fwd.adj.ac:8080", "/"))
            .await
            .expect("join")
            .expect("http_get");
    assert!(status_line.contains(" 200 "), "status: {status_line}");
    assert!(
        body.contains("X-Forwarded-Host=fwd.adj.ac:8080"),
        "X-Forwarded-Host dropped the original port: {body}"
    );

    // A client may legally split X-Forwarded-For across multiple header lines; every entry must
    // survive the collapse, not just the first.
    let (status_line, body) = tokio::task::spawn_blocking(move || {
        http_get_with_headers(
            proxy_port,
            "fwd.adj.ac",
            "/",
            &[
                ("X-Forwarded-For", "10.0.0.1"),
                ("X-Forwarded-For", "192.168.1.1"),
            ],
        )
    })
    .await
    .expect("join")
    .expect("http_get");
    assert!(status_line.contains(" 200 "), "status: {status_line}");
    assert!(
        body.contains("X-Forwarded-For=10.0.0.1, 192.168.1.1, 127.0.0.1"),
        "multi-line X-Forwarded-For not fully preserved: {body}"
    );

    let _ = sandbox.cmd().arg("down").arg("fwd").output().await;
    sandbox.stop_daemon().await;
}

/// Read exactly `n` bytes from the stream.
fn read_exact_n(stream: &mut TcpStream, n: usize) -> Result<Vec<u8>, String> {
    let mut buf = vec![0u8; n];
    stream
        .read_exact(&mut buf)
        .map_err(|e| format!("read_exact({n}): {e}"))?;
    Ok(buf)
}

/// Send one masked client text frame, read back one unmasked server frame, return its payload.
/// Raw frame I/O instead of a WS client crate keeps the dev-dependency surface at zero.
fn ws_echo_roundtrip(stream: &mut TcpStream, msg: &str) -> Result<String, String> {
    let mask = [0x12u8, 0x34, 0x56, 0x78];
    let payload = msg.as_bytes();
    assert!(payload.len() < 126, "test frames must fit a 7-bit length");
    let mut frame = vec![0x81, 0x80 | payload.len() as u8];
    frame.extend_from_slice(&mask);
    frame.extend(payload.iter().enumerate().map(|(i, b)| b ^ mask[i % 4]));
    stream
        .write_all(&frame)
        .map_err(|e| format!("write frame: {e}"))?;

    let hdr = read_exact_n(stream, 2)?;
    if hdr[0] != 0x81 {
        return Err(format!(
            "expected FIN text frame, got first byte {:#04x}",
            hdr[0]
        ));
    }
    let len = (hdr[1] & 0x7F) as usize;
    let body = read_exact_n(stream, len)?;
    String::from_utf8(body).map_err(|e| format!("payload not utf8: {e}"))
}

/// Issue #25: WebSocket upgrades must propagate through the proxy — handshake completes with a
/// 101, frames flow both ways afterwards, and a close from one side tears the tunnel down so the
/// peer sees EOF. This is the path Vite/Webpack/Next HMR rides on.
#[tokio::test]
async fn proxy_propagates_websocket_upgrade_and_pipes_frames() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_ws_echo_server(app_dir.path()).await;
    write_app(app_dir.path(), "ws", &cmd).await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    let proxy_port = sandbox.proxy_port;

    tokio::task::spawn_blocking(move || {
        let mut stream = TcpStream::connect(("127.0.0.1", proxy_port)).expect("connect proxy");
        stream
            .set_read_timeout(Some(Duration::from_secs(70)))
            .expect("set_read_timeout");

        let req = "GET /ws HTTP/1.1\r\n\
                   Host: ws.adj.ac\r\n\
                   Connection: Upgrade\r\n\
                   Upgrade: websocket\r\n\
                   Sec-WebSocket-Version: 13\r\n\
                   Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
        stream.write_all(req.as_bytes()).expect("write handshake");

        // Read the response head byte-by-byte up to the blank line — the upgraded stream
        // starts immediately after it and must not be swallowed by an over-eager buffer.
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).expect("read response head");
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head).to_string();
        let status_line = head.lines().next().unwrap_or("").to_string();
        assert!(
            status_line.contains(" 101 "),
            "expected 101 Switching Protocols, got: {status_line}\nfull head:\n{head}"
        );
        // Pin the handshake *value*, not just header presence. The RFC 6455 sample key
        // `dGhlIHNhbXBsZSBub25jZQ==` derives the well-known accept `s3pPLMBiTxaQ9kYGzzhZRbK+xOo=`;
        // asserting it proves the proxy relays the header bytes unchanged rather than merely that
        // some accept header exists. hyper may normalize the header *name* casing, so match the
        // name case-insensitively but compare the base64 value exactly (it is case-sensitive).
        let accept = head
            .lines()
            .find_map(|l| {
                let (name, value) = l.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("sec-websocket-accept")
                    .then(|| value.trim())
            })
            .unwrap_or_else(|| panic!("no Sec-WebSocket-Accept header in response head:\n{head}"));
        assert_eq!(
            accept, "s3pPLMBiTxaQ9kYGzzhZRbK+xOo=",
            "accept value must match the RFC 6455 derivation for the sample key"
        );

        // Two round-trips prove the tunnel stays open for continued bidirectional traffic,
        // not just a single buffered flush at upgrade time.
        let echo1 = ws_echo_roundtrip(&mut stream, "ping-1").expect("roundtrip 1");
        assert_eq!(echo1, "ping-1");
        let echo2 = ws_echo_roundtrip(&mut stream, "ping-2").expect("roundtrip 2");
        assert_eq!(echo2, "ping-2");

        // Close half of AC #2 ("until either side closes"): send a masked close frame. The echo
        // server returns on opcode 8, dropping its connection; copy_bidirectional must propagate
        // that shutdown so our next read hits EOF rather than hanging to the 70s read timeout.
        let close = [0x88u8, 0x80, 0x12, 0x34, 0x56, 0x78];
        stream.write_all(&close).expect("write close frame");
        let mut tail = [0u8; 1];
        let n = stream.read(&mut tail).expect("read after close");
        assert_eq!(
            n, 0,
            "expected EOF after the upstream closed, got byte {:#04x}",
            tail[0]
        );
    })
    .await
    .expect("join ws client");

    let _ = sandbox.cmd().arg("down").arg("ws").output().await;
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
    let (status_line, _body) =
        tokio::task::spawn_blocking(move || http_get(proxy_port, "slowboot.adj.ac", "/"))
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
    assert!(
        stdout.contains("sudo pfctl"),
        "missing sudo pfctl: {stdout}"
    );
    // The reload pipeline replaces the active NAT ruleset — output must warn about it.
    assert!(
        stdout.contains("replaces the active NAT ruleset"),
        "missing NAT-replacement warning: {stdout}"
    );
    // Target proxy port matches the sandbox override.
    assert!(
        stdout.contains(&format!("port {}", sandbox.proxy_port)),
        "stdout doesn't mention proxy port {}: {stdout}",
        sandbox.proxy_port
    );
    // Output must be a valid shell script: outside heredocs, every line is a
    // comment, blank, or an actual command — never a bare `rdr …` line that
    // the shell would try to execute. The anchor body appears twice (once as
    // docs, once inside a `<<EOF … EOF` heredoc); only the heredoc copy may be raw.
    let mut in_heredoc = false;
    for (i, line) in stdout.lines().enumerate() {
        if line.contains("<<EOF") {
            in_heredoc = true;
            continue;
        }
        if in_heredoc {
            if line.trim_start().starts_with("EOF") {
                in_heredoc = false;
            }
            continue;
        }
        assert!(
            !line.trim_start().starts_with("rdr "),
            "line {} prints a bare `rdr` directive outside a heredoc — would error when output is piped to a script: {line:?}",
            i + 1
        );
    }
}

/// Issue #61: HMR heartbeat pings ride *inside* the upgraded WebSocket tunnel, so they never
/// touch `last_request` and an otherwise-quiet dev session looks idle to the scanner. A live
/// tunnel must keep its app alive past `idle_timeout`; once that tunnel closes, the app idle-
/// stops the same as any other. Holds a *silent* tunnel open — no frames at all — so the test
/// turns purely on tunnel liveness, the exact case a byte-counting scheme would miss.
#[tokio::test]
async fn idle_scanner_keeps_app_alive_while_websocket_tunnel_is_open() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = write_ws_echo_server(app_dir.path()).await;
    // Tiny idle window so the test is tractable; the tunnel heartbeat must beat it.
    write_app_with_idle_timeout(app_dir.path(), "wsidle", &cmd, "2s").await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);

    let proxy_port = sandbox.proxy_port;
    let (connected_tx, connected_rx) = std::sync::mpsc::channel::<()>();
    let (close_tx, close_rx) = std::sync::mpsc::channel::<()>();

    // Hold the tunnel open on a side thread so the async test can poke `adj status` while the
    // session is live. The thread completes the handshake, signals `connected`, then blocks —
    // sending nothing — until the test tells it to close, proving an idle-but-open tunnel keeps
    // the app alive. On close it sends a close frame and asserts the tunnel tears down (EOF).
    let client = std::thread::spawn(move || -> Result<(), String> {
        let mut stream =
            TcpStream::connect(("127.0.0.1", proxy_port)).map_err(|e| format!("connect: {e}"))?;
        stream
            .set_read_timeout(Some(Duration::from_secs(70)))
            .map_err(|e| format!("set_read_timeout: {e}"))?;
        let req = "GET /ws HTTP/1.1\r\n\
                   Host: wsidle.adj.ac\r\n\
                   Connection: Upgrade\r\n\
                   Upgrade: websocket\r\n\
                   Sec-WebSocket-Version: 13\r\n\
                   Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n";
        stream
            .write_all(req.as_bytes())
            .map_err(|e| format!("write handshake: {e}"))?;
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut byte = [0u8; 1];
            stream
                .read_exact(&mut byte)
                .map_err(|e| format!("read response head: {e}"))?;
            head.push(byte[0]);
        }
        let head = String::from_utf8_lossy(&head);
        let status_line = head.lines().next().unwrap_or("");
        if !status_line.contains(" 101 ") {
            return Err(format!(
                "expected 101 Switching Protocols, got: {status_line}"
            ));
        }
        connected_tx
            .send(())
            .map_err(|e| format!("signal connected: {e}"))?;

        // Hold the tunnel open, silent, until the test signals close.
        close_rx
            .recv()
            .map_err(|e| format!("await close signal: {e}"))?;

        let close = [0x88u8, 0x80, 0x12, 0x34, 0x56, 0x78];
        stream
            .write_all(&close)
            .map_err(|e| format!("write close frame: {e}"))?;
        let mut tail = [0u8; 1];
        let n = stream
            .read(&mut tail)
            .map_err(|e| format!("read after close: {e}"))?;
        if n != 0 {
            return Err(format!(
                "expected EOF after close, got byte {:#04x}",
                tail[0]
            ));
        }
        Ok(())
    });

    // Wait until the tunnel is established (which means the proxy has lazy-booted the app).
    tokio::task::spawn_blocking(move || connected_rx.recv())
        .await
        .expect("join connected recv")
        .expect("client should connect and upgrade");

    // Hold the silent tunnel open well past the 2s idle window. Without a tunnel-aware idle
    // heartbeat the scanner SIGTERMs the app at ~2s; with it the app stays Running.
    tokio::time::sleep(Duration::from_secs(4)).await;
    let status = sandbox
        .cmd()
        .arg("status")
        .arg("wsidle")
        .output()
        .await
        .expect("status");
    let out = String::from_utf8_lossy(&status.stdout);
    assert!(
        out.contains("running"),
        "open WS tunnel must keep app alive past idle_timeout, got: {out}"
    );

    // Close the tunnel; the app should now idle-stop on its own.
    close_tx.send(()).expect("send close signal");
    tokio::task::spawn_blocking(move || client.join())
        .await
        .expect("join client task")
        .expect("client thread panicked")
        .expect("client assertions");

    tokio::time::sleep(Duration::from_secs(4)).await;
    let status = sandbox
        .cmd()
        .arg("status")
        .arg("wsidle")
        .output()
        .await
        .expect("status");
    let out = String::from_utf8_lossy(&status.stdout);
    assert!(
        out.contains("stopped"),
        "app must idle-stop after its only WS tunnel closed, got: {out}"
    );

    sandbox.stop_daemon().await;
}
