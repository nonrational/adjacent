<!-- Lesson for PR #52. Teaches one concept grounded in the real diff. -->

# PR #52 — Abort the readiness probe's connection task when the probe resolves

> **Rust lesson:** `tokio::spawn` returns a `JoinHandle`, and *dropping* that handle detaches the task so it keeps running on its own — to stop a background task once you're done with it, hold the handle and call `.abort()` (a no-op if it already finished), and for the leak-proof version let a `Drop` guard fire the abort on every exit path.
> **Tags:** `tokio-spawn` · `join-handle-abort` · `task-cancellation`
> **Merged:** 2026-06-14 · +86/−5 · [View PR](https://github.com/nonrational/adjacent/pull/52)

## The situation

The HTTP readiness probe (added in #29) opens a TCP connection to the app and speaks HTTP/1 through hyper. Hyper's client splits into two halves: a `sender` you push the request into, and a `conn` future that must be *driven* to actually pump bytes on the socket. The idiom is to `tokio::spawn` that `conn` future so it runs in the background while you await the response on `sender`.

But the whole probe is wrapped in a 500ms `tokio::time::timeout`. If the timer wins, the `attempt` future is dropped mid-flight — and that spawned connection task, which owns a real TCP socket, is left running with nobody watching it. This PR keeps a handle to that task and aborts it once the probe resolves, timeout or not, so the task and its socket can't outlive the probe.

## The Rust idea

Two facts a newcomer needs about spawned tasks.

**1. Dropping a `JoinHandle` does not stop the task — it *detaches* it.** `tokio::spawn(fut)` hands the future to the runtime and returns a `JoinHandle<T>`. Await the handle and you get the task's output. But here's the surprise if you're coming from threads, or from "a future is cancelled when you drop it": dropping the *handle* to a spawned task doesn't cancel anything. The task keeps running to completion on its own. A bare `tokio::spawn(...)` that throws the handle away is fire-and-forget — fine for work that finishes by itself, a leak for work that might not.

**2. Cancellation is explicit: `JoinHandle::abort()`.** It doesn't kill the task synchronously. It flags the task so the runtime drops it at its next `.await` point (or never starts it, if it hadn't run yet). Dropping the task runs the destructors of everything it owns — here, the hyper `conn` and its TCP socket. And `abort()` on an already-finished task is a harmless no-op, so calling it unconditionally after the probe is always safe: either it reclaims a lingering task, or it does nothing.

The subtle part is *guaranteeing* the abort runs. If you call it by hand, every exit path has to reach that call — every early `return`, every `?`, every panic. Miss one and the task leaks on that path. The RAII fix is the same one the language uses for locks and files: wrap the handle in a tiny struct whose `Drop` impl calls `abort()`, so the compiler runs it at scope exit on *every* path.

```rust
// Sketch — the general RAII form (not the exact shape #52 shipped; see below).
struct AbortOnDrop(tokio::task::JoinHandle<()>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}
```

`Drop` running on every normal exit is exactly what makes this leak-proof — and it's the flip side of PR #37, which is the deep dive on `Drop`'s reach *and* its one blind spot (a signal walks right past it).

## In this PR

The handle is parked on a slot declared *outside* both the `attempt` future and the `timeout`, so it survives even when the timer drops `attempt` mid-flight. `Option` because the spawn happens partway through `attempt` — the connect might fail with `?` before we ever spawn, so the slot is `Some` only once we got far enough to have a task.

```rust
// crates/adj/src/readiness.rs
let mut conn_task: Option<tokio::task::JoinHandle<()>> = None;
let attempt = async {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let stream = TcpStream::connect(addr).await.ok()?;
    let io = TokioIo::new(stream);
    let (mut sender, conn) = client_http1::handshake::<_, Empty<Bytes>>(io).await.ok()?;
    conn_task = Some(tokio::spawn(async move {
        let _ = conn.await;
    }));
    // ... build + send the request, read the status, drain the body ...
    Some(status.is_success())
};
```

Parking the handle *outside* the timed-out future is the crux: the thing that owns the task has to outlive the thing that might get cancelled. Then, once the probe has resolved one way or another, the abort fires unconditionally:

```rust
// crates/adj/src/readiness.rs
let ready = tokio::time::timeout(Duration::from_millis(500), attempt)
    .await
    .ok()
    .flatten()
    .unwrap_or(false);
// Abort unconditionally: a no-op once the task has finished, a cancellation (dropping the
// socket) when the probe timed out or the probe completed while the connection lingered.
if let Some(task) = conn_task {
    task.abort();
}
ready
```

Two honest notes on the shape it actually shipped:

- **It's a manual abort, not the `Drop` guard from the sketch.** That's fine *here* because `http_ready` has a single linear tail: nothing between the `timeout` resolving and the `if let ... abort()` can `return` early or `?` out, so every path reaches the abort. The day a function grows several `?` early returns between grabbing a task handle and finishing with it, that's the signal to promote this manual call into the `AbortOnDrop` guard — same story as a lock, where hand-written unlock works until someone adds an early return and the guard makes it structural.
- **The PR couldn't even reproduce the leak.** As the author's write-up notes, hyper 1.x happens to resolve the `conn` future when `sender` is dropped, so the detached task was already exiting on its own. The explicit `abort()` converts "cancellation that works by accident, because of a dependency's internals" into "cancellation we own" — and the two new tests pin the no-lingering-socket behavior against future hyper changes.

## Why it matters

The trap is reading `tokio::spawn(fut)` with the handle discarded as fire-and-forget *cleanup*, when it's the opposite: the task outlives the handle. Someone from a GC language expects the task to be collected once nothing references it; someone who's learned "drop a future to cancel it" gets bitten because a *spawned* task is detached, not a bare future. One leaked socket per probe, multiplied by a poll loop, is a genuine file-descriptor leak. Holding the `JoinHandle` and aborting it — ideally from a `Drop` guard so no exit path can skip it — is how you make the background task's lifetime a strict subset of the work that needed it.

## Related lessons

- **PR #29** built the `tokio::time::timeout` this abort partners with. #29 bounds *how long* the probe waits; #52 cleans up *what the probe spawned* when that bound fires. Timeout + abort are the two halves of a cancel-safe async operation — the first stops waiting, the second stops the work.
- **PR #37** is the converse-and-complement on `Drop`. #37: `Drop` runs on normal scope exit and panic unwinding, but a signal skips it. #52: *because* `Drop` runs on every normal exit path, it's the leak-proof home for an `abort()`. Read them together for the full map of what `Drop` covers and what it doesn't.

## Dig deeper

- [`JoinHandle::abort`](https://docs.rs/tokio/latest/tokio/task/struct.JoinHandle.html#method.abort) — the cancellation semantics, and the note that dropping a `JoinHandle` detaches (does not cancel) the task.
- [The Rust Book, ch. 15.3](https://doc.rust-lang.org/book/ch15-03-drop.html) — Running Code on Cleanup with the `Drop` Trait, the pattern behind the `AbortOnDrop` guard.
