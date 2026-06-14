use std::collections::{HashMap, HashSet};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use adj_protocol::{AppState, LogRecord, LogStream};
use anyhow::{anyhow, Context, Result};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::time::sleep;

use crate::env::load_env_file;
use crate::paths;
use crate::registry::{idle_timeout_for, AppConfig};

const TERM_GRACE: Duration = Duration::from_secs(5);
const PORT_ALLOC_ATTEMPTS: usize = 32;

#[derive(Default)]
pub struct Supervisor {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    apps: HashMap<String, AppRuntime>,
    // Ports we've handed to a supervised child that may not yet have bound them. We hold the
    // supervisor mutex around alloc + spawn, but the kernel can re-issue a just-freed port
    // before the child binds. Tracking reserved ports lets us retry the :0 probe and avoid
    // collisions between Adjacent-supervised processes.
    reserved_ports: HashSet<u16>,
}

struct AppRuntime {
    state: AppState,
    // Set when the daemon initiates termination (down, restart's down half, idle scanner). On
    // exit, this means the process should be recorded as Stopped rather than Crashed even if
    // the shell propagates the signal as a non-zero exit code.
    intentional_stop: bool,
    // Last time a proxied request was routed to this app, used by the idle scanner. Seeded at
    // boot so a freshly-booted app gets its full idle window before being a stop candidate.
    last_request: Instant,
    // Idle window for this app. `None` means idle shutdown is disabled. Captured at boot time
    // from the resolved config; we don't re-read adjacent.toml during the scan loop.
    idle_timeout: Option<Duration>,
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

    pub async fn up(&self, name: &str, app_dir: PathBuf, cfg: AppConfig) -> Result<u32> {
        let name = name.to_string();
        let mut inner = self.inner.lock().await;
        if let Some(rt) = inner.apps.get(&name) {
            if matches!(rt.state, AppState::Running { .. }) {
                return Err(anyhow!("app `{}` is already running", name));
            }
        }

        // Resolve env layers before any port allocation so a missing `env_file` or unreadable
        // file fails fast with a clear error (and doesn't leak a port reservation).
        let env_file_values = if let Some(rel) = cfg.env_file.as_deref() {
            let resolved = app_dir.join(rel);
            Some(load_env_file(&resolved)?)
        } else {
            None
        };

        // Defer reserving the port until after all the fallible-but-fast pre-spawn work
        // succeeds. The supervisor mutex serializes alloc+spawn, so nothing else can claim the
        // same port between `allocate_free_port` and the eventual `insert` below — this avoids
        // leaking a reservation if log-file setup fails.
        let port = allocate_free_port(&inner.reserved_ports)?;

        let log_path = paths::log_path(&name)?;
        paths::ensure_dirs()?;
        // We pipe stdout/stderr and re-emit each line as a JSONL record so `adj logs --json`
        // can recover per-line stream tags and timestamps. Plain-text `adj logs` just projects
        // the `line` field. Two file handles are not needed — the writer task owns the file.

        let port_env = cfg.port_env.as_deref().unwrap_or("PORT");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&cfg.cmd)
            .current_dir(&app_dir)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Layer env in the order documented in adjacent.toml: env_file values first, then the
        // committed `[env]` table overrides them, then PORT injection wins over both. The child
        // also inherits the daemon's env by default; explicit `.env(k, v)` calls override per-key.
        if let Some(values) = &env_file_values {
            for (k, v) in values {
                command.env(k, v);
            }
        }
        if let Some(values) = &cfg.env {
            for (k, v) in values {
                command.env(k, v);
            }
        }
        command.env(port_env, port.to_string());
        // Put the shell and everything it spawns in its own process group so we can signal
        // the whole tree on stop. Without this, SIGTERM/SIGKILL hit only `sh` and the real
        // long-running command (e.g. a dev server) is reparented to init and keeps the port.
        command.process_group(0);

        // Reserve the port immediately before spawn. Past this point, every error path must
        // release the reservation so it doesn't outlive the would-be child.
        inner.reserved_ports.insert(port);

        let mut child = match command.spawn() {
            Ok(c) => c,
            Err(err) => {
                inner.reserved_ports.remove(&port);
                return Err(anyhow::Error::from(err).context(format!("spawning `{}`", cfg.cmd)));
            }
        };

