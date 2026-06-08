use std::path::PathBuf;
use std::sync::Arc;

use adj_protocol::{AppSummary, Request, Response};
use anyhow::{anyhow, Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;

use crate::paths;
use crate::proxy;
use crate::registry::{self, Registry};
use crate::supervisor::Supervisor;

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
