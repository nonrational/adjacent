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

use crate::readiness::wait_ready;
use crate::registry::{self, AppConfig, Registry};
use crate::status;
use crate::supervisor::Supervisor;
use crate::tls;

const PROXY_PORT_ENV: &str = "ADJACENT_PROXY_PORT";
const HTTPS_PORT_ENV: &str = "ADJACENT_HTTPS_PORT";
const HOST_SUFFIX: &str = ".adj.ac";
pub const DEFAULT_BOOT_TIMEOUT_SECS: u64 = 60;
pub const READY_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Reserved subdomain used by `adj doctor` to probe the proxy without booting an app. The body
/// is fixed bytes so the doctor can match on equality, and the hostname is invalid for `adj add`
/// (the reserved-names list in `daemon.rs` blocks claiming it) so no one can shadow it.
pub const VERIFY_HOST: &str = "__adj_verify__.adj.ac";
pub const VERIFY_BODY: &str = "adj-port-forward-ok\n";

pub fn proxy_port() -> u16 {
    std::env::var(PROXY_PORT_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8080)
}

pub fn https_port() -> u16 {
    std::env::var(HTTPS_PORT_ENV)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8443)
}

/// Per-name boot lock map. Holding the inner `Mutex` while booting keeps the boot single-flight:
/// concurrent waiters acquire the mutex one at a time, but the first one finishes the boot so
/// every later acquirer sees the app already Running and returns immediately.
///
/// Entries are `Weak` so the map is bounded by in-flight boots rather than by every name ever
/// requested — there is no app-removal RPC to hook cleanup onto, so a strong-reference map
/// would keep entries for unregistered apps forever (issue #27).
#[derive(Default)]
pub struct BootGate {
    locks: Mutex<HashMap<String, std::sync::Weak<Mutex<()>>>>,
}

impl BootGate {
    fn new() -> Self {
        Self::default()
    }

    async fn lock_for(&self, name: &str) -> Arc<Mutex<()>> {
        let mut map = self.locks.lock().await;
        map.retain(|_, weak| weak.strong_count() > 0);
        if let Some(existing) = map.get(name).and_then(std::sync::Weak::upgrade) {
            return existing;
        }
        let lock = Arc::new(Mutex::new(()));
        map.insert(name.to_string(), Arc::downgrade(&lock));
        lock
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
            serve_plain(stream, sup, gate).await;
        });
    }
}

/// HTTPS listener: terminates TLS with the locally-issued wildcard cert, then dispatches into
/// the same per-request handler the HTTP path uses. Startup is best-effort — if the local CA
/// hasn't been generated yet the daemon logs and keeps serving HTTP only (AC #5 in issue #6).
pub async fn run_https(supervisor: Arc<Supervisor>) -> Result<()> {
    let server_config = tls::server_config()
        .map_err(|e| anyhow!("loading TLS config: {e}"))?;
    let acceptor = tokio_rustls::TlsAcceptor::from(server_config);

    let port = https_port();
    let addr: SocketAddr = ([127, 0, 0, 1], port).into();
    let listener = TcpListener::bind(addr)
        .await
        .map_err(|e| anyhow!("binding https listener at {addr}: {e}"))?;
    tracing::info!("adj https proxy listening at https://{addr}");

    let gate = Arc::new(BootGate::new());

    loop {
        let (stream, _peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(err) => {
                tracing::warn!("https accept failed: {err}");
                continue;
            }
        };
        let sup = supervisor.clone();
        let gate = gate.clone();
        let acceptor = acceptor.clone();
        tokio::spawn(async move {
            // TLS handshake is per-connection. Failures here are noisy under normal browser
            // probing (favicon retries, abandoned sessions) so log at debug, not warn.
            let tls_stream = match acceptor.accept(stream).await {
                Ok(s) => s,
                Err(err) => {
                    tracing::debug!("tls handshake failed: {err}");
                    return;
                }
            };
            serve_plain(tls_stream, sup, gate).await;
        });
    }
}

