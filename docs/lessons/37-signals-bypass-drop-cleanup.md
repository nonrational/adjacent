<!-- Lesson for PR #37. Teaches one concept grounded in the real diff. -->

# PR #37 — Handle SIGTERM in daemon socket cleanup

> **Rust lesson:** `Drop` runs on normal scope exit and panic unwinding, but a process killed by a signal skips destructors entirely — so external cleanup like unlinking a Unix socket has to be done explicitly in a `tokio::signal` handler.
> **Tags:** `tokio-signal` · `drop` · `graceful-shutdown`
> **Merged:** 2026-06-08 · +15/−4 · [View PR](https://github.com/nonrational/adjacent/pull/37)

## The situation

The daemon binds a Unix socket at `~/.adjacent/sock`. That's a real file on disk, and a stale one blocks the next boot — `UnixListener::bind` fails if the path already exists. The daemon already cleaned it up on Ctrl-C. But `brew services stop` and launchd don't send Ctrl-C (SIGINT); they send **SIGTERM**. So a service-managed shutdown left the socket behind, and the next start had to notice the corpse and remove it. This PR handles SIGTERM too, so the socket goes away cleanly on the shutdown path that actually ships.

## The Rust idea

Rust's usual answer to "clean up a resource" is RAII: give the resource a type, put the teardown in a `Drop` impl, and the compiler runs it for you when the value leaves scope. A `File` closes itself; a `MutexGuard` unlocks itself. You don't write cleanup at every early return, because `Drop` is wired to *scope exit*.

Here's the catch that trips people coming from garbage-collected or exception-based languages: **`Drop` only runs when Rust is in control of the exit.** There are exactly two paths that run destructors — a value going out of scope normally, and stack unwinding during a `panic!`. A signal is neither. When the kernel delivers SIGTERM (or SIGINT) and the process takes the default action, it just *stops*. No stack unwinds. No `Drop` fires. Whatever was living on the stack — including a `UnixListener` — is abandoned, and the OS reclaims memory and file descriptors, but it does **not** run your teardown logic or unlink a socket path you created.

So RAII cannot save you here. Even if you wrote a `Drop` impl that removed the socket file, SIGTERM would walk right past it. The only way to react to a signal is to *ask* the runtime to tell you when one arrives, then do the cleanup yourself before you exit.

That's what `tokio::signal` is for. Two shapes matter:

- `tokio::signal::ctrl_c()` returns a **future** that resolves once, when SIGINT arrives.
- `tokio::signal::unix::signal(SignalKind::terminate())` returns a `Result<Signal>` — a **stream** you poll with `.recv()`, because a signal can fire many times over a process's life.

You want to react to *whichever* comes first, so you race them with `tokio::select!` — the async "wait on several things, wake on the first to complete" primitive.

## In this PR

```rust
// crates/adj/src/daemon.rs

// Best-effort cleanup of the socket on shutdown signals so subsequent boots aren't blocked.
// SIGTERM matters specifically for `brew services stop` / launchd-driven shutdown; SIGINT
// covers interactive Ctrl-C in the foreground. Either signal triggers the same cleanup.
let socket_for_signal = socket.clone();
tokio::spawn(async move {
    let mut sigterm = match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(s) => s,
        Err(err) => {
            tracing::warn!("failed to install SIGTERM handler: {err}");
            return;
        }
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = sigterm.recv() => {}
    }
    let _ = std::fs::remove_file(&socket_for_signal);
    std::process::exit(0);
});
```

Walking the lines that matter:

- **Installing the handler can fail**, so `signal(...)` returns a `Result`. The `match` binds the `Signal` on success; on failure it warns and `return`s out of the task. That's deliberately best-effort — a daemon that can't install a SIGTERM handler should still serve requests, it just won't self-clean on that signal. (See PR #48 for the "warn, don't swallow" reflex.)
- **`tokio::select!`** races the two sources. Both arms have empty bodies (`{}`) — we don't care *which* signal woke us, only that one did. Whichever future resolves first, `select!` drops the other and control falls through to the shared cleanup below it. Without `select!` you'd `.await` one source and be deaf to the other.
- **The cleanup is explicit and ordered**: remove the socket file first, *then* exit. `let _ =` discards the `io::Result` from `remove_file` — if the file's already gone, there's nothing to fix.
- **`std::process::exit(0)`** is the punchline that proves the whole point. `process::exit` terminates immediately and **also does not run destructors** — it's the same "Rust isn't unwinding" situation the signal put us in. That's exactly why the `remove_file` call sits *above* it. If we were relying on `Drop` for the socket, `exit(0)` would skip it just like SIGTERM would. Cleanup has to happen by hand, before the process is gone.

## Why it matters

The trap is assuming RAII is a universal cleanup guarantee. In a GC language you'd reach for a shutdown hook and hit the same wall from the other side; in Rust it's tempting to think "I'll put it in `Drop`" and move on. But `Drop` is a contract about *scopes*, not about *process lifetime*. Signals, `abort()`, `std::process::exit`, and a hard `SIGKILL` (which you can't catch at all) all end the process without unwinding. Any resource that outlives the process's memory — a file on disk, a lock file, a socket path, a row in a database marked "in use" — needs an explicit teardown on the paths where `Drop` won't run. `tokio::signal` is how you get a chance to run that teardown for the signals you *can* catch.

## Related lessons

- **PR #52** is the exact converse of this one: it uses `Drop` deliberately, as an RAII guard that aborts a background task when a value goes out of scope. Read them together — #52 shows `Drop`'s reach (any normal scope exit, guaranteed), #37 shows its limit (a signal walks right past it).
- **PR #16** covers the `Arc<Mutex<…>>` sharing pattern; the same `socket.clone()` + `move` closure shape shows up here to hand the path into the spawned task.
- **PR #48** — the `match ... Err(err) => warn; return` arm is the "surface the error, don't swallow it" habit applied to handler installation.

## Dig deeper

- [`tokio::signal` module docs](https://docs.rs/tokio/latest/tokio/signal/index.html) — `ctrl_c`, the Unix `SignalKind` / `Signal::recv` stream API, and the caveats about async-signal-safety.
- [The Rust Book, ch. 15.3](https://doc.rust-lang.org/book/ch15-03-drop.html) — Running Code on Cleanup with the `Drop` Trait (what `Drop` guarantees — and, by omission, what it doesn't).
