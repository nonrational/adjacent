# Worktree Instances Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Let multiple git worktrees of the same app register with Adjacent, each routable at `<label>.<name>.adj.ac`, with a `remove`/`prune`/`stale` cleanup story.

**Architecture:** Instances are dotted registry keys (`feature-x.site`) in the existing flat `name → path` map — no structural parent/child. The client derives the label from the git branch when `adj add` runs inside a linked worktree. The TLS leaf re-issues with a `*.<base>.adj.ac` SAN per registered base name and hot-swaps via a rustls cert resolver. Spec: `docs/superpowers/specs/2026-06-11-worktree-instances-design.md`.

**Tech Stack:** Rust (workspace pinned 1.92.0), tokio, hyper, rustls 0.23 + rcgen + x509-parser (all already dependencies — no new crates).

---

## Project conventions you MUST follow

- **Commits:** plain descriptive messages, NO Conventional Commit prefixes ("feat:", "fix:" are forbidden). Always commit as the agent identity with explicit paths, never `-a`:
  ```bash
  git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit <paths> -m "$(cat <<'EOF'
  <message>

  Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
  EOF
  )"
  ```
- **Comments describe WHY, not WHAT.** The codebase uses dense block comments above tricky races — match that style.
- Run commands from the repo root `/Users/norton/src/adjacent`. The binary builds to `target/debug/adj`; integration tests find it via `CARGO_BIN_EXE_adj`.
- Integration tests sandbox all state via `ADJACENT_HOME=<tmpdir>` and spawn their own daemon — see `crates/adj/tests/proxy.rs` for the `Sandbox` harness pattern this plan copies.
- The full suite is `cargo test`. Some TLS unit tests are macOS-gated and touch the login keychain; they clean up after themselves.

---

### Task 1: Registry key helpers and dot rejection in app names

Dots become structural in registry keys (`<label>.<base>`), so `adjacent.toml` names must no longer contain them, and the rest of the codebase needs helpers to split keys.

**Files:**
- Modify: `crates/adj/src/registry.rs`

- [ ] **Step 1: Write the failing tests**

Add to the existing `mod tests` at the bottom of `crates/adj/src/registry.rs`:

```rust
    #[test]
    fn split_key_handles_bare_and_instance_keys() {
        assert_eq!(split_key("site"), (None, "site"));
        assert_eq!(split_key("feature-x.site"), (Some("feature-x"), "site"));
        assert_eq!(base_name("site"), "site");
        assert_eq!(base_name("feature-x.site"), "site");
    }

    #[test]
    fn read_app_config_rejects_dotted_names() {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        std::fs::write(
            tmp.path().join("adjacent.toml"),
            "name = \"a.b\"\ncmd = \"true\"\n",
        )
        .expect("write toml");
        let err = read_app_config(tmp.path()).unwrap_err();
        assert!(format!("{err:#}").contains('.'), "error should mention the dot: {err:#}");
    }

    #[test]
    fn registry_remove_deletes_entry() {
        let mut reg = Registry::default();
        reg.insert("site".into(), AppEntry { path: "/tmp/site".into() });
        assert!(reg.remove("site").is_some());
        assert!(reg.get("site").is_none());
        assert!(reg.remove("site").is_none());
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p adj split_key_handles -- --nocapture && cargo test -p adj read_app_config_rejects && cargo test -p adj registry_remove_deletes`
Expected: compile error — `split_key`, `base_name`, `Registry::remove` don't exist.

- [ ] **Step 3: Implement helpers and validation**

In `crates/adj/src/registry.rs`, add below the `Registry` impl (around line 80):

```rust
/// Split a registry key into `(label, base)`. Keys are either a bare app name (`site`) or a
/// worktree-instance key (`feature-x.site`). `add` enforces at most one dot, so `split_once`
/// is total here.
pub fn split_key(key: &str) -> (Option<&str>, &str) {
    match key.split_once('.') {
        Some((label, base)) => (Some(label), base),
        None => (None, key),
    }
}

/// The app name a registry key resolves config against: the part after the instance label,
/// or the whole key when there is no label.
pub fn base_name(key: &str) -> &str {
    split_key(key).1
}
```

Inside `impl Registry`, add:

```rust
    pub fn remove(&mut self, name: &str) -> Option<AppEntry> {
        self.apps.remove(name)
    }
```

In `read_app_config`, after the empty-name check (`registry.rs:91-93`), add:

```rust
    // Dots are structural in registry keys (`<label>.<name>` is a worktree instance), so a
    // dotted app name would make `feature-x.site` ambiguous. Reject at the source.
    if cfg.name.contains('.') {
        return Err(anyhow!(
            "app name `{}` contains `.` — dots are reserved for worktree instances (`<label>.<name>`)",
            cfg.name
        ));
    }
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p adj --bin adj registry`
Expected: all registry unit tests PASS, including the three new ones.

- [ ] **Step 5: Run the full suite to catch regressions**

Run: `cargo test`
Expected: PASS (no existing app fixture uses a dotted name).

- [ ] **Step 6: Commit**

