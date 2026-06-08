# `adj agent-instructions` Command Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add a new CLI subcommand `adj agent-instructions` that reads the `adjacent.toml` in a target directory (default: CWD) and prints a markdown steering doc explaining to AI coding agents how to interact with the Adjacent-supervised app — so the agent uses `adj status` / `adj logs` / `adj restart` instead of running the dev command directly.

**Architecture:** New subcommand wired into `crates/adj/src/main.rs`, backed by a small new module `crates/adj/src/agent_docs.rs` (parallel structure to the existing `portforward.rs`). Reads the manifest via the existing `registry::read_app_config`, builds the doc with `format!`, prints to stdout. No daemon involvement — like `install-port-forward`, this command is a pure local read-and-print. One integration test file `crates/adj/tests/agent_docs.rs` covers the happy path, the missing-TOML error, and the CWD default.

**Tech Stack:** Rust 1.92 (pinned via `.tool-versions`), `clap` for CLI, `anyhow` for errors, existing `registry::read_app_config` for TOML parsing, `tempfile` + `tokio::process::Command` for the integration test (already used by `tests/json_output.rs`).

**Resolves:** [nonrational/adjacent#35](https://github.com/nonrational/adjacent/issues/35)

---

## File Structure

- Create: `crates/adj/src/agent_docs.rs` — module that builds and prints the markdown doc.
- Modify: `crates/adj/src/main.rs` — register the module and CLI subcommand.
- Create: `crates/adj/tests/agent_docs.rs` — three integration tests.
- Modify: `README.md` — add a one-paragraph note about the new command.

---

## Background: command shape and content

The output is a single markdown document templated with two fields from `adjacent.toml`: `name` and `cmd`. Everything else is fixed boilerplate. The doc tells the agent four things:

1. The dev server is supervised — don't run `cmd` directly.
2. Read state with `adj status` / `adj logs` / `adj list`.
3. Change-and-verify loop: `adj restart` → `adj wait-ready` → hit `http://<name>.adj.ac/`.
4. `--json` exists on every read command; schema is in `crates/adj/JSON.md`.

Modeled on `install-port-forward` (`crates/adj/src/portforward.rs`):
- No daemon connection.
- `Ok(())` / `Err(anyhow)` return.
- Lives in its own module; one short public function.

---

## Task 1: Stub module and wire the CLI subcommand

**Files:**
- Create: `crates/adj/src/agent_docs.rs`
- Modify: `crates/adj/src/main.rs`

- [ ] **Step 1: Create the module stub**

Create `crates/adj/src/agent_docs.rs`:

```rust
use anyhow::Result;

/// Print a markdown steering doc telling AI coding agents how to interact with the
/// Adjacent-supervised app at `path` (or the current directory when `path` is `None`).
///
/// The doc is templated with the app `name` and `cmd` from `adjacent.toml`. No daemon
/// connection — this command is a pure local read-and-print.
pub fn emit(path: Option<String>) -> Result<()> {
    let _ = path;
    Ok(())
}
```

- [ ] **Step 2: Register the module and subcommand in `main.rs`**

In `crates/adj/src/main.rs`, add `mod agent_docs;` next to the other `mod` declarations (alphabetical order — after `mod agent_docs;` no wait, between `mod` and `mod client`):

Replace:

```rust
mod client;
mod daemon;
mod env;
mod paths;
mod portforward;
mod proxy;
mod readiness;
mod registry;
mod supervisor;
```

with:

```rust
mod agent_docs;
mod client;
mod daemon;
mod env;
mod paths;
mod portforward;
mod proxy;
mod readiness;
mod registry;
mod supervisor;
```

Add a new variant to `enum Cmd` (place it after `WaitReady`, before `InstallPortForward`):

```rust
    /// Print a markdown steering doc telling AI coding agents how to interact with
    /// the Adjacent-supervised app in the target directory.
    AgentInstructions {
        /// Directory containing `adjacent.toml`. Defaults to the current directory.
        #[arg(long)]
        path: Option<String>,
    },
```

Add a match arm in `main()`'s `match cli.cmd` (before `Cmd::InstallPortForward`):

```rust
        Cmd::AgentInstructions { path } => agent_docs::emit(path),
```

Note: `agent_docs::emit` is **synchronous**, so the match arm does not need `.await`. This is the same pattern as `Cmd::InstallPortForward => portforward::install()`.

- [ ] **Step 3: Build to verify the wiring compiles**

Run: `cargo build`
Expected: clean build, no warnings about the new module (the `let _ = path;` line suppresses the unused-variable warning).

- [ ] **Step 4: Commit**

```bash
git add crates/adj/src/agent_docs.rs crates/adj/src/main.rs
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit -m "$(cat <<'EOF'
Stub adj agent-instructions subcommand

Wires a new CLI subcommand and an empty agent_docs module. Output content
arrives in the next commit. Issue #35.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 2: TDD the happy-path output

**Files:**
- Create: `crates/adj/tests/agent_docs.rs`
- Modify: `crates/adj/src/agent_docs.rs`

- [ ] **Step 1: Write the failing integration test**

Create `crates/adj/tests/agent_docs.rs`:

```rust
// Tests for `adj agent-instructions`. The command reads adjacent.toml in a target dir
// and prints a markdown steering doc to stdout. It does not require the daemon.

use std::path::Path;

use tempfile::TempDir;
use tokio::process::Command;

fn adj_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}

