<!-- Lesson for PR #63. Teaches one concept grounded in the real diff. -->

# PR #63 — Add a fmt + clippy lint job to CI

> **Rust lesson:** Clippy is a Rust teacher wearing a linter's badge — each lint names an idiom (`single_match` → `if let`) or a footgun (`await_holding_lock`, `dead_code`), and the honest reply is either to adopt the idiom or to `#[allow]` it *with a reason*.
> **Tags:** `clippy` · `rustfmt` · `lints` · `idioms`
> **Merged:** 2026-06-14 · +604/−248 · [View PR](https://github.com/nonrational/adjacent/pull/63)

## The situation

The repo had never been gated on formatting or lints. This PR adds a CI job that runs two commands and fails the build if either complains:

```yaml
# .github/workflows/ci.yml
- run: cargo fmt --all -- --check
- run: cargo clippy --workspace --all-targets -- -D warnings
```

To land that gate green it had to fix everything the two commands surfaced. Be honest about the split: the vast majority of the +604/−248 is one mechanical `cargo fmt --all` commit — struct literals expanded to multi-line, long calls wrapped — with no behavior change and nothing to learn. The interesting part is a tiny second commit (+46/−7) where `clippy -D warnings` forced three real changes. That's the lesson: not the formatter, but what clippy *taught*.

## The Rust idea

Clippy ships ~700 lints, and almost none of them are about whitespace — rustfmt owns that. Clippy is about *idiom* and *correctness*. Each lint encodes a small piece of "here's how a seasoned Rust programmer would write this, and why." Running it is less like a spell-check and more like pairing with someone who keeps saying "there's a cleaner way." `-D warnings` (deny all warnings) turns that advice into a hard gate: the build fails until you either take the advice or explicitly, in writing, decline it.

Declining is a first-class move. `#[allow(clippy::some_lint)]` says "I read this lint, I understand what it's warning about, and it doesn't apply here." A clippy pass done right leaves behind a mix of both: code you improved, and a few `#[allow]`s each carrying a comment that explains the disagreement. This PR has one of each kind.

## In this PR

**1. `single_match` → `if let`.** Clippy's `single_match` fires when a `match` has one arm that does real work and a second arm that does nothing (`_ => {}`). Rust already has a construct for "run this code only if the value matches one pattern, otherwise skip" — that's `if let`. A two-arm match with an empty catch-all is `if let` wearing a costume, and reading it forces you to scan to the bottom to confirm the `_` really is empty.

```rust
// crates/adj/src/tls.rs — ensure_leaf
// before:
match (fs::read_to_string(&cert_path), fs::read_to_string(&key_path)) {
    (Ok(cert), Ok(key)) => match leaf_covers(&cert, sans) {
        Ok(true) => return Ok((cert, key)),
        Ok(false) | Err(_) => {}
    },
    _ => {}
}

// after:
if let (Ok(cert), Ok(key)) = (
    fs::read_to_string(&cert_path),
    fs::read_to_string(&key_path),
) {
    if let Ok(true) = leaf_covers(&cert, sans) {
        return Ok((cert, key));
    }
}
```

Both matches collapse the same way. Semantics are identical — only a readable, SAN-covering cached leaf short-circuits; everything else (a read error, a parse error, a cert that doesn't cover the wanted names) falls through to re-issue. But the `if let` version *says* that: "if both files read, and the leaf covers the SANs, use it." The `_ => {}` and `Ok(false) | Err(_) => {}` filler arms are gone, so there's nothing to double-check.

**2. `await_holding_lock` — the documented decline.** This lint flags holding a `std::sync::MutexGuard` across an `.await`. It's a real footgun: on a multi-threaded async runtime the task can be parked mid-await while still holding the lock, and another task on that same thread can block forever trying to acquire it — a deadlock. Clippy is right to warn by default. Here it's a false alarm, and the fix is to say so, not to restructure working test code:

```rust
// crates/adj/tests/tls.rs
#[cfg(target_os = "macos")]
#[tokio::test]
// The keychain guard intentionally spans the test's awaits to serialize on LOGIN_KEYCHAIN_LOCK;
// a current-thread tokio runtime can't deadlock on it, so the std-guard-across-await lint is moot.
#[allow(clippy::await_holding_lock)]
async fn install_ca_generates_files_and_prints_macos_command() {
```

The `#[allow]` is localized to the three tests that need it, not slapped on the whole crate. That matters: if the *supervisor* ever grows a real lock-across-await, the lint stays live to catch it. A repo-wide `#![allow]` would have silenced the warning everywhere and thrown away its value.

**3. `dead_code` / `unused_imports` under a cross-platform gate.** The clippy job runs on `ubuntu-latest`, but the keychain code is `#[cfg(target_os = "macos")]` — it compiles to nothing on Linux. So on the CI runner, every helper that only the macOS path uses becomes genuinely dead code, and `-D warnings` promotes those warnings to errors. The fix is to `#[cfg]`-gate the now-unused items to match their only caller:

```rust
// crates/adj/src/tls.rs
pub(crate) use keychain::delete as delete_keychain_ca;
// Only the macOS doctor checks load a keychain handle; on other targets the re-export is unused.
#[cfg(target_os = "macos")]
pub(crate) use keychain::load as load_keychain_ca;
```

...and for a stub that exists only to mirror an API on the platform where it's never called, a deliberate `#[allow(dead_code)]`:

```rust
// crates/adj/src/tls/keychain.rs — the non-macOS stub
// Mirrors the macOS KeychainKey API for symmetry; the non-macOS doctor path never calls
// it, so it's dead on these targets.
#[allow(dead_code)]
pub fn sign_canary(&self) -> Result<()> {
    Err(unsupported())
}
```

Same test scaffolding (`with_temp_home`, `HOME_LOCK`, the `curl_available` helper) got the same `#[cfg(target_os = "macos")]` treatment.

## Why it matters

Two takeaways a linter-skeptic misses.

First, `dead_code` under `--all-targets -D warnings` on a Linux runner is a *free cross-compilation check*. Adjacent is mostly developed on a Mac, and it's easy to leave a helper unused on platforms you never build locally. The CI job compiles the non-macOS `cfg` branches you'd otherwise never exercise, and catches the drift. The lint isn't nagging about tidiness — it's proving that every target still hangs together.

Second, a green clippy run doesn't mean "no lints fired." It means "every lint that fired was either fixed or explicitly declined with a reason." That's the discipline `-D warnings` enforces: you can't ignore advice by scrolling past it. You either take it, or you write down why you didn't — and the next reader gets that reasoning for free. An `#[allow]` with a comment is worth more than clean code that silently dodged the question.

## Related lessons

- PR #40 shipped the HTTPS + local-CA machinery. The `ensure_leaf` match and the keychain helpers clippy is cleaning up here are exactly that code — this PR is #40's neighborhood one linter-pass later.
- PR #48 is the same spirit in a different tool: it's about surfacing a `Result`'s `Err` instead of swallowing it. Clippy and the type system are both in the business of making the invisible visible — this PR just does it at the linter layer.

## Dig deeper

- [The Clippy lint index](https://rust-lang.github.io/rust-clippy/) — every lint, searchable, each with a before/after and the reasoning behind it. `single_match`, `await_holding_lock`, and the rustc `dead_code`/`unused_imports` lints all have entries worth reading.
