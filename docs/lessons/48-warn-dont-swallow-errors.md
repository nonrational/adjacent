<!-- Lesson for PR #48. Teaches one concept grounded in the real diff. -->

# PR #48 — Warn instead of silently defaulting on idle_timeout parse failure

> **Rust lesson:** A `Result` is a value you must consume, but combinators like `.unwrap_or(default)` let you throw the `Err` away — reach for `.unwrap_or_else(|err| …)` when the error is worth surfacing before you fall back.
> **Tags:** `result` · `error-handling` · `combinators`
> **Merged:** 2026-06-13 · +75/−2 · [View PR](https://github.com/nonrational/adjacent/pull/48)

## The situation

A bad `idle_timeout` string in `adjacent.toml` (say `"10x"`) used to fall back to the 15-minute default without a peep. `idle_timeout_for` called `.unwrap_or(Some(DEFAULT_IDLE_TIMEOUT))` on the parse `Result`, discarding the parse error. Today that path is unreachable — `read_app_config` validates eagerly — but a future caller that skips validation would turn a config typo into a silent default. This PR keeps the fallback but makes it observable.

## The Rust idea

Rust has no exceptions. A function that can fail returns `Result<T, E>` — an enum with two variants, `Ok(T)` and `Err(E)`. The failure is an ordinary value the caller holds in their hand, and the type system won't let you read the `T` without first acknowledging the `Err` might be there. That is the whole safety story: fallibility is visible in the signature and unavoidable at the call site.

But "unavoidable" is not the same as "surfaced." The standard library ships combinators that collapse a `Result` down to a plain value, and some of them drop the error on the floor:

- `.unwrap_or(default)` — returns the `Ok` value, or `default` on `Err`. The `E` is never bound to anything; it's gone.
- `.ok()` — turns `Result<T, E>` into `Option<T>`, discarding `E`.
- `let Ok(x) = … else { … }` — the `else` arm can't see the error either.

Each one is a legitimate tool and each one silently swallows the `Err`. The compiler is happy — you *did* handle the `Result` — so the type system stops nudging you the moment you pick a combinator that ignores the payload. Swallowing an error is a code smell precisely because it looks identical to handling it.

The fix is the lazy sibling: `.unwrap_or_else(|err| …)` takes a closure that receives the `Err` value. Now the error is bound to `err`, and you can log it, count it, or enrich it before returning your fallback. Same default, but the failure is no longer invisible.

(`_else` also means the fallback is computed lazily — the closure only runs on `Err` — but here the real win is getting your hands on `err`.)

## In this PR

```rust
// crates/adj/src/registry.rs

// before:
Some(s) => parse_idle_timeout(s).unwrap_or(Some(DEFAULT_IDLE_TIMEOUT)),

// after:
// Unreachable when callers go through `read_app_config`, which validates eagerly.
// Warn loudly anyway so a future code path that skips validation can't turn a
// config typo into a silent default.
Some(s) => parse_idle_timeout(s).unwrap_or_else(|err| {
    tracing::warn!(
        idle_timeout = s,
        error = %err,
        default = ?DEFAULT_IDLE_TIMEOUT,
        "invalid idle_timeout; falling back to default"
    );
    Some(DEFAULT_IDLE_TIMEOUT)
}),
```

The signature never changed — `idle_timeout_for` still returns `Option<Duration>`, so nothing downstream in `supervisor.rs` has to grow an error path. The only difference is that the `err` from `parse_idle_timeout` now flows into the closure instead of being dropped. `%err` formats it with `Display`, `?DEFAULT_IDLE_TIMEOUT` with `Debug` — both are `tracing`'s field syntax for capturing structured values.

The test pins the one observable change. Since the returned value is identical before and after, the assertion has to capture the log output:

```rust
// crates/adj/src/registry.rs
let resolved = tracing::subscriber::with_default(subscriber, || {
    idle_timeout_for(&cfg_with_idle_timeout(Some("10x")))
});

assert_eq!(resolved, Some(DEFAULT_IDLE_TIMEOUT));
let logs = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
assert!(
    logs.contains("invalid idle_timeout") && logs.contains("10x") && logs.contains("900s"),
    "expected a warn naming the bad value and the default it fell back to, got: {logs}"
);
```

## Why it matters

In a language with exceptions, the equivalent bug is a bare `catch (Exception e) {}` — the swallowed-error smell everyone recognizes. Rust doesn't force you to write that; it hands you a tidy one-liner (`.unwrap_or`) that does exactly the same thing and reads as intentional. The lesson is that Rust guarantees you *considered* the `Result`, not that you *surfaced* it. When the fallback is a real recovery worth keeping quiet about, `.unwrap_or` is fine. When the `Err` means "someone made a mistake" — a config typo, a malformed input — reach for `.unwrap_or_else` and at least say so on the way down.

## Related lessons

- PR #49 tightens the *producer* side of this same function neighborhood: it makes `parse_idle_timeout` return a precise `Err` (rejecting zero durations with a message pointing at `off`). #48 is about not swallowing that `Err`; #49 is about making it worth reading.

## Dig deeper

- [The Rust Book, ch. 9](https://doc.rust-lang.org/book/ch09-00-error-handling.html) — Error Handling (`Result`, the `?` operator, and when to recover vs. propagate)