        let pid = match child.id() {
            Some(p) => p,
            None => {
                inner.reserved_ports.remove(&port);
                return Err(anyhow!("spawned child has no pid"));
            }
        };

        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        // Spawn the log-writer task before we record state so the file exists by the time the
        // first byte is read — `--tail` on a brand-new app would otherwise race the file open.
        let log_writer = LogWriter::open(&log_path).await.with_context(|| {
            format!("opening log file {} for tagged writing", log_path.display())
        })?;
        let writer_handle = log_writer.handle();
        spawn_log_reader(stdout, LogStream::Stdout, writer_handle.clone());
        spawn_log_reader(stderr, LogStream::Stderr, writer_handle.clone());

        let started_at = OffsetDateTime::now_utc()
            .format(&Rfc3339)
            .unwrap_or_else(|_| String::new());

        let idle_timeout = idle_timeout_for(&cfg);
        inner.apps.insert(
            name.clone(),
            AppRuntime {
                state: AppState::Running {
                    pid,
                    port,
                    started_at: if started_at.is_empty() {
                        None
                    } else {
                        Some(started_at)
                    },
                },
                intentional_stop: false,
                last_request: Instant::now(),
                idle_timeout,
            },
        );
        // Detach the wait task so the supervisor can observe exit/crash without holding the lock.
        let inner_handle = self.inner.clone();
        let watch_name = name.clone();
        tokio::spawn(async move {
            let status = child.wait().await;
            // Drop the writer once the child is gone so the JSONL file flushes cleanly. Reader
            // tasks finish on EOF; this handle goes out of scope here.
            drop(writer_handle);
            drop(log_writer);
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
            // Release the port reservation regardless of how the process ended; the child has
            // exited so the kernel will not hand the same port to anything still bound here.
            guard.reserved_ports.remove(&port);
        });

