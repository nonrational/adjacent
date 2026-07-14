<!-- Lesson for PR #49. Teaches one concept grounded in the real diff. -->

# PR #49 — Reject zero idle_timeout with a pointer to off

> **Rust lesson:** Push validation *into* the parser so an invalid value can't exist downstream — and spend the extra line making the `Err` message tell the user what to do instead.
> **Tags:** `parse-dont-validate` · `error-handling` · `result`
> **Merged:** 2026-06-13 · +21/−0 · [View PR](https://github.com/nonrational/adjacent/pull/49)

## The situation

`idle_timeout = "0s"` used to parse cleanly into a zero `Duration`. That value then flowed into the idle scanner, which compares each app's idle age against its timeout every 500ms — a zero window means the app is *always* past its timeout, so it gets SIGTERM'd on the very next tick. The app quietly stops itself forever. This PR makes `parse_idle_timeout` reject zero at the door, and point the user at the thing they almost certainly meant: `"off"`.

## The Rust idea

**"Parse, don't validate."** There are two ways to handle an untrusted input.

- *Validate:* accept it into your normal type (a `Duration`), then check `if dur.is_zero()` at each place that cares. The type says nothing — every consumer has to remember the rule, and a new call site can forget it.
- *Parse:* do the check once, at the boundary, and return a type that *cannot represent the bad state*. Downstream code holds a value that is correct by construction.

`parse_idle_timeout` returns `Result<Option<Duration>>`, and that type already encodes three distinct outcomes:

- `Err(_)` — the input was malformed or nonsensical.
- `Ok(None)` — idle shutdown is explicitly disabled (`"off"`).
- `Ok(Some(d))` — a real, positive timeout.

Before this PR, a fourth state leaked through: `Ok(Some(Duration::ZERO))`, a "valid" timeout that behaves like a self-destruct. Moving the `is_zero()` check *inside* the parser deletes that state. After the function returns `Ok(Some(d))`, every caller can trust `d` is non-zero without checking — the scanner never has to defend against a zero window, because one can't reach it.

The second half of the lesson is the `Err` itself. Rust's `Result` makes failure a value you must handle, but it says nothing about the *quality* of the error. `Err(anyhow!("invalid duration"))` is technically a failure and practically useless. An actionable message names what went wrong **and** what to do instead.

## In this PR

The check sits at the end of the parser, right before the only `Ok(Some(...))` return:

```rust
// crates/adj/src/registry.rs
if dur.is_zero() {
    return Err(anyhow!(
        "idle_timeout of zero would stop the app on every idle scan; use `off` to disable idle shutdown"
    ));
}
Ok(Some(dur))
```

That message is the whole point. It has two clauses: the *consequence* ("would stop the app on every idle scan") so the user understands why zero is refused, and the *fix* ("use `off` to disable idle shutdown") so they know the exact next keystroke. `anyhow!` is the `anyhow` crate's macro for building an ad-hoc error from a format string — the same ergonomics as `format!`, but it produces an `Err` value.

The doc comment is updated to explain the *why* for the next reader, not just the *what*:

```rust
// crates/adj/src/registry.rs
/// Zero durations are rejected: a zero window would make the app a permanent shutdown
/// candidate (stopped on every scan tick), and users writing `"0s"` almost always mean
/// "disable" — which is what `"off"` does.
```

The test pins the contract — every zero unit is rejected, and the message steers toward `off`:

```rust
// crates/adj/src/registry.rs
#[test]
fn rejects_zero_and_points_at_off() {
    for raw in ["0s", "0ms", "0m", "0h"] {
        let err = parse_idle_timeout(raw).unwrap_err();
        assert!(
            err.to_string().contains("use `off`"),
            "error for {:?} should suggest `off`, got: {}",
            raw,
            err
        );
    }
}
```

Note what it asserts: not the whole string (brittle), but that the fix — `use `off`` — is present. The test guards the *actionable* part of the message specifically.

## Why it matters

Because the check lives in the parser and `read_app_config` calls it eagerly, the error surfaces at `adj add` / `adj up` time — while the user is looking at their config — instead of manifesting hours later as a server that mysteriously won't stay up. That's the payoff of parse-don't-validate: the failure appears at the boundary, with full context, once. A language that let you pass a raw `Duration` around and hope every consumer re-checks it would push that failure deep into the runtime, far from the typo that caused it.

## Related lessons

- **PR #48** is the mirror image — the *consumer* side of this exact function. #48 is about not *swallowing* the `Err` that `parse_idle_timeout` returns (reach for `.unwrap_or_else` over `.unwrap_or`); #49 is about making that `Err` precise and worth reading in the first place. A good error is a handshake: the producer writes something actionable, the consumer bothers to surface it.
- **PR #29** covered error *enums* (giving failures distinct types). #49 is the complementary skill: even a stringly-typed `anyhow!` error earns its keep when the *message* tells the user what to do.

## Dig deeper

- [Parse, don't validate](https://lexi-lambda.github.io/blog/2019/11/05/parse-don-t-validate/) — the canonical essay. Written for Haskell, but the core move (make illegal states unrepresentable by choosing return types that exclude them) is exactly what the `Result<Option<Duration>>` signature does here.
- [The Rust Book, ch. 9](https://doc.rust-lang.org/book/ch09-00-error-handling.html) — Error Handling: `Result`, the `?` operator, and recoverable-vs-unrecoverable failure.
