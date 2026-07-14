<!-- Lesson for PR #40. Teaches one concept grounded in the real diff. -->

# PR #40 — Serve HTTPS on :8443 via opt-in local CA

> **Rust lesson:** A generic function bounded by traits is compiled into a specialized copy per concrete type (monomorphization), so one serve loop drives both a plain `TcpStream` and a TLS-wrapped stream at zero runtime cost.
> **Tags:** `generics` · `trait-bounds`
> **Merged:** 2026-06-08 · +1242/−49 · [View PR](https://github.com/nonrational/adjacent/pull/40)

## The situation

The daemon already ran an HTTP reverse proxy. This PR adds an HTTPS listener on `:8443`
that terminates TLS with a locally-issued cert, then routes requests exactly like the HTTP
path. The two listeners differ only at accept time: one hands back a raw TCP stream, the
other a TLS stream wrapped around one. Everything after — read the request, look up the app,
proxy it — is identical. The problem: how do you write that shared "everything after" once
when the two streams are different concrete types?

## The Rust idea

A **generic function** with **trait bounds** is Rust's answer. You write the body once against
a type parameter `S`, and you constrain `S` to exactly the capabilities the body uses. The
compiler then does **monomorphization**: for every concrete type you actually call the function
with, it stamps out a separate, specialized copy with `S` replaced. `serve_plain::<TcpStream>`
and `serve_plain::<TlsStream<TcpStream>>` become two real functions in the binary.

This is *static dispatch*. In Java or Go you'd take an interface parameter and pay a vtable
lookup on every method call through it. Rust's monomorphized copies call the concrete methods
directly — inlinable, no indirection, no per-call cost. You trade a little binary size (two
copies) for speed. (Rust also offers `dyn Trait` for real dynamic dispatch when you *want* one
function and are willing to pay the vtable; here, static is the right call.)

The bounds are not decoration — each one is load-bearing:

- `AsyncRead + AsyncWrite` — the serve loop reads request bytes off the stream and writes the
  response back. Without both, `hyper`'s `serve_connection` won't accept the stream.
- `Unpin` — async I/O is poll-based and the runtime needs to move the stream around; `Unpin`
  promises it's safe to move even after polling has begun.
- `Send` — the connection is driven inside `tokio::spawn`, which may run it on a different
  worker thread, so the stream must be safe to send across threads.
- `'static` — the spawned task can outlive the stack frame that launched it, so `S` may not
  borrow anything shorter-lived.

Drop any one bound and the code that *needs* that capability fails to compile. The bound list
is the exact contract the body demands, checked at the call site, not at runtime.

## In this PR

The shared loop, written once against a type parameter:

```rust
// crates/adj/src/proxy.rs
/// Run one HTTP/1.1 connection against the proxy's per-request handler. Parameterized over the
/// underlying stream so the HTTP and HTTPS listeners share the same serve loop — the difference
/// between them is purely accept-time framing.
async fn serve_plain<S>(stream: S, sup: Arc<Supervisor>, gate: Arc<BootGate>)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let io = TokioIo::new(stream);
    let service = service_fn(move |req: Request<Incoming>| {
        // ... clone sup + gate, dispatch to handle(req, ...)
    });
    if let Err(err) = server_http1::Builder::new()
        .serve_connection(io, service)
        .with_upgrades()
        .await
    {
        tracing::debug!("proxy connection ended: {err}");
    }
}
```

The HTTP listener calls it with the raw accepted stream:

```rust
// crates/adj/src/proxy.rs — inside run()
tokio::spawn(async move {
    serve_plain(stream, sup, gate).await;   // S = TcpStream
});
```

The HTTPS listener does the TLS handshake first, then hands the *wrapped* stream to the same
function:

```rust
// crates/adj/src/proxy.rs — inside run_https()
let tls_stream = match acceptor.accept(stream).await {
    Ok(s) => s,
    Err(err) => {
        tracing::debug!("tls handshake failed: {err}");
        return;
    }
};
serve_plain(tls_stream, sup, gate).await;    // S = TlsStream<TcpStream>
```

Note there are no turbofish annotations at the call sites — the compiler infers `S` from the
argument's type. The `TlsStream` that `tokio_rustls` produces already implements `AsyncRead +
AsyncWrite + Unpin + Send`, so it satisfies the bounds and the identical body just works.

## Why it matters

The alternative is copy-paste: a second serve loop for the TLS stream, byte-for-byte the same
except the parameter type. Two copies drift. A bug fix or a `with_upgrades()` tweak lands in
one and gets forgotten in the other. Generics collapse them into a single source of truth that
the type system re-checks against every concrete stream you feed it.

A dynamically-typed language would let you pass either stream into one function and only find
out at runtime — mid-request, on a live connection — that one of them can't do something the
body assumed. Rust's bounds move that check to compile time: if a stream type is missing
`Send`, the `tokio::spawn` call site refuses to build. You cannot ship the broken combination.

## Related lessons

- The `run_https` task is spawned best-effort (logs at `error!` and exits if the CA is missing,
  leaving HTTP and the control plane serving) — a small lesson in fault-isolated `tokio` tasks.

## Dig deeper

- [The Rust Book, ch. 10.1](https://doc.rust-lang.org/book/ch10-01-syntax.html) — Generic Data Types
- [The Rust Book, ch. 10.2](https://doc.rust-lang.org/book/ch10-02-traits.html) — Traits: Defining Shared Behavior (see "Using Trait Bounds" and the note on monomorphization in ch. 10.1)
