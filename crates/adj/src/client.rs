use std::path::Path;
use std::time::Duration;

use adj_protocol::{ListEntryDto, LogRecord, Request, Response, StatsDto, StatusDto};
use anyhow::{anyhow, Context, Result};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::paths;
use crate::worktree;

async fn connect() -> Result<UnixStream> {
    let socket = paths::socket_path()?;
    match UnixStream::connect(&socket).await {
        Ok(s) => Ok(s),
        Err(err) => Err(anyhow!(
            "daemon not reachable at {} ({err}). Start it with `adj daemon`.",
            socket.display()
        )),
    }
}

async fn request(req: Request) -> Result<Response> {
    let stream = connect().await?;
    let (read_half, mut write_half) = stream.into_split();
    let mut bytes = serde_json::to_vec(&req)?;
    bytes.push(b'\n');
    write_half.write_all(&bytes).await?;
    write_half.shutdown().await.ok();

    let mut reader = BufReader::new(read_half);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    if line.is_empty() {
        return Err(anyhow!("daemon closed connection without a response"));
    }
    let resp: Response = serde_json::from_str(line.trim())
        .with_context(|| format!("parsing daemon response: {line}"))?;
    Ok(resp)
}

fn into_error(resp: Response) -> Result<Response> {
    match resp {
        Response::Error { message } => Err(anyhow!(message)),
        other => Ok(other),
    }
}

pub async fn add(path: String, label: Option<String>) -> Result<()> {
    // Canonicalize on the client side: relative paths must resolve against the user's CWD,
    // not the daemon's. The daemon may have been launched from anywhere (or by launchd).
    let canon = std::fs::canonicalize(&path).with_context(|| format!("resolving path {}", path))?;
    // `--label` wins; otherwise a linked git worktree names its instance after the branch.
    let label = match label {
        Some(l) => Some(l),
        None => worktree::detect_label(&canon)?,
    };
    let resp = into_error(
        request(Request::Add {
            path: canon.display().to_string(),
            label,
        })
        .await?,
    )?;
    if let Response::Added { name, path } = resp {
        println!("registered `{name}` at {path}");
    }
    Ok(())
}

pub async fn list(json: bool) -> Result<()> {
    let resp = into_error(request(Request::List).await?)?;
    if let Response::List { entries } = resp {
        if json {
            let dtos: Vec<ListEntryDto> = entries
                .iter()
                .map(|e| ListEntryDto {
                    name: &e.name,
                    path: &e.path,
                    state: &e.state,
                    stale: e.stale,
                })
                .collect();
            let out = serde_json::to_string(&dtos)?;
            println!("{out}");
            return Ok(());
        }
        if entries.is_empty() {
            println!("no apps registered");
            return Ok(());
        }
        for entry in &entries {
            if entry.stale {
                println!(
                    "{:<20} {:<10} {} (path missing — run `adj prune`)",
                    entry.name, "stale", entry.path
                );
            } else {
                println!("{:<20} {:<10} {}", entry.name, entry.state, entry.path);
            }
        }
    }
    Ok(())
}

pub async fn remove(name: String) -> Result<()> {
    let resp = into_error(request(Request::Remove { name }).await?)?;
    if let Response::Removed { name } = resp {
        println!("removed `{name}`");
    }
    Ok(())
}

pub async fn prune() -> Result<()> {
    let resp = into_error(request(Request::Prune).await?)?;
    if let Response::Pruned { removed } = resp {
        if removed.is_empty() {
            println!("nothing to prune");
        } else {
            for name in removed {
                println!("pruned `{name}`");
            }
        }
    }
    Ok(())
}

pub async fn up(name: String) -> Result<()> {
    into_error(request(Request::Up { name: name.clone() }).await?)?;
    println!("started `{name}`");
    Ok(())
}

pub async fn down(name: String) -> Result<()> {
    into_error(request(Request::Down { name: name.clone() }).await?)?;
    println!("stopped `{name}`");
    Ok(())
}

pub async fn restart(name: String) -> Result<()> {
    into_error(request(Request::Restart { name: name.clone() }).await?)?;
    println!("restarted `{name}`");
    Ok(())
}

pub async fn status(name: String, json: bool) -> Result<()> {
    if json {
        // For `--json` we need the app's registered path too. The List response carries it;
        // a single extra request keeps the protocol surface unchanged.
        let list_resp = into_error(request(Request::List).await?)?;
        let Response::List { entries } = list_resp else {
            return Err(anyhow!("unexpected response from daemon"));
        };
        let entry = entries
            .iter()
            .find(|e| e.name == name)
            .ok_or_else(|| anyhow!("no app named `{}`", name))?;
        let dto = StatusDto {
            name: &entry.name,
            path: &entry.path,
            state: &entry.state,
        };
        let out = serde_json::to_string(&dto)?;
        println!("{out}");
        return Ok(());
    }
    let resp = into_error(request(Request::Status { name }).await?)?;
    if let Response::Status { name, state } = resp {
        println!("{name}: {state}");
    }
    Ok(())
}

/// Parse a `--since` value into seconds. Accepts bare seconds (`90`) or a single unit suffix:
/// `s`, `m`, `h`. `0` means "the full window".
pub fn parse_since(s: &str) -> Result<u64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        _ => return Err(anyhow!("invalid --since `{s}` (use e.g. 30s, 5m, 1h)")),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| anyhow!("invalid --since `{s}` (use e.g. 30s, 5m, 1h)"))?;
    Ok(n * mult)
}

pub async fn stats(name: String, json: bool, since: String) -> Result<()> {
    let since_secs = parse_since(&since)?;
    let resp = into_error(
        request(Request::Stats {
            name: name.clone(),
            since_secs,
        })
        .await?,
    )?;
    let Response::Stats { stats } = resp else {
        return Err(anyhow!("unexpected response from daemon"));
    };
    if json {
        println!("{}", serde_json::to_string(&stats)?);
        return Ok(());
    }
    render_stats_table(&stats);
    Ok(())
}

