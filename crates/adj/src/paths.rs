use std::path::PathBuf;

use anyhow::{Context, Result};

const HOME_OVERRIDE_ENV: &str = "ADJACENT_HOME";

pub fn home_dir() -> Result<PathBuf> {
    if let Ok(override_path) = std::env::var(HOME_OVERRIDE_ENV) {
        return Ok(PathBuf::from(override_path));
    }
    let home = dirs::home_dir().context("could not determine home directory")?;
    Ok(home.join(".adjacent"))
}

pub fn logs_dir() -> Result<PathBuf> {
    Ok(home_dir()?.join("logs"))
}

pub fn socket_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("sock"))
}

pub fn registry_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("registry.toml"))
}

pub fn log_path(name: &str) -> Result<PathBuf> {
    Ok(logs_dir()?.join(format!("{name}.log")))
}

pub fn ensure_dirs() -> Result<()> {
    std::fs::create_dir_all(home_dir()?)?;
    std::fs::create_dir_all(logs_dir()?)?;
    Ok(())
}
