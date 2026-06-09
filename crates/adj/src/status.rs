use std::convert::Infallible;
use std::sync::Arc;

use adj_protocol::{AppSummary, StatusDto};
use bytes::Bytes;
use http_body_util::{combinators::BoxBody, BodyExt, Empty, Full};
use hyper::body::Incoming;
use hyper::{Method, Request, Response, StatusCode};

use crate::registry::Registry;
use crate::supervisor::Supervisor;

const HTML: &str = include_str!("../assets/status.html");

/// Serve the built-in dashboard for `status.adj.ac`. Two routes — `GET /` returns the static
/// shell, `GET /apps.json` returns the current snapshot — and the page polls the JSON endpoint
/// every couple of seconds to stay live. Both responses set `Cache-Control: no-store` so a
/// proxy or browser doesn't serve a stale snapshot back to the user.
pub async fn handle(
    req: Request<Incoming>,
    supervisor: Arc<Supervisor>,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    let path = req.uri().path();
    let method = req.method();
    let head_only = method == Method::HEAD;

    if !matches!(method, &Method::GET | &Method::HEAD) {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n");
    }

    match path {
        "/" | "" => html_response(HTML, head_only),
        "/apps.json" => apps_json(supervisor, head_only).await,
        _ => text_response(StatusCode::NOT_FOUND, "not found\n"),
    }
}

async fn apps_json(
    supervisor: Arc<Supervisor>,
    head_only: bool,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    let entries = match snapshot(supervisor).await {
        Ok(v) => v,
        Err(err) => {
            tracing::warn!("status snapshot failed: {err}");
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "snapshot failed\n");
        }
    };
    let dtos: Vec<StatusDto> = entries
        .iter()
        .map(|e| StatusDto {
            name: &e.name,
            path: &e.path,
            state: &e.state,
        })
        .collect();
    let body = match serde_json::to_vec(&dtos) {
        Ok(b) => b,
        Err(err) => {
            tracing::warn!("status serialize failed: {err}");
            return text_response(StatusCode::INTERNAL_SERVER_ERROR, "serialize failed\n");
        }
    };
    build_response(
        StatusCode::OK,
        "application/json; charset=utf-8",
        body,
        head_only,
    )
}

/// Snapshot the registry and query the supervisor for each app's state. Mirrors `daemon::list`'s
/// shape so the dashboard JSON is the same source of truth as `adj list --json`.
async fn snapshot(supervisor: Arc<Supervisor>) -> anyhow::Result<Vec<AppSummary>> {
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
    Ok(entries)
}

fn html_response(body: &'static str, head_only: bool) -> Response<BoxBody<Bytes, hyper::Error>> {
    build_response(
        StatusCode::OK,
        "text/html; charset=utf-8",
        body.as_bytes().to_vec(),
        head_only,
    )
}

fn text_response(status: StatusCode, message: &str) -> Response<BoxBody<Bytes, hyper::Error>> {
    build_response(
        status,
        "text/plain; charset=utf-8",
        message.as_bytes().to_vec(),
        false,
    )
}

fn build_response(
    status: StatusCode,
    content_type: &str,
    body: Vec<u8>,
    head_only: bool,
) -> Response<BoxBody<Bytes, hyper::Error>> {
    let content_length = body.len();
    let body_stream = if head_only {
        Empty::<Bytes>::new()
            .map_err(|never: Infallible| match never {})
            .boxed()
    } else {
        Full::new(Bytes::from(body))
            .map_err(|never: Infallible| match never {})
            .boxed()
    };
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, content_type)
        .header(hyper::header::CONTENT_LENGTH, content_length)
        .header(hyper::header::CACHE_CONTROL, "no-store")
        .body(body_stream)
        .unwrap_or_else(|_| {
            Response::new(
                Empty::<Bytes>::new()
                    .map_err(|never: Infallible| match never {})
                    .boxed(),
            )
        })
}
