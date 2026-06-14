//! Exercises the documented container convention (README "Containers"): an attached
//! `docker run --rm --init -p 127.0.0.1:$PORT:...` with `health_check_url`. Skips (passing)
//! when no Docker daemon is reachable, so the suite stays green on machines without Docker —
//! CI runs it on the Linux leg.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::process::{Child, Command};

const IMAGE: &str = "traefik/whoami";

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

fn http_get(proxy_port: u16, host: &str, path: &str) -> Result<(String, String), String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", proxy_port)).map_err(|e| format!("connect: {e}"))?;
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

async fn docker(args: &[&str]) -> Option<std::process::Output> {
    Command::new("docker").args(args).output().await.ok()
}

/// True when a Docker daemon is reachable and the test image is present (pulling it if needed).
async fn docker_ready() -> bool {
    match docker(&["info"]).await {
        Some(out) if out.status.success() => {}
        _ => return false,
    }
    if matches!(docker(&["image", "inspect", IMAGE]).await, Some(out) if out.status.success()) {
        return true;
    }
    matches!(docker(&["pull", IMAGE]).await, Some(out) if out.status.success())
}

/// `docker ps -a` ids matching the container name exactly.
async fn container_ids(cname: &str, all: bool) -> Vec<String> {
    let filter = format!("name=^{cname}$");
    let mut args = vec!["ps", "--format", "{{.ID}}", "--filter", &filter];
    if all {
        args.insert(1, "-a");
    }
    let out = docker(&args).await.expect("docker ps");
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect()
}

/// Force-removes the container on drop so a panicking assert doesn't leak it.
struct ContainerGuard(String);

impl Drop for ContainerGuard {
    fn drop(&mut self) {
        let _ = std::process::Command::new("docker")
            .args(["rm", "-f", &self.0])
            .output();
    }
}

#[tokio::test]
async fn docker_run_lazy_boots_and_down_stops_the_container() {
    if !docker_ready().await {
        eprintln!("skipping: no reachable Docker daemon (or {IMAGE} unavailable)");
        return;
    }

    let cname = format!("adj-test-whoami-{}", std::process::id());
    let _guard = ContainerGuard(cname.clone());

    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // The exact shape the README documents: attached, --rm, --init, $PORT mapped by the shell,
    // HTTP readiness because Docker binds the host port before the app inside is listening.
    let manifest = format!(
        "name = \"whoami\"\n\
         cmd = \"docker run --rm --init --name {cname} -p 127.0.0.1:$PORT:80 {IMAGE}\"\n\
         health_check_url = \"/\"\n\
         boot_timeout = 60\n"
    );
    write_manifest(app_dir.path(), &manifest).await;
    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {add:?}");

    // Lazy-boot through the proxy. whoami answers any GET with request details.
    let proxy_port = sandbox.proxy_port;
    let (status_line, body) =
        tokio::task::spawn_blocking(move || http_get(proxy_port, "whoami.adj.ac", "/"))
            .await
            .expect("join")
            .expect("http_get");
    assert!(status_line.contains(" 200 "), "expected 200: {status_line}");
    assert!(body.contains("Hostname"), "whoami body: {body}");

    assert_eq!(
        container_ids(&cname, false).await.len(),
        1,
        "container should be running after lazy boot"
    );

    // `adj down` SIGTERMs the process group; the attached docker client forwards it to the
    // container, which exits and (--rm) removes itself. The removal is asynchronous relative
    // to `down` returning, so poll.
    let down = sandbox
        .cmd()
        .arg("down")
        .arg("whoami")
        .output()
        .await
        .expect("down");
    assert!(down.status.success(), "down: {down:?}");

    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if container_ids(&cname, true).await.is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "container still present 15s after `adj down` — SIGTERM did not reach it"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let status = sandbox
        .cmd()
        .arg("status")
        .arg("whoami")
        .output()
        .await
        .expect("status");
    assert!(
        String::from_utf8_lossy(&status.stdout).contains("stopped"),
        "expected stopped after down: {status:?}"
    );

    sandbox.stop_daemon().await;
}
