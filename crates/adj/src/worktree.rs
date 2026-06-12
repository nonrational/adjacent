use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// Derive an instance label for `dir` when it is a linked git worktree. Returns `Ok(None)` for
/// a main checkout, a plain clone, or a non-git directory — those register under the bare app
/// name. Linked worktrees are recognizable without invoking git: their `.git` is a file (a
/// pointer into the main repo's metadata), not a directory.
pub fn detect_label(dir: &Path) -> Result<Option<String>> {
    if !dir.join(".git").is_file() {
        return Ok(None);
    }
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("running `git rev-parse --abbrev-ref HEAD`")?;
    if !out.status.success() {
        return Err(anyhow!(
            "directory looks like a git worktree but `git rev-parse` failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    let branch = String::from_utf8_lossy(&out.stdout).trim().to_string();
    // `--abbrev-ref HEAD` prints the literal string `HEAD` for a detached worktree — there is
    // no branch to name the instance after.
    if branch == "HEAD" {
        return Err(anyhow!(
            "worktree is on a detached HEAD — pass `--label <label>` to name the instance"
        ));
    }
    let label = sanitize_label(&branch);
    if label.is_empty() {
        return Err(anyhow!(
            "branch `{branch}` does not reduce to a usable DNS label — pass `--label <label>`"
        ));
    }
    Ok(Some(label))
}

/// Map a branch name onto the DNS-label charset the daemon accepts: lowercase, `/` and `_`
/// become `-`, anything else outside `[a-z0-9-]` is dropped.
pub fn sanitize_label(branch: &str) -> String {
    branch
        .to_ascii_lowercase()
        .chars()
        .map(|c| match c {
            '/' | '_' => '-',
            c => c,
        })
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_branch_names_to_dns_labels() {
        assert_eq!(sanitize_label("feature-x"), "feature-x");
        assert_eq!(sanitize_label("agents/Fix_Thing"), "agents-fix-thing");
        assert_eq!(sanitize_label("UPPER"), "upper");
        assert_eq!(sanitize_label("emoji-🦀-branch"), "emoji--branch");
        assert_eq!(sanitize_label("///"), "---");
        assert_eq!(sanitize_label("日本語"), "");
    }
}
