// Tests for `adj agent-instructions`. The command reads adjacent.toml in a target dir
// and prints a markdown steering doc to stdout. It does not require the daemon.

use std::path::Path;

use tempfile::TempDir;
use tokio::process::Command;

/// Run git in `dir` with a hermetic identity so the test doesn't depend on the developer's
/// global config — including gpg signing.
async fn git(dir: &Path, args: &[&str]) {
    let out = tokio::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=adj-test",
            "-c",
            "user.email=adj-test@example.com",
            "-c",
            "commit.gpgsign=false",
            "-c",
            "init.defaultBranch=main",
        ])
        .args(args)
        // Prevent the developer's global init.templateDir and core.hooksPath from leaking
        // into test repos — either can inject hooks that break the hermetic git setup.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .await
        .expect("run git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

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

#[tokio::test]
async fn defaults_to_cwd_when_path_flag_omitted() {
    let dir = TempDir::new().expect("tempdir");
    write_manifest(dir.path(), "cwdapp", "node server.js").await;

    let out = Command::new(adj_bin())
        .arg("agent-instructions")
        .current_dir(dir.path())
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
    assert!(
        stdout.contains("cwdapp") && stdout.contains("node server.js"),
        "stdout missing templated fields from CWD manifest: {stdout}"
    );
}

#[tokio::test]
async fn worktree_uses_instance_key_not_bare_name() {
    // An agent running agent-instructions from a linked worktree must be steered toward the
    // instance key (<label>.<name>) it was registered under — not the bare name which routes to
    // the main checkout's instance on the daemon.
    let repo = TempDir::new().expect("repo dir");
    git(repo.path(), &["init", "-q"]).await;
    let manifest = "name = \"site\"\ncmd = \"npm run dev\"\n";
    tokio::fs::write(repo.path().join("adjacent.toml"), manifest)
        .await
        .expect("write manifest");
    git(repo.path(), &["add", "-A"]).await;
    git(repo.path(), &["commit", "-q", "-m", "app skeleton"]).await;

    // Linked worktree on branch `feature-x` — label sanitizes to `feature-x`.
    let wt_parent = TempDir::new().expect("wt parent");
    let wt = wt_parent.path().join("wt");
    git(
        repo.path(),
        &["worktree", "add", "-b", "feature-x", wt.to_str().unwrap()],
    )
    .await;

    let out = Command::new(adj_bin())
        .arg("agent-instructions")
        .arg("--path")
        .arg(&wt)
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

    // All subcommand examples and the URL must use the instance key, not the bare name.
    for needle in [
        "adj status feature-x.site",
        "adj logs feature-x.site",
        "adj restart feature-x.site",
        "adj wait-ready feature-x.site",
        "feature-x.site.adj.ac",
    ] {
        assert!(stdout.contains(needle), "stdout missing `{needle}`: {stdout}");
    }

    // The bare name must not appear on its own as a subcommand target or URL — that would steer
    // the agent at the main checkout's instance.
    assert!(
        !stdout.contains("http://site.adj.ac/"),
        "stdout must not contain bare site URL: {stdout}"
    );
}

#[tokio::test]
async fn detached_head_worktree_falls_back_to_bare_name() {
    // A linked worktree on a detached HEAD has no branch to derive a label from. agent-instructions
    // is a best-effort read-and-print: it must still emit a usable doc (templated with the bare
    // name) rather than exiting non-zero and writing nothing — even though `adj add` would require
    // an explicit `--label` here.
    let home = TempDir::new().expect("home");
    let repo = TempDir::new().expect("repo dir");
    git(repo.path(), &["init", "-q"]).await;
    let manifest = "name = \"site\"\ncmd = \"npm run dev\"\n";
    tokio::fs::write(repo.path().join("adjacent.toml"), manifest)
        .await
        .expect("write manifest");
    git(repo.path(), &["add", "-A"]).await;
    git(repo.path(), &["commit", "-q", "-m", "app skeleton"]).await;

    let wt_parent = TempDir::new().expect("wt parent");
    let wt = wt_parent.path().join("wt");
    git(
        repo.path(),
        &["worktree", "add", "--detach", wt.to_str().unwrap()],
    )
    .await;

    let out = Command::new(adj_bin())
        .env("ADJACENT_HOME", home.path())
        .arg("agent-instructions")
        .arg("--path")
        .arg(&wt)
        .output()
        .await
        .expect("agent-instructions");

    assert!(
        out.status.success(),
        "agent-instructions must not fail on detached HEAD: status={:?} stderr={}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("adj status site") && stdout.contains("http://site.adj.ac/"),
        "doc should template the bare name on detached HEAD: {stdout}"
    );
    // The reason for the fallback is surfaced on stderr, not baked into the doc.
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("--label"),
        "stderr should explain the fallback: {stderr}"
    );
}
