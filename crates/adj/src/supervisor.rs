use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use adj_protocol::AppState;
use anyhow::{anyhow, Context, Result};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::paths;
use crate::registry::AppConfig;

const TERM_GRACE: Duration = Duration::from_secs(5);

#[derive(Default)]
pub struct Supervisor {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    apps: HashMap<String, AppRuntime>,
}

struct AppRuntime {
    state: AppState,
}

impl Supervisor {
    pub fn new() -> Self {
        Self::default()
    }

    pub async fn state(&self, name: &str) -> AppState {
        let inner = self.inner.lock().await;
        inner
            .apps
            .get(name)
            .map(|rt| rt.state.clone())
            .unwrap_or(AppState::Stopped)
    }

    pub async fn up(&self, app_dir: PathBuf, cfg: AppConfig) -> Result<u32> {
        let name = cfg.name.clone();
        let mut inner = self.inner.lock().await;
        if let Some(rt) = inner.apps.get(&name) {
            if matches!(rt.state, AppState::Running { .. }) {
                return Err(anyhow!("app `{}` is already running", name));
            }
        }

        let log_path = paths::log_path(&name)?;
        paths::ensure_dirs()?;
        let log_file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .with_context(|| format!("opening log file {}", log_path.display()))?;
        let stderr_file = log_file
            .try_clone()
            .context("cloning log file handle for stderr")?;

        let mut child = Command::new("sh")
            .arg("-c")
            .arg(&cfg.cmd)
            .current_dir(&app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(stderr_file))
            .spawn()
            .with_context(|| format!("spawning `{}`", cfg.cmd))?;

        let pid = child
            .id()
            .ok_or_else(|| anyhow!("spawned child has no pid"))?;

        inner.apps.insert(
            name.clone(),
            AppRuntime {
                state: AppState::Running { pid },
            },
        );
        // Detach the wait task so the supervisor can observe exit/crash without holding the lock.
        let inner_handle = self.inner.clone();
        let watch_name = name.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            let mut guard = inner_handle.lock().await;
            if let Some(rt) = guard.apps.get_mut(&watch_name) {
                match status {
                    Ok(s) => {
                        let code = s.code().unwrap_or_else(|| {
                            // signal-terminated processes have no exit code; surface 128+signal style
                            #[cfg(unix)]
                            {
                                use std::os::unix::process::ExitStatusExt;
                                s.signal().map(|sig| 128 + sig).unwrap_or(-1)
                            }
                            #[cfg(not(unix))]
                            {
                                -1
                            }
                        });
                        rt.state = if code == 0 {
                            AppState::Stopped
                        } else {
                            AppState::Crashed { exit_code: code }
                        };
                    }
                    Err(_) => {
                        rt.state = AppState::Crashed { exit_code: -1 };
                    }
                }
            }
        });

        Ok(pid)
    }

    pub async fn down(&self, name: &str) -> Result<()> {
        let pid = {
            let inner = self.inner.lock().await;
            match inner.apps.get(name).map(|rt| rt.state.clone()) {
                Some(AppState::Running { pid }) => pid,
                _ => return Err(anyhow!("app `{}` is not running", name)),
            }
        };

        let nix_pid = Pid::from_raw(pid as i32);
        let _ = kill(nix_pid, Signal::SIGTERM);

        let deadline = tokio::time::Instant::now() + TERM_GRACE;
        loop {
            sleep(Duration::from_millis(100)).await;
            let inner = self.inner.lock().await;
            match inner.apps.get(name).map(|rt| rt.state.clone()) {
                Some(AppState::Running { .. }) => {}
                _ => return Ok(()),
            }
            drop(inner);
            if tokio::time::Instant::now() >= deadline {
                break;
            }
        }
        let _ = kill(nix_pid, Signal::SIGKILL);
        // wait briefly for the watcher to flip state
        for _ in 0..50 {
            sleep(Duration::from_millis(100)).await;
            let inner = self.inner.lock().await;
            match inner.apps.get(name).map(|rt| rt.state.clone()) {
                Some(AppState::Running { .. }) => {}
                _ => return Ok(()),
            }
        }
        Err(anyhow!(
            "failed to terminate `{}` within grace window",
            name
        ))
    }
}
