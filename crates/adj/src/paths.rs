use std::path::PathBuf;

use anyhow::{Context, Result};

const HOME_OVERRIDE_ENV: &str = "ADJACENT_HOME";

pub fn home_dir() -> Result<PathBuf> {
    if let Ok(override_path) = std::env::var(HOME_OVERRIDE_ENV) {
        return Ok(PathBuf::from(override_path));
    }
    default_home_dir()
}

/// The ambient `~/.adjacent` path, ignoring `ADJACENT_HOME`. Used when we need to tell apart a
/// real install from a test sandbox (e.g. when picking a Keychain label).
pub fn default_home_dir() -> Result<PathBuf> {
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

/// Where the daemon records the proxy listener's actually-bound port. Only interesting when
/// `ADJACENT_PROXY_PORT=0` hands port selection to the kernel — see `proxy::report_bound_port`.
pub fn proxy_port_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("proxy.port"))
}

/// HTTPS counterpart of `proxy_port_path`. Absent when the HTTPS listener never bound
/// (no CA installed, or the port was taken).
pub fn https_port_path() -> Result<PathBuf> {
    Ok(home_dir()?.join("https.port"))
}

pub fn log_path(name: &str) -> Result<PathBuf> {
    Ok(logs_dir()?.join(format!("{name}.log")))
}

pub fn ensure_dirs() -> Result<()> {
    std::fs::create_dir_all(home_dir()?)?;
    std::fs::create_dir_all(logs_dir()?)?;
    Ok(())
}
