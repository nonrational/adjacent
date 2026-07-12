<!-- Lesson for PR #38. Teaches one concept grounded in the real diff. -->

# PR #38 — Fix flaky proxy single-flight test and add CI workflow

> **Rust lesson:** A deterministic async test drives real concurrency and asserts on *observable state* you can count, not on timing — and when such a test still flakes, the flake is a real race in the code, not test noise.
> **Tags:** `#[tokio::test]` · `test-determinism`
> **Merged:** 2026-06-08 · +45/−16 · [View PR](https://github.com/nonrational/adjacent/pull/38)

## The situation

A test fired three concurrent first-requests at a cold app and asserted two things:
every request got `200`, and the app booted exactly once. It passed locally, then
flaked in CI with a `502`. The obvious "fix" — sprinkle a `sleep`, bump a timeout —
would have buried the problem. The real fix was one deletion in the proxy. The PR also
added `.github/workflows/ci.yml` (build + test on every PR, toolchain pinned to 1.92.0,
cargo cache) so flakes surface on a clean machine instead of only on someone's laptop.

## The Rust idea

`#[tokio::test]` is the async cousin of `#[test]`. A plain `#[test]` can't `.await`
anything — there's no runtime to drive the futures. The macro wraps your `async fn` so
it spins up a Tokio runtime, runs the test to completion, and tears the runtime down.
That's all it does; the *discipline* is on you.

To test concurrency deterministically, two rules:

1. **Drive real concurrent work.** `tokio::task::spawn_blocking` hands each closure to a
   real OS thread, so three of them race for genuine. Don't simulate the race — cause it.
2. **Assert on observable state, never on timing.** A `sleep(100ms)` is a *guess* that the
   system finished within 100ms. On a loaded CI box it didn't, and the test flakes. A
   count of lines in a file, or an HTTP status code, is a *fact*. Assert the fact.

The test already did both. It spawns three real requests, then checks a spawn-counter file:

```rust
// crates/adj/tests/proxy.rs  (existing test, unchanged by this PR)
for h in handles {
    let (status_line, body) = h.await.expect("join").expect("http_get");
    assert!(status_line.contains(" 200 "), "status: {status_line}");
}
// ...
assert_eq!(spawns, 1, "expected single boot, saw {spawns} spawns");
```

So why flake? Because a correctly-written deterministic test that flakes is telling you
the *code under test* has a race. Here it did.

## In this PR

The proxy had a "fast path" that returned a port the instant the supervisor reported
`Running` — skipping the readiness wait. But the supervisor flips to `Running` the moment
it spawns the child shell, *before* the child has bound its port. A second concurrent
request that hit the fast path in that window forwarded to a dead socket: `502`.

```rust
// crates/adj/src/proxy.rs  —  removed (the flaky fast path)
if let AppState::Running { port, .. } = supervisor.state(name).await {
    return Ok(port);
}
```

The fix routes *every* request through the per-name lock and then `wait_ready`, which
polls the app's real readiness (a TCP probe that actually connects) before returning:

```rust
// crates/adj/src/proxy.rs  —  the fix
// We intentionally do NOT short-circuit on a Running state outside the lock. The supervisor
// flips state to Running the moment it spawns the child — before the child has bound its
// port. [...] would forward to a port that wasn't accepting yet and get back a spurious 502.
let name_lock = gate.lock_for(name).await;
let _guard = name_lock.lock().await;

if !matches!(supervisor.state(name).await, AppState::Running { .. }) {
    supervisor.up(entry.path.clone(), cfg.clone()).await.map_err(ProxyError::Other)?;
}
// ...falls through to wait_ready before forwarding
```

Notice the symmetry. The *test* waits on observable state (a spawn count), and the *fix*
makes the proxy wait on observable state (a live TCP connection) instead of trusting a
`Running` enum that leads reality by a few microseconds. Same principle on both sides:
don't trust a signal that fires before the thing it signals is true.

## Why it matters

In most languages the reflex for a flaky concurrency test is to add a delay until it goes
green. That doesn't remove the race — it just widens the window so the race loses *most*
of the time, and the `502` returns under production load when no test is watching. Rust
doesn't save you from this; the discipline does. Assert facts you can count, drive the
real race, and when a deterministic test flakes, hunt the bug in the code rather than
padding the test. The single-line deletion here is the whole fix — the test was right all
along.

## Related lessons

- **PR #24** built the per-name single-flight `BootGate` this test exercises — the `lock_for(name)` + re-check pattern is the code under test here.
- **PR #16** covers the `Arc<Mutex>` sharing that lets the supervisor state and boot gate be read from many concurrent request tasks at once.

## Dig deeper

- [Tokio docs — Testing](https://docs.rs/tokio/latest/tokio/attr.test.html) — what `#[tokio::test]` actually expands to, and its runtime flavors.
- [The Rust Book, ch. 11](https://doc.rust-lang.org/book/ch11-00-testing.html) — writing and running tests.
