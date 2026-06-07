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
    // Set when the daemon initiates termination (down, restart's down half). On exit, this
    // means the process should be recorded as Stopped rather than Crashed even if the shell
    // propagates the signal as a non-zero exit code.
    intentional_stop: bool,
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

        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&cfg.cmd)
            .current_dir(&app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::from(log_file))
            .stderr(Stdio::from(stderr_file));
        // Put the shell and everything it spawns in its own process group so we can signal
        // the whole tree on stop. Without this, SIGTERM/SIGKILL hit only `sh` and the real
        // long-running command (e.g. a dev server) is reparented to init and keeps the port.
        command.process_group(0);
        let mut child = command
            .spawn()
            .with_context(|| format!("spawning `{}`", cfg.cmd))?;

        let pid = child
            .id()
            .ok_or_else(|| anyhow!("spawned child has no pid"))?;

        inner.apps.insert(
            name.clone(),
            AppRuntime {
                state: AppState::Running { pid },
                intentional_stop: false,
            },
        );
        // Detach the wait task so the supervisor can observe exit/crash without holding the lock.
        let inner_handle = self.inner.clone();
        let watch_name = name.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            let mut guard = inner_handle.lock().await;
            if let Some(rt) = guard.apps.get_mut(&watch_name) {
                let intentional = rt.intentional_stop;
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
                        rt.state = if intentional || code == 0 {
                            AppState::Stopped
                        } else {
                            AppState::Crashed { exit_code: code }
                        };
                    }
                    Err(_) => {
                        rt.state = if intentional {
                            AppState::Stopped
                        } else {
                            AppState::Crashed { exit_code: -1 }
                        };
                    }
                }
                // Reset the flag so a subsequent crash (no `down`) reports Crashed correctly.
                rt.intentional_stop = false;
            }
        });

        Ok(pid)
    }

    pub async fn down(&self, name: &str) -> Result<()> {
        let pid = {
            let mut inner = self.inner.lock().await;
            let rt = inner
                .apps
                .get_mut(name)
                .ok_or_else(|| anyhow!("app `{}` is not running", name))?;
            let pid = match rt.state {
                AppState::Running { pid } => pid,
                _ => return Err(anyhow!("app `{}` is not running", name)),
            };
            // Flag the upcoming exit as intentional so the wait task records Stopped, not Crashed.
            rt.intentional_stop = true;
            pid
        };

        // Signal the whole process group (negative PID). The supervised PID is the shell's PID
        // which we made the process-group leader via .process_group(0), so -pid reaches every
        // descendant including the real dev server.
        let pgid = Pid::from_raw(-(pid as i32));
        let _ = kill(pgid, Signal::SIGTERM);

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
        let _ = kill(pgid, Signal::SIGKILL);
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
