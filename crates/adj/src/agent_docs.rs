use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::registry;
use crate::worktree;

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
    // Steer the agent at the instance key the daemon will actually route to. Prefer the key this
    // directory is registered under: an explicit `adj add --label` can differ from the branch,
    // and re-deriving from the branch here would point every command and URL at an unregistered
    // instance. Only when the dir isn't registered yet (the pre-`adj add` bootstrap case) do we
    // fall back to deriving the worktree label the same way `client::add` would.
    let key = match registered_key(&dir, &cfg.name) {
        Some(k) => k,
        None => derive_key(&dir, &cfg.name)?,
    };
    print!("{}", render(&key, &cfg.cmd));
    Ok(())
}

/// The registry key whose entry points at `dir`, if this directory is registered. Matches on the
/// canonical path (registry entries are stored canonical) and restricts to keys whose base equals
/// the manifest `name`, so a stale entry for a different app at a reused path can't mislabel the
/// doc. Returns `None` — falling back to derivation — when the dir isn't registered or the
/// registry can't be read.
fn registered_key(dir: &Path, name: &str) -> Option<String> {
    let canon = std::fs::canonicalize(dir).ok()?;
    let reg = registry::Registry::load().ok()?;
    reg.apps
        .iter()
        .find(|(key, entry)| entry.path == canon && registry::base_name(key) == name)
        .map(|(key, _)| key.clone())
}

/// Derive the key `client::add` would assign when the dir is not yet registered: a linked
/// worktree's branch label, else the bare name. Propagates `detect_label` errors (detached HEAD,
/// git failure) so the bootstrap case surfaces the same guidance `adj add` would.
fn derive_key(dir: &Path, name: &str) -> Result<String> {
    Ok(match worktree::detect_label(dir)? {
        Some(label) => format!("{label}.{name}"),
        None => name.to_string(),
    })
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
