//! Build-time version stamping. Prefer an explicit `ADJ_VERSION` (CI sets it to the release tag),
//! else fall back to `git describe` so local builds self-report their commit. When neither is
//! available (e.g. a source build with no `.git`), `main.rs` falls back to `CARGO_PKG_VERSION`.

use std::path::Path;
use std::process::Command;

fn git_describe() -> Option<String> {
    let out = Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty", "--match", "v*"])
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
    // Refresh the git-describe value when HEAD moves; workspace root is two levels up from here.
    if Path::new("../../.git/HEAD").exists() {
        println!("cargo:rerun-if-changed=../../.git/HEAD");
    }
}
