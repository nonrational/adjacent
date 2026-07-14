<!-- Lesson for PR #24. Teaches one concept grounded in the real diff. -->

# PR #24 — Reverse proxy + lazy-boot + hostname routing

> **Rust lesson:** To boot a resource exactly once under concurrent demand, gate it with a *per-key* lock and re-check the state after you acquire it — a short-lived map lock hands out the per-name lock, which one waiter holds across the slow work while the rest re-check and skip it.
> **Tags:** `single-flight` · `per-key-mutex`
> **Merged:** 2026-06-08 · +905/−0 · [View PR](https://github.com/nonrational/adjacent/pull/24)

## The situation

The proxy lazy-boots an app on its first request. But a browser opening a page fires many requests at once — HTML, then a dozen assets — and an agent might hit the same host in parallel too. If each first-request naively booted the app, three concurrent hits would spawn three dev servers fighting over one port. That's a *thundering herd*: N callers all racing to create the same thing. The fix is **single-flight** — one boot runs, everyone else waits for it and reuses the result.

## The Rust idea

Single-flight is a lock plus a **double check**. You check the state, and if the resource already exists you're done. If not, you take a lock, then check *again* — because another task may have finished the boot while you were waiting for the lock. Only if it's still missing do you do the work.

Two design choices make this fast:

1. **Per-key locking, not a global lock.** One `Mutex` guarding all boots would serialize *unrelated* apps: booting `site` would block the first request to `blog`. Instead we keep a `HashMap<name, Arc<Mutex<()>>>` — one lock per app name. Booting `site` and `blog` proceed independently; only concurrent boots of the *same* name serialize.

2. **Two lock granularities.** The map itself needs a lock (two tasks might insert a new name at once). But you only hold the map lock long enough to look up or create the per-name lock, then you `.clone()` the `Arc` and release the map. The slow part — the actual boot — is held under the per-name lock, not the map lock. If you held the map lock across the boot, you'd be back to a global lock and lose the whole point.

The `Mutex<()>` guards *nothing* — its value is the empty tuple. It's a pure mutual-exclusion token; the thing being protected is the boot side effect, not any data inside the lock.

## In this PR

The gate is a lock-of-locks. The map is behind one `Mutex`; each entry is an `Arc<Mutex<()>>` you can clone out and hold on its own:

```rust
// crates/adj/src/proxy.rs
#[derive(Default)]
pub struct BootGate {
    locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
}

impl BootGate {
    async fn lock_for(&self, name: &str) -> Arc<Mutex<()>> {
        let mut map = self.locks.lock().await;
        map.entry(name.to_string())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }
}
```

`lock_for` takes the map lock, gets-or-creates the per-name lock, clones the `Arc`, and returns — the map guard drops at the end of the function, so the map is free again immediately. The caller now holds a clone of the per-name lock and can hold *it* for as long as the boot takes.

`ensure_running` is the double check. A fast path avoids the lock entirely when the app is already hot, then the slow path serializes and re-checks:

```rust
// crates/adj/src/proxy.rs
async fn ensure_running(/* ... */) -> Result<u16, ProxyError> {
    // Fast path: already running. Skip taking the per-name lock to avoid head-of-line blocking
    // when the app is hot.
    if let AppState::Running { port, .. } = supervisor.state(name).await {
        return Ok(port);
    }

    // ... look up the registry entry + config once, outside the lock ...

    // Single-flight: serialize concurrent boot attempts for this name. The first holder runs the
    // boot; later holders re-check state under the lock and find Running.
    let name_lock = gate.lock_for(name).await;
    let _guard = name_lock.lock().await;

    if let AppState::Running { port, .. } = supervisor.state(name).await {
        return Ok(port);
    }

    supervisor
        .up(entry.path.clone(), cfg.clone())
        .await
        .map_err(ProxyError::Other)?;
    // ... then poll until the child binds its port ...
}
```

Three requests for `flight.adj.ac` arrive together. All three miss the fast path. All three call `lock_for` and get the *same* `Arc<Mutex<()>>`. One wins `name_lock.lock().await` and boots; the other two block on `.await` (yielding their worker threads, not spinning). When the winner drops `_guard`, the next task acquires the lock, hits the re-check, sees `Running`, and returns the port without booting. The PR's integration test asserts exactly this: three concurrent first-requests, `assert_eq!(spawns, 1)`.

The `let _guard = ...` binding matters. The guard is held until the end of the function scope; naming it `_guard` (not `_`) keeps it alive. Bind it to a bare `_` and Rust drops it *immediately*, releasing the lock before the boot even starts — silently defeating the whole mechanism.

## Why it matters

In a garbage-collected language you'd reach for a `ConcurrentHashMap.computeIfAbsent` or a library `singleflight` helper and mostly not think about the re-check. Do it by hand and the classic bug is checking state only *before* the lock: two tasks both see "stopped", both wait, both acquire the lock in turn, and both boot — the lock made them polite but didn't make the boot single. The second check, *inside* the lock, is the load-bearing line. The other trap is reaching for one global lock for simplicity, which quietly turns independent app boots into a queue. Per-key locking keeps unrelated work parallel; the double-check keeps related work single.

## Related lessons

- PR #16 introduces the base `Arc<Mutex<T>>` pattern this builds on — shared ownership across `tokio::spawn`ed tasks. Read that first; this lesson is the single-flight *variation* layered on top.

## Dig deeper

- [`tokio::sync::Mutex`](https://docs.rs/tokio/latest/tokio/sync/struct.Mutex.html) — the async mutex used here; note the docs' guidance on holding a guard across `.await`, which is exactly what the boot lock does.
