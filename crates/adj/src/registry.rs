use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

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