async fn write_manifest(dir: &Path, name: &str, cmd: &str) {
    let body = format!("name = \"{name}\"\ncmd = \"{cmd}\"\n");
    tokio::fs::write(dir.join("adjacent.toml"), body)
        .await
        .expect("write manifest");
}

#[tokio::test]
async fn emits_markdown_templated_with_app_name_and_cmd() {
    let dir = TempDir::new().expect("tempdir");
    write_manifest(dir.path(), "myapp", "npm run dev").await;

    let out = Command::new(adj_bin())
        .arg("agent-instructions")
        .arg("--path")
        .arg(dir.path())
        .output()
        .await
        .expect("agent-instructions");

    assert!(
        out.status.success(),
        "agent-instructions failed: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);

    // App name appears in the heading and example commands.
    assert!(
        stdout.contains("myapp"),
        "stdout missing app name `myapp`: {stdout}"
    );
    // The dev command appears so the agent knows what NOT to run.
    assert!(
        stdout.contains("npm run dev"),
        "stdout missing the dev cmd `npm run dev`: {stdout}"
    );
    // Key adj commands the agent should use are documented.
    for needle in [
        "adj status myapp",
        "adj logs myapp",
        "adj restart myapp",
        "adj wait-ready myapp",
    ] {
        assert!(stdout.contains(needle), "stdout missing `{needle}`: {stdout}");
    }
    // Proxy URL pattern.
    assert!(
        stdout.contains("myapp.adj.ac"),
        "stdout missing proxy URL `myapp.adj.ac`: {stdout}"
    );
    // No un-substituted template placeholders leaked through.
    assert!(
        !stdout.contains("{name}") && !stdout.contains("{cmd}"),
        "stdout contains un-substituted placeholder: {stdout}"
    );
}
```

- [ ] **Step 2: Run the test to verify it fails**

Run: `cargo test --test agent_docs emits_markdown_templated_with_app_name_and_cmd`
Expected: FAIL. The stub `emit` returns `Ok(())` and prints nothing, so the `stdout.contains("myapp")` assertion will fail.

- [ ] **Step 3: Implement `emit`**

Replace the body of `crates/adj/src/agent_docs.rs` with:

```rust
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::registry;

/// Print a markdown steering doc telling AI coding agents how to interact with the
/// Adjacent-supervised app at `path` (or the current directory when `path` is `None`).
///
/// The doc is templated with the app `name` and `cmd` from `adjacent.toml`. No daemon
/// connection — this command is a pure local read-and-print.
pub fn emit(path: Option<String>) -> Result<()> {
    let dir: PathBuf = match path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().context("resolving current directory")?,
    };
    let cfg = registry::read_app_config(&dir)?;
    print!("{}", render(&cfg.name, &cfg.cmd));
    Ok(())
}

