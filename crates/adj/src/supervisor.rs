use std::collections::{HashMap, HashSet};
use std::io::Write;
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use adj_protocol::AppState;
use anyhow::{anyhow, Context, Result};
use nix::sys::signal::{kill, Signal};
use nix::unistd::Pid;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{ChildStderr, ChildStdout, Command};
use tokio::sync::{mpsc, Mutex};
use tokio::time::sleep;

use crate::paths;
use crate::registry::AppConfig;

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
        let max_size = cfg.log_max_size_bytes()?;
        let max_files = cfg.log_max_files_value();
        let mut inner = self.inner.lock().await;
        if let Some(rt) = inner.apps.get(&name) {
            if matches!(rt.state, AppState::Running { .. }) {
                return Err(anyhow!("app `{}` is already running", name));
            }
        }

        // Defer reserving the port until after all the fallible-but-fast pre-spawn work
        // succeeds. The supervisor mutex serializes alloc+spawn, so nothing else can claim the
        // same port between `allocate_free_port` and the eventual `insert` below — this avoids
        // leaking a reservation if log-file setup fails.
        let port = allocate_free_port(&inner.reserved_ports)?;

        let log_path = paths::log_path(&name)?;
        paths::ensure_dirs()?;

        let port_env = cfg.port_env.as_deref().unwrap_or("PORT");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&cfg.cmd)
            .current_dir(&app_dir)
            .env(port_env, port.to_string())
            .stdin(Stdio::null())
            // Pipe stdout/stderr through this process so the writer task can rotate the
            // file when it crosses `log_max_size`. The previous implementation handed raw
            // OS handles to the child (Stdio::from(file)), which made the kernel append
            // directly — fast, but rotation would have required closing the child's
            // handles, which we can't reach into.
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
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
        spawn_log_writer(log_path.clone(), max_size, max_files, stdout, stderr)?;

        inner.apps.insert(
            name.clone(),
            AppRuntime {
                state: AppState::Running { pid, port },
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
            // Release the port reservation regardless of how the process ended; the child has
            // exited so the kernel will not hand the same port to anything still bound here.
            guard.reserved_ports.remove(&port);
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
                AppState::Running { pid, .. } => pid,
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

// One line of log output, plus a tag for diagnostics. We funnel both child streams through
// a single mpsc channel so the writer task gets a serialized stream — interleaving is "in
// the order the pipes flushed", which matches what the previous shared file handle gave us.
struct LogLine(Vec<u8>);

// Plumbs the child's stdout/stderr to a writer task that appends to <name>.log and rotates
// when the file crosses `max_size`. Rotation closes the active handle, shifts
// <name>.log.{N-1} ... <name>.log.1 down by one, renames <name>.log to <name>.log.1, prunes
// anything past `max_files`, and re-opens a fresh <name>.log. The path remains stable for
// `adj logs --tail`, but the inode changes — the tail-side code re-opens when it notices.
fn spawn_log_writer(
    log_path: PathBuf,
    max_size: u64,
    max_files: usize,
    stdout: Option<ChildStdout>,
    stderr: Option<ChildStderr>,
) -> Result<()> {
    // Bounded channel keeps memory bounded if the writer task falls behind the child.
    let (tx, rx) = mpsc::channel::<LogLine>(1024);

    if let Some(out) = stdout {
        spawn_pipe_pump(out, tx.clone());
    }
    if let Some(err) = stderr {
        spawn_pipe_pump(err, tx.clone());
    }
    drop(tx);

    // Open synchronously here (rather than inside the task) so spawn failures surface
    // through the supervisor's existing error path.
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .with_context(|| format!("opening log file {}", log_path.display()))?;
    let size = file
        .metadata()
        .map(|m| m.len())
        .unwrap_or(0);

    tokio::spawn(writer_loop(log_path, max_size, max_files, file, size, rx));
    Ok(())
}

fn spawn_pipe_pump<R>(reader: R, tx: mpsc::Sender<LogLine>)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut buf = BufReader::new(reader);
        let mut line: Vec<u8> = Vec::with_capacity(256);
        loop {
            line.clear();
            match buf.read_until(b'\n', &mut line).await {
                Ok(0) => break,
                Ok(_) => {
                    if tx.send(LogLine(line.clone())).await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });
}

async fn writer_loop(
    log_path: PathBuf,
    max_size: u64,
    max_files: usize,
    mut file: std::fs::File,
    mut size: u64,
    mut rx: mpsc::Receiver<LogLine>,
) {
    while let Some(LogLine(bytes)) = rx.recv().await {
        // Rotate *before* the write if appending this chunk would push us past the cap.
        // The check uses `>` so a chunk that exactly fills the cap is the last one in the
        // active file — clearer than letting size momentarily exceed max_size and then
        // rotating on the next iteration.
        if max_size > 0 && size + bytes.len() as u64 > max_size {
            match rotate(&log_path, max_files) {
                Ok(new_file) => {
                    file = new_file;
                    size = 0;
                }
                Err(err) => {
                    tracing::warn!("log rotation failed for {}: {err}", log_path.display());
                    // Fall through and keep writing to the existing file rather than
                    // dropping logs on the floor.
                }
            }
        }
        if let Err(err) = file.write_all(&bytes) {
            tracing::warn!("log write failed for {}: {err}", log_path.display());
            return;
        }
        size += bytes.len() as u64;
    }
    // Best-effort flush before exit; the file is closed when `file` drops.
    let _ = file.flush();
}

fn rotate(log_path: &Path, max_files: usize) -> Result<std::fs::File> {
    // Shift .{max_files-1} -> .{max_files}, ... , .1 -> .2. Anything ending up past
    // max_files is removed below. If max_files is 0 we treat the active log as the only
    // file we ever keep — rotation just truncates rather than archiving.
    if max_files == 0 {
        let f = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(log_path)
            .with_context(|| format!("truncating {}", log_path.display()))?;
        return Ok(f);
    }

    // Walk from highest to lowest so each rename has a free slot at i+1.
    for i in (1..max_files).rev() {
        let from = rotated_path(log_path, i);
        let to = rotated_path(log_path, i + 1);
        if from.exists() {
            std::fs::rename(&from, &to)
                .with_context(|| format!("rotating {} -> {}", from.display(), to.display()))?;
        }
    }
    let first = rotated_path(log_path, 1);
    if log_path.exists() {
        std::fs::rename(log_path, &first)
            .with_context(|| format!("rotating {} -> {}", log_path.display(), first.display()))?;
    }

    // Prune anything past the cap — including remnants from a previous run where
    // max_files was larger.
    let mut overflow = max_files + 1;
    loop {
        let p = rotated_path(log_path, overflow);
        if !p.exists() {
            break;
        }
        if let Err(err) = std::fs::remove_file(&p) {
            tracing::warn!("pruning {} failed: {err}", p.display());
            break;
        }
        overflow += 1;
    }

    let f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log_path)
        .with_context(|| format!("reopening {} after rotation", log_path.display()))?;
    Ok(f)
}

fn rotated_path(log_path: &Path, n: usize) -> PathBuf {
    let mut s = log_path.as_os_str().to_owned();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}
