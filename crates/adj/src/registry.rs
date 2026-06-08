use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};

use crate::paths;

pub const DEFAULT_LOG_MAX_SIZE_BYTES: u64 = 100 * 1024 * 1024;
pub const DEFAULT_LOG_MAX_FILES: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppConfig {
    pub name: String,
    pub cmd: String,
    /// Override the env var name used to inject the assigned port.
    /// When unset, Adjacent exports `PORT`. When set, it exports the named variable instead.
    #[serde(default)]
    pub port_env: Option<String>,
    /// Size cap before rotating `<name>.log`. Accepts a number with an optional unit suffix
    /// (e.g. `"100MB"`, `"1GB"`, `"1024"`). Defaults to 100MB when absent.
    #[serde(default)]
    pub log_max_size: Option<String>,
    /// Number of rotated files to keep alongside the active log (`.1` … `.N`). Defaults to 3.
    #[serde(default)]
    pub log_max_files: Option<usize>,
}

impl AppConfig {
    pub fn log_max_size_bytes(&self) -> Result<u64> {
        match self.log_max_size.as_deref() {
            Some(s) => parse_size(s)
                .with_context(|| format!("parsing log_max_size = {:?}", s)),
            None => Ok(DEFAULT_LOG_MAX_SIZE_BYTES),
        }
    }

    pub fn log_max_files_value(&self) -> usize {
        self.log_max_files.unwrap_or(DEFAULT_LOG_MAX_FILES)
    }
}

// Parse a size string like "100MB", "1.5 GB", "1024", "2 KiB". We accept both
// decimal (KB/MB/GB) and binary (KiB/MiB/GiB) suffixes; both map to powers of
// 1024 in practice — disk-log sizing doesn't justify dragging in SI vs IEC
// pedantry, and "100MB" universally means "around a hundred million bytes."
fn parse_size(input: &str) -> Result<u64> {
    let s = input.trim();
    if s.is_empty() {
        return Err(anyhow!("empty size string"));
    }
    let split = s
        .find(|c: char| c.is_ascii_alphabetic())
        .unwrap_or(s.len());
    let (num_part, unit_part) = s.split_at(split);
    let num_part = num_part.trim();
    let unit_part = unit_part.trim();
    let value: f64 = num_part
        .parse()
        .with_context(|| format!("not a number: {:?}", num_part))?;
    if value < 0.0 || !value.is_finite() {
        return Err(anyhow!("size must be finite and non-negative: {input}"));
    }
    let multiplier: u64 = match unit_part.to_ascii_uppercase().as_str() {
        "" | "B" => 1,
        "K" | "KB" | "KIB" => 1024,
        "M" | "MB" | "MIB" => 1024 * 1024,
        "G" | "GB" | "GIB" => 1024 * 1024 * 1024,
        "T" | "TB" | "TIB" => 1024_u64 * 1024 * 1024 * 1024,
        other => return Err(anyhow!("unknown size unit: {:?}", other)),
    };
    Ok((value * multiplier as f64) as u64)
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
    Ok(cfg)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bare_bytes() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
        assert_eq!(parse_size("0").unwrap(), 0);
    }

    #[test]
    fn parses_common_units() {
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("100MB").unwrap(), 100 * 1024 * 1024);
        assert_eq!(parse_size("1GB").unwrap(), 1024 * 1024 * 1024);
    }

    #[test]
    fn tolerates_whitespace_and_case() {
        assert_eq!(parse_size(" 2 mb ").unwrap(), 2 * 1024 * 1024);
        assert_eq!(parse_size("2 MiB").unwrap(), 2 * 1024 * 1024);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse_size("").is_err());
        assert!(parse_size("abc").is_err());
        assert!(parse_size("100ZZ").is_err());
    }

    #[test]
    fn defaults_when_no_log_max_size() {
        let cfg = AppConfig {
            name: "x".into(),
            cmd: "x".into(),
            port_env: None,
            log_max_size: None,
            log_max_files: None,
        };
        assert_eq!(cfg.log_max_size_bytes().unwrap(), DEFAULT_LOG_MAX_SIZE_BYTES);
        assert_eq!(cfg.log_max_files_value(), DEFAULT_LOG_MAX_FILES);
    }
}
