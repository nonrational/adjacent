use std::collections::{HashMap, HashSet};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

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
        // We pipe stdout/stderr and re-emit each line as a JSONL record so `adj logs --json`
        // can recover per-line stream tags and timestamps. Plain-text `adj logs` just projects
        // the `line` field. Two file handles are not needed — the writer task owns the file.

        let port_env = cfg.port_env.as_deref().unwrap_or("PORT");
        let mut command = Command::new("sh");
        command
            .arg("-c")
            .arg(&cfg.cmd)
            .current_dir(&app_dir)
            .env(port_env, port.to_string())
            .stdin(Stdio::null())
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
