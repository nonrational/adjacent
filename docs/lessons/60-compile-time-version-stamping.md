<!-- Lesson for PR #60. Teaches one concept grounded in the real diff. -->

# PR #60 — Alpha Homebrew distribution: release CI, in-repo tap formula, version stamping

> **Rust lesson:** A `build.rs` script runs *before* your crate compiles and can hand values to the compiler via `cargo:rustc-env=…` stdout directives, which `env!` (build fails if the var is missing) and `option_env!` (yields an `Option`, never fails) then bake into the binary as `&'static str` literals.
> **Tags:** `build-scripts` · `compile-time-env` · `macros`
> **Merged:** 2026-06-14 · +243/−1 · [View PR](https://github.com/nonrational/adjacent/pull/60)

## The situation

Shipping `adj` via Homebrew means `adj --version` has to report something true. A tagged release should say `0.1.0-alpha.1`; a build off a random local commit should say which commit (and whether the tree was dirty). None of that lives in the source — it lives in git and in CI env vars. This PR is mostly release plumbing (a Homebrew formula, a tag-triggered CI workflow, a `just release` target), but the one genuinely instructive Rust piece is how the binary *learns its own version* at compile time.

## The Rust idea

Rust has no runtime reflection for build metadata. A binary can't ask "what version am I?" from thin air — anything it knows must be baked in at compile time or read from disk at runtime. For a version string you want it baked in: a self-contained binary with no sidecar `VERSION` file to lose.

Rust gives you two macros that read the *compiler's* environment:

- **`env!("NAME")`** — expands to the value of environment variable `NAME` at compile time, as a `&'static str`. If `NAME` isn't set, **the build fails** with a clear error. Use it when the value must exist. Cargo always sets `CARGO_PKG_VERSION` from `Cargo.toml`, so `env!("CARGO_PKG_VERSION")` is guaranteed to compile.
- **`option_env!("NAME")`** — same idea, but expands to `Option<&'static str>`: `Some(v)` if the var was set at compile time, `None` if not. It **never fails the build**. Use it for metadata that may or may not be there.

The `!` marks these as macros, not functions. A function's argument is evaluated at runtime; these run *during compilation* and leave a string literal behind in the compiled code. That's the only way to reach the environment the compiler saw.

But where does a var like `ADJ_VERSION` come from during compilation? That's what **`build.rs`** is for. Cargo compiles and runs `build.rs` before compiling the crate, and the script talks back to Cargo by printing specially-formatted lines to stdout — `cargo:rustc-env=KEY=VALUE` sets an env var that `env!`/`option_env!` can then read while the crate compiles.

## In this PR

The const in `main.rs` picks the git/CI-derived version when present, else the Cargo version:

```rust
// crates/adj/src/main.rs
const VERSION: &str = match option_env!("ADJ_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};
```

The whole expression is a `const`, so it's evaluated at compile time — `match` works in const context. `option_env!("ADJ_VERSION")` is `Some` only if the build set that var; otherwise it falls through to `env!("CARGO_PKG_VERSION")`, which can't fail because Cargo always provides it. Then clap uses it:

```rust
// crates/adj/src/main.rs
#[command(
    name = "adj",
    version = VERSION,   // was: version,  (which defaults to CARGO_PKG_VERSION)
    about = "Adjacent: supervised local dev servers"
)]
```

The build script supplies `ADJ_VERSION`:

```rust
// crates/adj/build.rs
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
```

Walk the chain:

1. CI sets `ADJ_VERSION` to the release tag (the workflow's `Derive version from tag` step strips the leading `v`). Locally that var is unset, so the script runs `git describe --tags --always --dirty` — which yields `0.1.0-alpha.1` on the exact tag, or an abbreviated commit like `61b2f6e` off a plain commit, with a `-dirty` suffix appended when the working tree has uncommitted changes.
2. The script prints `cargo:rustc-env=ADJ_VERSION=<v>`, which sets that var for the crate compile that follows.
3. `option_env!("ADJ_VERSION")` in `main.rs` sees it and bakes it into `const VERSION`.

The two trailing directives are cache control. Build scripts are cached aggressively, so without a hint Cargo would keep the stale value forever. `cargo:rerun-if-env-changed=ADJ_VERSION` re-runs the script when that var changes; `cargo:rerun-if-changed=../../.git/HEAD` re-runs it when you commit (HEAD moves), so `git describe` refreshes. Build scripts run with their working directory set to the package root (`crates/adj`), which is why the repo's `.git/HEAD` is `../../` up.

Note the `.ok()?` calls in `git_describe`: they intentionally throw away git's errors and return `None`, which makes the whole thing fall back to the Cargo version. That's the *legitimate* face of swallowing a `Result` (see #48) — a source build with no `.git` directory shouldn't fail to compile just because `git describe` can't run.

The rest of the PR is distribution tooling, not Rust: `.github/workflows/release.yml` (tag-triggered arm64 build + GitHub prerelease), `Formula/adj.rb` and `render-formula.sh` (the in-repo Homebrew tap), and a `just release` target that tags the next alpha. Worth knowing they exist; not where the Rust lesson lives. The user-facing `brew install` / `cargo install` side is #69's subject.

## Why it matters

In many ecosystems you'd read the version from a file shipped next to the binary (a `package.json`, a bundled `VERSION`) or hardcode a string you forget to bump. Both drift, and the file can go missing. Compile-time stamping makes the binary self-describing with zero runtime dependencies. And the `env!` vs `option_env!` split encodes intent in the type: `env!` says "this must exist, fail the build if it doesn't" and hands you a `&str`; `option_env!` says "this might exist" and hands you an `Option<&str>` you're forced to handle. Typo `CARGO_PKG_VERSION` and the build stops immediately — you never ship a binary that reports the wrong version because a lookup silently returned empty.

## Related lessons

- PR #48 — *Warn instead of silently defaulting.* `build.rs` here uses `.ok()?` to deliberately drop git's errors and fall back; #48 is about the cases where that silent drop is a bug. Same combinator, opposite verdict — the difference is whether the `Err` means "someone made a mistake."

## Dig deeper

- [The Cargo Book — Build Scripts](https://doc.rust-lang.org/cargo/reference/build-scripts.html) — what `build.rs` can do and the full list of `cargo:` output directives (`rustc-env`, `rerun-if-changed`, `rerun-if-env-changed`).
- [std — `env!`](https://doc.rust-lang.org/std/macro.env.html) and [`option_env!`](https://doc.rust-lang.org/std/macro.option_env.html) — the compile-time environment macros, and why one fails the build while the other returns `Option`.
