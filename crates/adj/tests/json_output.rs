// Tests for `--json` on read commands. These verify the documented schema in JSON.md:
// stable shape across `list`, `status`, `logs`, and that `logs --tail --json` streams JSONL.

use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;
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
        while Instant::now() < deadline {
            if sock.exists() {
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
async fn list_json_empty_registry_is_empty_array() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let out = sandbox
        .cmd()
        .arg("list")
        .arg("--json")
        .output()
        .await
        .expect("list");
    assert!(out.status.success(), "list --json failed");
    let body = String::from_utf8_lossy(&out.stdout);
    let parsed: Value = serde_json::from_str(body.trim()).expect("parse list --json");
    assert!(parsed.is_array(), "list --json must be a JSON array");
    assert_eq!(parsed.as_array().unwrap().len(), 0, "empty registry → []");

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn list_json_running_app_has_port() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_app(app_dir.path(), "lj-run", "echo up; sleep 60").await;
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
        .arg("lj-run")
        .output()
        .await
        .expect("up");
    assert!(up.status.success(), "up: {:?}", up);

    let out = sandbox
        .cmd()
        .arg("list")
        .arg("--json")
        .output()
        .await
        .expect("list");
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("parse");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 1);
    let entry = &arr[0];
    assert_eq!(entry["name"], "lj-run");
    assert_eq!(entry["state"], "running");
    assert!(entry["port"].is_u64(), "running entry must have port");
    assert!(entry.get("path").is_some());

    let _ = sandbox
        .cmd()
        .arg("down")
        .arg("lj-run")
        .output()
        .await
        .expect("down");
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn list_json_stopped_and_crashed_omit_port() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    // Stopped app: register but don't boot.
    let stopped_dir = TempDir::new().expect("stopped");
    write_app(stopped_dir.path(), "lj-stop", "echo never; sleep 60").await;
    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(stopped_dir.path())
        .output()
        .await
        .expect("add stopped");

    // Crashed app: boot and let it exit non-zero.
    let crashed_dir = TempDir::new().expect("crashed");
    write_app(crashed_dir.path(), "lj-crash", "echo bye; exit 7").await;
    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(crashed_dir.path())
        .output()
        .await
        .expect("add crashed");
    let _ = sandbox
        .cmd()
        .arg("up")
        .arg("lj-crash")
        .output()
        .await
        .expect("up crashed");

    // Wait for crash to register.
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_crashed = false;
    while Instant::now() < deadline {
        let out = sandbox
            .cmd()
            .arg("status")
            .arg("lj-crash")
            .arg("--json")
            .output()
            .await
            .expect("status");
        let v: Value = match serde_json::from_slice(&out.stdout) {
            Ok(v) => v,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        if v["state"] == "crashed" {
            saw_crashed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(saw_crashed, "never saw crashed state");

    let out = sandbox
        .cmd()
        .arg("list")
        .arg("--json")
        .output()
        .await
        .expect("list");
    let parsed: Value = serde_json::from_slice(&out.stdout).expect("parse");
    let arr = parsed.as_array().expect("array");
    assert_eq!(arr.len(), 2);
    for entry in arr {
        let state = entry["state"].as_str().expect("state string");
        assert!(state == "stopped" || state == "crashed", "got: {state}");
        assert!(
            entry.get("port").is_none(),
            "non-running entries must omit `port`: {entry}"
        );
    }

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn status_json_running_includes_pid_port_started_at() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_app(app_dir.path(), "sj-run", "echo up; sleep 60").await;
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
        .arg("sj-run")
        .output()
        .await
        .expect("up");

    let out = sandbox
        .cmd()
        .arg("status")
        .arg("sj-run")
        .arg("--json")
        .output()
        .await
        .expect("status");
    let v: Value = serde_json::from_slice(&out.stdout).expect("parse status");
    assert_eq!(v["state"], "running");
    assert!(v["pid"].is_u64(), "running → pid present");
    assert!(v["port"].is_u64(), "running → port present");
    assert!(
        v["started_at"].is_string(),
        "running → started_at present (rfc3339)"
    );
    assert!(v.get("exit_code").is_none(), "running must omit exit_code");
    let ts = v["started_at"].as_str().unwrap();
    assert!(
        ts.contains('T') && (ts.ends_with('Z') || ts.contains('+') || ts.contains('-')),
        "started_at not rfc3339-ish: {ts}"
    );

    let _ = sandbox
        .cmd()
        .arg("down")
        .arg("sj-run")
        .output()
        .await
        .expect("down");
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn status_json_stopped_omits_runtime_fields() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_app(app_dir.path(), "sj-stop", "echo hi; sleep 60").await;
    let _ = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");

    let out = sandbox
        .cmd()
        .arg("status")
        .arg("sj-stop")
        .arg("--json")
        .output()
        .await
        .expect("status");
    let v: Value = serde_json::from_slice(&out.stdout).expect("parse status");
    assert_eq!(v["state"], "stopped");
    assert_eq!(v["name"], "sj-stop");
    assert!(v.get("pid").is_none());
    assert!(v.get("port").is_none());
    assert!(v.get("exit_code").is_none());
    assert!(v.get("started_at").is_none());

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn status_json_crashed_includes_exit_code() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_app(app_dir.path(), "sj-crash", "echo bye; exit 9").await;
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
        .arg("sj-crash")
        .output()
        .await
        .expect("up");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut got: Option<Value> = None;
    while Instant::now() < deadline {
        let out = sandbox
            .cmd()
            .arg("status")
            .arg("sj-crash")
            .arg("--json")
            .output()
            .await
            .expect("status");
        if let Ok(v) = serde_json::from_slice::<Value>(&out.stdout) {
            if v["state"] == "crashed" {
                got = Some(v);
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let v = got.expect("never saw crashed state in status --json");
    assert_eq!(v["state"], "crashed");
    assert_eq!(v["exit_code"], 9);
    assert!(v.get("pid").is_none());
    assert!(v.get("port").is_none());
    assert!(v.get("started_at").is_none());

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn logs_json_tags_stdout_and_stderr() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // Emit one line to stdout and one to stderr, then linger so the log writer flushes.
    write_app(
        app_dir.path(),
        "lg-tag",
        "echo out-marker; echo err-marker 1>&2; sleep 30",
    )
    .await;
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
        .arg("lg-tag")
        .output()
        .await
        .expect("up");

    let log_path = sandbox.home_path.join("logs").join("lg-tag.log");
    wait_for(
        || {
            std::fs::read_to_string(&log_path)
                .map(|s| s.contains("out-marker") && s.contains("err-marker"))
                .unwrap_or(false)
        },
        "both markers in log",
        Duration::from_secs(5),
    )
    .await;

    let out = sandbox
        .cmd()
        .arg("logs")
        .arg("lg-tag")
        .arg("--json")
        .output()
        .await
        .expect("logs --json");
    assert!(out.status.success(), "logs --json failed");
    let body = String::from_utf8_lossy(&out.stdout);
    let mut saw_out = false;
    let mut saw_err = false;
    for line in body.lines() {
        if line.trim().is_empty() {
            continue;
        }
        let v: Value =
            serde_json::from_str(line).unwrap_or_else(|e| panic!("invalid JSONL `{line}`: {e}"));
        assert!(v["ts"].is_string(), "ts missing in {line}");
        assert!(v["stream"].is_string(), "stream missing in {line}");
        assert!(v["line"].is_string(), "line missing in {line}");
        let stream = v["stream"].as_str().unwrap();
        let content = v["line"].as_str().unwrap();
        if content == "out-marker" {
            assert_eq!(stream, "stdout");
            saw_out = true;
        }
        if content == "err-marker" {
            assert_eq!(stream, "stderr");
            saw_err = true;
        }
    }
    assert!(saw_out, "no stdout-tagged record for out-marker");
    assert!(saw_err, "no stderr-tagged record for err-marker");

    let _ = sandbox
        .cmd()
        .arg("down")
        .arg("lg-tag")
        .output()
        .await
        .expect("down");
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn logs_tail_json_streams_new_records() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // A loop that prints a unique marker, then sleeps, lets us prove records arrive over time.
    write_app(
        app_dir.path(),
        "lg-tail",
        "for i in 1 2 3 4 5; do echo tail-marker-$i; sleep 0.2; done; sleep 30",
    )
    .await;
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
        .arg("lg-tail")
        .output()
        .await
        .expect("up");

    let mut tail = sandbox
        .cmd()
        .arg("logs")
        .arg("lg-tail")
        .arg("--tail")
        .arg("--json")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tail");
    let stdout = tail.stdout.take().expect("tail stdout");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let saw_marker = tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            line.clear();
            let n = reader.read_line(&mut line).await.expect("read tail");
            if n == 0 {
                continue;
            }
            let v: Value = match serde_json::from_str(line.trim()) {
                Ok(v) => v,
                Err(_) => continue,
            };
            // Every record must satisfy the schema.
            assert!(v["ts"].is_string(), "ts missing");
            assert!(v["stream"].is_string(), "stream missing");
            assert!(v["line"].is_string(), "line missing");
            if v["line"].as_str().unwrap().starts_with("tail-marker-") {
                return;
            }
        }
    })
    .await;
    saw_marker.expect("did not see a tail-marker record via --tail --json");

    let _ = tail.start_kill();
    let _ = tail.wait().await;
    let _ = sandbox
        .cmd()
        .arg("down")
        .arg("lg-tail")
        .output()
        .await
        .expect("down");
    sandbox.stop_daemon().await;
}
