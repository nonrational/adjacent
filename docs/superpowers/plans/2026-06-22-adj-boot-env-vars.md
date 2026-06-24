# ADJ_* Boot Environment Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Inject a daemon-owned `ADJ_*` environment namespace (name, host, and four URLs) into every supervised app at boot so a `cmd` can address its own external URLs (e.g. Hugo `--baseURL $ADJ_URL_HTTP`).

**Architecture:** The supervisor already owns env layering (`env_file → [env] → PORT`). Add one more daemon-owned layer in `supervisor::up()`. A new `ProxyPorts` value (built from the proxy's configured ports) resolves the daemon's externally-reachable HTTP/HTTPS ports at boot — using the configured value, or reading the kernel-assigned port back from the `proxy.port`/`https.port` file. A pure `adj_env()` helper formats the six variables. All three `up()` call sites stay unchanged because the supervisor builds the env itself.

**Tech Stack:** Rust (workspace crate `adj`), tokio, `tempfile` (dev-dependency, already present).

## Global Constraints

- **No Conventional Commit prefixes** in commit messages (no `feat:`/`fix:`). Plain descriptive messages. (Project convention — overrides the writing-plans skill's example.)
- End every commit message with the trailer: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`.
- **`git commit <path>`, never `git commit -a`.** Stage new files with a targeted `git add <path>` first.
- **Comments describe WHY, not WHAT.** Match the codebase's dense block-comment style above tricky logic.
- Reserved namespace: the six `ADJ_*` vars are daemon-owned and injected **after** `env_file`/`[env]`, so they win over user-set values. No validation forbidding user `ADJ_*` keys.
- `ADJ_NAME` is the **routing key** `up()` receives (carries the worktree label when present), never `cfg.name`.
- Host suffix is `proxy::HOST_SUFFIX` (`.adj.ac`) — one source of truth.
- Work happens on branch `adj-boot-env-vars` (already created; the spec is committed there).

**Exact value formats** (app `alannorton-com`, daemon HTTP `:8080`, HTTPS `:8443`):

| Var | Value |
|---|---|
| `ADJ_NAME` | `alannorton-com` |
| `ADJ_HOST` | `alannorton-com.adj.ac` |
| `ADJ_URL` | `https://alannorton-com.adj.ac` |
| `ADJ_URL_HTTP` | `http://alannorton-com.adj.ac` |
| `ADJ_URL_DIRECT` | `https://alannorton-com.adj.ac:8443` (omitted if HTTPS port unresolvable) |
| `ADJ_URL_HTTP_DIRECT` | `http://alannorton-com.adj.ac:8080` |

---

### Task 1: `ProxyPorts` resolver, threaded into `Supervisor`

Introduce the port-resolution type and give the supervisor access to it. No `ADJ_*` injection yet — that is Task 2.

**Files:**
- Modify: `crates/adj/src/supervisor.rs` (add `ProxyPorts` + `read_port_file`; change `Supervisor` struct and `new()`; update the two unit-test constructors at lines ~431, ~450)
- Modify: `crates/adj/src/daemon.rs:47` (construct `Supervisor` with `ProxyPorts::from_env()`)

**Interfaces:**
- Produces:
  - `pub struct ProxyPorts` with `pub fn from_env() -> Self`, `pub fn http(&self) -> Option<u16>`, `pub fn https(&self) -> Option<u16>`, and `Default` (8080/8443).
  - `pub fn new(proxy_ports: ProxyPorts) -> Supervisor` (constructor signature changes from no-arg).
- Consumes: `crate::proxy::proxy_port()`, `crate::proxy::https_port()` (existing `pub fn`s returning `u16`), `crate::paths::proxy_port_path()`, `crate::paths::https_port_path()` (existing, return `Result<PathBuf>`).

- [ ] **Step 1: Write the failing tests**

Add to the existing `#[cfg(test)] mod tests` in `crates/adj/src/supervisor.rs` (the module that already holds `down_if_idle_*`):

```rust
#[test]
fn proxy_ports_configured_value_wins_without_touching_files() {
    // Non-zero configured ports are authoritative — no port-file read needed. This is the
    // path the real daemon takes (defaults 8080/8443).
    let p = ProxyPorts::default();
    assert_eq!(p.http(), Some(8080));
    assert_eq!(p.https(), Some(8443));
}

#[test]
fn read_port_file_parses_present_file_and_none_otherwise() {
    use std::io::Write;
    // Present + parseable → the kernel-assigned port written back by the listener.
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("proxy.port");
    let mut f = std::fs::File::create(&path).unwrap();
    writeln!(f, "54321").unwrap();
    assert_eq!(read_port_file(Ok(path.clone())), Some(54321));

    // Absent file → None (HTTPS listener that never bound, e.g. no CA).
    assert_eq!(read_port_file(Ok(dir.path().join("missing.port"))), None);
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p adj proxy_ports_configured_value_wins read_port_file_parses`
Expected: FAIL to compile — `ProxyPorts` and `read_port_file` don't exist yet.

- [ ] **Step 3: Add `ProxyPorts` and `read_port_file`**

In `crates/adj/src/supervisor.rs`, after the `const PORT_ALLOC_ATTEMPTS` line (~24) and before `pub struct Supervisor`, add:

```rust
/// The proxy's externally-reachable listener ports, resolved at boot for the `ADJ_*` URLs.
///
/// Holds the *configured* values (`ADJACENT_PROXY_PORT` / `ADJACENT_HTTPS_PORT`, default
/// 8080/8443). A non-zero configured port is authoritative. Only the kernel-assigned case
/// (`0`, used by the test sandboxes) consults the port file the listener wrote after binding
/// — the same env-or-file discovery other processes use to find this daemon. Resolution is
/// deferred to `up()` time, not construction: at construction the listeners haven't bound, so
/// a `0` port has no value yet.
pub struct ProxyPorts {
    http_configured: u16,
    https_configured: u16,
}

impl Default for ProxyPorts {
    fn default() -> Self {
        Self {
            http_configured: 8080,
            https_configured: 8443,
        }
    }
}

impl ProxyPorts {
    pub fn from_env() -> Self {
        Self {
            http_configured: crate::proxy::proxy_port(),
            https_configured: crate::proxy::https_port(),
        }
    }

    pub fn http(&self) -> Option<u16> {
        if self.http_configured != 0 {
            Some(self.http_configured)
        } else {
            read_port_file(paths::proxy_port_path())
        }
    }

    pub fn https(&self) -> Option<u16> {
        if self.https_configured != 0 {
            Some(self.https_configured)
        } else {
            read_port_file(paths::https_port_path())
        }
    }
}

/// Read a kernel-assigned port back from a listener's port file. Best-effort: a missing or
/// unparseable file (the HTTPS listener that never bound, say) yields `None`, which drops the
/// corresponding `_DIRECT` URL rather than pointing it at a dead port.
fn read_port_file(path: Result<PathBuf>) -> Option<u16> {
    let path = path.ok()?;
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}
```

- [ ] **Step 4: Thread `ProxyPorts` into `Supervisor`**

Replace the struct definition and constructor. Change:

```rust
#[derive(Default)]
pub struct Supervisor {
    inner: Arc<Mutex<Inner>>,
}
```

to:

```rust
pub struct Supervisor {
    inner: Arc<Mutex<Inner>>,
    proxy_ports: ProxyPorts,
}
```

And change the constructor:

```rust
impl Supervisor {
    pub fn new() -> Self {
        Self::default()
    }
```

to:

```rust
impl Supervisor {
    pub fn new(proxy_ports: ProxyPorts) -> Self {
        Self {
            inner: Arc::default(),
            proxy_ports,
        }
    }
```

(Dropping `#[derive(Default)]` on `Supervisor` forces explicit port construction — there is no meaningful default daemon without real ports.)

- [ ] **Step 5: Update the three `Supervisor::new()` call sites**

In `crates/adj/src/daemon.rs:47`, change:

```rust
    let supervisor = Arc::new(Supervisor::new());
```

to:

```rust
    let supervisor = Arc::new(Supervisor::new(crate::supervisor::ProxyPorts::from_env()));
```

In `crates/adj/src/supervisor.rs`, the two unit-test constructors (in `mod tests`, currently `Supervisor::new()` at ~431 and ~450) — change each:

```rust
        let sup = Supervisor::new();
```

to:

```rust
        let sup = Supervisor::new(ProxyPorts::default());
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p adj proxy_ports_configured_value_wins read_port_file_parses`
Expected: PASS (2 tests).

- [ ] **Step 7: Build the whole workspace to confirm no other callers broke**

Run: `cargo build`
Expected: clean build. (If a `Supervisor::new()` call was missed, the compiler points at it — fix it the same way.)

- [ ] **Step 8: Commit**

```bash
git add crates/adj/src/supervisor.rs crates/adj/src/daemon.rs
git commit crates/adj/src/supervisor.rs crates/adj/src/daemon.rs -m "$(cat <<'EOF'
Add ProxyPorts resolver and thread it into Supervisor

ProxyPorts holds the proxy's configured HTTP/HTTPS ports and resolves the
externally-reachable port at boot (configured value, or the kernel-assigned
port read back from the port file). Threaded into Supervisor for the upcoming
ADJ_* URL injection; no behavior change yet.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 2: `adj_env()` helper and `ADJ_*` injection in `up()`

Build the six variables and inject them after the existing env layers.

**Files:**
- Modify: `crates/adj/src/proxy.rs:28` (make `HOST_SUFFIX` `pub`)
- Modify: `crates/adj/src/supervisor.rs` (add `adj_env()`; inject in `up()` after the `PORT` line; add unit tests)

**Interfaces:**
- Consumes: `crate::proxy::HOST_SUFFIX` (`&str`, becomes `pub`); `ProxyPorts::http()`/`https()` from Task 1; the `name: &str` arg of `up()`.
- Produces: `fn adj_env(name: &str, http: Option<u16>, https: Option<u16>) -> Vec<(String, String)>` (module-private; unit-tested in-module).

- [ ] **Step 1: Make `HOST_SUFFIX` public**

In `crates/adj/src/proxy.rs:28`, change:

```rust
const HOST_SUFFIX: &str = ".adj.ac";
```

to:

```rust
pub const HOST_SUFFIX: &str = ".adj.ac";
```

- [ ] **Step 2: Write the failing tests**

Add to the `#[cfg(test)] mod tests` in `crates/adj/src/supervisor.rs`:

```rust
#[test]
fn adj_env_builds_all_six_vars() {
    let vars: std::collections::HashMap<String, String> =
        adj_env("alannorton-com", Some(8080), Some(8443))
            .into_iter()
            .collect();
    assert_eq!(vars["ADJ_NAME"], "alannorton-com");
    assert_eq!(vars["ADJ_HOST"], "alannorton-com.adj.ac");
    assert_eq!(vars["ADJ_URL"], "https://alannorton-com.adj.ac");
    assert_eq!(vars["ADJ_URL_HTTP"], "http://alannorton-com.adj.ac");
    assert_eq!(vars["ADJ_URL_DIRECT"], "https://alannorton-com.adj.ac:8443");
    assert_eq!(vars["ADJ_URL_HTTP_DIRECT"], "http://alannorton-com.adj.ac:8080");
}

#[test]
fn adj_env_uses_routing_key_for_worktree_instances() {
    // A worktree instance is keyed `<label>.<name>` and routes at `<label>.<name>.adj.ac`.
    let vars: std::collections::HashMap<String, String> =
        adj_env("feature-x.site", Some(8080), Some(8443))
            .into_iter()
            .collect();
    assert_eq!(vars["ADJ_NAME"], "feature-x.site");
    assert_eq!(vars["ADJ_HOST"], "feature-x.site.adj.ac");
    assert_eq!(vars["ADJ_URL"], "https://feature-x.site.adj.ac");
}

#[test]
fn adj_env_omits_https_direct_when_port_unresolved() {
    let vars: std::collections::HashMap<String, String> =
        adj_env("site", Some(8080), None).into_iter().collect();
    assert!(!vars.contains_key("ADJ_URL_DIRECT"));
    // The portless canonical https URL and the http direct URL still appear.
    assert_eq!(vars["ADJ_URL"], "https://site.adj.ac");
    assert_eq!(vars["ADJ_URL_HTTP_DIRECT"], "http://site.adj.ac:8080");
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p adj adj_env_`
Expected: FAIL to compile — `adj_env` doesn't exist.

- [ ] **Step 4: Implement `adj_env()`**

In `crates/adj/src/supervisor.rs`, add near `ProxyPorts` (e.g. directly after the `read_port_file` function):

```rust
/// Build the daemon-owned `ADJ_*` boot variables for an app addressed at `name` (the routing
/// key — carries the worktree label when present). `http`/`https` are the proxy's
/// externally-reachable listener ports, already resolved. A `None` port drops the matching
/// `_DIRECT` URL rather than pointing it at a dead port; the portless canonical URLs always
/// appear. `http` is `None` only in pathological cases (the HTTP listener always binds), but
/// it is guarded symmetrically.
fn adj_env(name: &str, http: Option<u16>, https: Option<u16>) -> Vec<(String, String)> {
    let host = format!("{name}{}", crate::proxy::HOST_SUFFIX);
    let mut vars = vec![
        ("ADJ_NAME".to_string(), name.to_string()),
        ("ADJ_HOST".to_string(), host.clone()),
        ("ADJ_URL".to_string(), format!("https://{host}")),
        ("ADJ_URL_HTTP".to_string(), format!("http://{host}")),
    ];
    if let Some(p) = https {
        vars.push(("ADJ_URL_DIRECT".to_string(), format!("https://{host}:{p}")));
    }
    if let Some(p) = http {
        vars.push(("ADJ_URL_HTTP_DIRECT".to_string(), format!("http://{host}:{p}")));
    }
    vars
}
```

- [ ] **Step 5: Inject `ADJ_*` in `up()`**

In `crates/adj/src/supervisor.rs::up()`, find the PORT injection line (~122):

```rust
        command.env(port_env, port.to_string());
```

Immediately after it, add:

```rust
        // Daemon-owned ADJ_* namespace: the app's own external identity and URLs. Injected
        // after env_file/[env] so these authoritative values win over anything a user set.
        // `name` is the routing key, so the host and URLs address the exact vhost the proxy
        // serves (worktree label included). Resolving the proxy ports here may read a port
        // file, but only in the kernel-assigned (`0`) sandbox case; the default daemon uses
        // the configured constants and touches no file.
        for (k, v) in adj_env(&name, self.proxy_ports.http(), self.proxy_ports.https()) {
            command.env(k, v);
        }
```

- [ ] **Step 6: Run the tests to verify they pass**

Run: `cargo test -p adj adj_env_`
Expected: PASS (3 tests).

- [ ] **Step 7: Build to confirm the `pub const` change and injection compile**

Run: `cargo build`
Expected: clean build.

- [ ] **Step 8: Commit**

```bash
git add crates/adj/src/supervisor.rs crates/adj/src/proxy.rs
git commit crates/adj/src/supervisor.rs crates/adj/src/proxy.rs -m "$(cat <<'EOF'
Inject ADJ_* boot environment into supervised apps

up() now injects ADJ_NAME, ADJ_HOST, and the four ADJ_URL* variables after
the env_file/[env] layers so a cmd can address its own external URLs (e.g.
Hugo --baseURL). The _DIRECT URLs carry the daemon's real listener ports;
the https _DIRECT URL is dropped when that port can't be resolved.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 3: End-to-end integration test

Prove the vars reach a real child process with correct values, against a running daemon.

**Files:**
- Create: `crates/adj/tests/boot_env.rs`

**Interfaces:**
- Consumes: the `adj` binary via `env!("CARGO_BIN_EXE_adj")`; the same `Sandbox` harness shape used across `crates/adj/tests/`. Sets `ADJACENT_PROXY_PORT=18080` / `ADJACENT_HTTPS_PORT=18443` so the `_DIRECT` URLs are deterministic (these are configured, non-zero values — read directly, so a bind collision with another daemon is irrelevant; the app boots via the control-plane `up`, not the proxy).

- [ ] **Step 1: Write the test (it fails until the daemon under test injects the vars)**

Create `crates/adj/tests/boot_env.rs`:

```rust
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
    let body = format!("name = \"{name}\"\ncmd = \"{cmd}\"\n");
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
        if text.contains("NAME=") {
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
```

- [ ] **Step 2: Run the test**

Run: `cargo test -p adj --test boot_env`
Expected: PASS (1 test). If it fails on the `:18443`/`:18080` assertions, confirm Task 2's injection ran and the env names match.

- [ ] **Step 3: Commit**

```bash
git add crates/adj/tests/boot_env.rs
git commit crates/adj/tests/boot_env.rs -m "$(cat <<'EOF'
Add end-to-end test for ADJ_* boot environment

Boots an app whose cmd echoes the ADJ_* vars into its log and asserts the
injected name, host, and four URLs reach the child with correct values
(proxy/https ports pinned so the _DIRECT URLs are deterministic).

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Task 4: Documentation

Document the namespace where users and agents meet it: the `adj add` scaffold, the per-app agent doc, and the `adjacent.toml` reference in `CLAUDE.md`.

**Files:**
- Modify: `crates/adj/src/scaffold.rs` (`render()` `# Optional:` block; add a unit-test assertion)
- Modify: `crates/adj/src/agent_docs.rs` (`render()` — add an `ADJ_*` section)
- Modify: `CLAUDE.md` (the "Registry & config" per-app config block)

**Interfaces:**
- No code interfaces. `scaffold::render` and `agent_docs::render` output strings; existing tests use `contains`, so added lines don't break them.

- [ ] **Step 1: Write the failing scaffold test**

In `crates/adj/src/scaffold.rs`, the `#[cfg(test)] mod tests` has a test that asserts `render(Some("myapp"), Some("npm run dev"))` contents (~line 274). Add one assertion to it:

```rust
        assert!(toml.contains("# ADJ_URL_HTTP"), "{toml}");
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p adj -- --test-threads=1 render`
Expected: FAIL — the scaffold doesn't mention `ADJ_URL_HTTP` yet.

(If the test name is ambiguous, run the specific scaffold render test, e.g. `cargo test -p adj render_includes_optional` — use the actual test fn name shown in the failure output.)

- [ ] **Step 3: Update the scaffold `# Optional:` block**

In `crates/adj/src/scaffold.rs::render()`, change the `# Optional:` block (currently ending at the `health_check_url` line) to add the `ADJ_*` note. Replace:

```rust
         # Optional:\n\
         # port_env = \"PORT\"\n\
         # env_file = \".env.local\"\n\
         # idle_timeout = \"15m\"          # \"30s\" / \"1h\" / \"off\"\n\
         # health_check_url = \"/healthz\"\n"
```

with:

```rust
         # Optional:\n\
         # port_env = \"PORT\"\n\
         # env_file = \".env.local\"\n\
         # idle_timeout = \"15m\"          # \"30s\" / \"1h\" / \"off\"\n\
         # health_check_url = \"/healthz\"\n\
         \n\
         # Adjacent injects these into `cmd` at boot (reserved, daemon-owned):\n\
         #   $ADJ_NAME $ADJ_HOST\n\
         #   $ADJ_URL $ADJ_URL_HTTP                  # https/http, clean (needs install-port-forward)\n\
         #   $ADJ_URL_DIRECT $ADJ_URL_HTTP_DIRECT    # same, with the daemon's real ports\n\
         # e.g. hugo: cmd = \"hugo server --appendPort=false --port $PORT --baseURL $ADJ_URL_HTTP\"\n"
```

- [ ] **Step 4: Run the scaffold test to verify it passes**

Run: `cargo test -p adj render`
Expected: PASS.

- [ ] **Step 5: Add an `ADJ_*` section to the per-app agent doc**

In `crates/adj/src/agent_docs.rs::render()`, insert a new section before the closing `## JSON output` section. After the `## Manual control (usually not needed)` block and before `## JSON output`, add:

```rust
## Boot environment

Adjacent injects these into `{name}`'s `cmd` at boot (reserved `ADJ_*` namespace,
daemon-owned — they win over `env_file` / `[env]`):

- `$ADJ_NAME` — `{name}` (the routing key)
- `$ADJ_HOST` — `{name}.adj.ac`
- `$ADJ_URL` / `$ADJ_URL_HTTP` — clean base URL (assumes `adj install-port-forward`)
- `$ADJ_URL_DIRECT` / `$ADJ_URL_HTTP_DIRECT` — same, carrying the daemon's real listener ports

```

(Keep the existing raw-string format; `{name}` interpolates as elsewhere in the template. The trailing blank line preserves spacing before `## JSON output`.)

- [ ] **Step 6: Run the agent_docs tests**

Run: `cargo test -p adj --test agent_docs`
Expected: PASS (the existing tests assert `contains`, unaffected). Also run the in-module test: `cargo test -p adj render_substitutes_name_and_cmd` — Expected: PASS.

- [ ] **Step 7: Update the `CLAUDE.md` reference**

In `CLAUDE.md`, in the "Registry & config" section, immediately after the per-app config ```toml``` block (the one ending with `idle_timeout = "15m"`), add this paragraph:

```markdown
At boot the daemon injects a reserved, daemon-owned `ADJ_*` namespace into `cmd` (after
`env_file`/`[env]`, so these win): `ADJ_NAME` (routing key), `ADJ_HOST` (`<name>.adj.ac`),
and four base URLs — `ADJ_URL`/`ADJ_URL_HTTP` (clean, assume the port-forward) and
`ADJ_URL_DIRECT`/`ADJ_URL_HTTP_DIRECT` (carrying the daemon's real listener ports). Lets a
`cmd` address its own origin, e.g. `hugo server --appendPort=false --port $PORT --baseURL $ADJ_URL_HTTP`.
```

- [ ] **Step 8: Commit**

```bash
git add crates/adj/src/scaffold.rs crates/adj/src/agent_docs.rs CLAUDE.md
git commit crates/adj/src/scaffold.rs crates/adj/src/agent_docs.rs CLAUDE.md -m "$(cat <<'EOF'
Document the ADJ_* boot environment

Surface the reserved ADJ_* namespace where users and agents meet it: the
`adj add` scaffold's Optional block (with a Hugo example), the per-app agent
doc, and the adjacent.toml reference in CLAUDE.md.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

### Final verification

- [ ] **Run the full suite**

Run: `cargo test`
Expected: all tests pass, including the new `boot_env` integration test and the supervisor unit tests.

- [ ] **Confirm clippy is clean (matches CI)**

Run: `cargo clippy --all-targets -- -D warnings`
Expected: no warnings. (If `read_port_file`'s `Result<PathBuf>` argument trips `clippy::result_large_err` or similar, address per the lint's guidance.)