        Ok(pid)
    }

    pub async fn down(&self, name: &str) -> Result<()> {
        let pid = self.begin_intentional_stop(name, None).await?;
        let pid = pid.ok_or_else(|| anyhow!("app `{}` is not running", name))?;
        self.finish_stop(name, pid).await
    }

    /// Like `down`, but re-checks `last_request` under the supervisor lock before flipping
    /// `intentional_stop`. If a proxied request arrived after the scanner snapshot — i.e. the
    /// idle window is no longer satisfied — returns `Ok(false)` and leaves the app running.
    /// Otherwise terminates the app the same way `down` does and returns `Ok(true)`.
    ///
    /// Closes the scanner-vs-proxy race: the scanner snapshots candidates, releases the lock,
    /// then calls into here per-name. Between snapshot and SIGTERM a request can arrive, get
    /// routed to a still-Running app, and then be cut off mid-forward when the scanner kills
    /// the process. Re-checking the timestamp under the lock here means that interleaving
    /// always either (a) stops the app before the request is routed, or (b) skips the stop.
    pub async fn down_if_idle(&self, name: &str, window: Duration) -> Result<bool> {
        let Some(pid) = self.begin_intentional_stop(name, Some(window)).await? else {
            return Ok(false);
        };
        self.finish_stop(name, pid).await?;
        Ok(true)
    }

    /// Mark an app as intentionally stopping and return its pid. If `min_idle` is `Some(d)`,
    /// the call is a no-op (returns `Ok(None)`) when the app's `last_request` is more recent
    /// than `d` — the request-vs-scanner race guard. `Ok(None)` is also returned when the app
    /// is not currently running and `min_idle` is set (caller treats this as "skip this tick"),
    /// or as an error to surface to the user when `min_idle` is `None` (i.e. an explicit
    /// `adj down`).
    async fn begin_intentional_stop(
        &self,
        name: &str,
        min_idle: Option<Duration>,
    ) -> Result<Option<u32>> {
        let mut inner = self.inner.lock().await;
        let Some(rt) = inner.apps.get_mut(name) else {
            // For idle-scanner callers a missing entry just means "skip"; the explicit `down`
            // path turns `Ok(None)` back into an error above.
            if min_idle.is_some() {
                return Ok(None);
            }
            return Err(anyhow!("app `{}` is not running", name));
        };
        let pid = match rt.state {
            AppState::Running { pid, .. } => pid,
            _ => {
                if min_idle.is_some() {
                    return Ok(None);
                }
                return Err(anyhow!("app `{}` is not running", name));
            }
        };
        if let Some(window) = min_idle {
            if rt.last_request.elapsed() < window {
                return Ok(None);
            }
        }
        // Flag the upcoming exit as intentional so the wait task records Stopped, not Crashed.
        rt.intentional_stop = true;
        Ok(Some(pid))
    }

    /// SIGTERM the process group, wait for the grace window, escalate to SIGKILL if needed.
    /// Shared tail of `down` and `down_if_idle`.
    async fn finish_stop(&self, name: &str, pid: u32) -> Result<()> {
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

    /// Drop the runtime entry for a deregistered app so a future re-registration starts from
    /// a clean `Stopped` slate. Refuses to forget a Running entry — the proxy's lazy boot can
    /// resurrect an app between `down` and the registry save (it reads the registry without
    /// the registry lock), and dropping a live entry would orphan that process beyond the
    /// idle scanner's reach. Returns whether the entry is gone (true if removed or never present).
    ///
    /// Callers don't need to act on the bool for correctness: if a Running entry survives here
    /// because of the resurrection race, the idle scanner's next sweep reaps it once its registry
    /// row is gone — regardless of the app's idle_timeout (`"off"` included), since the scanner
    /// treats any running-but-unregistered app as an orphan. `adj down <name>` also still works
    /// because `down` never consults the registry directly — it operates purely on supervisor
    /// state.
    ///
    /// Port reservations are not leaked: the wait task that observes process exit already
    /// calls `reserved_ports.remove(&port)` unconditionally, so by the time a non-Running
    /// entry exists the port slot is already free.
    pub async fn forget(&self, name: &str) -> bool {
        let mut inner = self.inner.lock().await;
        match inner.apps.get(name) {
            Some(rt) if matches!(rt.state, AppState::Running { .. }) => false,
            Some(_) => {
                inner.apps.remove(name);
                true
            }
            None => true,
        }
    }

    /// Stamp `name`'s last-request timestamp. Called by the proxy on every routed request so
    /// the idle scanner can tell which apps are quiet. A no-op for apps not currently running.
    pub async fn touch_idle(&self, name: &str) {
        let mut inner = self.inner.lock().await;
        if let Some(rt) = inner.apps.get_mut(name) {
            if matches!(rt.state, AppState::Running { .. }) {
                rt.last_request = Instant::now();
            }
        }
    }

    /// Snapshot every running app's idle status. Returned tuples are `(name, idle_for)` —
    /// callers compare against the app's configured `idle_timeout` to decide whether to stop.
    pub async fn idle_candidates(&self) -> Vec<(String, Duration, Option<Duration>)> {
        let inner = self.inner.lock().await;
        let now = Instant::now();
        inner
            .apps
            .iter()
            .filter_map(|(name, rt)| {
                if !matches!(rt.state, AppState::Running { .. }) {
                    return None;
                }
                Some((
                    name.clone(),
                    now.saturating_duration_since(rt.last_request),
                    rt.idle_timeout,
                ))
            })
            .collect()
    }
}

#[cfg(test)]
impl Supervisor {
    /// Insert a fake `Running` runtime entry for unit tests. Lets us exercise the lock-side
    /// behavior of `down_if_idle` without spawning a real process.
    async fn insert_fake_running(&self, name: &str, last_request: Instant) {
        let mut inner = self.inner.lock().await;
        inner.apps.insert(
            name.to_string(),
            AppRuntime {
                state: AppState::Running {
                    pid: 1,
                    port: 1,
                    started_at: None,
                },
                intentional_stop: false,
                last_request,
                idle_timeout: None,
            },
        );
    }