fn render_stats_table(s: &StatsDto) {
    let mins = s.window_secs / 60;
    println!("{} — last {mins}m, {} requests", s.name, s.total_requests);
    if let Some(p) = &s.process {
        println!(
            "  process: cpu {:.0}%  rss {}  threads {}  fds {}",
            p.cpu_pct,
            human_bytes(p.rss_bytes),
            p.threads,
            p.fds
        );
    }
    if s.routes.is_empty() {
        println!("  (no requests in window)");
        return;
    }
    println!(
        "  {:<34} {:>6} {:>7} {:>7} {:>6}",
        "route", "count", "p50", "p95", "err%"
    );
    for r in &s.routes {
        let errs = r.status_4xx + r.status_5xx;
        let err_pct = if r.count > 0 {
            errs as f64 * 100.0 / r.count as f64
        } else {
            0.0
        };
        println!(
            "  {:<34} {:>6} {:>5}ms {:>5}ms {:>5.0}%",
            truncate(&r.route, 34),
            r.count,
            r.latency_ms.p50,
            r.latency_ms.p95,
            err_pct
        );
    }
    if !s.slowest_raw.is_empty() {
        println!("  slowest:");
        for raw in &s.slowest_raw {
            println!(
                "    {:>5}ms  {} {} ({})",
                raw.latency_ms, raw.method, raw.path, raw.status
            );
        }
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.0}{}", UNITS[u])
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}

pub async fn wait_ready(name: String, timeout_secs: u64) -> Result<()> {
    // The daemon blocks the response until ready or timeout — the client just waits on the
    // socket. Errors (timeout, crash, not registered) come back as `Response::Error`.
    into_error(
        request(Request::WaitReady {
            name: name.clone(),
            timeout_secs,
        })
        .await?,
    )?;
    println!("ready `{name}`");
    Ok(())
}

pub async fn logs(name: String, tail: bool, json: bool) -> Result<()> {
    let resp = into_error(request(Request::LogPath { name: name.clone() }).await?)?;
    let Response::LogPath { path } = resp else {
        return Err(anyhow!("unexpected response from daemon"));
    };
    let path = std::path::PathBuf::from(path);
    if !path.exists() {
        if tail {
            // Still allow tailing — file will be created when the app boots.
            tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(&path)
                .await
                .with_context(|| format!("creating log file {}", path.display()))?;
        } else {
            return Err(anyhow!(
                "no log file yet at {} (has `{}` ever been started?)",
                path.display(),
                name
            ));
        }
    }
    match (tail, json) {
        (false, false) => print_file_plain(&path).await,
        (true, false) => tail_file_plain(&path).await,
        (false, true) => print_file_json(&path).await,
        (true, true) => tail_file_json(&path).await,
    }
}

// In plain-text mode we project each JSONL record's `line` field. Lines that don't parse
// (e.g. legacy logs written before this slice landed) are passed through verbatim so users
// don't lose data during the transition.
async fn print_file_plain(path: &Path) -> Result<()> {
    let file = File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        emit_plain_line(&line, &mut stdout).await?;
    }
    stdout.flush().await?;
    Ok(())
}

async fn tail_file_plain(path: &Path) -> Result<()> {
    let file = File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut reader = BufReader::new(file);
    let mut stdout = tokio::io::stdout();
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await? {
            0 => {
                stdout.flush().await?;
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            _ => {
                emit_plain_line(&line, &mut stdout).await?;
                stdout.flush().await?;
            }
        }
    }
}

// In JSON mode we stream the file's contents as-is. The supervisor already writes valid
// JSONL records, so callers can pipe directly into `jq` or any JSONL parser.
async fn print_file_json(path: &Path) -> Result<()> {
    let mut file = File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut stdout = tokio::io::stdout();
    let mut buf = vec![0u8; 8192];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        stdout.write_all(&buf[..n]).await?;
    }
    stdout.flush().await?;
    Ok(())
}

async fn tail_file_json(path: &Path) -> Result<()> {
    let mut file = File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut stdout = tokio::io::stdout();
    let mut buf = vec![0u8; 8192];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            stdout.flush().await?;
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        }
        stdout.write_all(&buf[..n]).await?;
        stdout.flush().await?;
    }
}

async fn emit_plain_line<W>(raw_line: &str, out: &mut W) -> Result<()>
where
    W: tokio::io::AsyncWrite + Unpin,
{
    // Try to parse as our JSONL record; on failure, write the line verbatim. This makes the
    // plain view degrade gracefully if a log file mixes record formats.
    let trimmed = raw_line.trim_end_matches(['\r', '\n']);
    if trimmed.is_empty() {
        out.write_all(b"\n").await?;
        return Ok(());
    }
    if let Ok(record) = serde_json::from_str::<LogRecord>(trimmed) {
        out.write_all(record.line.as_bytes()).await?;
        out.write_all(b"\n").await?;
    } else {
        out.write_all(raw_line.as_bytes()).await?;
        if !raw_line.ends_with('\n') {
            out.write_all(b"\n").await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_since_supports_units_and_bare_seconds() {
        assert_eq!(parse_since("0").unwrap(), 0);
        assert_eq!(parse_since("30s").unwrap(), 30);
        assert_eq!(parse_since("5m").unwrap(), 300);
        assert_eq!(parse_since("1h").unwrap(), 3600);
        assert_eq!(parse_since("90").unwrap(), 90);
        assert!(parse_since("nonsense").is_err());
        assert!(parse_since("5d").is_err());
    }
}