```bash
git add crates/adj/src/registry.rs
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit crates/adj/src/registry.rs -m "$(cat <<'EOF'
Add registry key helpers for worktree instances, reject dotted app names

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: Two-label host parsing in the proxy

`feature-x.site.adj.ac` must resolve to registry key `feature-x.site`. Deeper hosts stay rejected.

**Files:**
- Modify: `crates/adj/src/proxy.rs:364-371` (`name_from_host`) and its unit tests (`proxy.rs:401-409`)

- [ ] **Step 1: Update the unit test to the new contract (failing)**

Replace the `extracts_name_from_adj_ac_host` test in `crates/adj/src/proxy.rs` with:

```rust
    #[test]
    fn extracts_name_from_adj_ac_host() {
        assert_eq!(name_from_host("echo.adj.ac"), Some("echo".into()));
        assert_eq!(name_from_host("ECHO.adj.ac"), Some("echo".into()));
        // Worktree instances are `<label>.<name>` — exactly one dot in the prefix.
        assert_eq!(name_from_host("feature-x.site.adj.ac"), Some("feature-x.site".into()));
        // Deeper nesting is not a registrable key.
        assert_eq!(name_from_host("a.b.c.adj.ac"), None);
        // Empty labels on either side of the dot are invalid.
        assert_eq!(name_from_host(".site.adj.ac"), None);
        assert_eq!(name_from_host("x..adj.ac"), None);
        assert_eq!(name_from_host("example.com"), None);
        assert_eq!(name_from_host(".adj.ac"), None);
    }
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p adj --bin adj extracts_name_from_adj_ac_host`
Expected: FAIL — `feature-x.site.adj.ac` currently returns `None` (the `prefix.contains('.')` guard).

- [ ] **Step 3: Implement**

Replace `name_from_host` in `crates/adj/src/proxy.rs`:

```rust
fn name_from_host(host: &str) -> Option<String> {
    let lower = host.to_ascii_lowercase();
    let prefix = lower.strip_suffix(HOST_SUFFIX)?;
    // Accept `<name>` or `<label>.<name>` — at most one dot, no empty label on either side.
    // Anything deeper has no registrable key, so reject rather than guess.
    if prefix.is_empty() || prefix.matches('.').count() > 1 {
        return None;
    }
    if prefix.split('.').any(|part| part.is_empty()) {
        return None;
    }
    Some(prefix.to_string())
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p adj --bin adj extracts_name_from_adj_ac_host && cargo test --test proxy`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/adj/src/proxy.rs
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit crates/adj/src/proxy.rs -m "$(cat <<'EOF'
Accept two-label hosts in proxy routing for worktree instances

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: Register instances with `adj add --label`

End-to-end: the protocol carries an optional label, the daemon composes and validates the `<label>.<base>` key, the CLI exposes `--label`, and a registered instance boots and routes. This task also fixes the two `cfg.name != name` checks that would otherwise refuse to boot an instance (its registry key is `demo.site` but its `adjacent.toml` says `site`).

**Files:**
- Modify: `crates/adj-protocol/src/lib.rs:6` (`Request::Add`)
- Modify: `crates/adj/src/daemon.rs` (`add`, `dispatch`, `up`)
- Modify: `crates/adj/src/proxy.rs:268-275` (`ensure_running` name check)
- Modify: `crates/adj/src/client.rs:49-64` (`add`)
- Modify: `crates/adj/src/main.rs:36,103` (CLI arg + dispatch)
- Test: `crates/adj/tests/worktree.rs` (new file)

- [ ] **Step 1: Create the integration test harness + first failing test**

Create `crates/adj/tests/worktree.rs`. The harness is the same Sandbox pattern as `tests/proxy.rs` (each integration test file carries its own copy), plus a marker-file echo server so different directories of the same app are distinguishable, plus a `git` helper used by later tasks:

```rust
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
    let body = format!("name = \"{name}\"\ncmd = \"exec /usr/bin/python3 server.py\"\n");
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
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test --test worktree`
Expected: FAIL — `error: unexpected argument '--label' found` (the CLI doesn't know the flag yet; the assert on `add.status.success()` trips).

- [ ] **Step 3: Add `label` to the protocol**

In `crates/adj-protocol/src/lib.rs`, change the `Add` variant:

```rust
    Add {
        path: String,
        /// Register as a named instance `<label>.<name>` (routes at `<label>.<name>.adj.ac`).
        /// `None` registers under the bare app name as before.
        #[serde(default)]
        label: Option<String>,
    },
```

- [ ] **Step 4: Compose and validate the key in the daemon**

In `crates/adj/src/daemon.rs`, update the dispatch arm:

```rust
        Request::Add { path, label } => add(path, label, registry_lock).await,
```

Replace the `add` function with:

```rust
async fn add(
    path: String,
    label: Option<String>,
    registry_lock: Arc<Mutex<()>>,
) -> Result<Response> {
    // The client canonicalizes against the user's CWD before sending. We require absolute
    // paths here so we never silently resolve against the daemon's CWD.
    let candidate = PathBuf::from(&path);
    if !candidate.is_absolute() {
        return Err(anyhow!(
            "expected absolute path, got `{}` (client should canonicalize before send)",
            path
        ));
    }
    let canon = std::fs::canonicalize(&candidate)
        .with_context(|| format!("resolving path {}", path))?;
    let cfg = registry::read_app_config(&canon)?;
    if RESERVED_NAMES.contains(&cfg.name.as_str()) {
        return Err(anyhow!(
            "`{}` is a reserved name (claimed by the daemon for built-in routes like the status dashboard and the doctor probe) — rename the app in adjacent.toml",
            cfg.name
        ));
    }
    // The client derives labels (from `--label` or the git branch), but the daemon owns
    // validation: the label becomes a DNS label in `<label>.<name>.adj.ac` and a path
    // component of the log file, so the charset is restricted at the trust boundary.
    let key = match &label {
        Some(label) => {
            validate_label(label)?;
            if RESERVED_NAMES.contains(&label.as_str()) {
                return Err(anyhow!("`{label}` is a reserved name — pick another label"));
            }
            format!("{label}.{}", cfg.name)
        }
        None => cfg.name.clone(),
    };
    // Serialize add operations so two concurrent calls can't both pass uniqueness and race on save.
    let _guard = registry_lock.lock().await;
    let mut reg = Registry::load()?;
    if reg.get(&key).is_some() {
        return Err(anyhow!(
            "an app named `{key}` is already registered (use `--label` to register another instance)"
        ));
    }
    reg.insert(
        key.clone(),
        registry::AppEntry {
            path: canon.clone(),
        },
    );
    reg.save()?;
    Ok(Response::Added {
        name: key,
        path: canon.display().to_string(),
    })
}

fn validate_label(label: &str) -> Result<()> {
    let valid = !label.is_empty()
        && label
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-');
    if !valid {
        return Err(anyhow!(
            "label `{label}` must be a DNS label: lowercase letters, digits, and `-` only"
        ));
    }
    Ok(())
}
```

- [ ] **Step 5: Fix the base-name checks so instances can boot**

**Re-key the supervisor by the registry key.** `Supervisor::up` currently derives its map key from `cfg.name` (`crates/adj/src/supervisor.rs:69-70`: `let name = cfg.name.clone();`). For an instance, the registry key is `demo.site` but `cfg.name` is `site` — so the boot would register under `site`, `wait_ready("demo.site", …)` would poll a key that never leaves `Stopped` (guaranteed 504), two instances of one base would collide on the supervisor key, and both would interleave into `site.log`. The key can't be derived from `cfg`, so pass it explicitly.

In `crates/adj/src/supervisor.rs`, change the signature and drop the derivation:

```rust
    pub async fn up(&self, name: &str, app_dir: PathBuf, cfg: AppConfig) -> Result<u32> {
        let name = name.to_string();
```

Update all three call sites to pass the registry key:

- `crates/adj/src/daemon.rs` `up()` (~line 332): `supervisor.up(&name, entry.path, cfg).await?;`
- `crates/adj/src/daemon.rs` `restart()` (~line 352): `supervisor.up(&name, entry.path, cfg).await?;`
- `crates/adj/src/proxy.rs` `ensure_running()` (~line 290): `.up(name, entry.path.clone(), cfg.clone())`

**Then fix the base-name checks.** In `crates/adj/src/daemon.rs` `up()` (currently `if cfg.name != name` around line 324), replace the check with:

```rust
    // An instance key is `<label>.<cfg.name>`; only the base must match the manifest. A full
    // equality check here would refuse to boot every registered worktree instance.
    if registry::base_name(&name) != cfg.name {
        return Err(anyhow!(
            "adjacent.toml at {} declares name `{}`, which does not match `{}`",
            entry.path.display(),
            cfg.name,
            name
        ));
    }
```

In `crates/adj/src/proxy.rs` `ensure_running()` (currently `if cfg.name != name` around line 268), replace with:

```rust
    if registry::base_name(name) != cfg.name {
        return Err(ProxyError::Other(anyhow!(
            "adjacent.toml at {} declares name `{}`, which does not match `{}`",
            entry.path.display(),
            cfg.name,
            name
        )));
    }
```

- [ ] **Step 6: Thread the label through CLI and client**

In `crates/adj/src/main.rs`, change the `Add` variant and its dispatch:

```rust
    /// Register an app from a directory containing adjacent.toml.
    Add {
        path: String,
        /// Register as a named instance: `<label>.<name>.adj.ac`. Defaults to the sanitized
        /// git branch name when the directory is a linked git worktree.
        #[arg(long)]
        label: Option<String>,
    },
```

```rust
        Cmd::Add { path, label } => client::add(path, label).await,
```

In `crates/adj/src/client.rs`, change `add`:

```rust
pub async fn add(path: String, label: Option<String>) -> Result<()> {
    // Canonicalize on the client side: relative paths must resolve against the user's CWD,
    // not the daemon's. The daemon may have been launched from anywhere (or by launchd).
    let canon = std::fs::canonicalize(&path)
        .with_context(|| format!("resolving path {}", path))?;
    let resp = into_error(
        request(Request::Add {
            path: canon.display().to_string(),
            label,
        })
        .await?,
    )?;
    if let Response::Added { name, path } = resp {
        println!("registered `{name}` at {path}");
    }
    Ok(())
}
```

- [ ] **Step 7: Run the test to verify it passes**

Run: `cargo test --test worktree`
Expected: `label_flag_registers_routable_instance` PASS.

- [ ] **Step 8: Run the full suite**

Run: `cargo test`
Expected: PASS — existing `add` callers are unaffected (`label` defaults to `None` over the wire).

- [ ] **Step 9: Commit**

```bash
git add crates/adj-protocol/src/lib.rs crates/adj/src/daemon.rs crates/adj/src/proxy.rs crates/adj/src/client.rs crates/adj/src/main.rs crates/adj/tests/worktree.rs
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit crates/adj-protocol/src/lib.rs crates/adj/src/daemon.rs crates/adj/src/proxy.rs crates/adj/src/client.rs crates/adj/src/main.rs crates/adj/tests/worktree.rs -m "$(cat <<'EOF'
Register worktree instances with adj add --label

Instance keys are <label>.<name> in the flat registry; the proxy and
up paths match manifests by base name so instances can boot.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Auto-derive the label from the git branch

`adj add` inside a linked worktree (its `.git` is a file, not a directory) derives the label from the branch name without any flag. Detection is client-side — the client has the CWD and git context, consistent with the canonicalize-client-side rule.

**Files:**
- Create: `crates/adj/src/worktree.rs`
- Modify: `crates/adj/src/main.rs:5-18` (module list)
- Modify: `crates/adj/src/client.rs` (`add`)
- Test: unit tests in `worktree.rs`, integration tests in `crates/adj/tests/worktree.rs`

- [ ] **Step 1: Write the failing unit tests**

Create `crates/adj/src/worktree.rs` containing only the tests first:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_branch_names_to_dns_labels() {
        assert_eq!(sanitize_label("feature-x"), "feature-x");
        assert_eq!(sanitize_label("agents/Fix_Thing"), "agents-fix-thing");
        assert_eq!(sanitize_label("UPPER"), "upper");
        assert_eq!(sanitize_label("emoji-🦀-branch"), "emoji--branch");
        assert_eq!(sanitize_label("///"), "---");
        assert_eq!(sanitize_label("日本語"), "");
    }
}
```

Register the module in `crates/adj/src/main.rs` (alphabetical, after `mod tls;`):

```rust
mod worktree;
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p adj --bin adj sanitizes_branch_names`
Expected: compile error — `sanitize_label` doesn't exist.

- [ ] **Step 3: Implement detection + sanitization**

Fill in `crates/adj/src/worktree.rs` above the tests:

```rust
use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// Derive an instance label for `dir` when it is a linked git worktree. Returns `Ok(None)` for
/// a main checkout, a plain clone, or a non-git directory — those register under the bare app
/// name. Linked worktrees are recognizable without invoking git: their `.git` is a file (a
/// pointer into the main repo's metadata), not a directory.
pub fn detect_label(dir: &Path) -> Result<Option<String>> {
    if !dir.join(".git").is_file() {
        return Ok(None);
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("running `git rev-parse --abbrev-ref HEAD`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "directory looks like a git worktree but `git rev-parse` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // `--abbrev-ref HEAD` prints the literal string `HEAD` for a detached worktree — there is
    // no branch to name the instance after.
    if branch == "HEAD" {
        return Err(anyhow!(
            "worktree is on a detached HEAD — pass `--label <label>` to name the instance"
        ));
    }
    let label = sanitize_label(&branch);
    if label.is_empty() {
        return Err(anyhow!(
            "branch `{branch}` does not reduce to a usable DNS label — pass `--label <label>`"
        ));
    }
    Ok(Some(label))
}

/// Map a branch name onto the DNS-label charset the daemon accepts: lowercase, `/` and `_`
/// become `-`, anything else outside `[a-z0-9-]` is dropped.
pub fn sanitize_label(branch: &str) -> String {
    branch
        .to_ascii_lowercase()
        .chars()
        .map(|c| match c {
            '/' | '_' => '-',
            c => c,
        })
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect()
}
```

In `crates/adj/src/client.rs`, add the import and use detection when no flag was given:

```rust
use crate::worktree;
```

In `add`, after the `canonicalize` line and before building the request:

```rust
    // `--label` wins; otherwise a linked git worktree names its instance after the branch.
    let label = match label {
        Some(l) => Some(l),
        None => worktree::detect_label(&canon)?,
    };
```

- [ ] **Step 4: Run unit tests**

Run: `cargo test -p adj --bin adj sanitizes_branch_names`
Expected: PASS.

- [ ] **Step 5: Add the integration tests (failing only if implementation is wrong)**

Append to `crates/adj/tests/worktree.rs`:

```rust
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
```

- [ ] **Step 6: Run the integration tests**

Run: `cargo test --test worktree`
Expected: all three tests PASS.

- [ ] **Step 7: Run the full suite**

Run: `cargo test`
Expected: PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/adj/src/worktree.rs crates/adj/src/main.rs crates/adj/src/client.rs crates/adj/tests/worktree.rs
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit crates/adj/src/worktree.rs crates/adj/src/main.rs crates/adj/src/client.rs crates/adj/tests/worktree.rs -m "$(cat <<'EOF'
Derive instance labels from the git branch in linked worktrees

adj add detects a linked worktree (.git is a file), sanitizes the
branch name to a DNS label, and registers <label>.<name>. Detached
HEAD or an unsanitizable branch directs the user to --label.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 5: Stale detection, `adj remove`, and `adj prune`

Deleted worktrees leave registry entries pointing at dead paths. `list` flags them `stale`, requests to them 502 with a message naming the fix, `remove <name>` deletes one entry, `prune` deletes every stale entry. No auto-pruning — the registry never mutates behind the user's back.

**Files:**
- Modify: `crates/adj-protocol/src/lib.rs` (`Request`, `Response`, `AppSummary`, `ListEntryDto`)
- Modify: `crates/adj/src/daemon.rs` (dispatch, `list`, new `remove`/`prune`)
- Modify: `crates/adj/src/proxy.rs` (`ensure_running` stale check)
- Modify: `crates/adj/src/client.rs` (`list` rendering, new `remove`/`prune`)
- Modify: `crates/adj/src/main.rs` (CLI variants)
- Test: `crates/adj/tests/worktree.rs`

- [ ] **Step 1: Write the failing integration test**

Append to `crates/adj/tests/worktree.rs`:

```rust
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
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test --test worktree prune_clears && cargo test --test worktree remove_stops`
Expected: FAIL — `error: unrecognized subcommand 'prune'` / `'remove'`.

- [ ] **Step 3: Extend the protocol**

In `crates/adj-protocol/src/lib.rs`:

Add to `Request`:

```rust
    /// Delete one registry entry, stopping the app first if it is running.
    Remove { name: String },
    /// Delete every registry entry whose registered path no longer exists on disk.
    Prune,
```

Add to `Response`:

```rust
    Removed { name: String },
    Pruned { removed: Vec<String> },
```

Add the `stale` field to `AppSummary`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSummary {
    pub name: String,
    pub path: String,
    pub state: AppState,
    /// True when the registered path no longer exists on disk (e.g. a deleted worktree).
    /// Skipped on the wire when false so pre-stale daemons and clients interoperate.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
}
```

Add `stale` to `ListEntryDto` and emit it only when true (JSON.md contract: optional fields present only when meaningful):

```rust
pub struct ListEntryDto<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub state: &'a AppState,
    pub stale: bool,
}
```

In its `Serialize` impl, after the `port` entry:

```rust
        if self.stale {
            map.serialize_entry("stale", &true)?;
        }
```

- [ ] **Step 4: Implement daemon-side**

In `crates/adj/src/daemon.rs`:

Dispatch arms:

```rust
        Request::Remove { name } => remove(name, supervisor, registry_lock).await,
        Request::Prune => prune(supervisor, registry_lock).await,
```

`list()` gains the stale bit — replace the `entries.push` call:

```rust
        entries.push(AppSummary {
            name: name.clone(),
            path: entry.path.display().to_string(),
            state,
            stale: !entry.path.exists(),
        });
```

New functions:

```rust
async fn remove(
    name: String,
    supervisor: Arc<Supervisor>,
    registry_lock: Arc<Mutex<()>>,
) -> Result<Response> {
    let _guard = registry_lock.lock().await;
    let mut reg = Registry::load()?;
    if reg.get(&name).is_none() {
        return Err(anyhow!("no app named `{}`", name));
    }
    // Stop before deregistering so removal can't leave an orphan process running against an
    // entry that no longer exists.
    if matches!(
        supervisor.state(&name).await,
        adj_protocol::AppState::Running { .. }
    ) {
        supervisor.down(&name).await?;
    }
    reg.remove(&name);
    reg.save()?;
    Ok(Response::Removed { name })
}

async fn prune(supervisor: Arc<Supervisor>, registry_lock: Arc<Mutex<()>>) -> Result<Response> {
    let _guard = registry_lock.lock().await;
    let mut reg = Registry::load()?;
    let stale: Vec<String> = reg
        .apps
        .iter()
        .filter(|(_, entry)| !entry.path.exists())
        .map(|(name, _)| name.clone())
        .collect();
    for name in &stale {
        // A process can outlive its deleted cwd on unix, so a stale entry may still be
        // running. Best-effort stop — a failure shouldn't block deregistering the corpse.
        if matches!(
            supervisor.state(name).await,
            adj_protocol::AppState::Running { .. }
        ) {
            if let Err(err) = supervisor.down(name).await {
                tracing::warn!("stopping stale `{name}` during prune failed: {err}");
            }
        }
        reg.remove(name);
    }
    if !stale.is_empty() {
        reg.save()?;
    }
    Ok(Response::Pruned { removed: stale })
}
```

- [ ] **Step 5: 502 for stale paths in the proxy**

In `crates/adj/src/proxy.rs` `ensure_running()`, right after the registry lookup resolves `entry` (before `read_app_config`):

```rust
    // A registered path can vanish out from under us (deleted worktree, deleted folder). Name
    // the cause and the fix instead of letting read_app_config produce a confusing
    // "no adjacent.toml found" boot failure.
    if !entry.path.exists() {
        return Err(ProxyError::Other(anyhow!(
            "registered path {} no longer exists — run `adj prune`",
            entry.path.display()
        )));
    }
```

- [ ] **Step 6: Client + CLI**

In `crates/adj/src/main.rs`, add variants after `Restart`:

```rust
    /// Remove an app from the registry (stopping it first if running).
    Remove { name: String },
    /// Remove every registry entry whose directory no longer exists on disk.
    Prune,
```

and dispatch arms:

```rust
        Cmd::Remove { name } => client::remove(name).await,
        Cmd::Prune => client::prune().await,
```

In `crates/adj/src/client.rs`, add:

```rust
pub async fn remove(name: String) -> Result<()> {
    let resp = into_error(request(Request::Remove { name }).await?)?;
    if let Response::Removed { name } = resp {
        println!("removed `{name}`");
    }
    Ok(())
}

pub async fn prune() -> Result<()> {
    let resp = into_error(request(Request::Prune).await?)?;
    if let Response::Pruned { removed } = resp {
        if removed.is_empty() {
            println!("nothing to prune");
        } else {
            for name in removed {
                println!("pruned `{name}`");
            }
        }
    }
    Ok(())
}
```

Update `list` rendering — the human view marks stale entries, and the DTO construction carries the new field. Replace the entry loop and DTO mapping in `list`:

```rust
            let dtos: Vec<ListEntryDto> = entries
                .iter()
                .map(|e| ListEntryDto {
                    name: &e.name,
                    path: &e.path,
                    state: &e.state,
                    stale: e.stale,
                })
                .collect();
```

```rust
        for entry in entries {
            if entry.stale {
                println!(
                    "{:<20} {:<10} {} (path missing — run `adj prune`)",
                    entry.name, "stale", entry.path
                );
            } else {
                println!("{:<20} {:<10} {}", entry.name, entry.state, entry.path);
            }
        }
```

- [ ] **Step 7: Run the tests**

Run: `cargo test --test worktree`
Expected: all five tests PASS.

- [ ] **Step 8: Run the full suite**

Run: `cargo test`
Expected: PASS. `tests/json_output.rs` asserts the list schema — `stale` is absent for healthy apps, so existing assertions hold; if any test constructs `AppSummary` or `ListEntryDto` directly, add the `stale` field there.

- [ ] **Step 9: Commit**

```bash
git add crates/adj-protocol/src/lib.rs crates/adj/src/daemon.rs crates/adj/src/proxy.rs crates/adj/src/client.rs crates/adj/src/main.rs crates/adj/tests/worktree.rs
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit crates/adj-protocol/src/lib.rs crates/adj/src/daemon.rs crates/adj/src/proxy.rs crates/adj/src/client.rs crates/adj/src/main.rs crates/adj/tests/worktree.rs -m "$(cat <<'EOF'
Add adj remove and adj prune, flag registry entries with dead paths as stale

Requests routed to a stale entry 502 with a message naming the fix.
No auto-pruning: the registry only changes when the user asks.

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 6: Per-app wildcard SANs with a hot-swapping cert resolver

`*.adj.ac` matches one label, so `feature-x.site.adj.ac` needs a `*.site.adj.ac` SAN. The leaf re-issues whenever the registry's SAN set changes and hot-swaps via a `ResolvesServerCert` impl — no HTTPS-listener restart, no CA changes (the `adj.ac` nameConstraint already permits any depth).

**Files:**
- Modify: `crates/adj/src/tls.rs` (`registry_sans`, `ensure_leaf(sans)`, `leaf_covers`, `issue_leaf(sans)`, `LeafResolver`, `server_config`, existing unit tests)
- Modify: `crates/adj/src/proxy.rs:102-114` (`run_https` takes the resolver)
- Modify: `crates/adj/src/daemon.rs` (resolver construction + reload after add/remove/prune)

- [ ] **Step 1: Write the failing unit tests**

Add to `mod tests` in `crates/adj/src/tls.rs` (note: `registry_sans_adds_wildcard_per_base` is pure — not macOS-gated; the other is keychain-backed and gated like its neighbors):

```rust
    #[test]
    fn registry_sans_adds_wildcard_per_base() {
        use crate::registry::AppEntry;
        let mut reg = Registry::default();
        reg.insert("site".into(), AppEntry { path: "/tmp/a".into() });
        reg.insert("feature-x.site".into(), AppEntry { path: "/tmp/b".into() });
        reg.insert("api".into(), AppEntry { path: "/tmp/c".into() });
        let expected: Vec<String> = ["adj.ac", "*.adj.ac", "*.api.adj.ac", "*.site.adj.ac"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(registry_sans(&reg), expected);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn leaf_reissues_when_san_set_changes() {
        with_temp_home(|| {
            generate_ca().expect("generate_ca");
            let base: Vec<String> = vec!["adj.ac".into(), "*.adj.ac".into()];
            let (pem1, _) = ensure_leaf(&base).expect("first issue");
            // Same set → the cached leaf comes back byte-identical (no needless keychain work).
            let (pem1b, _) = ensure_leaf(&base).expect("cached");
            assert_eq!(pem1, pem1b);
            let widened: Vec<String> =
                vec!["adj.ac".into(), "*.adj.ac".into(), "*.site.adj.ac".into()];
            let (pem2, _) = ensure_leaf(&widened).expect("re-issue");
            assert_ne!(pem1, pem2, "SAN change must re-issue the leaf");
            assert!(leaf_covers(&pem2, &widened).expect("parse"));
            assert!(!leaf_covers(&pem2, &base).expect("parse"));
        });
    }
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p adj --bin adj registry_sans_adds && cargo test -p adj --bin adj leaf_reissues`
Expected: compile error — `registry_sans`, `leaf_covers` don't exist and `ensure_leaf` takes no arguments.

- [ ] **Step 3: Implement SAN computation and conditional re-issue**

In `crates/adj/src/tls.rs`, add imports:

```rust
use rustls::server::{ClientHello, ResolvesServerCert};
use rustls::sign::CertifiedKey;
use std::sync::RwLock;

use crate::registry::{self, Registry};
```

Add below the `APEX_HOST` consts:

```rust
/// The leaf SAN set for a registry snapshot: the v1 apex + single-label wildcard, plus a
/// `*.<base>.adj.ac` wildcard per distinct base name so worktree instances
/// (`<label>.<base>.adj.ac`) validate. A wildcard matches exactly one label, so the per-base
/// entries can't be folded into `*.adj.ac`. Deterministic order (apex, wildcard, sorted bases)
/// makes set comparison against an issued cert a plain Vec equality.
pub fn registry_sans(reg: &Registry) -> Vec<String> {
    let mut sans = vec![APEX_HOST.to_string(), WILDCARD_HOST.to_string()];
    let mut bases: Vec<&str> = reg.apps.keys().map(|k| registry::base_name(k)).collect();
    bases.sort_unstable();
    bases.dedup();
    for base in bases {
        sans.push(format!("*.{base}.adj.ac"));
    }
    sans
}
```

Change `ensure_leaf` to take the desired SANs and re-issue on mismatch:

```rust
/// Read the leaf cert + key from disk, re-issuing when missing OR when the on-disk SAN set no
/// longer matches the desired one (an app was added/removed since issuance, or the leaf
/// predates worktree instances). `generate_ca` still deletes the leaf on CA rotation, so this
/// single mechanism covers fresh-install, post-rotation, and SAN-drift paths.
fn ensure_leaf(sans: &[String]) -> Result<(String, String)> {
    let cert_path = leaf_cert_path()?;
    let key_path = leaf_key_path()?;
    if cert_path.exists() && key_path.exists() {
        let cert = fs::read_to_string(&cert_path)
            .with_context(|| format!("reading {}", cert_path.display()))?;
        let key = fs::read_to_string(&key_path)
            .with_context(|| format!("reading {}", key_path.display()))?;
        if leaf_covers(&cert, sans)? {
            return Ok((cert, key));
        }
    }
    issue_leaf(sans)
}

/// True when the leaf's DNS SANs equal the desired set exactly (order-insensitive).
fn leaf_covers(cert_pem: &str, sans: &[String]) -> Result<bool> {
    let (_, parsed) = x509_parser::pem::parse_x509_pem(cert_pem.as_bytes())
        .map_err(|e| anyhow!("parsing leaf PEM: {e}"))?;
    let cert = parsed
        .parse_x509()
        .map_err(|e| anyhow!("parsing leaf X.509: {e}"))?;
    let mut have: Vec<String> = cert
        .subject_alternative_name()
        .map_err(|e| anyhow!("reading leaf SANs: {e}"))?
        .map(|ext| {
            ext.value
                .general_names
                .iter()
                .filter_map(|gn| match gn {
                    x509_parser::extensions::GeneralName::DNSName(n) => Some(n.to_string()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    have.sort_unstable();
    let mut want: Vec<String> = sans.to_vec();
    want.sort_unstable();
    Ok(have == want)
}
```

Change `issue_leaf` to take SANs — replace the hardcoded params block (`tls.rs:160-169`):

```rust
fn issue_leaf(sans: &[String]) -> Result<(String, String)> {
```

```rust
    let mut leaf_params =
        CertificateParams::new(sans.to_vec()).context("building leaf cert params")?;
    leaf_params.distinguished_name = leaf_dn();
    leaf_params.subject_alt_names = sans
        .iter()
        .map(|s| {
            Ok(SanType::DnsName(
                s.as_str().try_into().with_context(|| format!("SAN `{s}`"))?,
            ))
        })
        .collect::<Result<Vec<_>>>()?;
```

(The rest of `issue_leaf` — CA loading, signing, writing — is unchanged.)

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p adj --bin adj registry_sans_adds && cargo test -p adj --bin adj leaf_reissues`
Expected: PASS. (The crate won't fully compile yet if `server_config()` callers are broken — fix in the next step before running if so; `ensure_leaf()` call inside `server_config` needs updating: pass `&registry_sans(&Registry::load()?)` temporarily or proceed straight to Step 5.)

- [ ] **Step 5: Add the resolver and rebuild `server_config` around it**

In `crates/adj/src/tls.rs`, replace `server_config` with:

```rust
/// Serves the daemon's leaf cert and re-issues it when the registry's SAN set changes, so a
/// newly added worktree instance gets a valid cert without an HTTPS-listener restart.
//
// Note for the implementer: `ResolvesServerCert` requires `Debug`. If `#[derive(Debug)]`
// fails because rustls's `CertifiedKey` doesn't implement it in our pinned version, write a
// manual impl that prints the SAN list instead:
//   impl std::fmt::Debug for LeafResolver { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("LeafResolver").field("sans", &*self.sans.read().expect("sans lock")).finish_non_exhaustive() } }
#[derive(Debug)]
pub struct LeafResolver {
    current: RwLock<Arc<CertifiedKey>>,
    /// SANs baked into `current`; compared on reload so an unchanged registry skips the
    /// keychain signature entirely.
    sans: RwLock<Vec<String>>,
}

impl LeafResolver {
    /// Build the resolver from the on-disk CA, issuing a leaf that covers the current
    /// registry. Errors when the CA is missing — callers treat that as "HTTPS not opted in".
    pub fn new() -> Result<Arc<Self>> {
        if !ca_exists()? {
            return Err(anyhow!(
                "local CA not found — run `adj install-ca` to generate one"
            ));
        }
        let sans = registry_sans(&Registry::load()?);
        let key = certified_key_for(&sans)?;
        Ok(Arc::new(Self {
            current: RwLock::new(Arc::new(key)),
            sans: RwLock::new(sans),
        }))
    }

    /// Recompute the SAN set from the registry; re-issue and swap the served cert if changed.
    pub fn reload(&self) -> Result<()> {
        let sans = registry_sans(&Registry::load()?);
        if *self.sans.read().expect("sans lock") == sans {
            return Ok(());
        }
        let key = certified_key_for(&sans)?;
        *self.current.write().expect("cert lock") = Arc::new(key);
        *self.sans.write().expect("sans lock") = sans;
        Ok(())
    }
}

impl ResolvesServerCert for LeafResolver {
    fn resolve(&self, _hello: ClientHello<'_>) -> Option<Arc<CertifiedKey>> {
        Some(self.current.read().expect("cert lock").clone())
    }
}

fn certified_key_for(sans: &[String]) -> Result<CertifiedKey> {
    let (cert_pem, key_pem) = ensure_leaf(sans)?;
    let chain = parse_cert_chain(&cert_pem).context("parsing leaf certificate chain")?;
    let key_der = parse_private_key(&key_pem).context("parsing leaf private key")?;
    // rustls 0.23 with the default `aws_lc_rs` feature auto-installs the process default
    // crypto provider, matching the previous with_single_cert behavior.
    let signing_key = rustls::crypto::aws_lc_rs::sign::any_supported_type(&key_der)
        .context("building leaf signing key")?;
    Ok(CertifiedKey::new(chain, signing_key))
}

/// Build a `rustls::ServerConfig` around the hot-swapping resolver. Caller is expected to
/// surface errors (HTTPS listener startup is best-effort: no CA → log and skip).
pub fn server_config(resolver: Arc<LeafResolver>) -> Result<Arc<ServerConfig>> {
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_cert_resolver(resolver);
    Ok(Arc::new(config))
}
```

Update the existing unit tests that called the old zero-arg `server_config()`:

- `server_config_errors_without_ca` → assert on the resolver instead:

```rust
    #[cfg(target_os = "macos")]
    #[test]
    fn server_config_errors_without_ca() {
        with_temp_home(|| {
            let err = LeafResolver::new().unwrap_err();
            assert!(format!("{err}").contains("install-ca"));
        });
    }
```

- `server_config_succeeds_after_generate`:

```rust
    #[cfg(target_os = "macos")]
    #[test]
    fn server_config_succeeds_after_generate() {
        with_temp_home(|| {
            generate_ca().expect("generate_ca");
            let resolver = LeafResolver::new().expect("resolver");
            let _cfg = server_config(resolver).expect("server_config");
            assert!(leaf_cert_path().unwrap().exists());
            assert!(leaf_key_path().unwrap().exists());
        });
    }
```

- `leaf_issuer_matches_ca_subject_after_reissue`: replace its `let _cfg = server_config().expect("server_config");` line with `let _resolver = LeafResolver::new().expect("resolver");` (the leaf still gets issued to disk).

- [ ] **Step 6: Wire the resolver through the daemon**

In `crates/adj/src/proxy.rs`, change `run_https` to accept the resolver:

```rust
pub async fn run_https(
    supervisor: Arc<Supervisor>,
    resolver: Arc<tls::LeafResolver>,
) -> Result<()> {
    let server_config =
        tls::server_config(resolver).map_err(|e| anyhow!("loading TLS config: {e}"))?;
```

(The rest of the function is unchanged.)

In `crates/adj/src/daemon.rs` `run()`, replace the HTTPS spawn block (`daemon.rs:67-75`) with:

```rust
    // HTTPS listener and SAN re-issue share one resolver. Construction fails when the CA is
    // missing — that's "HTTPS not opted in": skip the listener, and registry changes skip the
    // leaf re-issue. Same degraded-not-fatal posture as before.
    let resolver = match tls::LeafResolver::new() {
        Ok(r) => Some(r),
        Err(err) => {
            tracing::error!("https listener disabled: {err}");
            None
        }
    };
    if let Some(resolver) = resolver.clone() {
        let https_supervisor = supervisor.clone();
        tokio::spawn(async move {
            if let Err(err) = proxy::run_https(https_supervisor, resolver).await {
                tracing::error!("https listener exited: {err}");
            }
        });
    }
```

Add `use crate::tls;` to daemon.rs imports if not already present.

Thread the resolver into dispatch: change `handle_client` and `dispatch` signatures to take `resolver: Option<Arc<tls::LeafResolver>>`, pass it from the accept loop (`let resolver = resolver.clone();` alongside `sup`/`reg_lock`), and hand it to the three registry-mutating arms:

```rust
        Request::Add { path, label } => add(path, label, registry_lock, resolver).await,
        Request::Remove { name } => remove(name, supervisor, registry_lock, resolver).await,
        Request::Prune => prune(supervisor, registry_lock, resolver).await,
```

At the end of `add`, `remove`, and `prune` (each takes the extra `resolver: Option<Arc<tls::LeafResolver>>` parameter), after `reg.save()?` (in `prune`: after the conditional save, only when `removed` is non-empty):

```rust
    // Best-effort: a failed re-issue means HTTPS serves the previous SAN set until the next
    // registry change; the registry mutation itself already succeeded.
    if let Some(resolver) = &resolver {
        if let Err(err) = resolver.reload() {
            tracing::warn!("leaf cert re-issue after registry change failed: {err}");
        }
    }
```

- [ ] **Step 7: Run the full suite**

Run: `cargo test`
Expected: PASS, including `tests/tls.rs` (integration HTTPS test still works — the resolver serves the same leaf `with_single_cert` used to) and the two new unit tests.

- [ ] **Step 8: Commit**

```bash
git add crates/adj/src/tls.rs crates/adj/src/proxy.rs crates/adj/src/daemon.rs
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit crates/adj/src/tls.rs crates/adj/src/proxy.rs crates/adj/src/daemon.rs -m "$(cat <<'EOF'
Issue per-app wildcard SANs and hot-swap the TLS leaf on registry changes

The leaf now carries *.<name>.adj.ac per registered base so worktree
instances validate. A ResolvesServerCert impl swaps the re-issued
leaf in place; the CA and trust anchor are untouched (nameConstraints
already permits any depth under adj.ac).

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

### Task 7: Documentation

**Files:**
- Modify: `crates/adj/JSON.md`
- Modify: `CLAUDE.md` (registry section + commands)
- Modify: `README.md` (commands; follow its existing tone — the user cuts bloat)

- [ ] **Step 1: JSON.md**

In the `adj list --json` section: add `"stale": true` to one example entry (give it a deleted path), and a row to the field table:

```markdown
| `stale` | boolean | present iff the registered path no longer exists on disk |
```

Update the write-commands line near the top to include the new commands:

```markdown
Write commands (`add`, `up`, `down`, `restart`, `remove`, `prune`) do not accept `--json` in v1.
```

- [ ] **Step 2: CLAUDE.md**

In the **Registry & config** section, after the registry.toml sentence, add:

```markdown
Registry keys may carry one structural dot: `<label>.<name>` is a worktree instance
(`feature-x.site` routes at `feature-x.site.adj.ac`). `adj add` inside a linked git worktree
derives the label from the branch name (`--label` overrides); `adj remove <name>` deletes one
entry, `adj prune` deletes every entry whose path is gone, and `adj list` flags those as stale.
App names in `adjacent.toml` therefore cannot contain dots. The TLS leaf carries a
`*.<name>.adj.ac` SAN per registered base name and re-issues on registry changes.
```

- [ ] **Step 3: README.md**

The `## Usage` section (README.md:35-61) is a `--help` dump. Insert two lines after the `restart` row, matching the new clap declaration order:

```text
  remove                Remove an app from the registry (stopping it first if running)
  prune                 Remove every registry entry whose directory no longer exists on disk
```

Add a new section between `## Usage` and `## Agent Integration`:

```markdown
## Worktrees

Four agents in four git worktrees of the same repo can all register. `adj add` inside a
linked worktree names the instance after its branch: the worktree of `site` on branch
`feature-x` serves at `feature-x.site.adj.ac`, while the main checkout keeps `site.adj.ac`.
No flags needed (`--label` overrides the branch name); each worktree gets its own process,
port and logs. When a worktree is deleted, `adj list` flags the leftover entry as stale and
`adj prune` clears them all.
```

- [ ] **Step 4: Verify and commit**

Run: `cargo test` (docs only — confirms nothing else drifted)
Expected: PASS.

```bash
git add crates/adj/JSON.md CLAUDE.md README.md
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit crates/adj/JSON.md CLAUDE.md README.md -m "$(cat <<'EOF'
Document worktree instances, remove/prune, and the stale list field

Co-Authored-By: Claude Fable 5 <noreply@anthropic.com>
EOF
)"
```

---

## Final verification

- [ ] `cargo test` — full suite green.
- [ ] Manual smoke (optional but recommended): in a scratch repo with two worktrees, `cargo run -- daemon` plus `ADJACENT_HOME=$(mktemp -d) ADJACENT_PROXY_PORT=9090` env, `adj add` from both, `curl -H 'Host: <branch>.<name>.adj.ac' 127.0.0.1:9090`, delete one worktree, `adj list`, `adj prune`.
- [ ] Use superpowers:finishing-a-development-branch — the branch is `worktree-instances` (the spec commit is already on it); PR body uses `Resolves #N` if an issue exists, and the agent never approves or merges.
