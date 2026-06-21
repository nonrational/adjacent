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
