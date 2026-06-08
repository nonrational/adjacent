use std::path::Path;
use std::time::Duration;

use adj_protocol::{Request, Response};
use anyhow::{anyhow, Context, Result};
use tokio::fs::File;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;

use crate::paths;

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

pub async fn add(path: String) -> Result<()> {
    // Canonicalize on the client side: relative paths must resolve against the user's CWD,
    // not the daemon's. The daemon may have been launched from anywhere (or by launchd).
    let canon = std::fs::canonicalize(&path)
        .with_context(|| format!("resolving path {}", path))?;
    let resp = into_error(
        request(Request::Add {
            path: canon.display().to_string(),
        })
        .await?,
    )?;
    if let Response::Added { name, path } = resp {
        println!("registered `{name}` at {path}");
    }
    Ok(())
}

pub async fn list() -> Result<()> {
    let resp = into_error(request(Request::List).await?)?;
    if let Response::List { entries } = resp {
        if entries.is_empty() {
            println!("no apps registered");
            return Ok(());
        }
        for entry in entries {
            println!("{:<20} {:<10} {}", entry.name, entry.state, entry.path);
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

pub async fn status(name: String) -> Result<()> {
    let resp = into_error(request(Request::Status { name }).await?)?;
    if let Response::Status { name, state } = resp {
        println!("{name}: {state}");
    }
    Ok(())
}

pub async fn logs(name: String, tail: bool) -> Result<()> {
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
    if tail {
        tail_file(&path).await
    } else {
        print_file(&path).await
    }
}

async fn print_file(path: &Path) -> Result<()> {
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

async fn tail_file(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let mut file = File::open(path)
        .await
        .with_context(|| format!("opening {}", path.display()))?;
    let mut current_inode = file.metadata().await.ok().map(|m| m.ino());
    let mut stdout = tokio::io::stdout();
    let mut buf = vec![0u8; 8192];
    // Drain the existing contents first so a non-tail-style "show then follow" feels normal.
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        stdout.write_all(&buf[..n]).await?;
    }
    stdout.flush().await?;

    // Poll for new content. Simple and dependency-free; sufficient for v1.
    //
    // Rotation detection: the supervisor renames <name>.log -> <name>.log.1 and creates a
    // fresh <name>.log. Our open handle still points to the renamed inode and would never
    // see new writes again. Each idle tick we re-stat the path; if its inode no longer
    // matches the one we opened, we drain the old handle to EOF (post-rotation tail of
    // the rotated file) and switch to the new file from offset 0.
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            tokio::time::sleep(Duration::from_millis(200)).await;
            if let Ok(meta) = tokio::fs::metadata(path).await {
                let new_inode = Some(meta.ino());
                if new_inode != current_inode {
                    // Drain anything still buffered on the old inode before switching, so we
                    // don't drop the tail of the just-rotated file.
                    loop {
                        let m = file.read(&mut buf).await?;
                        if m == 0 {
                            break;
                        }
                        stdout.write_all(&buf[..m]).await?;
                    }
                    file = File::open(path)
                        .await
                        .with_context(|| format!("re-opening {}", path.display()))?;
                    current_inode = new_inode;
                }
            }
            continue;
        }
        stdout.write_all(&buf[..n]).await?;
        stdout.flush().await?;
    }
}
