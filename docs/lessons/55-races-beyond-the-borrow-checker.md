<!-- Lesson for PR #55. Teaches one concept grounded in the real diff. -->

# PR #55 — Close the test-sandbox port race behind connection-reset flakes

> **Rust lesson:** `Send`/`Sync` and the borrow checker eliminate in-process *data* races, but logical races over shared OS state — a port, a file, a row in someone else's database — are still entirely on you.
> **Tags:** `race-condition` · `TOCTOU`
> **Merged:** 2026-06-14 · +176/−72 · [View PR](https://github.com/nonrational/adjacent/pull/55)

## The situation

Integration tests each spin up their own daemon in a sandbox and need a free proxy port. The old trick: bind `127.0.0.1:0`, let the kernel pick a port, read the number, close the socket, then hand that number to the daemon to re-bind. Under `cargo test` seven suites run at once, all drawing from the same kernel ephemeral-port range. Another process could claim the port in the gap between the test closing it and the daemon re-binding it. The readiness probe's bare TCP connect then hit a *foreign* listener, called it ready, and the real request landed on a stranger's socket: "Connection reset by peer". It flaked roughly 1-in-45.

## The Rust idea

Rust's concurrency guarantees are famous, and it's easy to over-read them. Here's the precise boundary.

A **data race** is two threads touching the *same memory location* at the same time, at least one of them writing, with no synchronization. That is undefined behavior in C, a heisenbug in Go, and a *compile error* in Rust. The mechanism is two marker traits the compiler tracks for every type:

- **`Send`** — safe to *move* to another thread.
- **`Sync`** — safe to *share* by reference (`&T`) across threads.

`Arc<Mutex<T>>` is `Send + Sync`; `Rc<RefCell<T>>` is neither. So `tokio::spawn` accepts the first and rejects the second, and the borrow checker won't let you reach shared memory except through the lock. Data races: gone, before the program ever runs.

But note the load-bearing phrase: *the same memory location*. The guarantee is about **memory your process owns**. The moment two processes contend over something outside that model — a TCP port in the kernel's table, a file on disk, a network resource — the compiler has no view of it. No `&mut` crosses the boundary, no trait describes it, so there is nothing to check. This bug is a **TOCTOU** race — *time-of-check to time-of-use*: you check a fact (this port is free), and by the time you use it (bind it), the fact has changed. Classic, and completely invisible to `Send`/`Sync`.

## In this PR

The old code even confessed the race in its own doc comment:

```rust
// crates/adj/tests/proxy.rs  (deleted)
/// Bind :0, read the assigned port, close. The kernel may reissue this number to anyone before
/// the daemon claims it, but for a localhost test that's rare enough to accept.
fn pick_port() -> u16 {
    let l = std::net::TcpListener::bind("127.0.0.1:0").expect("bind :0");
    l.local_addr().expect("local_addr").port()
}
```

"Rare enough to accept" held until seven suites ran concurrently. The `bind → read → close → re-bind` sequence has a window, and a window is all a race needs.

The fix removes the window entirely. Don't pick a port and hand it off — let the daemon bind `:0` *itself*, so the port is born bound to the right process and never freed. The daemon then publishes the actually-bound port to a file:

```rust
// crates/adj/src/proxy.rs
let listener = TcpListener::bind(addr)
    .await
    .map_err(|e| anyhow!("binding proxy listener at {addr}: {e}"))?;
let bound = listener
    .local_addr()
    .map_err(|e| anyhow!("reading proxy listener addr: {e}"))?
    .port();
report_bound_port(crate::paths::proxy_port_path(), bound);
```

The test passes `0` and learns the real port by reading that file:

```rust
// crates/adj/tests/proxy.rs
// 0 = the daemon binds a kernel-assigned port; start_daemon learns the real port
// from the proxy.port file. Picking a free port here and re-binding it in the
// daemon raced concurrent test processes drawing from the same ephemeral range,
// which flaked as "connection reset by peer" against a foreign listener.
proxy_port: 0,
```

```rust
// crates/adj/tests/proxy.rs
// proxy.port is written after bind, so a parsed port means the listener is live —
// and unambiguously ours, unlike a bare TCP connect to a guessed port.
if self.proxy_port == 0 {
    if let Some(p) = read_port_file(&port_file) {
        self.proxy_port = p;
    }
}
```

Two races collapse into zero. There's no free-then-rebind gap because the listener that binds the port is the one that keeps it. And the readiness signal gets stronger for free: a parsed `proxy.port` file means the listener is live *and ours*, where the old bare `TcpStream::connect` couldn't tell our daemon from a squatter on the same port.

## Why it matters

The trap is assuming Rust's "fearless concurrency" covers all concurrency. It covers exactly one thing — shared memory inside your address space — and covers it completely. Everything with an identity outside your process is ordinary distributed-systems state, and the same discipline you'd use in any language applies: don't check-then-act across a gap where another actor can intervene. Here the fix is to eliminate the gap (bind once, keep it) rather than shrink it. Other flavors of the same lesson: hold the resource instead of releasing and reacquiring it, make the operation atomic (`O_CREAT | O_EXCL`, a `CAS`), or retry on the losing side. The borrow checker will happily let you write a textbook TOCTOU bug — it was never its job to stop you.

## Related lessons

- **PR #16** — the `Arc<Mutex<T>>` pattern whose guarantee this lesson bounds: it prevents data races on in-process memory, and *only* that.
- **PR #24** — single-flight boot gate, an *in-process* logical race solved with a lock; contrast it with #55, an *OS-level* race a lock can't touch.
- **PR #50** — GCing the per-name lock map, another case of managing shared in-process state safely.

## Dig deeper

- [The Rust Book, ch. 16](https://doc.rust-lang.org/book/ch16-00-concurrency.html) — Fearless Concurrency. Read it for what `Send`/`Sync` buy you, but read it knowing the chapter is about **data races** specifically; the port race in this PR is deliberately outside its scope.
