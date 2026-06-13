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
use crate::tls;

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

    // Best-effort cleanup of the socket on Ctrl-C so subsequent boots aren't blocked.
    let socket_for_signal = socket.clone();
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            let _ = std::fs::remove_file(&socket_for_signal);
            std::process::exit(0);
        }
    });

    // Reverse proxy runs in the same process as the control-plane listener; failures here are
    // logged but don't kill the daemon — the control plane is still useful without the proxy.
    let proxy_supervisor = supervisor.clone();
    tokio::spawn(async move {
        if let Err(err) = proxy::run(proxy_supervisor).await {
            tracing::error!("proxy listener exited: {err}");
        }
    });

    // HTTPS listener and SAN re-issue share one resolver, published through a OnceLock the
    // dispatch path reads. The resolver is built *inside* the spawned task because
    // `LeafResolver::new()` hits the keychain, and keychain access can stall on a macOS
    // "allow access" prompt (unsigned cargo-built binary — the hash changes every rebuild).
    // Built synchronously here, a stalled prompt would hang the control socket and proxy too;
    // built in the task, only HTTPS waits. Construction failure means "HTTPS not opted in"
    // (no CA): skip the listener, and registry changes skip the leaf re-issue.
    //
    // Accepted race: an `add` landing between `LeafResolver::new()`'s registry read and the
    // `set` below finds the cell empty and skips its reload, so the leaf stays stale until
    // the next registry change. The window is one keychain roundtrip wide and self-corrects —
    // not worth serializing daemon startup against the dispatch path.
    let resolver: Arc<std::sync::OnceLock<Arc<tls::LeafResolver>>> =
        Arc::new(std::sync::OnceLock::new());
    {
        let cell = resolver.clone();
        let https_supervisor = supervisor.clone();
        tokio::spawn(async move {
            match tls::LeafResolver::new() {
                Ok(r) => {
                    let _ = cell.set(r.clone());
                    if let Err(err) = proxy::run_https(https_supervisor, r).await {
                        tracing::error!("https listener exited: {err}");
                    }
                }
                Err(err) => tracing::error!("https listener disabled: {err}"),
            }
        });
    }

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
        let resolver = resolver.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, sup, reg_lock, resolver).await {
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
    resolver: Arc<std::sync::OnceLock<Arc<tls::LeafResolver>>>,
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

    let response = match dispatch(req, supervisor, registry_lock, resolver).await {
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
    resolver: Arc<std::sync::OnceLock<Arc<tls::LeafResolver>>>,
) -> Result<Response> {
    match req {
        Request::Ping => Ok(Response::Ok),
        Request::Add { path, label } => add(path, label, registry_lock, resolver).await,
        Request::List => list(supervisor).await,
        Request::Up { name } => up(name, supervisor).await,
        Request::Down { name } => down(name, supervisor).await,
        Request::Restart { name } => restart(name, supervisor).await,
        Request::Status { name } => status(name, supervisor).await,
        Request::LogPath { name } => log_path(name).await,
        Request::WaitReady { name, timeout_secs } => wait_ready(name, timeout_secs, supervisor).await,
        Request::Remove { name } => remove(name, supervisor, registry_lock, resolver).await,
        Request::Prune => prune(supervisor, registry_lock, resolver).await,
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

/// Names that point at daemon-served subdomains and cannot be claimed by a user app.
/// Keep this list short — every entry takes a `<name>.adj.ac` namespace away from real apps.
/// `__adj_verify__` is the doctor probe target; reserving it stops a user app from shadowing
/// the marker handler (the handler also wins at request time, but defense-in-depth keeps the
/// error story coherent — registering it would never actually take effect).
const RESERVED_NAMES: &[&str] = &["status", "__adj_verify__"];

async fn add(
    path: String,
    label: Option<String>,
    registry_lock: Arc<Mutex<()>>,
    resolver: Arc<std::sync::OnceLock<Arc<tls::LeafResolver>>>,
) -> Result<Response> {
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
    if RESERVED_NAMES.contains(&cfg.name.as_str()) {
        return Err(anyhow!(
            "`{}` is a reserved name (claimed by the daemon for built-in routes like the status dashboard and the doctor probe) — rename the app in adjacent.toml",
            cfg.name
        ));
    }
    // The client derives labels (from `--label` or the git branch), but the daemon owns
    // validation: the label becomes a DNS label in `<label>.<name>.adj.ac` and a path
    // component of the log file, so the charset is restricted at the trust boundary.
    let key = match &label {
        Some(label) => {
            validate_label(label)?;
            if RESERVED_NAMES.contains(&label.as_str()) {
                return Err(anyhow!("`{label}` is a reserved name — pick another label"));
            }
            format!("{label}.{}", cfg.name)
        }
        None => cfg.name.clone(),
    };
    // Serialize add operations so two concurrent calls can't both pass uniqueness and race on save.
    let _guard = registry_lock.lock().await;
    let mut reg = Registry::load()?;
    if reg.get(&key).is_some() {
        return Err(if label.is_some() {
            anyhow!("an app named `{key}` is already registered — use a different `--label`")
        } else {
            anyhow!(
                "an app named `{key}` is already registered (use `--label` to register another instance)"
            )
        });
    }
    reg.insert(
        key.clone(),
        registry::AppEntry {
            path: canon.clone(),
        },
    );
    reg.save()?;
    // Best-effort: a failed re-issue means HTTPS serves the previous SAN set until the next
    // registry change; the registry mutation itself already succeeded.
    if let Some(r) = resolver.get() {
        if let Err(err) = r.reload() {
            tracing::warn!("leaf cert re-issue after registry change failed: {err}");
        }
    }
    Ok(Response::Added {
        name: key,
        path: canon.display().to_string(),
    })
}

fn validate_label(label: &str) -> Result<()> {
    // A worktree label has the same DNS-label constraints as an app name (both become labels in
    // `<label>.<name>.adj.ac` and SANs in the TLS leaf). `sanitize_label` on the client side
    // trims and truncates, so this only rejects hand-supplied `--label` values that violate the
    // constraints directly.
    registry::validate_dns_label("label", label)
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
            stale: !entry.path.exists(),
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
    // Give the same curated error the proxy gives rather than letting read_app_config emit a
    // confusing "no adjacent.toml found" when the worktree or folder has been deleted.
    if !entry.path.exists() {
        return Err(anyhow!(
            "registered path {} no longer exists — run `adj prune`",
            entry.path.display()
        ));
    }
    let cfg = registry::read_app_config(&entry.path)?;
    // An instance key is `<label>.<cfg.name>`; only the base must match the manifest. A full
    // equality check here would refuse to boot every registered worktree instance.
    if registry::base_name(&name) != cfg.name {
        return Err(anyhow!(
            "adjacent.toml at {} declares name `{}`, which does not match `{}`",
            entry.path.display(),
            cfg.name,
            name
        ));
    }
    supervisor.up(&name, entry.path, cfg).await?;
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
    // Give the same curated error the proxy gives rather than letting read_app_config emit a
    // confusing "no adjacent.toml found" when the worktree or folder has been deleted.
    if !entry.path.exists() {
        return Err(anyhow!(
            "registered path {} no longer exists — run `adj prune`",
            entry.path.display()
        ));
    }
    let cfg = registry::read_app_config(&entry.path)?;
    // An instance key is `<label>.<cfg.name>`; only the base must match the manifest. A full
    // equality check here would refuse to restart every registered worktree instance.
    if registry::base_name(&name) != cfg.name {
        return Err(anyhow!(
            "adjacent.toml at {} declares name `{}`, which does not match `{}`",
            entry.path.display(),
            cfg.name,
            name
        ));
    }
    supervisor.up(&name, entry.path, cfg).await?;
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

async fn remove(
    name: String,
    supervisor: Arc<Supervisor>,
    registry_lock: Arc<Mutex<()>>,
    resolver: Arc<std::sync::OnceLock<Arc<tls::LeafResolver>>>,
) -> Result<Response> {
    let _guard = registry_lock.lock().await;
    let mut reg = Registry::load()?;
    if reg.get(&name).is_none() {
        return Err(anyhow!("no app named `{}`", name));
    }
    // Stop before deregistering so removal can't leave an orphan process running against an
    // entry that no longer exists.
    if matches!(
        supervisor.state(&name).await,
        adj_protocol::AppState::Running { .. }
    ) {
        supervisor.down(&name).await?;
    }
    // Clear the supervisor's AppRuntime so a subsequent `adj add` + boot sees a clean Stopped
    // slate rather than the last run's state (e.g. Crashed from a previous life of the app).
    //
    // Known race: the proxy's `ensure_running` reads the registry *without* the registry lock,
    // so a request racing `remove` can lazy-boot the app back between our `down` and the
    // `reg.save()` below. `forget` refuses to drop a Running entry for exactly this reason —
    // the resurrected process stays in the supervisor map where the idle scanner can reap it,
    // and `adj down <name>` still works because `down` operates on supervisor state, not the
    // registry. Accepted for a local dev tool over giving the proxy a registry-lock dependency.
    supervisor.forget(&name).await;
    reg.remove(&name);
    reg.save()?;
    // Best-effort: a failed re-issue means HTTPS serves the previous SAN set until the next
    // registry change; the registry mutation itself already succeeded.
    if let Some(r) = resolver.get() {
        if let Err(err) = r.reload() {
            tracing::warn!("leaf cert re-issue after registry change failed: {err}");
        }
    }
    Ok(Response::Removed { name })
}

async fn prune(
    supervisor: Arc<Supervisor>,
    registry_lock: Arc<Mutex<()>>,
    resolver: Arc<std::sync::OnceLock<Arc<tls::LeafResolver>>>,
) -> Result<Response> {
    let _guard = registry_lock.lock().await;
    let mut reg = Registry::load()?;
    let stale: Vec<String> = reg
        .apps
        .iter()
        .filter(|(_, entry)| !entry.path.exists())
        .map(|(name, _)| name.clone())
        .collect();
    for name in &stale {
        // A process can outlive its deleted cwd on unix, so a stale entry may still be
        // running. Best-effort stop — a failure shouldn't block deregistering the corpse.
        if matches!(
            supervisor.state(name).await,
            adj_protocol::AppState::Running { .. }
        ) {
            if let Err(err) = supervisor.down(name).await {
                tracing::warn!("stopping stale `{name}` during prune failed: {err}");
            }
        }
        // Same ghost-state cleanup as remove: drop the AppRuntime so re-adding the same path
        // later doesn't inherit the old run's state. The same resurrection race documented on
        // `remove` applies here; the idle scanner is the backstop.
        supervisor.forget(name).await;
        reg.remove(name);
    }
    if !stale.is_empty() {
        reg.save()?;
        // Best-effort: a failed re-issue means HTTPS serves the previous SAN set until the next
        // registry change; the registry mutation itself already succeeded.
        if let Some(r) = resolver.get() {
            if let Err(err) = r.reload() {
                tracing::warn!("leaf cert re-issue after registry change failed: {err}");
            }
        }
    }
    Ok(Response::Pruned { removed: stale })
}
