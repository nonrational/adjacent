use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use nix::sys::signal::kill;
use nix::unistd::Pid;
use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
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
    async fn new() -> Self {
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
        c.env("RUST_LOG", "warn");
        c
    }

    async fn start_daemon(&mut self) {
        let mut c = self.cmd();
        c.arg("daemon");
        c.stdout(Stdio::null());
        c.stderr(Stdio::null());
        let child = c.spawn().expect("spawn daemon");
        self.daemon = Some(child);

        // Wait for the socket to appear and respond.
        let deadline = Instant::now() + Duration::from_secs(5);
        let sock = self.home_path.join("sock");
        while Instant::now() < deadline {
            if sock.exists() {
                // Try a status call against an unknown app; we expect a clean error not a connection failure.
                let out = self
                    .cmd()
                    .arg("status")
                    .arg("__probe__")
                    .output()
                    .await
                    .expect("probe");
                let stderr = String::from_utf8_lossy(&out.stderr);
                if !stderr.contains("daemon not reachable") {
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
    let manifest = dir.join("adjacent.toml");
    let body = format!("name = \"{name}\"\ncmd = \"{cmd}\"\n");
    tokio::fs::write(manifest, body).await.expect("write toml");
}

async fn wait_for<F>(mut check: F, label: &str, deadline: Duration)
where
    F: FnMut() -> bool,
{
    let start = Instant::now();
    while start.elapsed() < deadline {
        if check() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("timed out waiting for: {label}");
}

#[tokio::test]
async fn tracer_add_up_logs_down() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    // App that prints a marker, then backgrounds a sleep and records its PID. We need a real
    // descendant PID (not the shell's PID) to probe whether `adj down` reached the full
    // process group — without the process-group fix, signals hit only `sh` and the sleep
    // gets reparented to init.
    let app_dir = TempDir::new().expect("app dir");
    let descendant_pid_file = app_dir.path().join("descendant.pid");
    let cmd = format!(
        "echo hello-from-demo; sleep 60 & echo $! > {}; wait",
        descendant_pid_file.display()
    );
    write_app(app_dir.path(), "demo", &cmd).await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(
        add.status.success(),
        "add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );

    let list = sandbox.cmd().arg("list").output().await.expect("list");
    assert!(list.status.success());
    let list_out = String::from_utf8_lossy(&list.stdout);
    assert!(list_out.contains("demo"), "list output: {list_out}");

    let up = sandbox
        .cmd()
        .arg("up")
        .arg("demo")
        .output()
        .await
        .expect("up");
    assert!(
        up.status.success(),
        "up failed: stderr={}",
        String::from_utf8_lossy(&up.stderr)
    );

    let log_path = sandbox.home_path.join("logs").join("demo.log");
    wait_for(
        || {
            std::fs::read_to_string(&log_path)
                .map(|s| s.contains("hello-from-demo"))
                .unwrap_or(false)
        },
        "log file to contain marker",
        Duration::from_secs(5),
    )
    .await;

    let logs = sandbox
        .cmd()
        .arg("logs")
        .arg("demo")
        .output()
        .await
        .expect("logs");
    assert!(logs.status.success());
    let logs_out = String::from_utf8_lossy(&logs.stdout);
    assert!(
        logs_out.contains("hello-from-demo"),
        "logs output missing marker: {logs_out}"
    );

    // status should report running
    let status = sandbox
        .cmd()
        .arg("status")
        .arg("demo")
        .output()
        .await
        .expect("status");
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_out.contains("running"),
        "status output: {status_out}"
    );

    // logs --tail streaming check.
    let mut tail_child = sandbox
        .cmd()
        .arg("logs")
        .arg("demo")
        .arg("--tail")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tail");
    let stdout = tail_child.stdout.take().expect("tail stdout");
    let mut reader = BufReader::new(stdout);
    let mut buf = String::new();
    let deadline = Duration::from_secs(5);
    let read_initial = tokio::time::timeout(deadline, async {
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf).await.expect("read tail");
            if n == 0 {
                continue;
            }
            if buf.contains("hello-from-demo") {
                return;
            }
        }
    });
    read_initial.await.expect("did not see marker via --tail");
    let _ = tail_child.start_kill();
    let _ = tail_child.wait().await;

    // Capture the descendant PID *before* we tear the app down — without the process-group
    // fix, this PID survives `adj down` (orphaned to init) even though the supervisor
    // reports the app stopped.
    wait_for(
        || descendant_pid_file.exists(),
        "descendant.pid to appear",
        Duration::from_secs(5),
    )
    .await;
    let descendant_pid: i32 = std::fs::read_to_string(&descendant_pid_file)
        .expect("read descendant pid")
        .trim()
        .parse()
        .expect("parse descendant pid");

    let down = sandbox
        .cmd()
        .arg("down")
        .arg("demo")
        .output()
        .await
        .expect("down");
    assert!(
        down.status.success(),
        "down failed: stderr={}",
        String::from_utf8_lossy(&down.stderr)
    );

    // After `down`, status must report `stopped` — the daemon distinguishes intentional
    // termination from a crash.
    let status = sandbox
        .cmd()
        .arg("status")
        .arg("demo")
        .output()
        .await
        .expect("status");
    let status_out = String::from_utf8_lossy(&status.stdout);
    assert!(
        status_out.contains("stopped"),
        "expected status `stopped` after down, got: {status_out}"
    );

    // The descendant `sleep` must actually be gone — `kill(pid, 0)` should fail with ESRCH.
    // Poll briefly to give the kernel a beat to reap it.
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut gone = false;
    while Instant::now() < deadline {
        match kill(Pid::from_raw(descendant_pid), None) {
            Err(nix::errno::Errno::ESRCH) => {
                gone = true;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    assert!(
        gone,
        "descendant PID {descendant_pid} still alive after down — process-group signal leaked"
    );

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn commands_fail_cleanly_without_daemon() {
    let sandbox = Sandbox::new().await;
    let out = sandbox.cmd().arg("list").output().await.expect("list");
    assert!(!out.status.success(), "list should fail with no daemon");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("daemon not reachable"),
        "expected friendly error, got: {stderr}"
    );
    // No panic markers in stderr.
    assert!(!stderr.contains("panicked"), "panic in stderr: {stderr}");
}

#[tokio::test]
async fn add_resolves_relative_path_against_client_cwd() {
    // Regression: canonicalize used to happen on the daemon side, which resolves relative
    // paths against the daemon's CWD (the launchd home, in production). The client must
    // canonicalize before sending so `adj add ./child` resolves against the user's CWD.
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let parent = TempDir::new().expect("parent dir");
    let child_dir = parent.path().join("child");
    std::fs::create_dir(&child_dir).expect("mkdir child");
    write_app(&child_dir, "rel-demo", "echo hi; sleep 30").await;

    // Spawn `adj add ./child` with CWD set to the parent directory. The daemon is rooted
    // elsewhere, so a daemon-side canonicalize would either fail or land on the wrong path.
    let add = sandbox
        .cmd()
        .current_dir(parent.path())
        .arg("add")
        .arg("./child")
        .output()
        .await
        .expect("add");
    assert!(
        add.status.success(),
        "relative-path add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );

    let registry = sandbox.home_path.join("registry.toml");
    let raw = std::fs::read_to_string(&registry).expect("read registry");
    let expected = std::fs::canonicalize(&child_dir).expect("canonical child");
    assert!(
        raw.contains(&expected.display().to_string()),
        "registry missing canonical child path. expected substring `{}`, got:\n{}",
        expected.display(),
        raw
    );

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn crash_is_reported_with_exit_code() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // Exit non-zero quickly.
    write_app(app_dir.path(), "crasher", "echo about-to-die; exit 42").await;

    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    let _ = sandbox
        .cmd()
        .arg("up")
        .arg("crasher")
        .output()
        .await
        .expect("up");

    // Poll status until it flips to crashed.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut last = String::new();
    while Instant::now() < deadline {
        let status = sandbox
            .cmd()
            .arg("status")
            .arg("crasher")
            .output()
            .await
            .expect("status");
        last = String::from_utf8_lossy(&status.stdout).into_owned();
        if last.contains("crashed") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(last.contains("crashed"), "never saw crashed state: {last}");
    assert!(last.contains("42"), "exit code missing from status: {last}");

    sandbox.stop_daemon().await;
}
