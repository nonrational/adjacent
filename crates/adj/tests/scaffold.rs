use std::process::Stdio;
use std::time::{Duration, Instant};

use tempfile::TempDir;
use tokio::process::{Child, Command};

fn adj_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}

// Minimal sandbox mirroring tests/tracer.rs: isolated ADJACENT_HOME + a daemon child.
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

    fn registry_has(&self, name: &str) -> bool {
        std::fs::read_to_string(self.home_path.join("registry.toml"))
            .map(|s| s.contains(name))
            .unwrap_or(false)
    }
}

#[tokio::test]
async fn add_scaffolds_and_registers_when_cmd_detected() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let parent = TempDir::new().unwrap();
    let app = parent.path().join("myapp");
    std::fs::create_dir(&app).unwrap();
    std::fs::write(app.join("package.json"), r#"{"scripts":{"dev":"vite"}}"#).unwrap();

    let add = sandbox.cmd().arg("add").arg(&app).output().await.unwrap();
    assert!(
        add.status.success(),
        "add failed: stdout={} stderr={}",
        String::from_utf8_lossy(&add.stdout),
        String::from_utf8_lossy(&add.stderr)
    );

    let toml = std::fs::read_to_string(app.join("adjacent.toml")).unwrap();
    assert!(toml.contains("name = \"myapp\""), "{toml}");
    assert!(toml.contains("cmd = \"npm run dev\""), "{toml}");

    assert!(sandbox.registry_has("myapp"), "myapp should be registered");

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn add_scaffolds_but_does_not_register_when_cmd_undetected() {
    // The not-detected path returns before contacting the daemon, so we deliberately don't
    // start one — confirming scaffolding is purely client-side.
    let sandbox = Sandbox::new().await;

    let parent = TempDir::new().unwrap();
    let app = parent.path().join("myapp");
    std::fs::create_dir(&app).unwrap();

    let add = sandbox.cmd().arg("add").arg(&app).output().await.unwrap();
    assert!(
        !add.status.success(),
        "expected non-zero exit when cmd undetected"
    );
    let stderr = String::from_utf8_lossy(&add.stderr);
    assert!(
        stderr.contains("couldn't detect a dev command"),
        "stderr: {stderr}"
    );
    assert!(stderr.contains("CONTRIBUTING"), "stderr: {stderr}");

    // The file is written despite the non-zero exit.
    assert!(
        app.join("adjacent.toml").exists(),
        "manifest should be written"
    );
    assert!(
        !sandbox.registry_has("myapp"),
        "myapp must not be registered"
    );
}

#[tokio::test]
async fn add_does_not_overwrite_existing_manifest() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let parent = TempDir::new().unwrap();
    let app = parent.path().join("keepapp");
    std::fs::create_dir(&app).unwrap();
    let manifest = app.join("adjacent.toml");
    let original = "name = \"keep\"\ncmd = \"echo hi; sleep 30\"\n";
    std::fs::write(&manifest, original).unwrap();

    let add = sandbox.cmd().arg("add").arg(&app).output().await.unwrap();
    assert!(
        add.status.success(),
        "add failed: stderr={}",
        String::from_utf8_lossy(&add.stderr)
    );

    let after = std::fs::read_to_string(&manifest).unwrap();
    assert_eq!(after, original, "existing manifest must not be rewritten");
    assert!(sandbox.registry_has("keep"), "keep should be registered");

    sandbox.stop_daemon().await;
}