/// Run one HTTP/1.1 connection against the proxy's per-request handler. Parameterized over the
/// underlying stream so the HTTP and HTTPS listeners share the same serve loop — the difference
/// between them is purely accept-time framing.
async fn serve_plain<S>(stream: S, sup: Arc<Supervisor>, gate: Arc<BootGate>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
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

    // Reserved subdomain: the built-in dashboard. Handled in-process, never touches the
    // registry or boot gate. Listed in `daemon::RESERVED_NAMES` so `adj add` refuses to claim it.
    if name == "status" {
        return status::handle(req, supervisor).await;
    }

    // Verify-marker: `adj doctor` hits this to confirm the port-forward rule routes a request
    // to the daemon. Short-circuits before `ensure_running` so the probe doesn't accidentally
    // spawn an app — important because the doctor can fire on a fresh install with no apps.
    if host == VERIFY_HOST {
        let body = Full::new(Bytes::from(VERIFY_BODY))
            .map_err(|never: Infallible| match never {})
            .boxed();
        return Response::builder()
            .status(StatusCode::OK)
            .header(hyper::header::CONTENT_TYPE, "text/plain; charset=utf-8")
            .body(body)
            .expect("verify response builds");
    }

    let upstream_port = match ensure_running(&name, supervisor.clone(), gate).await {
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

    // Record the request against the idle tracker before forwarding so a forward that hangs
    // for the duration of `idle_timeout` doesn't get spuriously stopped mid-stream.
    supervisor.touch_idle(&name).await;

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
    // Look up the registered path/config once outside the lock; needed for the boot path and
    // to surface NotRegistered without taking the per-name lock.
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

    // Single-flight: serialize boot attempts for this name. The first holder runs the boot;
    // later holders find the app Running on re-check and skip straight to wait_ready, which
    // confirms the upstream is actually accepting before returning a port.
    //
    // We intentionally do NOT short-circuit on a Running state outside the lock. The supervisor
    // flips state to Running the moment it spawns the child — before the child has bound its
    // port. A second concurrent first-request that observed Running here and skipped wait_ready
    // would forward to a port that wasn't accepting yet and get back a spurious 502. See #28.
    let name_lock = gate.lock_for(name).await;
    let _guard = name_lock.lock().await;

    // Capture the deadline before `up()` so a slow spawn counts against the boot budget —
    // otherwise the total wait is up() time *plus* boot_timeout (issue #27).
    let boot_timeout = boot_timeout_for(&cfg);
    let deadline = tokio::time::Instant::now() + boot_timeout;

    if !matches!(supervisor.state(name).await, AppState::Running { .. }) {
        supervisor
            .up(entry.path.clone(), cfg.clone())
            .await
            .map_err(ProxyError::Other)?;
    }

    match wait_ready(name, supervisor.as_ref(), &cfg, deadline).await {
        Ok(port) => Ok(port),
        Err(crate::readiness::ReadinessError::Timeout) => Err(ProxyError::BootTimeout),
        Err(crate::readiness::ReadinessError::Crashed { exit_code }) => {
            Err(ProxyError::Other(anyhow!(
                "app `{name}` crashed during boot (exit {exit_code})"
            )))
        }
    }
}

pub fn boot_timeout_for(cfg: &AppConfig) -> Duration {
    Duration::from_secs(cfg.boot_timeout.unwrap_or(DEFAULT_BOOT_TIMEOUT_SECS))
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

    #[tokio::test]
    async fn boot_gate_prunes_entries_once_unused() {
        let gate = BootGate::new();
        let lock_a = gate.lock_for("a").await;
        assert_eq!(gate.locks.lock().await.len(), 1);

        // While `a`'s lock is still held, asking for `b` must not prune it — and asking for
        // `a` again must hand back the same Arc (single-flight depends on identity).
        let lock_b = gate.lock_for("b").await;
        let lock_a2 = gate.lock_for("a").await;
        assert!(Arc::ptr_eq(&lock_a, &lock_a2));
        assert_eq!(gate.locks.lock().await.len(), 2);

        drop(lock_a);
        drop(lock_a2);
        drop(lock_b);
        // All strong refs gone — the next lock_for sweeps the dead entries.
        let _lock_c = gate.lock_for("c").await;
        let map = gate.locks.lock().await;
        assert_eq!(map.len(), 1);
        assert!(map.contains_key("c"));
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
