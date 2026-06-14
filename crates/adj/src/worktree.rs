use std::path::Path;

use anyhow::{anyhow, Context, Result};

/// Derive an instance label for `dir` when it is a linked git worktree. Returns `Ok(None)` for
/// a main checkout, a plain clone, a git submodule, or a non-git directory — those register
/// under the bare app name.
///
/// A linked worktree's `.git` is a file whose first line is `gitdir: <common>/.git/worktrees/<id>`.
/// A submodule's `.git` is also a file, but the pointer goes through `/modules/` instead.
/// We inspect the pointer to distinguish them — cheap string check, no extra git invocations.
pub fn detect_label(dir: &Path) -> Result<Option<String>> {
    if !dir.join(".git").is_file() {
        return Ok(None);
    }

    // Read the gitdir pointer and inspect it. A linked worktree always has the shape:
    //   gitdir: <common>/.git/worktrees/<id>
    // A submodule has the shape:
    //   gitdir: <root>/.git/modules/<name>
    // Anchor on the git-internal `/.git/worktrees/` segment, not a bare `/worktrees/` substring:
    // a submodule whose repo simply lives under a directory named `worktrees`
    // (`…/worktrees/super/.git/modules/sub`) contains `/worktrees/` but not `/.git/worktrees/`,
    // and must not be mistaken for a linked worktree.
    let gitfile = std::fs::read_to_string(dir.join(".git")).context("reading .git file")?;
    let first_line = gitfile.lines().next().unwrap_or("").trim();
    let pointer = first_line
        .strip_prefix("gitdir:")
        .map(str::trim)
        .unwrap_or("");
    if !pointer.contains("/.git/worktrees/") {
        return Ok(None);
    }

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--abbrev-ref", "HEAD"])
        .output()
        .context("running `git rev-parse --abbrev-ref HEAD` (pass `--label` to skip detection)")?;
    if !out.status.success() {
        return Err(anyhow!(
            "directory looks like a git worktree but `git rev-parse` failed: {} — pass `--label <label>` to name the instance",
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
/// become `-`, anything else outside `[a-z0-9-]` is dropped. Edge hyphens are trimmed and
/// the result is capped at 63 octets (the DNS label limit).
pub fn sanitize_label(branch: &str) -> String {
    let raw: String = branch
        .to_ascii_lowercase()
        .chars()
        .map(|c| match c {
            '/' | '_' => '-',
            c => c,
        })
        .filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-')
        .collect();

    // Trim leading/trailing hyphens that fall-out from non-alphanumeric branch prefixes/suffixes
    // (e.g. `_wip` → `-wip` → `wip`), then cap at the DNS label maximum of 63 octets, then
    // trim again in case truncation exposed a trailing hyphen.
    let trimmed = raw.trim_matches('-');
    let truncated = &trimmed[..trimmed.len().min(63)];
    truncated.trim_end_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn sanitizes_branch_names_to_dns_labels() {
        assert_eq!(sanitize_label("feature-x"), "feature-x");
        assert_eq!(sanitize_label("agents/Fix_Thing"), "agents-fix-thing");
        assert_eq!(sanitize_label("UPPER"), "upper");
        assert_eq!(sanitize_label("emoji-🦀-branch"), "emoji--branch");
        // All slashes map to hyphens, then edge hyphens are trimmed → empty string.
        assert_eq!(sanitize_label("///"), "");
        assert_eq!(sanitize_label("日本語"), "");
        // Leading/trailing non-alphanumeric chars produce edge hyphens that must be trimmed.
        assert_eq!(sanitize_label("_wip"), "wip");
        assert_eq!(sanitize_label("--x--"), "x");
        // Branches longer than 63 chars must be capped and must not end in a hyphen.
        let long = "a".repeat(30) + "-" + &"b".repeat(40);
        let result = sanitize_label(&long);
        assert!(result.len() <= 63, "len {} > 63", result.len());
        assert!(!result.ends_with('-'), "trailing hyphen in {result:?}");
    }

    #[test]
    fn submodule_dot_git_file_returns_none() {
        // A submodule's .git pointer goes through /modules/, not /worktrees/.
        // detect_label must return Ok(None) without invoking git.
        let dir = TempDir::new().expect("tempdir");
        let mut f = std::fs::File::create(dir.path().join(".git")).expect("create .git");
        writeln!(f, "gitdir: /some/repo/.git/modules/sub").expect("write");
        drop(f);

        let result = detect_label(dir.path()).expect("no error for submodule");
        assert!(
            result.is_none(),
            "submodule should return Ok(None), got {result:?}"
        );
    }

    #[test]
    fn submodule_under_worktrees_named_dir_returns_none() {
        // A submodule whose superproject simply lives under a directory named `worktrees`. The
        // pointer contains `/worktrees/` but goes through `/.git/modules/`, so anchoring on the
        // bare substring would misclassify it as a linked worktree and derive a branch label.
        let dir = TempDir::new().expect("tempdir");
        let mut f = std::fs::File::create(dir.path().join(".git")).expect("create .git");
        writeln!(f, "gitdir: /home/me/worktrees/super/.git/modules/sub").expect("write");
        drop(f);

        let result = detect_label(dir.path()).expect("no error for submodule");
        assert!(
            result.is_none(),
            "submodule under a `worktrees`-named dir should return Ok(None), got {result:?}"
        );
    }
}
