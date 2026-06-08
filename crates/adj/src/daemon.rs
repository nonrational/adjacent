use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use adj_protocol::{AppSummary, Request, Response};
use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::paths;
use crate::proxy;
use crate::readiness::{wait_ready as readiness_wait, ReadinessError};
use crate::registry::{self, Registry};
use crate::supervisor::Supervisor;

/// How often the idle scanner looks at the supervised apps to decide whether any should be
/// stopped. The poll cost is tiny (one mutex lock per pass) so a short interval keeps the
/// observable shutdown latency reasonable even for small `idle_timeout` values.
const IDLE_SCAN_INTERVAL: Duration = Duration::from_millis(500);

pub async fn run() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    paths::ensure_dirs()?;
    let socket = paths::socket_path()?;
    if socket.exists() {
        if probe_existing_daemon(&socket).await {
            return Err(anyhow!(
                "another adj daemon is already listening at {}",
                socket.display()
            ));
        }
        let _ = std::fs::remove_file(&socket);
    }

    let listener = UnixListener::bind(&socket)
        .with_context(|| format!("binding socket at {}", socket.display()))?;
    tracing::info!("adj daemon listening at {}", socket.display());

    let supervisor = Arc::new(Supervisor::new());
    let registry_lock: Arc<Mutex<()>> = Arc::new(Mutex::new(()));

    // Best-effort cleanup of the socket on shutdown signals so subsequent boots aren't blocked.
    // SIGTERM matters specifically for `brew services stop` / launchd-driven shutdown; SIGINT
    // covers interactive Ctrl-C in the foreground. Either signal triggers the same cleanup.
    let socket_for_signal = socket.clone();
    tokio::spawn(async move {
        let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(s) => s,
            Err(err) => {
                tracing::warn!("failed to install SIGTERM handler: {err}");
                return;
            }
        };
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
        let _ = std::fs::remove_file(&socket_for_signal);
        std::process::exit(0);
    });

    // Reverse proxy runs in the same process as the control-plane listener; failures here are
    // logged but don't kill the daemon — the control plane is still useful without the proxy.
    let proxy_supervisor = supervisor.clone();
    tokio::spawn(async move {
        if let Err(err) = proxy::run(proxy_supervisor).await {
            tracing::error!("proxy listener exited: {err}");
        }
    });

    // Idle scanner: periodically stop apps whose last-routed-request is older than their
    // configured idle_timeout. Chose a scan loop over per-app timers — no per-app timer state
    // to manage, and the scan itself is one mutex acquisition every IDLE_SCAN_INTERVAL.
    let idle_supervisor = supervisor.clone();
    tokio::spawn(async move {
        idle_scanner(idle_supervisor).await;
    });

    loop {
        let (stream, _addr) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!("accept failed: {err}");
                continue;
            }
        };
        let sup = supervisor.clone();
        let reg_lock = registry_lock.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, sup, reg_lock).await {
                tracing::warn!("client handler error: {err}");
            }
        });
    }
}

async fn probe_existing_daemon(socket: &PathBuf) -> bool {
    match UnixStream::connect(socket).await {
        Ok(mut s) => {
            let req = Request::Ping;
            let bytes = serde_json::to_vec(&req).unwrap_or_default();
            if s.write_all(&bytes).await.is_err() {
                return false;
            }
            if s.write_all(b"\n").await.is_err() {
                return false;
            }
            // We don't care about the response; presence means another daemon is alive.
            true
        }
        Err(_) => false,
    }
}

async fn handle_client(
    stream: UnixStream,
    supervisor: Arc<Supervisor>,
    registry_lock: Arc<Mutex<()>>,
) -> Result<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    let n = reader.read_line(&mut line).await?;
    if n == 0 {
        return Ok(());
    }
    let req: Request = match serde_json::from_str(line.trim()) {
        Ok(r) => r,
        Err(err) => {
            let resp = Response::Error {
                message: format!("invalid request: {err}"),
            };
            send_response(&mut write_half, &resp).await?;
            return Ok(());
        }
    };

    let response = match dispatch(req, supervisor, registry_lock).await {
        Ok(r) => r,
        Err(err) => Response::Error {
            message: format!("{err}"),
        },
    };
    send_response(&mut write_half, &response).await?;
    Ok(())
}

