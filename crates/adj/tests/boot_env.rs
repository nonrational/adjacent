// End-to-end: a supervised app sees the daemon-owned ADJ_* variables with correct values.
// Boots via the control-plane `up` (no proxy routing needed) and reads the echoed line back
// out of the app's log. The proxy/https ports are pinned to fixed non-zero values so the
// `_DIRECT` URLs are deterministic regardless of what else is bound on the host.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::process::{Child, Command};

fn adj_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}

struct Sandbox {
    _home: TempDir,
    home_path: std::path::PathBuf,
    daemon: Option<Child>,
}

impl Sandbox {
    fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let home_path = home.path().to_path_buf();
        Self {
            _home: home,
            home_path,
            daemon: None,
        }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(adj_bin());
        c.env("ADJACENT_HOME", &self.home_path);
        c.env("ADJACENT_PROXY_PORT", "18080");
        c.env("ADJACENT_HTTPS_PORT", "18443");
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
        self.daemon = Some(c.spawn().expect("spawn daemon"));

        let deadline = Instant::now() + Duration::from_secs(5);
        let sock = self.home_path.join("sock");
        while Instant::now() < deadline {
            if sock.exists() {
                let out = self
                    .cmd()
                    .arg("status")
                    .arg("__probe__")
                    .output()
                    .await
                    .expect("probe");
                if !String::from_utf8_lossy(&out.stderr).contains("daemon not reachable") {
                    return;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("daemon did not come up within 5s");
    }

    async fn stop_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

async fn write_app(dir: &Path, name: &str, cmd: &str) {
    // Use TOML literal strings (single-quoted) for cmd so embedded double-quotes in the shell
    // command don't break TOML parsing. Literal strings pass the value through verbatim.
    let body = format!("name = \"{name}\"\ncmd = '{cmd}'\n");
    tokio::fs::write(dir.join("adjacent.toml"), body)
        .await
        .expect("write toml");
}

#[tokio::test]
async fn boot_injects_adj_env_vars() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    // The cmd echoes every ADJ_* var into stdout (captured to the JSONL log), then sleeps so
    // the app stays Running while we read the log back.
    let app_dir = TempDir::new().expect("app dir");
    write_app(
        app_dir.path(),
        "alannorton-com",
        "echo \"NAME=$ADJ_NAME HOST=$ADJ_HOST URL=$ADJ_URL HTTP=$ADJ_URL_HTTP \
         DIRECT=$ADJ_URL_DIRECT HTTPDIRECT=$ADJ_URL_HTTP_DIRECT\"; sleep 60",
    )
    .await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add failed: {add:?}");

    let up = sandbox
        .cmd()
        .arg("up")
        .arg("alannorton-com")
        .output()
        .await
        .expect("up");
    assert!(up.status.success(), "up failed: {up:?}");

    // The echo is captured asynchronously; poll the log until the marker line lands.
    let deadline = Instant::now() + Duration::from_secs(5);
    let logs = loop {
        let out = sandbox
            .cmd()
            .arg("logs")
            .arg("alannorton-com")
            .output()
            .await
            .expect("logs");
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        if text.contains("NAME=alannorton-com") {
            break text;
        }
        if Instant::now() >= deadline {
            panic!("ADJ_* echo never appeared in logs; got:\n{text}");
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };

    assert!(logs.contains("NAME=alannorton-com"), "logs: {logs}");
    assert!(logs.contains("HOST=alannorton-com.adj.ac"), "logs: {logs}");
    assert!(logs.contains("URL=https://alannorton-com.adj.ac"), "logs: {logs}");
    assert!(logs.contains("HTTP=http://alannorton-com.adj.ac"), "logs: {logs}");
    assert!(
        logs.contains("DIRECT=https://alannorton-com.adj.ac:18443"),
        "logs: {logs}"
    );
    assert!(
        logs.contains("HTTPDIRECT=http://alannorton-com.adj.ac:18080"),
        "logs: {logs}"
    );

    let _ = sandbox
        .cmd()
        .arg("down")
        .arg("alannorton-com")
        .output()
        .await
        .expect("down");
    sandbox.stop_daemon().await;
}
