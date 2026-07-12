<!-- Lesson for PR #53. Teaches one concept grounded in the real diff. -->

# PR #53 — Propagate WebSocket upgrades through the proxy

> **Rust lesson:** An HTTP `Upgrade` trades parsed HTTP framing for the raw byte stream underneath — you reserve that stream with `hyper::upgrade::on` *while you still own the request*, then splice both raw halves with `tokio::io::copy_bidirectional`.
> **Tags:** `http-upgrade` · `copy-bidirectional` · `ownership`
> **Merged:** 2026-06-14 · +238/−2 · [View PR](https://github.com/nonrational/adjacent/pull/53)

## The situation

The proxy forwarded ordinary HTTP fine, but WebSocket requests (Vite/Webpack/Next HMR)
died at the proxy boundary. A WebSocket starts as a normal GET carrying `Upgrade: websocket`;
the app answers `101 Switching Protocols` and from that byte onward the connection is no
longer HTTP — it's a raw, full-duplex byte stream. The proxy has to notice the handshake,
let it through, then get out of the way and shovel bytes both directions until someone hangs up.

## The Rust idea

An upgrade is a **handoff**. Normally `hyper` owns the socket and parses framed HTTP messages
for you. After a 101 there are no more messages to parse — there's a raw TCP stream underneath
that hyper is finished with. `hyper::upgrade::on(&mut req)` hands you a **future** (`OnUpgrade`)
that resolves to that raw stream *once the upgrade completes*. Think of it as a claim ticket:
you reserve the stream now and collect it later.

Two Rust facts make the *timing* of that claim load-bearing:

- **`send_request(req)` moves the request.** Sending it upstream consumes `req` by value, and
  the upgrade handle lives inside the request's extension map. Once the request is moved, you
  can never reach back in for it. So you must claim the handle *before* the move — that's the
  only window where the server half is still yours.
- **The downstream upgrade only resolves after you return the 101.** hyper's server loop
  completes the client-side upgrade only once `forward` hands back the `101` response. So
  awaiting the downstream future inline — before returning — would wait for something that
  can't happen until you return. Deadlock. The fix is to `tokio::spawn` a detached task: return
  the 101, and let that task collect both halves whenever they're ready.

Once you hold both raw streams, `tokio::io::copy_bidirectional(&mut a, &mut b)` runs the two
copies concurrently — `a → b` and `b → a` — and returns when both directions hit EOF. It
replaces a hand-rolled "spawn two copy loops and coordinate the shutdown" dance with one call.

## In this PR

Claim the downstream handle up front, gated on the `Upgrade` header so non-upgrade requests
pay nothing (`.then(|| …)` only runs the closure when the `bool` is `true`):

```rust
// crates/adj/src/proxy.rs
let downstream_upgrade = req
    .headers()
    .contains_key(hyper::header::UPGRADE)
    .then(|| hyper::upgrade::on(&mut req));
```

Drive the upstream client connection `with_upgrades()` so a 101 doesn't tear it down — the
upstream's raw half has to survive past the handshake for us to claim it too:

```rust
// crates/adj/src/proxy.rs
if let Err(err) = conn.with_upgrades().await {
    tracing::debug!("upstream connection ended: {err}");
}
```

On a 101, splice the two raw streams in a **detached** task:

```rust
// crates/adj/src/proxy.rs
if resp.status() == StatusCode::SWITCHING_PROTOCOLS {
    if let Some(downstream) = downstream_upgrade {
        let upstream = hyper::upgrade::on(&mut resp);
        tokio::spawn(async move {
            let (down, up) = match tokio::join!(downstream, upstream) {
                (Ok(d), Ok(u)) => (d, u),
                // ...
            };
            let mut down = TokioIo::new(down);
            let mut up = TokioIo::new(up);
            if let Err(err) = tokio::io::copy_bidirectional(&mut down, &mut up).await {
                tracing::debug!("upgraded tunnel closed: {err}");
            }
        });
    }
}
```

`tokio::join!(downstream, upstream)` awaits both claim tickets concurrently and yields a tuple
of `Result`s; only when both raw streams are in hand does the pump start. The `TokioIo::new`
wrappers matter: hyper's upgraded stream speaks hyper's own IO traits, but `copy_bidirectional`
is a tokio function expecting `AsyncRead + AsyncWrite`, so `TokioIo` adapts one to the other.

## Why it matters

The trap is claiming the stream too late. It's natural to reach for the upgrade handle *after*
you have the response — that's when a WebSocket "feels" established. But `send_request` already
moved the request by then, and Rust's ownership rules make that a hard compile error, not a
subtle runtime bug: the handle is simply gone. The type system forces you to grab it during the
one valid window. The second trap — awaiting the downstream upgrade inline — compiles fine and
then hangs forever; the `tokio::spawn` is what keeps it from deadlocking, and the comment in the
diff spells out why. A garbage-collected proxy in another language would let you hold a dangling
reference to the consumed request and discover the mistake only when the tunnel silently never
opens.

## Related lessons

- **PR #40** taught the generic `serve_plain<S>` loop these connections ride on — note it already
  calls `.with_upgrades()` on the *server* side. #53 is the client-side other half: without both,
  the upgrade completes on neither end.
- **PR #51** taught typed headers; the `hyper::header::UPGRADE` constant here is that same idea —
  a typed name instead of a stringly-typed `"upgrade"` lookup.

## Dig deeper

- [`hyper::upgrade`](https://docs.rs/hyper/latest/hyper/upgrade/index.html) — the `on()` function and the `OnUpgrade` future, plus why `with_upgrades()` is required on the connection.
- [`tokio::io::copy_bidirectional`](https://docs.rs/tokio/latest/tokio/io/fn.copy_bidirectional.html) — the concurrent two-way byte pump and its EOF semantics.
