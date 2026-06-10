use std::net::SocketAddr;
use std::time::Duration;

use adj_protocol::AppState;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::client::conn::http1 as client_http1;
use hyper::Request;
use hyper_util::rt::TokioIo;
use tokio::net::TcpStream;

use crate::proxy::READY_POLL_INTERVAL;
use crate::registry::AppConfig;
use crate::supervisor::Supervisor;

/// Outcome of waiting for an app to become ready. The proxy and `adj wait-ready` map this into
/// their respective error surfaces.
#[derive(Debug)]
pub enum ReadinessError {
    /// The supervisor reports the app is not running and the deadline passed before it bound
    /// its port (or the configured health URL never returned 2xx).
    Timeout,
    /// The supervised process exited non-zero while we were waiting.
    Crashed { exit_code: i32 },
}

/// Block until `name` is ready or `deadline` elapses. Returns the port the child bound to.
///
/// Readiness check:
/// - If `cfg.health_check_url` is set, HTTP-GET that path on the assigned port and consider the
///   app ready when the response status is 2xx.
/// - Otherwise, fall back to a TCP-connect probe (current default).
///
/// We poll the supervisor state on every iteration so a crash during boot surfaces immediately
/// rather than waiting out the full timeout.
pub async fn wait_ready(
    name: &str,
    supervisor: &Supervisor,
    cfg: &AppConfig,
    deadline: tokio::time::Instant,
) -> Result<u16, ReadinessError> {
    loop {
        match supervisor.state(name).await {
            AppState::Running { port, .. } => {
                if probe_once(port, cfg.health_check_url.as_deref()).await {
                    return Ok(port);
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(ReadinessError::Timeout);
                }
                tokio::time::sleep(READY_POLL_INTERVAL).await;
            }
            AppState::Crashed { exit_code } => {
                return Err(ReadinessError::Crashed { exit_code });
            }
            AppState::Stopped => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(ReadinessError::Timeout);
                }
                tokio::time::sleep(READY_POLL_INTERVAL).await;
            }
        }
    }
}

/// One readiness probe attempt. Returns true if the app is ready, false otherwise. Errors and
/// non-2xx responses both map to false so the caller's poll loop keeps trying until the
/// deadline elapses.
async fn probe_once(port: u16, health_check_url: Option<&str>) -> bool {
    match health_check_url {
        Some(path) => http_ready(port, path).await,
        None => tcp_ready_once(port).await,
    }
}

async fn tcp_ready_once(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect(addr).await.is_ok()
}

/// Issue a single HTTP GET to `127.0.0.1:<port><path>` and return true on a 2xx response.
/// A short per-attempt timeout keeps the poll loop responsive when the app accepts the TCP
/// connect but never writes a response.
async fn http_ready(port: u16, path: &str) -> bool {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };
    // The spawned hyper connection task is parked on this slot so it can be aborted once the
    // probe resolves. A timed-out probe drops `sender`, and hyper 1.x happens to propagate
    // that by resolving the conn future — but that is drop-propagation behavior we don't
    // control. The explicit abort guarantees the task (and its socket) never outlives the
    // probe, regardless of hyper internals.
    let mut conn_task: Option<tokio::task::JoinHandle<()>> = None;
    let attempt = async {
        let addr = SocketAddr::from(([127, 0, 0, 1], port));
        let stream = TcpStream::connect(addr).await.ok()?;
        let io = TokioIo::new(stream);
        let (mut sender, conn) = client_http1::handshake::<_, Empty<Bytes>>(io).await.ok()?;
        conn_task = Some(tokio::spawn(async move {
            let _ = conn.await;
        }));
        let req = Request::builder()
            .uri(&path)
            .header(hyper::header::HOST, format!("127.0.0.1:{port}"))
            .header(hyper::header::USER_AGENT, "adj-readiness/1")
            .body(Empty::<Bytes>::new())
            .ok()?;
        let resp = sender.send_request(req).await.ok()?;
        let status = resp.status();
        // Drain so the upstream connection task can finish cleanly.
        let _ = resp.into_body().collect().await;
        Some(status.is_success())
    };
    let ready = tokio::time::timeout(Duration::from_millis(500), attempt)
        .await
        .ok()
        .flatten()
        .unwrap_or(false);
    // Abort unconditionally: a no-op once the task has finished, a cancellation (dropping the
    // socket) when the probe timed out or the probe completed while the connection lingered.
    if let Some(task) = conn_task {
        task.abort();
    }
    ready
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// After N timed-out probes against a hung-but-listening server (accepts, holds the socket,
    /// never responds), the runtime must carry no leftover connection tasks — only the server
    /// task from this test survives. Each `#[tokio::test]` gets its own runtime, so the alive
    /// count isn't polluted by unrelated tasks.
    #[tokio::test]
    async fn repeated_timed_out_probes_do_not_accumulate_tasks() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let mut held = Vec::new();
            loop {
                let (sock, _) = listener.accept().await.unwrap();
                held.push(sock);
            }
        });
        let metrics = tokio::runtime::Handle::current().metrics();
        let baseline = metrics.num_alive_tasks();
        for _ in 0..10 {
            assert!(!http_ready(port, "/healthz").await);
        }
        // Aborted tasks unwind asynchronously; poll briefly instead of asserting instantly.
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        while metrics.num_alive_tasks() > baseline {
            assert!(
                tokio::time::Instant::now() < deadline,
                "connection tasks leaked: {} alive vs baseline {}",
                metrics.num_alive_tasks(),
                baseline
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// Regression test for the probe-task leak: a server that accepts but never responds makes
    /// the probe time out, and the aborted connection task must drop the socket — observed by
    /// the server reading EOF. Pre-fix, the orphaned task held the socket open indefinitely.
    #[tokio::test]
    async fn timed_out_probe_aborts_connection_task() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 1024];
            loop {
                match sock.read(&mut buf).await {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
        });

        assert!(!http_ready(port, "/healthz").await);

        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("upstream connection not closed after probe timeout")
            .unwrap();
    }
}
