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

/// Bind :0, read the assigned port, close. The kernel may reissue this number to anyone before
/// the daemon claims it, but for a localhost test that's rare enough to accept.
fn pick_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    l.local_addr().expect("local_addr").port()
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
            proxy_port: pick_port(),
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
        let proxy_addr = format!("127.0.0.1:{}", self.proxy_port);
        let mut sock_ready = false;
        let mut proxy_ready = false;
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
            if !proxy_ready && TcpStream::connect(&proxy_addr).is_ok() {
                proxy_ready = true;
            }
            if sock_ready && proxy_ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("daemon did not come up within 5s (sock={sock_ready}, proxy={proxy_ready})");
    }

    async fn stop_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

/// Run git in `dir` with a hermetic identity so the test doesn't depend on (or trip over)
/// the developer's global config — including gpg signing.
async fn git(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=adj-test",
            "-c",
            "user.email=adj-test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        // Prevent the developer's global init.templateDir and core.hooksPath from leaking
        // into test repos — either can inject hooks that break the hermetic git setup.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .await
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Echo server that responds with the contents of `marker.txt` from its working directory.
/// The supervisor runs `cmd` with the app dir as cwd, so each registered directory (main
/// checkout vs worktree) serves its own marker — that's how the tests tell instances apart.
async fn write_echo_server(dir: &Path) {
    let py = r#"import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        body = open("marker.txt", "rb").read()
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a, **kw):
        pass
ThreadingHTTPServer(("127.0.0.1", int(os.environ["PORT"])), H).serve_forever()
"#;
    tokio::fs::write(dir.join("server.py"), py)
        .await
        .expect("write server.py");
}

async fn write_app(dir: &Path, name: &str) {
    write_app_with_cmd(dir, name, "exec /usr/bin/python3 server.py").await;
}

async fn write_app_with_cmd(dir: &Path, name: &str, cmd: &str) {
    let body = format!("name = \"{name}\"\ncmd = \"{cmd}\"\n");
    tokio::fs::write(dir.join("adjacent.toml"), body)
        .await
        .expect("write toml");
}

async fn write_marker(dir: &Path, marker: &str) {
    tokio::fs::write(dir.join("marker.txt"), marker)
        .await
        .expect("write marker");
}

/// Send an HTTP GET to the proxy with the given Host header, return (status_line, body).
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

async fn http_get_async(proxy_port: u16, host: &str, path: &str) -> (String, String) {
    let host = host.to_string();
    let path = path.to_string();
    tokio::task::spawn_blocking(move || http_get(proxy_port, &host, &path))
        .await
        .expect("join")
        .expect("http_get")
}

