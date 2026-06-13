use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub name: String,
    pub cmd: String,
    /// Override the env var name used to inject the assigned port.
    /// When unset, Adjacent exports `PORT`. When set, it exports the named variable instead.
    #[serde(default)]
    pub port_env: Option<String>,
    /// Committed-safe environment variables merged into the spawned process env.
    /// On conflict with `env_file`, this table wins. PORT injection always wins over both.
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    /// Path to a dotenv-format file, resolved relative to the registered app directory.
    /// Missing files are a startup error.
    #[serde(default)]
    pub env_file: Option<String>,
    /// Per-app override for the proxy lazy-boot timeout, in seconds. Caps how long the proxy
    /// will hold an incoming request waiting for a boot to reach TCP-ready before returning 504.
    #[serde(default)]
    pub boot_timeout: Option<u64>,
    /// Optional HTTP path to poll for readiness on boot. When set, the daemon GETs
    /// `http://127.0.0.1:<port><health_check_url>` repeatedly and treats the app as ready when
    /// the response is 2xx. When unset, the daemon falls back to the TCP-open probe.
    #[serde(default)]
    pub health_check_url: Option<String>,
    /// Per-app idle shutdown duration. Accepts `"15m"`, `"30s"`, `"1h"`, or `"off"` to disable.
    /// When unset, the daemon uses a default of 15 minutes.
    #[serde(default)]
    pub idle_timeout: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Registry {
    #[serde(default)]
    pub apps: BTreeMap<String, AppEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppEntry {
    pub path: PathBuf,
}

impl Registry {
    pub fn load() -> Result<Self> {
        let path = paths::registry_path()?;
        if !path.exists() {
            return Ok(Self::default());
        }
        let raw = std::fs::read_to_string(&path)
            .with_context(|| format!("reading registry at {}", path.display()))?;
        let reg: Registry = toml::from_str(&raw).context("parsing registry.toml")?;
        Ok(reg)
    }

    pub fn save(&self) -> Result<()> {
        paths::ensure_dirs()?;
        let path = paths::registry_path()?;
        let raw = toml::to_string_pretty(self).context("serializing registry.toml")?;
        std::fs::write(&path, raw)
            .with_context(|| format!("writing registry at {}", path.display()))?;
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&AppEntry> {
        self.apps.get(name)
    }

    pub fn insert(&mut self, name: String, entry: AppEntry) {
        self.apps.insert(name, entry);
    }
}

pub fn read_app_config(dir: &Path) -> Result<AppConfig> {
    let manifest = dir.join("adjacent.toml");
    if !manifest.exists() {
        return Err(anyhow!("no adjacent.toml found at {}", manifest.display()));
    }
    let raw = std::fs::read_to_string(&manifest)
        .with_context(|| format!("reading {}", manifest.display()))?;
    let cfg: AppConfig =
        toml::from_str(&raw).with_context(|| format!("parsing {}", manifest.display()))?;
    if cfg.name.trim().is_empty() {
        return Err(anyhow!("adjacent.toml is missing a non-empty `name`"));
    }
    if cfg.cmd.trim().is_empty() {
        return Err(anyhow!("adjacent.toml is missing a non-empty `cmd`"));
    }
    // Validate idle_timeout eagerly so a typo fails at `add`/`up` time, not silently.
    if let Some(s) = cfg.idle_timeout.as_deref() {
        parse_idle_timeout(s)
            .with_context(|| format!("parsing idle_timeout = \"{}\"", s))?;
    }
    Ok(cfg)
}

/// Default idle timeout when the per-app field is unset. 15 minutes — long enough for a
/// developer to step away and come back to a still-running app, short enough that a forgotten
/// app doesn't hold the port indefinitely.
pub const DEFAULT_IDLE_TIMEOUT: Duration = Duration::from_secs(15 * 60);

/// Parse the `idle_timeout` TOML field. Accepts `"15m"`, `"30s"`, `"1h"`, `"500ms"`, or
/// `"off"` (case-insensitive) to disable idle shutdown. Returns `Ok(None)` for `"off"`.
/// Zero durations are rejected: a zero window would make the app a permanent shutdown
/// candidate (stopped on every scan tick), and users writing `"0s"` almost always mean
/// "disable" — which is what `"off"` does.
pub fn parse_idle_timeout(raw: &str) -> Result<Option<Duration>> {
    let s = raw.trim();
    if s.eq_ignore_ascii_case("off") || s.eq_ignore_ascii_case("disabled") {
        return Ok(None);
    }
    // Split into numeric prefix and unit suffix. We don't pull in `humantime` for this — a
    // hand-rolled parser keeps the dependency footprint flat.
    let unit_start = s
        .find(|c: char| !c.is_ascii_digit())
        .ok_or_else(|| anyhow!("missing unit suffix (expected `s`, `m`, `h`, or `ms`)"))?;
    if unit_start == 0 {
        return Err(anyhow!("missing leading number"));
    }
    let (num_part, unit_part) = s.split_at(unit_start);
    let n: u64 = num_part
        .parse()
        .with_context(|| format!("not a non-negative integer: `{}`", num_part))?;
    let dur = match unit_part {
        "ms" => Duration::from_millis(n),
        "s" => Duration::from_secs(n),
        "m" => Duration::from_secs(n * 60),
        "h" => Duration::from_secs(n * 60 * 60),
        other => return Err(anyhow!("unknown duration unit `{}`", other)),
    };
    if dur.is_zero() {
        return Err(anyhow!(
            "idle_timeout of zero would stop the app on every idle scan; use `off` to disable idle shutdown"
        ));
    }
    Ok(Some(dur))
}

/// Resolve `idle_timeout` to a Duration, applying the default when unset. Returns `None` when
/// idle shutdown is disabled (`"off"`). Invalid values warn and fall back to the default
/// rather than erroring — see the comment on the parse arm below.
pub fn idle_timeout_for(cfg: &AppConfig) -> Option<Duration> {
    match cfg.idle_timeout.as_deref() {
        None => Some(DEFAULT_IDLE_TIMEOUT),
        // Unreachable when callers go through `read_app_config`, which validates eagerly.
        // Warn loudly anyway so a future code path that skips validation can't turn a
        // config typo into a silent default.
        Some(s) => parse_idle_timeout(s).unwrap_or_else(|err| {
            tracing::warn!(
                idle_timeout = s,
                error = %err,
                default = ?DEFAULT_IDLE_TIMEOUT,
                "invalid idle_timeout; falling back to default"
            );
            Some(DEFAULT_IDLE_TIMEOUT)
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_supported_units() {
        assert_eq!(parse_idle_timeout("30s").unwrap(), Some(Duration::from_secs(30)));
        assert_eq!(parse_idle_timeout("15m").unwrap(), Some(Duration::from_secs(900)));
        assert_eq!(parse_idle_timeout("2h").unwrap(), Some(Duration::from_secs(7200)));
        assert_eq!(parse_idle_timeout("250ms").unwrap(), Some(Duration::from_millis(250)));
    }

    #[test]
    fn off_disables_idle_shutdown() {
        assert_eq!(parse_idle_timeout("off").unwrap(), None);
        assert_eq!(parse_idle_timeout("OFF").unwrap(), None);
        assert_eq!(parse_idle_timeout("disabled").unwrap(), None);
    }

    fn cfg_with_idle_timeout(idle_timeout: Option<&str>) -> AppConfig {
        AppConfig {
            name: "test".into(),
            cmd: "true".into(),
            port_env: None,
            env: None,
            env_file: None,
            boot_timeout: None,
            health_check_url: None,
            idle_timeout: idle_timeout.map(str::to_owned),
        }
    }

    #[test]
    fn idle_timeout_for_resolves_unset_valid_and_off() {
        assert_eq!(idle_timeout_for(&cfg_with_idle_timeout(None)), Some(DEFAULT_IDLE_TIMEOUT));
        assert_eq!(
            idle_timeout_for(&cfg_with_idle_timeout(Some("30s"))),
            Some(Duration::from_secs(30))
        );
        assert_eq!(idle_timeout_for(&cfg_with_idle_timeout(Some("off"))), None);
    }

    #[test]
    fn idle_timeout_for_warns_and_falls_back_to_default_on_parse_failure() {
        // The old `unwrap_or` returned the same value on parse failure; the warn is the only
        // observable difference, so capture the subscriber output and pin both.
        #[derive(Clone, Default)]
        struct Capture(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);
        impl std::io::Write for Capture {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                self.0.lock().unwrap().extend_from_slice(buf);
                Ok(buf.len())
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
            type Writer = Capture;
            fn make_writer(&'a self) -> Capture {
                self.clone()
            }
        }

        let capture = Capture::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(capture.clone())
            .finish();
        let resolved = tracing::subscriber::with_default(subscriber, || {
            idle_timeout_for(&cfg_with_idle_timeout(Some("10x")))
        });

        assert_eq!(resolved, Some(DEFAULT_IDLE_TIMEOUT));
        let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
        assert!(
            logs.contains("invalid idle_timeout") && logs.contains("10x") && logs.contains("900s"),
            "expected a warn naming the bad value and the default it fell back to, got: {logs}"
        );
    }

    #[test]
    fn rejects_zero_and_points_at_off() {
        for raw in ["0s", "0ms", "0m", "0h"] {
            let err = parse_idle_timeout(raw).unwrap_err();
            assert!(
                err.to_string().contains("use `off`"),
                "error for {:?} should suggest `off`, got: {}",
                raw,
                err
            );
        }
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_idle_timeout("").is_err());
        assert!(parse_idle_timeout("m").is_err());
        assert!(parse_idle_timeout("10").is_err());
        assert!(parse_idle_timeout("10x").is_err());
        assert!(parse_idle_timeout("abc").is_err());
    }
}
