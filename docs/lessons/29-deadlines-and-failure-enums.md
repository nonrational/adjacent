<!-- Lesson for PR #29. Teaches one concept grounded in the real diff. -->

# PR #29 — Readiness URL, wait-ready, and idle shutdown

> **Rust lesson:** Bound async work two ways — thread a `deadline: Instant` through a poll loop, and wrap a single hang-prone `await` in `tokio::time::timeout` — then model the outcomes as a custom enum so each caller can react to *why* the wait ended, not just *that* it did.
> **Tags:** `tokio-timeout` · `deadlines` · `error-enum`
> **Merged:** 2026-06-08 · +1102/−56 · [View PR](https://github.com/nonrational/adjacent/pull/29)

## The situation

When a request arrives for a stopped app, the proxy lazy-boots it and holds the request until the app answers. That "wait until ready" is unbounded by nature: the app might take two seconds, or crash on startup, or accept the TCP connection but never write a byte. This PR pulled the wait into `readiness.rs`, added an optional `health_check_url` (poll for a 2xx instead of a bare TCP open), and shared the same code with the new `adj wait-ready` command. Both callers need the same thing: a wait that *always* ends, and an answer that says *how* it ended.

## The Rust idea

Two distinct tools, because there are two distinct hangs to bound.

**Bounding the whole operation — a deadline.** An `Instant` is an absolute point on the monotonic clock. Compute it once (`now() + timeout`), thread it into the loop, and each pass checks `now() >= deadline`. An absolute deadline stays fixed no matter how many times you re-enter the check; a relative "time remaining" would have to be recomputed on every iteration. This bounds the *sum* of many poll attempts.

**Bounding one attempt — `tokio::time::timeout`.** A single `.await` can block forever — an HTTP GET to a socket that accepts but never responds never resolves. `tokio::time::timeout(dur, fut)` races `fut` against a timer and returns `Result<T, Elapsed>`: `Ok(value)` if the future finished first, `Err(Elapsed)` if the clock won. This bounds one attempt so a single hung probe can't stall the whole poll cadence.

**Modeling the outcome — a custom enum.** You could return `Result<u16, anyhow::Error>`, but then "crashed" and "timed out" are both just strings, and a caller who wants to treat them differently is stuck matching on message text. Instead, define an enum with one variant per outcome. Now the caller `match`es on the variant, and — the payoff — a crash can *short-circuit* the poll loop instead of waiting out the full deadline.

## In this PR

The outcome type. Note it's a plain `#[derive(Debug)]` enum — no `thiserror`, no `Display`, no `std::error::Error` impl. It never needs one: it's an internal signal that each caller matches immediately and translates into its own error surface.

```rust
// crates/adj/src/readiness.rs
#[derive(Debug)]
pub enum ReadinessError {
    /// The supervisor reports the app is not running and the deadline passed before it bound
    /// its port (or the configured health URL never returned 2xx).
    Timeout,
    /// The supervised process exited non-zero while we were waiting.
    Crashed { exit_code: i32 },
}
```

The wait itself takes the `deadline` as a parameter and polls. The `Crashed` arm returns *immediately* — that's the whole reason the enum earns its keep:

```rust
// crates/adj/src/readiness.rs
pub async fn wait_ready(
    name: &str,
    supervisor: &Supervisor,
    cfg: &AppConfig,
    deadline: tokio::time::Instant,
) -> Result<u16, ReadinessError> {
    loop {
        match supervisor.state(name).await {
            AppState::Running { port, .. } => {
                if probe_once(port, cfg.health_check_url.as_deref()).await {
                    return Ok(port);
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(ReadinessError::Timeout);
                }
                tokio::time::sleep(READY_POLL_INTERVAL).await;
            }
            AppState::Crashed { exit_code } => {
                return Err(ReadinessError::Crashed { exit_code });
            }
            AppState::Stopped => {
                if tokio::time::Instant::now() >= deadline {
                    return Err(ReadinessError::Timeout);
                }
                tokio::time::sleep(READY_POLL_INTERVAL).await;
            }
        }
    }
}
```

Inside a single probe, `tokio::time::timeout` bounds the one `await` that could hang forever — an app that completes the TCP handshake but never writes a response:

```rust
// crates/adj/src/readiness.rs
    tokio::time::timeout(Duration::from_millis(500), attempt)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
```

`timeout(...)` yields `Result<Option<bool>, Elapsed>`. `.ok()` drops the `Elapsed` into `Option<Option<bool>>`, `.flatten()` collapses the nesting, and `.unwrap_or(false)` maps *both* failure shapes — the timer fired, or the request errored — to "not ready, poll again." A timed-out probe is not fatal; it just means try once more before the outer deadline.

The payoff lands at the callers, where the *same two variants* fan out to *different* surfaces. In the daemon's `wait-ready` handler each becomes a distinct CLI-facing message:

```rust
// crates/adj/src/daemon.rs
    match readiness_wait(&name, supervisor.as_ref(), &cfg, deadline).await {
        Ok(_) => Ok(Response::Ok),
        Err(ReadinessError::Timeout) => Err(anyhow!(
            "app `{name}` did not become ready within {timeout:?}"
        )),
        Err(ReadinessError::Crashed { exit_code }) => Err(anyhow!(
            "app `{name}` crashed while waiting for ready (exit {exit_code})"
        )),
    }
```

The proxy's boot path matches the identical enum but maps `Timeout` to an HTTP 504 and `Crashed` to a 502. One outcome type, two error surfaces, zero string-sniffing.

## Why it matters

Collapse `ReadinessError` back into a single `anyhow::Error` string and two things break. First, the boot path loses the short-circuit: a crash would fall through as "still not ready," and the proxy would poll a dead process until the 60-second deadline before finally giving up — a full minute of a user staring at a hung request for an app that died in the first 200ms. The `Crashed` variant is what lets the loop bail the instant the supervisor reports the exit. Second, without either bound — the outer deadline or the inner `timeout` — a never-ready app pins the request (or the `adj wait-ready` process) forever. A language with exceptions would let you `throw` on timeout, but you'd still be hand-rolling the "which failure was it" dispatch on message strings; the enum makes the answer a type the compiler checks you handled.

## Related lessons

- PR #48 revisits the combinator move in this very file — `.ok().flatten().unwrap_or(false)` deliberately swallows the `Elapsed` and error cases because a failed probe is a *recovery*, not a mistake. #48 is about the opposite call: when swallowing an error is the wrong default.
- PR #16 introduced the `Arc<Supervisor>` that `wait_ready` borrows as `&Supervisor` — the shared, lock-guarded state the poll loop reads on every pass.
- PR #52 later hardens the inner probe: the diff here relied on hyper's drop behavior to tear down the connection after a timed-out attempt, and #52 replaces that with an explicit task `abort()` — the cancellation counterpart to this lesson's timeout.

## Dig deeper

- [`tokio::time::timeout`](https://docs.rs/tokio/latest/tokio/time/fn.timeout.html) — the future-vs-timer race and its `Result<T, Elapsed>` return.
- [`thiserror`](https://docs.rs/thiserror/latest/thiserror/) — reach for it the day `ReadinessError` needs a `Display` impl to cross an API boundary; here a plain `Debug` enum matched at the call site was enough.
