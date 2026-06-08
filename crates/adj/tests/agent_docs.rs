// Tests for `adj agent-instructions`. The command reads adjacent.toml in a target dir
// and prints a markdown steering doc to stdout. It does not require the daemon.

use std::path::Path;

use tempfile::TempDir;
use tokio::process::Command;

fn adj_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}

async fn write_manifest(dir: &Path, name: &str, cmd: &str) {
    let body = format!("name = \"{name}\"\ncmd = \"{cmd}\"\n");
    tokio::fs::write(dir.join("adjacent.toml"), body)
        .await
        .expect("write manifest");
}

#[tokio::test]
async fn emits_markdown_templated_with_app_name_and_cmd() {
    let dir = TempDir::new().expect("tempdir");
    write_manifest(dir.path(), "myapp", "npm run dev").await;

    let out = Command::new(adj_bin())
        .arg("agent-instructions")
        .arg("--path")
        .arg(dir.path())
        .output()
        .await
        .expect("agent-instructions");

    assert!(
        out.status.success(),
        "agent-instructions failed: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );

    let stdout = String::from_utf8_lossy(&out.stdout);

    // App name appears in the heading and example commands.
    assert!(
        stdout.contains("myapp"),
        "stdout missing app name `myapp`: {stdout}"
    );
    // The dev command appears so the agent knows what NOT to run.
    assert!(
        stdout.contains("npm run dev"),
        "stdout missing the dev cmd `npm run dev`: {stdout}"
    );
    // Key adj commands the agent should use are documented.
    for needle in [
        "adj status myapp",
        "adj logs myapp",
        "adj restart myapp",
        "adj wait-ready myapp",
    ] {
        assert!(stdout.contains(needle), "stdout missing `{needle}`: {stdout}");
    }
    // Proxy URL pattern.
    assert!(
        stdout.contains("myapp.adj.ac"),
        "stdout missing proxy URL `myapp.adj.ac`: {stdout}"
    );
    // No un-substituted template placeholders leaked through.
    assert!(
        !stdout.contains("{name}") && !stdout.contains("{cmd}"),
        "stdout contains un-substituted placeholder: {stdout}"
    );
}

#[tokio::test]
async fn errors_when_manifest_missing() {
    let dir = TempDir::new().expect("tempdir");
    // Intentionally do NOT write adjacent.toml.

    let out = Command::new(adj_bin())
        .arg("agent-instructions")
        .arg("--path")
        .arg(dir.path())
        .output()
        .await
        .expect("agent-instructions");

    assert!(
        !out.status.success(),
        "expected non-zero exit when adjacent.toml is missing"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("no adjacent.toml found"),
        "stderr should explain the missing manifest, got: {stderr}"
    );
}
