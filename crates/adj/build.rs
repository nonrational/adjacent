//! Build-time version stamping. Prefer an explicit `ADJ_VERSION` (CI sets it to the release tag),
//! else fall back to `git describe` so local builds self-report their commit. When neither is
//! available (e.g. a source build with no `.git`), `main.rs` falls back to `CARGO_PKG_VERSION`.
//!
//! A dirty working tree appends `+` (e.g. `0.1.0-alpha.2-2-g5b5c283+`) so a build with
//! uncommitted changes is never mistaken for the pristine commit it sits on.

use std::path::Path;
use std::process::Command;

fn git_describe() -> Option<String> {
    let out = Command::new("git")
        .args([
            "describe",
            "--tags",
            "--always",
            // `=+` overrides the default `-dirty` suffix: a dirty tree gets a trailing `+`.
            "--dirty=+",
            "--match",
            "v*",
        ])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let v = String::from_utf8(out.stdout).ok()?;
    let v = v.trim().trim_start_matches('v').to_string();
    (!v.is_empty()).then_some(v)
}

fn main() {
    let version = std::env::var("ADJ_VERSION")
        .ok()
        .filter(|s| !s.is_empty())
        .or_else(git_describe);

    if let Some(v) = version {
        println!("cargo:rustc-env=ADJ_VERSION={v}");
    }

    println!("cargo:rerun-if-env-changed=ADJ_VERSION");
    // Re-stamp when the git state changes; workspace root is two levels up from here. HEAD covers
    // commits and checkouts; the index covers staging, which is what flips `--dirty` on or off.
    // (Unstaged-only edits don't touch either file, so they won't re-stamp until staged or built
    // clean — an accepted limitation of caching the version in a build script.)
    for git_path in ["../../.git/HEAD", "../../.git/index"] {
        if Path::new(git_path).exists() {
            println!("cargo:rerun-if-changed={git_path}");
        }
    }
}