async fn send_response(
    write_half: &mut tokio::net::unix::OwnedWriteHalf,
    resp: &Response,
) -> Result<()> {
    let mut bytes = serde_json::to_vec(resp)?;
    bytes.push(b'\n');
    write_half.write_all(&bytes).await?;
    write_half.shutdown().await.ok();
    Ok(())
}

async fn dispatch(
    req: Request,
    supervisor: Arc<Supervisor>,
    registry_lock: Arc<Mutex<()>>,
) -> Result<Response> {
    match req {
        Request::Ping => Ok(Response::Ok),
        Request::Add { path } => add(path, registry_lock).await,
        Request::List => list(supervisor).await,
        Request::Up { name } => up(name, supervisor).await,
        Request::Down { name } => down(name, supervisor).await,
        Request::Restart { name } => restart(name, supervisor).await,
        Request::Status { name } => status(name, supervisor).await,
        Request::LogPath { name } => log_path(name).await,
        Request::WaitReady { name, timeout_secs } => wait_ready(name, timeout_secs, supervisor).await,
    }
}

async fn wait_ready(
    name: String,
    timeout_secs: u64,
    supervisor: Arc<Supervisor>,
) -> Result<Response> {
    let reg = Registry::load()?;
    let entry = reg
        .get(&name)
        .ok_or_else(|| anyhow!("no app named `{}`", name))?
        .clone();
    let cfg = registry::read_app_config(&entry.path)?;
    // Fail fast if the app isn't running yet. Without this, the poll loop in `readiness::wait_ready`
    // sits on connection-refused until `boot_timeout` (60s default) and the user gets no signal
    // that they were supposed to `adj up` first.
    match supervisor.state(&name).await {
        adj_protocol::AppState::Running { .. } => {}
        adj_protocol::AppState::Stopped | adj_protocol::AppState::Crashed { .. } => {
            return Err(anyhow!(
                "app `{name}` is not running; run `adj up {name}` first"
            ));
        }
    }
    let timeout = if timeout_secs == 0 {
        proxy::boot_timeout_for(&cfg)
    } else {
        Duration::from_secs(timeout_secs)
    };
    let deadline = tokio::time::Instant::now() + timeout;
    match readiness_wait(&name, supervisor.as_ref(), &cfg, deadline).await {
        Ok(_) => Ok(Response::Ok),
        Err(ReadinessError::Timeout) => Err(anyhow!(
            "app `{name}` did not become ready within {timeout:?}"
        )),
        Err(ReadinessError::Crashed { exit_code }) => Err(anyhow!(
            "app `{name}` crashed while waiting for ready (exit {exit_code})"
        )),
    }
}

/// Periodic sweep: any app whose `last_request` is older than its `idle_timeout` gets stopped
/// the same way `adj down` would. Apps with idle_timeout disabled are skipped.
///
/// The scanner snapshots candidates, releases the supervisor lock, then asks the supervisor
/// to stop each one — but only after re-checking `last_request` under the lock. Without the
/// re-check, a proxied request can land between snapshot and SIGTERM, see `Running` in the
/// proxy's fast path, route to the about-to-be-killed process, and turn the shutdown into a
/// spurious 502. `down_if_idle` returns `Ok(false)` in that case and the scanner moves on.
async fn idle_scanner(supervisor: Arc<Supervisor>) {
    loop {
        tokio::time::sleep(IDLE_SCAN_INTERVAL).await;
        let candidates = supervisor.idle_candidates().await;
        for (name, idle_for, idle_timeout) in candidates {
            let Some(window) = idle_timeout else {
                continue;
            };
            if idle_for >= window {
                tracing::info!(
                    "stopping `{name}` after {idle_for:?} idle (threshold {window:?})"
                );
                match supervisor.down_if_idle(&name, window).await {
                    Ok(true) => {}
                    Ok(false) => {
                        tracing::debug!(
                            "skipped idle shutdown of `{name}` — request arrived after scan snapshot"
                        );
                    }
                    Err(err) => {
                        tracing::warn!("idle shutdown of `{name}` failed: {err}");
                    }
                }
            }
        }
    }
}

