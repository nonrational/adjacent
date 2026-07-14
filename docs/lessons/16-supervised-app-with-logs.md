<!-- Lesson for PR #16. Teaches one concept grounded in the real diff. -->

# PR #16 — Tracer: supervised app with logs

> **Rust lesson:** To mutate state from an async task that outlives the function that spawned it, hand the task an `Arc<Mutex<T>>` clone — `Arc` gives shared ownership across threads, the `Mutex` makes the mutation safe.
> **Tags:** `Arc<Mutex<T>>` · `tokio::spawn`
> **Merged:** 2026-06-08 · +2369/−0 · [View PR](https://github.com/nonrational/adjacent/pull/16)

## The situation

This is the tracer-bullet PR that stood the project up: register a dev-server directory, boot it, capture its logs, and report whether it's running, stopped, or crashed. The core is a `Supervisor` that spawns a child process and then needs to know, later and asynchronously, when that child exits — and update its recorded state accordingly. The catch: the code that spawns the child returns immediately, but the exit can happen minutes later. Two different tasks end up touching the same state, and Rust will not let that happen by accident.

## The Rust idea

In a garbage-collected language you would capture `this` in a callback and mutate a field whenever the process exits. Nobody checks who else is touching that field at the same time.

Rust makes the sharing explicit, for two reasons:

1. **Lifetimes.** A task handed to `tokio::spawn` must be `'static` — it may outlive the function that created it, so it cannot borrow anything on that function's stack (including `&self`). It has to *own* what it uses.
2. **Threads.** Tokio's default runtime is multi-threaded, so the spawned task may run on a different worker thread than the one that spawned it. Anything it captures must be safe to send and share across threads (`Send + Sync`).

`Arc<Mutex<T>>` is the idiomatic answer to both:

- `Arc<T>` — an **a**tomically **r**eference-**c**ounted pointer. Cloning it doesn't copy the data; it bumps a refcount and hands back a second owner of the *same* `T`. Both owners are `'static`, so both can go into spawned tasks.
- `Mutex<T>` — guards the interior so only one task mutates at a time. `tokio::sync::Mutex::lock().await` *yields* the worker thread while waiting instead of blocking it, which is what you want inside async code.

The single-threaded shortcut `Rc<RefCell<T>>` won't compile here: `Rc` isn't `Send`, so `tokio::spawn` rejects it.

## In this PR

The supervisor holds all app state behind one shared, locked handle:

```rust
// crates/adj/src/supervisor.rs
#[derive(Default)]
pub struct Supervisor {
    inner: Arc<Mutex<Inner>>,
}

#[derive(Default)]
struct Inner {
    apps: HashMap<String, AppRuntime>,
}
```

`up()` spawns the child, records it as `Running`, then detaches a task to wait for the exit:

```rust
// crates/adj/src/supervisor.rs
pub async fn up(&self, app_dir: PathBuf, cfg: AppConfig) -> Result<u32> {
    let name = cfg.name.clone();
    let mut inner = self.inner.lock().await;
    // ...
    let mut child = command
        .spawn()
        .with_context(|| format!("spawning `{}`", cfg.cmd))?;

    let pid = child.id().ok_or_else(|| anyhow!("spawned child has no pid"))?;

    inner.apps.insert(
        name.clone(),
        AppRuntime { state: AppState::Running { pid }, intentional_stop: false },
    );

    // Detach the wait task so the supervisor can observe exit/crash without holding the lock.
    let inner_handle = self.inner.clone();
    let watch_name = name.clone();
    tokio::spawn(async move {
        let status = child.wait().await;
        let mut guard = inner_handle.lock().await;
        if let Some(rt) = guard.apps.get_mut(&watch_name) {
            // ... record Stopped, or Crashed { exit_code }
        }
    });

    Ok(pid)
}
```

The load-bearing lines are the two just before `tokio::spawn`:

- `let inner_handle = self.inner.clone();` clones the **`Arc`**, not the `Inner`. The new handle points at the same `Mutex<Inner>` the `Supervisor` still owns.
- `async move { ... }` moves `inner_handle`, `watch_name`, and `child` *into* the task. Now the task owns everything it touches — no borrow of `self`, so the `'static` requirement is satisfied.

When the child eventually exits, the task takes the lock (`inner_handle.lock().await`) and flips the app's `state` from `Running` to `Stopped` or `Crashed`. Meanwhile `state()`, `down()`, and the next `up()` all reach the same `HashMap` through the same `Mutex` — one writer at a time, no data race, enforced at compile time.

## Why it matters

Without `Arc`, you can't share the state at all: the spawned task can't borrow `self`, and moving `self.inner` into it would leave the `Supervisor` with nothing. Without the `Mutex`, two tasks writing the same `HashMap` is a data race — which in most languages is a heisenbug you find in production, and in Rust is a compile error you fix before lunch. The pattern is verbose on purpose: it forces you to name, up front, exactly which state is shared and how it's synchronized.

## Related lessons

- Later PRs reuse this exact `Arc<Mutex<...>>` foundation for trickier concurrency — the proxy's lazy-boot single-flight and the idle scanner's re-check-under-lock both build on the supervisor's `Inner` mutex. Those PRs are the place to teach the *variations* (single-flight, closing the request-vs-scanner race), not the base pattern taught here.

## Dig deeper

- [The Rust Book, ch. 16.3](https://doc.rust-lang.org/book/ch16-03-shared-state.html) — Shared-State Concurrency (`Mutex<T>` and `Arc<T>`)
