<!-- Lesson for PR #36. Non-Rust: README docs; teaches doc-as-contract + Rust doctests. -->

# PR #36 — Document `health_check_url`, `idle_timeout`, and `adj wait-ready` in README

> **Rust lesson:** None — this PR is pure README prose (+8/−1), no Rust code. The lesson is that user-facing docs *are* the config contract, and the Rust-ecosystem angle worth knowing is **doctests**: `cargo test` compiles and runs the ```rust``` examples in your doc comments, so documented examples can't silently rot.
> **Tags:** `docs-as-contract` · `doctests`
> **Merged:** 2026-06-08 · +8/−1 · [View PR](https://github.com/nonrational/adjacent/pull/36)

## The situation

Three features already shipped — readiness probing, idle shutdown, and `adj
wait-ready` — but the README didn't mention them. If a config key works in the
code and isn't in the docs, nobody outside the codebase knows it exists. This PR
closes that gap: three bullets plus two new lines in the example `adjacent.toml`.

## The idea (no Rust this time)

For a config-driven tool, the README *is* the contract. A user reads the example
`adjacent.toml`, copies it, and trusts that `idle_timeout = "30m"` does what the
comment says. The docs aren't decoration on top of the behavior — for anyone who
doesn't read the source, they *are* the behavior.

Which raises the perennial problem: prose drifts. Rename a key, change a default,
and the README quietly lies until someone notices. Plain Markdown like this
README has no defense against that — a human has to catch it.

## In this PR

The diff adds the optional fields to the example config, with defaults called out
in comments:

```toml
# README.md
name = "site"
cmd = "npm run dev"           # must bind to $PORT

# Optional:
health_check_url = "/healthz" # poll for 2xx instead of TCP-open
idle_timeout = "30m"          # stop after no requests (default "15m", or "off")
```

And a bullet stating the contract in prose:

```markdown
- Idle shutdown. Apps stop after no proxied requests for `idle_timeout`
  (default `"15m"`, accepts `"30s"` / `"1h"` / `"off"`).
```

Nothing here is compiled or tested. If someone later changes the default from
`"15m"` to `"20m"` in the Rust code, this Markdown keeps claiming `"15m"` and no
tool complains.

## Why it matters — the Rust angle

Rust's answer to doc drift is the **doctest**. When you put a fenced ```rust```
block in a `///` doc comment, `cargo test` extracts it, compiles it, and runs it
as a real test:

```rust
/// Parses an idle-timeout string into a `Duration`.
///
/// ```
/// # use adj::parse_timeout;
/// assert_eq!(parse_timeout("30s").unwrap().as_secs(), 30);
/// ```
pub fn parse_timeout(s: &str) -> Result<Duration, ParseError> { /* ... */ }
```

If the function signature changes or the behavior drifts, that example stops
compiling or fails its assertion, and CI goes red. The documented example
*cannot* silently rot, because it's also a test.

This README is plain Markdown, so it gets none of that — the diff doesn't
demonstrate doctests, and honesty demands saying so. But the contrast is the
lesson: a config table in prose is only as correct as the last human who read it;
a doctest is correct as long as CI is green.

## Related lessons

- PR #16 documents the supervised-app-with-logs flow — same "docs describe a
  contract the code enforces" shape, from the other direction.

## Dig deeper

- [rustdoc — Documentation tests](https://doc.rust-lang.org/rustdoc/write-documentation/documentation-tests.html) — how `cargo test` runs the examples in your docs, and the `#`-hidden-line trick shown above.