async fn add(path: String, registry_lock: Arc<Mutex<()>>) -> Result<Response> {
    // The client canonicalizes against the user's CWD before sending. We require absolute
    // paths here so we never silently resolve against the daemon's CWD.
    let candidate = PathBuf::from(&path);
    if !candidate.is_absolute() {
        return Err(anyhow!(
            "expected absolute path, got `{}` (client should canonicalize before send)",
            path
        ));
    }
    let canon = std::fs::canonicalize(&candidate)
        .with_context(|| format!("resolving path {}", path))?;
    let cfg = registry::read_app_config(&canon)?;
    // Serialize add operations so two concurrent calls can't both pass uniqueness and race on save.
    let _guard = registry_lock.lock().await;
    let mut reg = Registry::load()?;
    if reg.get(&cfg.name).is_some() {
        return Err(anyhow!("an app named `{}` is already registered", cfg.name));
    }
    reg.insert(
        cfg.name.clone(),
        registry::AppEntry {
            path: canon.clone(),
        },
    );
    reg.save()?;
    Ok(Response::Added {
        name: cfg.name,
        path: canon.display().to_string(),
    })
}

async fn list(supervisor: Arc<Supervisor>) -> Result<Response> {
    let reg = Registry::load()?;
    let mut entries = Vec::with_capacity(reg.apps.len());
    for (name, entry) in &reg.apps {
        let state = supervisor.state(name).await;
        entries.push(AppSummary {
            name: name.clone(),
            path: entry.path.display().to_string(),
            state,
        });
    }
    Ok(Response::List { entries })
}

async fn up(name: String, supervisor: Arc<Supervisor>) -> Result<Response> {
    let reg = Registry::load()?;
    let entry = reg
        .get(&name)
        .ok_or_else(|| anyhow!("no app named `{}`", name))?
        .clone();
    let cfg = registry::read_app_config(&entry.path)?;
    if cfg.name != name {
        return Err(anyhow!(
            "adjacent.toml at {} declares name `{}`, not `{}`",
            entry.path.display(),
            cfg.name,
            name
        ));
    }
    supervisor.up(entry.path, cfg).await?;
    Ok(Response::Ok)
}

async fn down(name: String, supervisor: Arc<Supervisor>) -> Result<Response> {
    supervisor.down(&name).await?;
    Ok(Response::Ok)
}

async fn restart(name: String, supervisor: Arc<Supervisor>) -> Result<Response> {
    let state = supervisor.state(&name).await;
    if matches!(state, adj_protocol::AppState::Running { .. }) {
        supervisor.down(&name).await?;
    }
    let reg = Registry::load()?;
    let entry = reg
        .get(&name)
        .ok_or_else(|| anyhow!("no app named `{}`", name))?
        .clone();
    let cfg = registry::read_app_config(&entry.path)?;
    supervisor.up(entry.path, cfg).await?;
    Ok(Response::Ok)
}

async fn status(name: String, supervisor: Arc<Supervisor>) -> Result<Response> {
    let reg = Registry::load()?;
    if reg.get(&name).is_none() {
        return Err(anyhow!("no app named `{}`", name));
    }
    let state = supervisor.state(&name).await;
    Ok(Response::Status { name, state })
}

async fn log_path(name: String) -> Result<Response> {
    let reg = Registry::load()?;
    if reg.get(&name).is_none() {
        return Err(anyhow!("no app named `{}`", name));
    }
    let path = paths::log_path(&name)?;
    Ok(Response::LogPath {
        path: path.display().to_string(),
    })
}