    async fn intentional_stop_flag(&self, name: &str) -> Option<bool> {
        let inner = self.inner.lock().await;
        inner.apps.get(name).map(|rt| rt.intentional_stop)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn down_if_idle_skips_when_request_arrived_after_snapshot() {
        // Scenario: the idle scanner observed `last_request` older than the window, then a
        // proxied request landed and bumped `last_request` to ~now. By the time the scanner
        // asks the supervisor to stop the app, the window is no longer satisfied. The guard
        // re-checks under the lock and returns Ok(false), leaving the app alone.
        let sup = Supervisor::new();
        sup.insert_fake_running("hot", Instant::now()).await;
        let result = sup
            .down_if_idle("hot", Duration::from_secs(30))
            .await
            .expect("call should not error");
        assert!(!result, "expected Ok(false) when last_request is recent");
        // No SIGTERM was sent because we returned before begin_intentional_stop flipped the
        // flag — verify intentional_stop is still false so the watcher would correctly mark
        // a real crash as Crashed.
        assert_eq!(sup.intentional_stop_flag("hot").await, Some(false));
        // And the state is still Running.
        assert!(matches!(sup.state("hot").await, AppState::Running { .. }));
    }

    #[tokio::test]
    async fn down_if_idle_skips_when_app_missing() {
        // Idle scanner snapshot can name an app that's since been removed; the guard treats
        // missing entries as "skip" rather than an error so the scanner loop stays quiet.
        let sup = Supervisor::new();
        let result = sup
            .down_if_idle("ghost", Duration::from_secs(0))
            .await
            .expect("missing entry should not error");
        assert!(!result);
    }
}

// LogWriter serializes appends to the JSONL log file from multiple reader tasks (one per
// stream). It owns a tokio mutex around the file handle; readers send fully-formed records.
struct LogWriter {
    file: Arc<Mutex<tokio::fs::File>>,
}

#[derive(Clone)]
struct LogWriterHandle {
    file: Arc<Mutex<tokio::fs::File>>,
}

impl LogWriter {
    async fn open(path: &Path) -> Result<Self> {
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .await?;
        Ok(Self {
            file: Arc::new(Mutex::new(file)),
        })
    }

    fn handle(&self) -> LogWriterHandle {
        LogWriterHandle {
            file: self.file.clone(),
        }
    }
}

impl LogWriterHandle {
    async fn write_record(&self, record: &LogRecord) -> Result<()> {
        let mut bytes = serde_json::to_vec(record)?;
        bytes.push(b'\n');
        let mut guard = self.file.lock().await;
        guard.write_all(&bytes).await?;
        // Flush each record so `--tail` sees lines promptly. This is a v1 trade — write
        // amplification is acceptable for the visibility win.
        guard.flush().await?;
        Ok(())
    }
}

fn spawn_log_reader<R>(reader: Option<R>, stream: LogStream, writer: LogWriterHandle)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let Some(reader) = reader else {
        return;
    };
    tokio::spawn(async move {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    // BufReader::read_line keeps the trailing newline; strip both LF and CRLF
                    // so the JSONL record's `line` field is the bare content.
                    let trimmed = line.trim_end_matches(['\r', '\n']);
                    let ts = OffsetDateTime::now_utc()
                        .format(&Rfc3339)
                        .unwrap_or_else(|_| String::new());
                    let record = LogRecord {
                        ts,
                        stream,
                        line: trimmed.to_string(),
                    };
                    if writer.write_record(&record).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

// Ask the kernel for a free TCP port by binding `:0` on the loopback, then close the listener
// and hand the number to the child. There's a small race window between close and the child
// binding — the kernel could in principle reissue the same port to a concurrent allocation.
// Two mitigations:
//   1. The supervisor mutex serializes alloc+spawn across `up` calls.
//   2. `reserved_ports` remembers ports still associated with a supervised child so a follow-up
//      :0 probe that happens to draw the same number retries.
fn allocate_free_port(reserved: &HashSet<u16>) -> Result<u16> {
    for _ in 0..PORT_ALLOC_ATTEMPTS {
        let listener = TcpListener::bind(("127.0.0.1", 0))
            .context("binding 127.0.0.1:0 to discover a free port")?;
        let port = listener
            .local_addr()
            .context("reading local_addr of probe listener")?
            .port();
        drop(listener);
        if !reserved.contains(&port) {
            return Ok(port);
        }
    }
    Err(anyhow!(
        "could not find a free port not already reserved by Adjacent after {} attempts",
        PORT_ALLOC_ATTEMPTS
    ))
}