#[tokio::test]
async fn label_flag_registers_routable_instance() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_echo_server(app_dir.path()).await;
    write_marker(app_dir.path(), "hello-from-instance").await;
    write_app(app_dir.path(), "site").await;

    let add = sandbox
        .cmd()
        .arg("add")
        .arg("--label")
        .arg("demo")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {add:?}");
    let stdout = String::from_utf8_lossy(&add.stdout);
    assert!(stdout.contains("demo.site"), "stdout: {stdout}");

    let (status_line, body) =
        http_get_async(sandbox.proxy_port, "demo.site.adj.ac", "/").await;
    assert!(status_line.contains(" 200 "), "status: {status_line}");
    assert!(body.contains("hello-from-instance"), "body: {body}");

    // Only the instance was registered — the bare name must not resolve.
    let (nf_status, _) = http_get_async(sandbox.proxy_port, "site.adj.ac", "/").await;
    assert!(nf_status.contains(" 404 "), "expected 404 for bare name: {nf_status}");

    // Invalid labels are rejected daemon-side.
    let bad = sandbox
        .cmd()
        .arg("add")
        .arg("--label")
        .arg("Bad_Label")
        .arg(app_dir.path())
        .output()
        .await
        .expect("add bad label");
    assert!(!bad.status.success(), "uppercase/underscore label must be rejected");

    let _ = sandbox.cmd().arg("down").arg("demo.site").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn worktree_add_derives_label_from_branch() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    // Main checkout: a real git repo whose committed tree carries adjacent.toml + server.py,
    // so `git worktree add` materializes both in the linked worktree.
    let repo = TempDir::new().expect("repo dir");
    git(repo.path(), &["init", "-q"]).await;
    write_echo_server(repo.path()).await;
    write_app(repo.path(), "site").await;
    git(repo.path(), &["add", "-A"]).await;
    git(repo.path(), &["commit", "-q", "-m", "app skeleton"]).await;
    // marker.txt stays untracked on purpose: each directory writes its own, which is how the
    // assertions below tell the two instances apart.
    write_marker(repo.path(), "from-main").await;

    // Linked worktree on a branch that exercises sanitization (slash, underscore, caps).
    let wt_parent = TempDir::new().expect("wt parent");
    let wt = wt_parent.path().join("wt");
    git(
        repo.path(),
        &["worktree", "add", "-b", "agents/Fix_Thing", wt.to_str().unwrap()],
    )
    .await;
    write_marker(&wt, "from-worktree").await;

    let add_main = sandbox.cmd().arg("add").arg(repo.path()).output().await.expect("add main");
    assert!(add_main.status.success(), "add main: {add_main:?}");
    let main_out = String::from_utf8_lossy(&add_main.stdout);
    assert!(main_out.contains("`site`"), "main registered bare: {main_out}");

    let add_wt = sandbox.cmd().arg("add").arg(&wt).output().await.expect("add wt");
    assert!(add_wt.status.success(), "add wt: {add_wt:?}");
    let wt_out = String::from_utf8_lossy(&add_wt.stdout);
    assert!(
        wt_out.contains("agents-fix-thing.site"),
        "worktree registered as instance: {wt_out}"
    );

    let (s1, b1) = http_get_async(sandbox.proxy_port, "site.adj.ac", "/").await;
    assert!(s1.contains(" 200 "), "main status: {s1}");
    assert!(b1.contains("from-main"), "main body: {b1}");

    let (s2, b2) =
        http_get_async(sandbox.proxy_port, "agents-fix-thing.site.adj.ac", "/").await;
    assert!(s2.contains(" 200 "), "worktree status: {s2}");
    assert!(b2.contains("from-worktree"), "worktree body: {b2}");

    let _ = sandbox.cmd().arg("down").arg("site").output().await;
    let _ = sandbox.cmd().arg("down").arg("agents-fix-thing.site").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn detached_worktree_requires_explicit_label() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let repo = TempDir::new().expect("repo dir");
    git(repo.path(), &["init", "-q"]).await;
    write_echo_server(repo.path()).await;
    write_app(repo.path(), "site").await;
    git(repo.path(), &["add", "-A"]).await;
    git(repo.path(), &["commit", "-q", "-m", "app skeleton"]).await;

    let wt_parent = TempDir::new().expect("wt parent");
    let wt = wt_parent.path().join("wt");
    git(
        repo.path(),
        &["worktree", "add", "--detach", wt.to_str().unwrap()],
    )
    .await;

    let add = sandbox.cmd().arg("add").arg(&wt).output().await.expect("add");
    assert!(!add.status.success(), "detached worktree add must fail");
    let stderr = String::from_utf8_lossy(&add.stderr);
    assert!(stderr.contains("--label"), "error should point at --label: {stderr}");

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn prune_clears_deleted_worktrees() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let repo = TempDir::new().expect("repo dir");
    git(repo.path(), &["init", "-q"]).await;
    write_echo_server(repo.path()).await;
    write_app(repo.path(), "site").await;
    git(repo.path(), &["add", "-A"]).await;
    git(repo.path(), &["commit", "-q", "-m", "app skeleton"]).await;
    write_marker(repo.path(), "from-main").await;

    let wt_parent = TempDir::new().expect("wt parent");
    let wt = wt_parent.path().join("wt");
    git(
        repo.path(),
        &["worktree", "add", "-b", "feature-x", wt.to_str().unwrap()],
    )
    .await;
    write_marker(&wt, "from-worktree").await;

    for dir in [repo.path(), wt.as_path()] {
        let add = sandbox.cmd().arg("add").arg(dir).output().await.expect("add");
        assert!(add.status.success(), "add {dir:?}: {add:?}");
    }

    // Boot the instance once so prune also exercises the "still running" path.
    let (s, _) = http_get_async(sandbox.proxy_port, "feature-x.site.adj.ac", "/").await;
    assert!(s.contains(" 200 "), "pre-delete boot: {s}");

    // Simulate the agent's branch merging: the worktree directory disappears.
    std::fs::remove_dir_all(&wt).expect("delete worktree");

    // list --json flags the dead entry; the healthy one carries no `stale` key.
    let list = sandbox.cmd().args(["list", "--json"]).output().await.expect("list");
    let entries: serde_json::Value =
        serde_json::from_slice(&list.stdout).expect("parse list json");
    let by_name = |name: &str| -> serde_json::Value {
        entries
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == name)
            .unwrap_or_else(|| panic!("no entry `{name}` in {entries}"))
            .clone()
    };
    assert_eq!(by_name("feature-x.site")["stale"], serde_json::json!(true));
    assert!(
        by_name("site").get("stale").is_none(),
        "healthy entry must not carry a stale key: {entries}"
    );

    // Requests to a stale entry get a 502 that names the fix.
    let (stale_status, stale_body) =
        http_get_async(sandbox.proxy_port, "feature-x.site.adj.ac", "/").await;
    assert!(stale_status.contains(" 502 "), "stale status: {stale_status}");
    assert!(stale_body.contains("adj prune"), "stale body: {stale_body}");

    // Prune removes exactly the stale entry and reports it.
    let prune = sandbox.cmd().arg("prune").output().await.expect("prune");
    assert!(prune.status.success(), "prune: {prune:?}");
    let prune_out = String::from_utf8_lossy(&prune.stdout);
    assert!(prune_out.contains("feature-x.site"), "prune stdout: {prune_out}");

    let list2 = sandbox.cmd().args(["list", "--json"]).output().await.expect("list2");
    let entries2: serde_json::Value =
        serde_json::from_slice(&list2.stdout).expect("parse list2 json");
    let names: Vec<&str> = entries2
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["site"], "only the healthy app survives prune");

    // A second prune is a no-op.
    let prune2 = sandbox.cmd().arg("prune").output().await.expect("prune2");
    let prune2_out = String::from_utf8_lossy(&prune2.stdout);
    assert!(prune2_out.contains("nothing to prune"), "prune2: {prune2_out}");

    let _ = sandbox.cmd().arg("down").arg("site").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn remove_stops_and_deregisters_one_app() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_echo_server(app_dir.path()).await;
    write_marker(app_dir.path(), "removable").await;
    write_app(app_dir.path(), "gone").await;

    let add = sandbox.cmd().arg("add").arg(app_dir.path()).output().await.expect("add");
    assert!(add.status.success(), "add: {add:?}");

    // Boot it so remove exercises the down-first path.
    let (s, _) = http_get_async(sandbox.proxy_port, "gone.adj.ac", "/").await;
    assert!(s.contains(" 200 "), "boot: {s}");

    let rm = sandbox.cmd().arg("remove").arg("gone").output().await.expect("remove");
    assert!(rm.status.success(), "remove: {rm:?}");
    assert!(String::from_utf8_lossy(&rm.stdout).contains("gone"));

    let (nf, _) = http_get_async(sandbox.proxy_port, "gone.adj.ac", "/").await;
    assert!(nf.contains(" 404 "), "after remove: {nf}");

    // Removing an unknown name is an error.
    let rm2 = sandbox.cmd().arg("remove").arg("gone").output().await.expect("remove2");
    assert!(!rm2.status.success(), "second remove must fail");

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn removed_app_reregisters_with_clean_state() {
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    // Register a crash-on-start app so the supervisor records a Crashed entry, giving us a
    // non-trivial pre-remove state to assert against after re-registration.
    let crash_dir = TempDir::new().expect("crash dir");
    write_app_with_cmd(crash_dir.path(), "phoenix", "exit 7").await;

    let add = sandbox.cmd().arg("add").arg(crash_dir.path()).output().await.expect("add");
    assert!(add.status.success(), "add: {add:?}");

    // Explicitly boot it so the supervisor has a Running → Crashed transition to record.
    let _ = sandbox.cmd().arg("up").arg("phoenix").output().await.expect("up");

    // Poll until the supervisor records the crash (the wait task is async; give it 5s).
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let list = sandbox.cmd().args(["list", "--json"]).output().await.expect("list");
        let entries: serde_json::Value = serde_json::from_slice(&list.stdout).expect("parse");
        let state = entries
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "phoenix")
            .and_then(|e| e["state"].as_str())
            .unwrap_or("")
            .to_string();
        if state == "crashed" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "phoenix did not crash within 5s, state={state}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    // Remove and immediately re-add the same directory.
    let rm = sandbox.cmd().arg("remove").arg("phoenix").output().await.expect("remove");
    assert!(rm.status.success(), "remove: {rm:?}");

    let add2 = sandbox.cmd().arg("add").arg(crash_dir.path()).output().await.expect("re-add");
    assert!(add2.status.success(), "re-add: {add2:?}");

    // The re-added app must start from a clean Stopped slate — not the pre-remove Crashed state.
    // Without supervisor::forget() the old AppRuntime lingers and list reports "crashed".
    let list = sandbox.cmd().args(["list", "--json"]).output().await.expect("list2");
    let entries: serde_json::Value = serde_json::from_slice(&list.stdout).expect("parse2");
    let entry = entries
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "phoenix")
        .expect("phoenix entry");
    assert_eq!(
        entry["state"], "stopped",
        "re-added app must start from a clean slate: {entries}"
    );

    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn agent_instructions_prefers_registered_label_over_branch() {
    // `adj add --label` can register a key that differs from the branch. agent-instructions must
    // steer at the registered key, not re-derive the (different) branch label — otherwise every
    // command and URL in the doc points at an unregistered instance.
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let repo = TempDir::new().expect("repo dir");
    git(repo.path(), &["init", "-q"]).await;
    write_echo_server(repo.path()).await;
    write_app(repo.path(), "site").await;
    git(repo.path(), &["add", "-A"]).await;
    git(repo.path(), &["commit", "-q", "-m", "app skeleton"]).await;

    // Linked worktree on branch `feature-x` — the branch label would derive to `feature-x`.
    let wt_parent = TempDir::new().expect("wt parent");
    let wt = wt_parent.path().join("wt");
    git(
        repo.path(),
        &["worktree", "add", "-b", "feature-x", wt.to_str().unwrap()],
    )
    .await;

    // Register the worktree under an explicit label that does NOT match the branch.
    let add = sandbox
        .cmd()
        .arg("add")
        .arg("--label")
        .arg("prod")
        .arg(&wt)
        .output()
        .await
        .expect("add");
    assert!(add.status.success(), "add: {add:?}");
    assert!(
        String::from_utf8_lossy(&add.stdout).contains("prod.site"),
        "add registered the explicit label: {add:?}"
    );

    let out = sandbox
        .cmd()
        .arg("agent-instructions")
        .arg("--path")
        .arg(&wt)
        .output()
        .await
        .expect("agent-instructions");
    assert!(out.status.success(), "agent-instructions: {out:?}");
    let stdout = String::from_utf8_lossy(&out.stdout);

    assert!(
        stdout.contains("adj status prod.site") && stdout.contains("prod.site.adj.ac"),
        "doc must use the registered key `prod.site`: {stdout}"
    );
    assert!(
        !stdout.contains("feature-x.site"),
        "doc must not re-derive the branch label `feature-x.site`: {stdout}"
    );

    let _ = sandbox.cmd().arg("down").arg("prod.site").output().await;
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn idle_scanner_reaps_deregistered_running_app() {
    // Backstop for the remove/prune resurrection race: a Running app whose registry row has gone
    // away must be reaped even when idle_timeout is "off" (the idle window never fires). Simulate
    // the post-deregistration state by dropping the registry row out from under a live process,
    // then assert the scanner stops it. Without the backstop an idle-off orphan leaks forever.
    let mut sandbox = Sandbox::new().await;
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_echo_server(app_dir.path()).await;
    write_marker(app_dir.path(), "orphan").await;
    // idle_timeout = "off" so the orphan backstop is the ONLY thing that can stop it.
    tokio::fs::write(
        app_dir.path().join("adjacent.toml"),
        "name = \"ghost\"\ncmd = \"exec /usr/bin/python3 server.py\"\nidle_timeout = \"off\"\n",
    )
    .await
    .expect("write toml");

    let add = sandbox.cmd().arg("add").arg(app_dir.path()).output().await.expect("add");
    assert!(add.status.success(), "add: {add:?}");

    // Boot it.
    let (s, _) = http_get_async(sandbox.proxy_port, "ghost.adj.ac", "/").await;
    assert!(s.contains(" 200 "), "boot: {s}");

    // Drop the registry row directly, leaving the supervisor with a Running entry and no registry
    // backing — the exact state the documented resurrection race produces.
    let registry_path = sandbox.home_path.join("registry.toml");
    tokio::fs::write(&registry_path, "").await.expect("truncate registry");

    // Give the 500ms scanner several sweeps to observe the unregistered-but-running app and reap
    // it, then re-register the same directory so `status` (which requires a registry row) can read
    // the supervisor's post-reap state.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let readd = sandbox.cmd().arg("add").arg(app_dir.path()).output().await.expect("re-add");
    assert!(readd.status.success(), "re-add: {readd:?}");

    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let out = sandbox
            .cmd()
            .args(["status", "ghost", "--json"])
            .output()
            .await
            .expect("status");
        let v: serde_json::Value =
            serde_json::from_slice(&out.stdout).unwrap_or_else(|_| serde_json::json!({}));
        if v["state"] == "stopped" {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "scanner did not reap the deregistered idle-off app: {v}"
        );
        tokio::time::sleep(Duration::from_millis(150)).await;
    }

    sandbox.stop_daemon().await;
}
