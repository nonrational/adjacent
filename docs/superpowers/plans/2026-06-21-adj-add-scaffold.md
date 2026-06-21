# `adj add` Scaffold Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make `adj add <path>` generate a default `adjacent.toml` when the directory has none — registering in one step when it can detect the dev command, and otherwise scaffolding a starter file with guidance.

**Architecture:** A new pure, client-side module `crates/adj/src/scaffold.rs` derives the app name from the directory basename, detects the dev command from marker files (an ordered, table-driven detector list), and renders the `adjacent.toml` body. `client::add` calls it when the manifest is missing, writes the file, and either falls through to the existing registration path (cmd detected) or returns a guidance error without registering (cmd not detected). No daemon or wire-protocol changes.

**Tech Stack:** Rust, `serde_json` (parse `package.json` / `deno.json`), `anyhow`, `clap`, `tokio`; tests use `tempfile`. All already in `crates/adj/Cargo.toml`.

## Global Constraints

- Rust toolchain `1.92.0` (pinned in `.tool-versions`).
- **No new dependencies.** Use `serde_json` and `toml` already in `crates/adj/Cargo.toml`; `tempfile` is the dev-dep for tests.
- **No daemon or wire-protocol changes** — generation is entirely client-side in `client::add`. The daemon still only reads `adjacent.toml` and registers `name → path`.
- **Never overwrite** an existing `adjacent.toml`.
- Generation is high-confidence only: emit a `cmd` solely when a marker file names the runner/framework. Otherwise leave `cmd` unset and do not register.
- Commit messages: **no Conventional Commit prefixes** ("fix:", "feat:"). Plain descriptive messages. Commit with `git add <paths>` + `git commit <paths>`, never `git commit -a`.
- Comments describe **WHY, not WHAT**; match the codebase's dense block-comment style above tricky logic.
- Names locked: brand **Adjacent**, CLI `adj`, config `adjacent.toml`. Repo slug for the contributor link: `nonrational/adjacent`.
- Commit message footer line, verbatim: `Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>`

---

## File Structure

- **Create** `crates/adj/src/scaffold.rs` — pure scaffold engine: `Scaffold` struct, `build()`, plus private `sanitize_name`, `detect_cmd` (+ `detect_deno`, `detect_node`, `node_runner`, `first_script`), `render`. In-module `#[cfg(test)]` unit tests.
- **Modify** `crates/adj/src/main.rs` — declare `mod scaffold;`.
- **Modify** `crates/adj/src/client.rs` — `add()` scaffolds a missing manifest before registering.
- **Create** `crates/adj/tests/scaffold.rs` — integration tests (detected / not-detected / no-overwrite).
- **Create** `CONTRIBUTING.md` — documents the detector table and how to add a row.

---

## Task 1: Scaffold engine (`scaffold.rs`)

**Files:**
- Create: `crates/adj/src/scaffold.rs`
- Modify: `crates/adj/src/main.rs` (add `mod scaffold;`)
- Test: in-module `#[cfg(test)] mod tests` in `crates/adj/src/scaffold.rs`

**Interfaces:**
- Produces:
  - `pub struct Scaffold { pub name: Option<String>, pub detected_cmd: Option<String>, pub toml: String }`
  - `pub fn build(dir: &std::path::Path) -> Scaffold`
  - `name` is `None` when the basename doesn't reduce to a usable DNS label; `detected_cmd` is `None` when no high-confidence signal is present; `toml` is always renderable (placeholders fill in the `None` fields).
- Consumes: nothing from other new tasks. Pure functions over the filesystem.

- [ ] **Step 1: Add the module declaration**

In `crates/adj/src/main.rs`, add the module next to the others (after `mod registry;` is fine). Use `#[allow(dead_code)]` for now — nothing calls it until Task 2, and the binary build would otherwise warn:

```rust
#[allow(dead_code)] // wired into client::add in the next task
mod scaffold;
```

- [ ] **Step 2: Write the failing unit tests**

