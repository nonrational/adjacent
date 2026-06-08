use std::collections::HashMap;
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use adj_protocol::AppState;
use anyhow::{anyhow, Result};
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::client::conn::http1 as client_http1;
use hyper::server::conn::http1 as server_http1;
use hyper::service::service_fn;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

use crate::registry::{self, Registry};
use crate::supervisor::Supervisor;

const PROXY_PORT_ENV: &str = "ADJACENT_PROXY_PORT";
const HOST_SUFFIX: &str = ".adj.ac";
const DEFAULT_BOOT_TIMEOUT_SECS: u64 = 60;
const TCP_READY_POLL_INTERVAL: Duration = Duration::from_millis(50);

pub fn proxy_port() -> u16 {
    std::env::var(PROXY_PORT_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080)
}

/// Per-name boot lock map. Holding the inner `Mutex` while booting keeps the boot single-flight:
/// concurrent waiters acquire the mutex one at a time, but the first one finishes the boot so
/// every later acquirer sees the app already Running and returns immediately.
#[derive(Default)]
pub struct BootGate {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl BootGate {
    fn new() -> Self {
        Self::default()
    }

    async fn lock_for(&self, name: &str) -> Arc<Mutex<()>> {
        let mut map = self.locks.lock().await;
        map.entry(name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}

pub async fn run(supervisor: Arc<Supervisor>) -> Result<()> {
    let port = proxy_port();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow!("binding proxy listener at {addr}: {e}"))?;
    tracing::info!("adj proxy listening at http://{addr}");

    let gate = Arc::new(BootGate::new());

    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!("proxy accept failed: {err}");
                continue;
            }
        };
        let sup = supervisor.clone();
        let gate = gate.clone();
        tokio::spawn(async move {
            let io = TokioIo::new(stream);
            let service = service_fn(move |req: Request<Incoming>| {
                let sup = sup.clone();
                let gate = gate.clone();
                async move { Ok::<_, Infallible>(handle(req, sup, gate).await) }
            });
            if let Err(err) = server_http1::Builder::new()
                .serve_connection(io, service)
                .with_upgrades()
                .await
            {
                tracing::debug!("proxy connection ended: {err}");
            }
        });
    }
}

async fn handle(
    req: Request<Incoming>,
    supervisor: Arc<Supervisor>,
    gate: Arc<BootGate>,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    let host = match host_from_request(&req) {
        Some(h) => h,
        None => return error_response(StatusCode::BAD_REQUEST, "missing or invalid Host header"),
    };
    let name = match name_from_host(&host) {
        Some(n) => n,
        None => {
            return error_response(
                StatusCode::NOT_FOUND,
                &format!("host `{host}` is not a `*{HOST_SUFFIX}` name"),
            )
        }
    };

    let upstream_port = match ensure_running(&name, supervisor, gate).await {
        Ok(p) => p,
        Err(ProxyError::NotRegistered) => {
            return error_response(
                StatusCode::NOT_FOUND,
                &format!("no app named `{name}` is registered"),
            )
        }
        Err(ProxyError::BootTimeout) => {
            return error_response(
                StatusCode::GATEWAY_TIMEOUT,
                &format!("app `{name}` did not become ready within boot timeout"),
            )
        }
        Err(ProxyError::Other(err)) => {
            tracing::warn!("proxy lazy-boot failed for `{name}`: {err}");
            return error_response(
                StatusCode::BAD_GATEWAY,
                &format!("failed to boot `{name}`: {err}"),
            );
        }
    };

    match forward(req, upstream_port).await {
        Ok(resp) => resp,
        Err(err) => {
            tracing::warn!("proxy forward to `{name}:{upstream_port}` failed: {err}");
            error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream `{name}` error: {err}"),
            )
        }
    }
}

#[derive(Debug)]
enum ProxyError {
    NotRegistered,
    BootTimeout,
    Other(anyhow::Error),
}

impl From<anyhow::Error> for ProxyError {
    fn from(err: anyhow::Error) -> Self {
        ProxyError::Other(err)
    }
}

