use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};

fn adj_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}

struct Sandbox {
    _home: TempDir,
    home_path: PathBuf,
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

async fn write_manifest(dir: &Path, body: &str) {
    tokio::fs::write(dir.join("adjacent.toml"), body)
        .await
        .expect("write toml");
}

fn rotated(name: &str, n: usize) -> String {
    format!("{name}.log.{n}")
}

#[tokio::test]
async fn log_rotates_when_active_file_exceeds_max_size() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // Emit ~64 bytes per second, well over the 1KB cap. Each iteration writes a tagged
    // line so we can correlate the file contents back to the source iteration.
    let cmd = "i=0; while :; do i=$((i+1)); printf 'tick-%04d-padding-padding-padding-padding-padding\\n' $i; sleep 0.05; done";
    write_manifest(
        app_dir.path(),
        &format!(
            "name = \"rotator\"\ncmd = \"{cmd}\"\nlog_max_size = \"1KB\"\nlog_max_files = 3\n"
        ),
    )
    .await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {:?}", add);
    let up = sandbox
        .cmd()
        .arg("up")
        .arg("rotator")
        .output()
        .await
        .expect("up");
    assert!(up.status.success(), "up: {:?}", up);

    let logs_dir = sandbox.home_path.join("logs");
    // Wait until at least .1 and .2 exist — proves rotation happened more than once.
    wait_for(
        || {
            logs_dir.join(rotated("rotator", 1)).exists()
                && logs_dir.join(rotated("rotator", 2)).exists()
        },
        "rotator.log.1 and rotator.log.2 to appear",
        Duration::from_secs(10),
    )
    .await;

    // Active file should be small (post-rotation start).
    let active = logs_dir.join("rotator.log");
    let active_size = std::fs::metadata(&active).expect("active stat").len();
    assert!(
        active_size <= 2 * 1024,
        "active log unexpectedly large after rotation: {active_size}"
    );

    let _ = sandbox
        .cmd()
        .arg("down")
        .arg("rotator")
        .output()
        .await
        .expect("down");
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn old_rotated_files_are_pruned_to_max_files() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    let cmd = "i=0; while :; do i=$((i+1)); printf 'pp-%04d-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n' $i; sleep 0.02; done";
    // Keep only 2 rotated files. With a 512-byte cap and continuous emission, rotation
    // will quickly accumulate more than 2 files — but `.3` should never persist.
    write_manifest(
        app_dir.path(),
        &format!(
            "name = \"pruner\"\ncmd = \"{cmd}\"\nlog_max_size = \"512\"\nlog_max_files = 2\n"
        ),
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
        .arg("pruner")
        .output()
        .await
        .expect("up");

    let logs_dir = sandbox.home_path.join("logs");
    // Wait until we know rotation has happened at least 3 times (i.e. there's been a
    // moment where .3 would have existed had we not pruned).
    wait_for(
        || logs_dir.join(rotated("pruner", 2)).exists(),
        "pruner.log.2 to appear",
        Duration::from_secs(10),
    )
    .await;

    // Give the rotator enough cycles to attempt creating a .3.
    tokio::time::sleep(Duration::from_millis(800)).await;

    let three = logs_dir.join(rotated("pruner", 3));
    assert!(
        !three.exists(),
        "pruner.log.3 should have been pruned but exists"
    );

    let _ = sandbox
        .cmd()
        .arg("down")
        .arg("pruner")
        .output()
        .await
        .expect("down");
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn defaults_apply_when_config_omits_rotation_settings() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_manifest(
        app_dir.path(),
        "name = \"defaults\"\ncmd = \"echo hello-defaults; sleep 30\"\n",
    )
    .await;

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
        .arg("defaults")
        .output()
        .await
        .expect("up");
    assert!(up.status.success(), "up: {:?}", up);

    let active = sandbox.home_path.join("logs").join("defaults.log");
    wait_for(
        || {
            std::fs::read_to_string(&active)
                .map(|s| s.contains("hello-defaults"))
                .unwrap_or(false)
        },
        "defaults.log to contain marker",
        Duration::from_secs(5),
    )
    .await;

    // No rotation should occur under default 100MB cap.
    let logs_dir = sandbox.home_path.join("logs");
    assert!(
        !logs_dir.join(rotated("defaults", 1)).exists(),
        "rotated file should not exist under default 100MB cap"
    );

    let _ = sandbox
        .cmd()
        .arg("down")
        .arg("defaults")
        .output()
        .await
        .expect("down");
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn tail_continues_streaming_across_rotation() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    // Emit numbered lines steadily. A 1KB cap and ~50-byte lines means rotation fires
    // every ~20 lines. Across the test we'll see line numbers from before and after the
    // first rotation — proving `--tail` survived it.
    let cmd = "i=0; while :; do i=$((i+1)); printf 'tail-line-%05d-xxxxxxxxxxxxxxxxxxxxxxxxxxxxxx\\n' $i; sleep 0.05; done";
    write_manifest(
        app_dir.path(),
        &format!(
            "name = \"tailer\"\ncmd = \"{cmd}\"\nlog_max_size = \"1KB\"\nlog_max_files = 3\n"
        ),
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
        .arg("tailer")
        .output()
        .await
        .expect("up");

    let mut tail_child = sandbox
        .cmd()
        .arg("logs")
        .arg("tailer")
        .arg("--tail")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn tail");
    let stdout = tail_child.stdout.take().expect("tail stdout");
    let mut reader = BufReader::new(stdout);

    // Read lines until we've seen a rotation happen *and* we see a line whose number is
    // higher than any line that fit in the original file. Concretely: wait until
    // tailer.log.2 exists (so we know we've rotated at least twice), then keep reading
    // until we see a line tagged with a number higher than the highest we'd already
    // observed before that rotation.
    let logs_dir = sandbox.home_path.join("logs");
    let logs_dir_for_wait = logs_dir.clone();
    let mut highest_pre_rotation: Option<u32> = None;
    let mut got_post_rotation = false;

    let deadline = tokio::time::timeout(Duration::from_secs(15), async {
        let mut buf = String::new();
        loop {
            buf.clear();
            let n = reader.read_line(&mut buf).await.expect("tail read");
            if n == 0 {
                continue;
            }
            // Pull the line number out: "tail-line-NNNNN-..."
            if let Some(rest) = buf.strip_prefix("tail-line-") {
                let num: u32 = rest[..5].parse().unwrap_or(0);
                let rotated_twice = logs_dir_for_wait
                    .join(rotated("tailer", 2))
                    .exists();
                if !rotated_twice {
                    highest_pre_rotation =
                        Some(highest_pre_rotation.map_or(num, |h| h.max(num)));
                } else if let Some(h) = highest_pre_rotation {
                    if num > h + 5 {
                        // require a clear post-rotation line, not just a fluke read.
                        got_post_rotation = true;
                        return;
                    }
                }
            }
        }
    })
    .await;

    let _ = tail_child.start_kill();
    let _ = tail_child.wait().await;
    let _ = sandbox
        .cmd()
        .arg("down")
        .arg("tailer")
        .output()
        .await
        .expect("down");
    sandbox.stop_daemon().await;

    deadline.expect("did not see post-rotation lines via --tail in time");
    assert!(got_post_rotation, "did not observe a post-rotation line");
}