Create `crates/adj/src/scaffold.rs` with only the test module first (it won't compile yet — that's the failing state):

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    // ---- name sanitization ----

    #[test]
    fn sanitizes_basenames_to_dns_labels() {
        assert_eq!(sanitize_name("myapp"), "myapp");
        assert_eq!(sanitize_name("My App"), "my-app");
        assert_eq!(sanitize_name("Some_Repo.v2"), "some-repo-v2");
        assert_eq!(sanitize_name("--weird--"), "weird");
        // Repeated separators collapse to a single hyphen.
        assert_eq!(sanitize_name("a   b"), "a-b");
        // Non-ASCII reduces away; pathological input yields empty.
        assert_eq!(sanitize_name("日本語"), "");
        // Capped at 63 and never ends in a hyphen.
        let long = "a".repeat(70);
        assert!(sanitize_name(&long).len() <= 63);
    }

    // ---- node detection ----

    fn write(dir: &std::path::Path, name: &str, body: &str) {
        std::fs::write(dir.join(name), body).unwrap();
    }

    #[test]
    fn detects_node_with_npm_by_default() {
        let d = TempDir::new().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"dev":"vite"}}"#);
        assert_eq!(detect_cmd(d.path()).as_deref(), Some("npm run dev"));
    }

    #[test]
    fn node_runner_follows_lockfile() {
        for (lock, runner) in [
            ("pnpm-lock.yaml", "pnpm"),
            ("yarn.lock", "yarn"),
            ("bun.lockb", "bun"),
        ] {
            let d = TempDir::new().unwrap();
            write(d.path(), "package.json", r#"{"scripts":{"dev":"vite"}}"#);
            write(d.path(), lock, "");
            assert_eq!(
                detect_cmd(d.path()).as_deref(),
                Some(format!("{runner} run dev").as_str()),
                "lockfile {lock}"
            );
        }
    }

    #[test]
    fn script_priority_is_dev_then_start_then_serve() {
        let d = TempDir::new().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"serve":"x","start":"y"}}"#);
        assert_eq!(detect_cmd(d.path()).as_deref(), Some("npm run start"));
    }

    #[test]
    fn node_without_matching_script_is_undetected() {
        let d = TempDir::new().unwrap();
        write(d.path(), "package.json", r#"{"scripts":{"build":"x"}}"#);
        assert_eq!(detect_cmd(d.path()), None);
    }

    // ---- other stacks ----

    #[test]
    fn detects_deno_tasks() {
        let d = TempDir::new().unwrap();
        write(d.path(), "deno.json", r#"{"tasks":{"dev":"deno run -A main.ts"}}"#);
        assert_eq!(detect_cmd(d.path()).as_deref(), Some("deno task dev"));
    }

    #[test]
    fn deno_wins_over_node_when_both_present() {
        let d = TempDir::new().unwrap();
        write(d.path(), "deno.json", r#"{"tasks":{"dev":"x"}}"#);
        write(d.path(), "package.json", r#"{"scripts":{"dev":"vite"}}"#);
        assert_eq!(detect_cmd(d.path()).as_deref(), Some("deno task dev"));
    }

    #[test]
    fn detects_django_rails_rack() {
        let d = TempDir::new().unwrap();
        write(d.path(), "manage.py", "");
        assert_eq!(detect_cmd(d.path()).as_deref(), Some("python manage.py runserver"));

        let d = TempDir::new().unwrap();
        std::fs::create_dir(d.path().join("bin")).unwrap();
        write(d.path(), "bin/rails", "");
        assert_eq!(detect_cmd(d.path()).as_deref(), Some("bin/rails server"));

        let d = TempDir::new().unwrap();
        write(d.path(), "config.ru", "");
        assert_eq!(detect_cmd(d.path()).as_deref(), Some("bundle exec rackup"));
    }

    #[test]
    fn empty_dir_is_undetected() {
        let d = TempDir::new().unwrap();
        assert_eq!(detect_cmd(d.path()), None);
    }

    // ---- render + build ----

    #[test]
    fn renders_detected_manifest() {
        let toml = render(Some("myapp"), Some("npm run dev"));
        assert!(toml.contains("name = \"myapp\""), "{toml}");
        assert!(toml.contains("cmd = \"npm run dev\""), "{toml}");
        assert!(toml.contains("# port_env = \"PORT\""), "{toml}");
        // The generated manifest must parse back through the real config reader.
        assert!(toml::from_str::<toml::Value>(&toml).is_ok(), "invalid toml: {toml}");
    }

    #[test]
    fn renders_placeholder_when_undetected() {
        let toml = render(None, None);
        assert!(toml.contains("cmd = \"\""), "{toml}");
        assert!(toml.contains("TODO"), "{toml}");
        assert!(toml::from_str::<toml::Value>(&toml).is_ok(), "invalid toml: {toml}");
    }

    #[test]
    fn build_derives_name_from_basename() {
        let parent = TempDir::new().unwrap();
        let app = parent.path().join("MyApp");
        std::fs::create_dir(&app).unwrap();
        std::fs::write(app.join("package.json"), r#"{"scripts":{"dev":"vite"}}"#).unwrap();
        let s = build(&app);
        assert_eq!(s.name.as_deref(), Some("myapp"));
        assert_eq!(s.detected_cmd.as_deref(), Some("npm run dev"));
        assert!(s.toml.contains("name = \"myapp\""));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p adj --lib scaffold`
Expected: FAIL — compile errors (`sanitize_name`, `detect_cmd`, `render`, `build`, `Scaffold` not found).

- [ ] **Step 4: Implement the module**

Prepend the implementation above the test module in `crates/adj/src/scaffold.rs`:

```rust
use std::path::Path;

use serde_json::{Map, Value};

/// A generated `adjacent.toml` plus what we could infer. `name` is `None` when the directory
/// basename doesn't reduce to a usable DNS label; `detected_cmd` is `None` when no
/// high-confidence dev-command signal is present. `toml` is always renderable — `None` fields
/// become clearly-marked TODO placeholders so the user has a complete starting point.
pub struct Scaffold {
    pub name: Option<String>,
    pub detected_cmd: Option<String>,
    pub toml: String,
}

/// Build a scaffold for `dir`. Pure with respect to `dir`'s contents — it reads marker files
/// but writes nothing; the caller owns the single file write.
pub fn build(dir: &Path) -> Scaffold {
    let name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .map(sanitize_name)
        .filter(|s| !s.is_empty());
    let detected_cmd = detect_cmd(dir);
    let toml = render(name.as_deref(), detected_cmd.as_deref());
    Scaffold {
        name,
        detected_cmd,
        toml,
    }
}

/// Map a directory basename onto the DNS-label charset the daemon accepts. Distinct from
/// `worktree::sanitize_label`: that one drops characters it can't map and keeps runs of
/// hyphens; for a human-facing directory name we instead turn every non-`[a-z0-9]` run into a
/// single `-` (so `My App` → `my-app`, not `myapp`), then trim edges and cap at the 63-octet
/// DNS label limit. Re-validated by `read_app_config` via `validate_dns_label` on the next read.
fn sanitize_name(raw: &str) -> String {
    let mut out = String::new();
    let mut prev_dash = false;
    for c in raw.to_ascii_lowercase().chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            out.push(c);
            prev_dash = false;
        } else if !prev_dash {
            out.push('-');
            prev_dash = true;
        }
    }
    // Trim edge hyphens, cap at 63, then trim again in case truncation exposed a trailing one.
    // Every retained char is ASCII, so byte slicing is always on a char boundary.
    let trimmed = out.trim_matches('-');
    let capped = &trimmed[..trimmed.len().min(63)];
    capped.trim_end_matches('-').to_string()
}

/// Ordered, table-driven detection: first high-confidence signal wins. The order resolves
/// ambiguity (a repo carrying both `deno.json` tasks and `package.json` scripts is treated as
/// Deno). Stacks without a confident signal fall through to `None`; the caller turns that into
/// a "set cmd yourself / add a detector" message rather than guessing.
fn detect_cmd(dir: &Path) -> Option<String> {
    if let Some(cmd) = detect_deno(dir) {
        return Some(cmd);
    }
    if let Some(cmd) = detect_node(dir) {
        return Some(cmd);
    }
    if dir.join("manage.py").is_file() {
        return Some("python manage.py runserver".to_string());
    }
    if dir.join("bin/rails").is_file() {
        return Some("bin/rails server".to_string());
    }
    if dir.join("config.ru").is_file() {
        return Some("bundle exec rackup".to_string());
    }
    None
}

fn detect_deno(dir: &Path) -> Option<String> {
    // deno.jsonc may carry comments that serde_json can't parse; we only detect when the file
    // parses as plain JSON. A parse failure means "not confident", not an error.
    for file in ["deno.json", "deno.jsonc"] {
        let Ok(raw) = std::fs::read_to_string(dir.join(file)) else {
            continue;
        };
        let Ok(val) = serde_json::from_str::<Value>(&raw) else {
            continue;
        };
        if let Some(tasks) = val.get("tasks").and_then(Value::as_object) {
            if let Some(script) = first_script(tasks) {
                return Some(format!("deno task {script}"));
            }
        }
    }
    None
}

fn detect_node(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join("package.json")).ok()?;
    let val: Value = serde_json::from_str(&raw).ok()?;
    let scripts = val.get("scripts")?.as_object()?;
    let script = first_script(scripts)?;
    Some(format!("{} run {script}", node_runner(dir)))
}

/// The package manager is inferred from the lockfile present in the directory. `<runner> run
/// <script>` is valid for all four. pnpm is checked first, then bun, then yarn; a bare
/// `package-lock.json` or no lockfile at all falls back to npm.
fn node_runner(dir: &Path) -> &'static str {
    if dir.join("pnpm-lock.yaml").exists() {
        "pnpm"
    } else if dir.join("bun.lockb").exists() || dir.join("bun.lock").exists() {
        "bun"
    } else if dir.join("yarn.lock").exists() {
        "yarn"
    } else {
        "npm"
    }
}

/// First present of `dev` → `start` → `serve` in a scripts/tasks table.
fn first_script(map: &Map<String, Value>) -> Option<&'static str> {
    ["dev", "start", "serve"]
        .into_iter()
        .find(|k| map.contains_key(*k))
}

/// Render the `adjacent.toml` body. `None` fields become TODO placeholders. Detected commands
/// come from a fixed vocabulary (none contain `"`) and `sanitize_name` guarantees `name` is
/// `[a-z0-9-]`, so no TOML string escaping is needed here.
fn render(name: Option<&str>, cmd: Option<&str>) -> String {
    let name_line = match name {
        Some(n) => format!("name = \"{n}\"\n"),
        None => "name = \"app\"  # TODO: set a name (lowercase letters, digits, `-`)\n".to_string(),
    };
    let cmd_line = match cmd {
        Some(c) => format!("cmd = \"{c}\"\n"),
        None => "cmd = \"\"  # TODO: set your dev command, e.g. \"npm run dev\"\n".to_string(),
    };
    format!(
        "# Generated by `adj add` — edit and re-run if needed.\n\
         {name_line}\
         {cmd_line}\
         \n\
         # Optional:\n\
         # port_env = \"PORT\"\n\
         # env_file = \".env.local\"\n\
         # idle_timeout = \"15m\"          # \"30s\" / \"1h\" / \"off\"\n\
         # health_check_url = \"/healthz\"\n"
    )
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p adj --lib scaffold`
Expected: PASS (all scaffold unit tests green).

- [ ] **Step 6: Confirm the binary still builds clean**

Run: `cargo build -p adj`
Expected: builds with no warnings (the `#[allow(dead_code)]` covers the not-yet-wired module).

- [ ] **Step 7: Commit**

```bash
git add crates/adj/src/scaffold.rs crates/adj/src/main.rs
git commit crates/adj/src/scaffold.rs crates/adj/src/main.rs -m "$(cat <<'EOF'
Add scaffold engine for generating adjacent.toml

Pure client-side module: derive name from directory basename, detect the
dev command from marker files (Deno/Node/Django/Rails/Rack, ordered), and
render the manifest body. Wired into `adj add` in a follow-up.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: Wire scaffolding into `client::add`

**Files:**
- Modify: `crates/adj/src/client.rs` (`add()` and imports)
- Modify: `crates/adj/src/main.rs` (drop the `#[allow(dead_code)]` on `mod scaffold;`)

**Interfaces:**
- Consumes: `scaffold::build(&Path) -> Scaffold` (Task 1).
- Produces: no new public surface; changes `adj add`'s behavior when `adjacent.toml` is missing.

- [ ] **Step 1: Remove the dead-code allowance**

In `crates/adj/src/main.rs`, change the module declaration back to a plain one — it's used now:

```rust
mod scaffold;
```

- [ ] **Step 2: Import the module in client.rs**

Add to the `use crate::` group near the top of `crates/adj/src/client.rs` (it already imports `paths` and `worktree`):

```rust
use crate::scaffold;
```

- [ ] **Step 3: Replace the body of `add()`**

In `crates/adj/src/client.rs`, replace the existing `pub async fn add(...)` with:

```rust
pub async fn add(path: String, label: Option<String>) -> Result<()> {
    // Canonicalize on the client side: relative paths must resolve against the user's CWD,
    // not the daemon's. The daemon may have been launched from anywhere (or by launchd).
    let canon = std::fs::canonicalize(&path).with_context(|| format!("resolving path {}", path))?;

    // Scaffold a default manifest when the directory has none. The write happens client-side
    // for the same reason as canonicalization: the file belongs in the user's working tree,
    // and we never want the daemon (possibly rooted at `/` under launchd) writing into it.
    // An existing manifest is left untouched.
    let manifest = canon.join("adjacent.toml");
    if !manifest.exists() {
        let scaffold = scaffold::build(&canon);
        std::fs::write(&manifest, &scaffold.toml)
            .with_context(|| format!("writing {}", manifest.display()))?;
        match (&scaffold.name, &scaffold.detected_cmd) {
            (Some(_), Some(cmd)) => {
                println!("generated adjacent.toml (cmd = \"{cmd}\")");
                // fall through to registration
            }
            // No dev command: registering an app that can't boot is its own annoyance, so we
            // write the starter file but stop. Non-zero exit (an Err) makes the "do something"
            // explicit for scripts and agents chaining `adj add . && adj up`.
            (_, None) => {
                return Err(anyhow!(
                    "couldn't detect a dev command — wrote a starter adjacent.toml at {}.\n  \
                     set `cmd` (e.g. \"npm run dev\"), then run `adj add .` again.\n  \
                     know the command for this stack? add a detector:\n  \
                     https://github.com/nonrational/adjacent (see CONTRIBUTING)",
                    manifest.display()
                ));
            }
            // Rare: a basename that doesn't reduce to a DNS label. cmd is known but we won't
            // guess a name.
            (None, Some(_)) => {
                return Err(anyhow!(
                    "wrote a starter adjacent.toml at {}, but couldn't derive a name from the \
                     directory.\n  set `name` (lowercase letters, digits, `-`), then run \
                     `adj add .` again.",
                    manifest.display()
                ));
            }
        }
    }

    // `--label` wins; otherwise a linked git worktree names its instance after the branch.
    let label = match label {
        Some(l) => Some(l),
        None => worktree::detect_label(&canon)?,
    };
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

- [ ] **Step 4: Build and run the existing suite**

Run: `cargo build -p adj && cargo test -p adj`
Expected: PASS — the binary builds clean and all existing tests still pass (existing integration tests write their own `adjacent.toml` before `adj add`, so the new scaffold branch never fires for them).

- [ ] **Step 5: Manual smoke check**

Run:
```bash
ADJACENT_HOME=$(mktemp -d) cargo run -q -p adj -- add /tmp/does-not-exist-xyz 2>&1 || true
```
Expected: the `resolving path` error (canonicalize fails before scaffolding), confirming we don't write into non-existent dirs.

- [ ] **Step 6: Commit**

```bash
git add crates/adj/src/client.rs crates/adj/src/main.rs
git commit crates/adj/src/client.rs crates/adj/src/main.rs -m "$(cat <<'EOF'
Scaffold a default adjacent.toml in `adj add` when missing

When the target directory has no adjacent.toml, generate one. If the dev
command is detected, register in one step; otherwise write the starter
file and exit non-zero with guidance. Existing manifests are untouched.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Integration tests (`tests/scaffold.rs`)

**Files:**
- Create: `crates/adj/tests/scaffold.rs`

**Interfaces:**
- Consumes: the `adj` binary via `CARGO_BIN_EXE_adj`; the `Sandbox` pattern (daemon + `ADJACENT_HOME`) mirrored from `tests/tracer.rs`.

- [ ] **Step 1: Write the integration tests**

Create `crates/adj/tests/scaffold.rs`:

```rust
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
    assert!(!add.status.success(), "expected non-zero exit when cmd undetected");
    let stderr = String::from_utf8_lossy(&add.stderr);
    assert!(stderr.contains("couldn't detect a dev command"), "stderr: {stderr}");
    assert!(stderr.contains("CONTRIBUTING"), "stderr: {stderr}");

    // The file is written despite the non-zero exit.
    assert!(app.join("adjacent.toml").exists(), "manifest should be written");
    assert!(!sandbox.registry_has("myapp"), "myapp must not be registered");
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
```

- [ ] **Step 2: Run the integration tests**

Run: `cargo test -p adj --test scaffold`
Expected: PASS (all three tests green).

- [ ] **Step 3: Commit**

```bash
git add crates/adj/tests/scaffold.rs
git commit crates/adj/tests/scaffold.rs -m "$(cat <<'EOF'
Integration tests for `adj add` scaffolding

Cover the detected (file written + registered), undetected (file written,
not registered, non-zero exit), and existing-manifest (untouched) paths.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: `CONTRIBUTING.md`

**Files:**
- Create: `CONTRIBUTING.md`

**Interfaces:**
- Consumes: nothing. Referenced by the not-detected message from Task 2.

- [ ] **Step 1: Write CONTRIBUTING.md**

Create `CONTRIBUTING.md` at the repo root:

```markdown
# Contributing to Adjacent

## Build and test

Rust toolchain is pinned in `.tool-versions` — `asdf install` from the repo root.

- `cargo build` — workspace build (`target/debug/adj`).
- `cargo test` — full suite (unit + the integration tests under `crates/adj/tests/`).
- `cargo test -p adj --lib scaffold` — just the dev-command detector unit tests.

Tests sandbox their state via `ADJACENT_HOME=<tmpdir>` and start their own daemon, so they
never touch your real `~/.adjacent/`.

## Adding a dev-command detector

`adj add` scaffolds a default `adjacent.toml` and, when it recognizes the project's stack,
fills in `cmd` so the app registers in one step. Detection lives in
`crates/adj/src/scaffold.rs` as an ordered list — **first match wins** — and fires only on
**high-confidence signals** (a file that names the runner or framework). Anything it doesn't
recognize falls through to a "set `cmd` yourself" message; that's the escape hatch, so a
detector should never guess.

Current detectors, in order:

| # | Stack | Signal | Emitted `cmd` |
|---|-------|--------|---------------|
| 1 | Deno | `deno.json` / `deno.jsonc` has a `tasks.{dev,start,serve}` | `deno task <script>` |
| 2 | Node (npm/pnpm/yarn/bun) | `package.json` has `scripts.{dev,start,serve}` | `<runner> run <script>` |
| 3 | Python / Django | `manage.py` exists | `python manage.py runserver` |
| 4 | Ruby / Rails | `bin/rails` exists | `bin/rails server` |
| 5 | Ruby / Rack | `config.ru` exists (no `bin/rails`) | `bundle exec rackup` |

To add one:

1. Add a branch to `detect_cmd` (or a `detect_<stack>` helper) keyed on a file that
   unambiguously identifies the stack. Place it so more specific signals win over more
   general ones.
2. Emit the command a developer would actually run for a local dev server.
3. Add a unit test in the `tests` module of `scaffold.rs` covering the new signal.
4. Add a row to the table above.

Script-name priority across stacks is `dev` → `start` → `serve`.

## Conventions

- Commit messages: plain and descriptive, **no Conventional Commit prefixes** (`fix:`, `feat:`).
- PR bodies use `Resolves #N`.
- Comments describe **why**, not what.
```

- [ ] **Step 2: Commit**

```bash
git add CONTRIBUTING.md
git commit CONTRIBUTING.md -m "$(cat <<'EOF'
Add CONTRIBUTING with the dev-command detector guide

Documents the scaffold detector table and how to add a row — the target of
the "add a detector" pointer in `adj add`'s not-detected message.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Final verification

- [ ] Run the whole suite: `cargo test` — expected: all green.
- [ ] Lint: `cargo clippy --all-targets` — expected: no warnings.
- [ ] End-to-end sanity in a throwaway dir:
  ```bash
  export ADJACENT_HOME=$(mktemp -d)
  cargo run -q -p adj -- daemon &   # background
  mkdir -p /tmp/adjdemo && echo '{"scripts":{"dev":"vite"}}' > /tmp/adjdemo/package.json
  cargo run -q -p adj -- add /tmp/adjdemo   # expect: generated + registered
  cargo run -q -p adj -- list               # expect: adjdemo listed
  ```
  Expected: `generated adjacent.toml (cmd = "npm run dev")` then `registered \`adjdemo\``.

---

## Self-Review

**Spec coverage:**
- Enhance `adj add`, no new subcommand, client-side, never overwrite → Task 2.
- cmd detected → write + register one step; not detected → write + don't register + message → Task 2.
- Table-driven detectors (Deno/Node/Django/Rails/Rack), script priority, Node runner from lockfile, order resolves ambiguity → Task 1.
- name from sanitized basename → Task 1 (`sanitize_name`, `build`).
- Lean generated template → Task 1 (`render`).
- Not-detected message points at `nonrational/adjacent` (see CONTRIBUTING) → Task 2; CONTRIBUTING doc → Task 4.
- Tests: unit (detection per stack + name sanitization) + integration (detected/not-detected/no-overwrite) → Tasks 1 and 3.

**Placeholder scan:** No TBD/TODO-as-plan-gaps. (The literal `TODO` strings are intended *generated-file* content, asserted by tests.)

**Type consistency:** `Scaffold { name: Option<String>, detected_cmd: Option<String>, toml: String }` and `build(&Path) -> Scaffold` are used identically in Task 1 (definition + tests) and Task 2 (consumer). `detect_cmd`/`render`/`sanitize_name` are private, exercised by in-module tests via `super::*`.