async fn ensure_running(
    name: &str,
    supervisor: Arc<Supervisor>,
    gate: Arc<BootGate>,
) -> Result<u16, ProxyError> {
    // Fast path: already running. Skip taking the per-name lock to avoid head-of-line blocking
    // when the app is hot.
    if let AppState::Running { port, .. } = supervisor.state(name).await {
        return Ok(port);
    }

    // Look up the registered path/config once outside the lock; needed for the boot path.
    let reg = Registry::load().map_err(ProxyError::Other)?;
    let entry = match reg.get(name) {
        Some(e) => e.clone(),
        None => return Err(ProxyError::NotRegistered),
    };
    let cfg = registry::read_app_config(&entry.path).map_err(ProxyError::Other)?;
    if cfg.name != name {
        return Err(ProxyError::Other(anyhow!(
            "adjacent.toml at {} declares name `{}`, not `{}`",
            entry.path.display(),
            cfg.name,
            name
        )));
    }
    let boot_timeout = Duration::from_secs(cfg.boot_timeout.unwrap_or(DEFAULT_BOOT_TIMEOUT_SECS));

    // Single-flight: serialize concurrent boot attempts for this name. The first holder runs the
    // boot; later holders re-check state under the lock and find Running.
    let name_lock = gate.lock_for(name).await;
    let _guard = name_lock.lock().await;

    if let AppState::Running { port, .. } = supervisor.state(name).await {
        return Ok(port);
    }

    supervisor
        .up(entry.path.clone(), cfg.clone())
        .await
        .map_err(ProxyError::Other)?;

    // Resolve the assigned port and wait for the child to bind it.
    let deadline = tokio::time::Instant::now() + boot_timeout;
    loop {
        match supervisor.state(name).await {
            AppState::Running { port, .. } => {
                if tcp_ready(port, deadline).await {
                    return Ok(port);
                }
                return Err(ProxyError::BootTimeout);
            }
            AppState::Crashed { exit_code } => {
                return Err(ProxyError::Other(anyhow!(
                    "app `{name}` crashed during boot (exit {exit_code})"
                )));
            }
            AppState::Stopped => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(ProxyError::BootTimeout);
                }
                tokio::time::sleep(TCP_READY_POLL_INTERVAL).await;
            }
        }
    }
}

/// Poll TCP-connect to 127.0.0.1:port until the deadline. Returns true if the child accepts a
/// connection; false if the deadline elapses first.
async fn tcp_ready(port: u16, deadline: tokio::time::Instant) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    loop {
        if TcpStream::connect(addr).await.is_ok() {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(TCP_READY_POLL_INTERVAL).await;
    }
}

async fn forward(
    mut req: Request<Incoming>,
    upstream_port: u16,
) -> Result<Response<BoxBody<Bytes, hyper::Error>>> {
    let upstream_addr = SocketAddr::from(([127, 0, 0, 1], upstream_port));
    let stream = TcpStream::connect(upstream_addr)
        .await
        .map_err(|e| anyhow!("connecting to upstream {upstream_addr}: {e}"))?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = client_http1::handshake(io)
        .await
        .map_err(|e| anyhow!("upstream handshake: {e}"))?;
    tokio::spawn(async move {
        if let Err(err) = conn.await {
            tracing::debug!("upstream connection ended: {err}");
        }
    });

    // Rewrite the Host header to the upstream's loopback address so apps that key on Host (e.g.
    // dev servers with host-allowlist checks) accept the request.
    let upstream_host = format!("127.0.0.1:{upstream_port}");
    req.headers_mut().insert(
        hyper::header::HOST,
        upstream_host.parse().expect("loopback host header valid"),
    );

    let resp = sender
        .send_request(req)
        .await
        .map_err(|e| anyhow!("sending upstream request: {e}"))?;
    let (parts, body) = resp.into_parts();
    Ok(Response::from_parts(parts, body.boxed()))
}

fn host_from_request<B>(req: &Request<B>) -> Option<String> {
    if let Some(h) = req.headers().get(hyper::header::HOST) {
        if let Ok(s) = h.to_str() {
            return Some(strip_port(s));
        }
    }
    req.uri().host().map(|s| s.to_string())
}

fn strip_port(host: &str) -> String {
    // Strip a trailing :port if present. IPv6 literals would be bracketed; for v1 we only
    // handle bare hostnames since the protocol is `<name>.adj.ac`.
    match host.rfind(':') {
        Some(idx) if host[idx + 1..].chars().all(|c| c.is_ascii_digit()) => host[..idx].to_string(),
        _ => host.to_string(),
    }
}

fn name_from_host(host: &str) -> Option<String> {
    let lower = host.to_ascii_lowercase();
    let prefix = lower.strip_suffix(HOST_SUFFIX)?;
    if prefix.is_empty() || prefix.contains('.') {
        return None;
    }
    Some(prefix.to_string())
}

fn error_response(status: StatusCode, message: &str) -> Response<BoxBody<Bytes, hyper::Error>> {
    let body = Full::new(Bytes::from(format!("{message}\n")))
        .map_err(|never: Infallible| match never {})
        .boxed();
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(body)
        .unwrap_or_else(|_| {
            Response::new(
                Empty::<Bytes>::new()
                    .map_err(|never: Infallible| match never {})
                    .boxed(),
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_optional_port_suffix() {
        assert_eq!(strip_port("echo.adj.ac"), "echo.adj.ac");
        assert_eq!(strip_port("echo.adj.ac:8080"), "echo.adj.ac");
        assert_eq!(strip_port("echo.adj.ac:80"), "echo.adj.ac");
    }

    #[test]
    fn extracts_name_from_adj_ac_host() {
        assert_eq!(name_from_host("echo.adj.ac"), Some("echo".into()));
        assert_eq!(name_from_host("ECHO.adj.ac"), Some("echo".into()));
        // Multi-label subdomains aren't supported by registration (one name = one DNS label).
        assert_eq!(name_from_host("a.b.adj.ac"), None);
        assert_eq!(name_from_host("example.com"), None);
        assert_eq!(name_from_host(".adj.ac"), None);
    }
}