fn render(name: &str, cmd: &str) -> String {
    format!(
        r#"# Working with `{name}` via Adjacent

This project's dev server is supervised by **Adjacent** (`adj`). The agent does not
start the server directly — `adj` lazy-boots it on the first proxied request, captures
stdout/stderr to `~/.adjacent/logs/{name}.log`, and stops it on idle.

## Don't run the dev command yourself

Don't run `{cmd}` directly. Adjacent owns the process. Running it directly
double-binds the port and Adjacent loses visibility into the log stream.

## Read state

- `adj status {name}` — current state (`stopped` / `running` / `crashed`).
- `adj logs {name}` — print recent log lines.
- `adj logs {name} --tail` — stream new log lines (`Ctrl-C` to stop).
- `adj list` — every registered app and its state.

## Change-and-verify loop

When you edit code that does not hot-reload:

1. `adj restart {name}`
2. `adj wait-ready {name}` — blocks until the app reports ready.
3. Hit `http://{name}.adj.ac/` to verify behavior.

## Manual control (usually not needed)

- `adj up {name}` — boot now.
- `adj down {name}` — stop now (SIGTERM, then SIGKILL after a grace period).

## JSON output

Every read command (`list`, `status`, `logs`) accepts `--json` for a stable,
machine-parseable shape. The schema is in `crates/adj/JSON.md`.
"#
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_substitutes_name_and_cmd() {
        let out = render("myapp", "npm run dev");
        assert!(out.contains("myapp"));
        assert!(out.contains("npm run dev"));
        assert!(!out.contains("{name}"));
        assert!(!out.contains("{cmd}"));
    }
}
```

Note: `print!` (not `println!`) — the raw string already ends with `\n`. Using `println!` would add a spurious blank trailing line.

- [ ] **Step 4: Run the test and the unit test to verify they pass**

Run: `cargo test --test agent_docs emits_markdown_templated_with_app_name_and_cmd`
Expected: PASS.

Run: `cargo test --lib --bin adj agent_docs::tests::render_substitutes_name_and_cmd`
Expected: PASS.

- [ ] **Step 5: Commit**

```bash
git add crates/adj/src/agent_docs.rs crates/adj/tests/agent_docs.rs
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit -m "$(cat <<'EOF'
Render agent-instructions markdown templated with app name and cmd

Reads adjacent.toml via registry::read_app_config, prints a steering doc to
stdout. Tells agents to use adj status / logs / restart / wait-ready instead
of running the dev command directly. Issue #35.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 3: Error when `adjacent.toml` is missing

**Files:**
- Modify: `crates/adj/tests/agent_docs.rs`

The existing `registry::read_app_config` already returns `Err("no adjacent.toml found at <path>")` when the manifest is absent — see `crates/adj/src/registry.rs:85`. This task locks that behavior in with a test.

- [ ] **Step 1: Add the failing test**

Append to `crates/adj/tests/agent_docs.rs`:

```rust
#[tokio::test]
async fn errors_when_manifest_missing() {
    let dir = TempDir::new().expect("tempdir");
    // Intentionally do NOT write adjacent.toml.

    let out = Command::new(adj_bin())
        .arg("agent-instructions")
        .arg("--path")
        .arg(dir.path())
        .output()
        .await
        .expect("agent-instructions");

    assert!(
        !out.status.success(),
        "expected non-zero exit when adjacent.toml is missing"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no adjacent.toml found"),
        "stderr should explain the missing manifest, got: {stderr}"
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test --test agent_docs errors_when_manifest_missing`
Expected: PASS on the first run. `read_app_config` already errors as required; this test simply documents that contract for the new command.

If it fails: re-read `crates/adj/src/registry.rs:82-103` — the existing error message changed, or `emit` is swallowing the error. Fix `emit` so it propagates the error from `read_app_config` (the `?` in the current implementation already does this).

- [ ] **Step 3: Commit**

```bash
git add crates/adj/tests/agent_docs.rs
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit -m "$(cat <<'EOF'
Test agent-instructions errors when adjacent.toml is missing

Locks in the existing read_app_config error contract for the new command.
Issue #35.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 4: Default to the current working directory when `--path` is omitted

**Files:**
- Modify: `crates/adj/tests/agent_docs.rs`

The implementation in Task 2 already calls `std::env::current_dir()` when `path` is `None`. This task verifies that path through an integration test.

- [ ] **Step 1: Add the test that uses `current_dir` on the spawned process**

Append to `crates/adj/tests/agent_docs.rs`:

```rust
#[tokio::test]
async fn defaults_to_cwd_when_path_flag_omitted() {
    let dir = TempDir::new().expect("tempdir");
    write_manifest(dir.path(), "cwdapp", "node server.js").await;

    let out = Command::new(adj_bin())
        .arg("agent-instructions")
        .current_dir(dir.path())
        .output()
        .await
        .expect("agent-instructions");

    assert!(
        out.status.success(),
        "agent-instructions failed: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("cwdapp") && stdout.contains("node server.js"),
        "stdout missing templated fields from CWD manifest: {stdout}"
    );
}
```

- [ ] **Step 2: Run the test**

Run: `cargo test --test agent_docs defaults_to_cwd_when_path_flag_omitted`
Expected: PASS. `emit` already falls back to `std::env::current_dir()` and `tokio::process::Command::current_dir` sets the spawned process's CWD before launch.

If it fails: confirm the spawned binary really sees the tempdir as its CWD (it should — `Command::current_dir` is `chdir` before exec). Re-check the `match path { None => std::env::current_dir() ... }` branch in `agent_docs::emit`.

- [ ] **Step 3: Run the whole suite to confirm nothing else regressed**

Run: `cargo test`
Expected: all tests pass (unit + four pre-existing integration files + the new `agent_docs.rs`).

- [ ] **Step 4: Commit**

```bash
git add crates/adj/tests/agent_docs.rs
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit -m "$(cat <<'EOF'
Test agent-instructions defaults to CWD when --path is omitted

Issue #35.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 5: Document the new command in README

**Files:**
- Modify: `README.md`

- [ ] **Step 1: Add a short subsection under the existing CLI walkthrough**

In `README.md`, after the existing minimal `adjacent.toml` block and before the closing paragraph about `ent/`, append a new section. The current README ends at:

```
Then `curl -H 'Host: site.adj.ac' http://127.0.0.1:8080/` lazy-boots the app and proxies through. Full `--json` output schema in [`crates/adj/JSON.md`](crates/adj/JSON.md).

The landing page sources live in `ent/`; `just serve` runs `npx live-server` against it.
```

Insert this section between those two paragraphs:

```markdown
## Telling agents about `adj`

When a coding agent runs in a directory with `adjacent.toml`, it needs to know to use `adj` instead of starting the dev server itself. `adj agent-instructions` prints a markdown steering doc — pipe it into the agent's instructions file:

```sh
cd path/to/your/app
adj agent-instructions >> CLAUDE.md   # or AGENTS.md
```

The doc names the app, names the dev command the agent should not run, and lists the `adj` subcommands the agent should use to read state, restart, and verify changes.
```

Note: the fenced ` ```sh ` block inside the inserted markdown is literal — when adding it via Edit, preserve the inner backticks exactly. The outer fence in this plan is just for display.

- [ ] **Step 2: Commit**

```bash
git add README.md
git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit -m "$(cat <<'EOF'
Document agent-instructions in README

Issue #35.

Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>
EOF
)"
```

---

## Task 6: Open the PR

**Files:** none (git/gh only).

- [ ] **Step 1: Switch GitHub auth to the agent identity**

Read `~/.claude/projects/-Users-norton-src-adjacent/memory/project_agent_identity.md` first if you have not already. Then:

```bash
gh auth switch -u nonreagent
gh auth setup-git
```

- [ ] **Step 2: Push the branch and open the PR**

```bash
git push -u origin HEAD
gh pr create --title "Add adj agent-instructions command" --body "$(cat <<'EOF'
## Summary

- Adds `adj agent-instructions`, a new subcommand that reads `adjacent.toml` in the target directory and prints a markdown steering doc explaining to AI coding agents how to interact with the Adjacent-supervised app.
- Mirrors the `install-port-forward` pattern: no daemon connection, pure local read-and-print.
- Default target is the current working directory; `--path <dir>` overrides.

Resolves #35

## Test plan

- [x] `cargo test --test agent_docs` — three integration tests cover happy path, missing manifest, and CWD default.
- [x] `cargo test` — full suite stays green.
- [ ] Manual: `cd` into an Adjacent-managed app and run `adj agent-instructions` — confirm the rendered markdown names the right app, names the right dev command, and lists `adj status` / `logs` / `restart` / `wait-ready`.

🤖 Generated with [Claude Code](https://claude.com/claude-code)
EOF
)"
```

- [ ] **Step 3: Restore the human's GitHub auth**

```bash
gh auth switch -u nonrational
```

- [ ] **Step 4: Self-review per the agent identity protocol**

Read `~/.claude/projects/-Users-norton-src-adjacent/memory/project_agent_identity.md` for the self-review step. **Do not** run `gh pr review --approve` or `gh pr merge` — the human reviews and merges.

---

## Self-review

**Spec coverage.** The issue asks for "a command to emit a steering doc to tell Claude (or other agents) how to use `adj` to interact with the server if toml is present." Task 1 wires the command; Task 2 emits the doc (templated with the app name and cmd from the TOML); Task 3 covers the "if toml is present" guard (errors clearly when absent); Task 4 makes the command ergonomic from inside the app directory; Task 5 documents discoverability; Task 6 ships it.

**Placeholders.** Every `cargo test ...` line names the exact test. Every code block contains real code. No "TBD", "add error handling", or "similar to Task N".

**Type consistency.** `emit(path: Option<String>) -> Result<()>` is used identically across tasks. The CLI flag is `--path` throughout. The clap variant is `AgentInstructions { path: Option<String> }` everywhere it appears. The integration test file is `crates/adj/tests/agent_docs.rs` throughout. The subcommand name on the CLI is `agent-instructions` (kebab) — clap derives this from the `AgentInstructions` (Pascal) variant automatically, matching how `InstallPortForward` becomes `install-port-forward` today.
