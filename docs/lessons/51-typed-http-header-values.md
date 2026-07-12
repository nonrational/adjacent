<!-- Lesson for PR #51. Teaches one concept grounded in the real diff. -->

# PR #51 — Forward X-Forwarded-* headers from the proxy

> **Rust lesson:** In the `http` crate a header value is a `HeaderValue` — a validated byte-string newtype, not a `String` — so building one is fallible (`HeaderValue::from_str` returns a `Result`), and that fallibility is how the type system blocks header injection.
> **Tags:** `newtypes` · `typed-headers` · `fallible-construction`
> **Merged:** 2026-06-14 · +247/−13 · [View PR](https://github.com/nonrational/adjacent/pull/51)

## The situation

The proxy rewrites the incoming `Host` header to `127.0.0.1:<port>` so upstream dev servers with host-allowlist checks accept the request. That rewrite erases the browser's real origin. This PR restores it with the three standard reverse-proxy headers — `X-Forwarded-Host`, `X-Forwarded-For`, `X-Forwarded-Proto` — so an app can still reconstruct where the request actually came from. To set them, the code has to *construct* header values, and that is where Rust's types show up.

## The Rust idea

The `http` crate (re-exported through `hyper`) does not model headers as `HashMap<String, String>`. Keys are `HeaderName`, values are `HeaderValue`, and the collection is `HeaderMap`. A `HeaderValue` is a **newtype** wrapping validated bytes: a header value may not contain a carriage return, line feed, or NUL. Those are exactly the bytes an attacker would use to split one header into two — the classic header/response-injection attack. Encoding the rule in a type makes the illegal value *unrepresentable*: the only way to get a `HeaderValue` is through a constructor that checks.

So **construction is fallible**. `HeaderValue::from_str(s)` returns `Result<HeaderValue, InvalidHeaderValue>` — hand it a string with a stray `\r\n` and you get `Err`, never a poisoned value. When the input is a compile-time constant you already know is legal, `HeaderValue::from_static("http")` skips the runtime check and hands back a `HeaderValue` directly (no `Result` to unwrap; it only panics if *you* wrote an illegal literal).

**Reading is fallible too.** `HeaderValue::to_str()` returns a `Result`, because the wrapped bytes are opaque and aren't guaranteed to be valid UTF-8 — a `HeaderValue` is not a Rust `String`, so getting a `&str` back out can fail.

Contrast a `String`-typed header map in Python, JS, or Go: nothing stops you writing a newline into a value, and the injection surfaces (if ever) as a runtime security bug. Rust makes the unsafe value impossible to build in the first place.

## In this PR

The whole helper, from `crates/adj/src/proxy.rs`:

```rust
// crates/adj/src/proxy.rs
fn set_forwarded_headers(
    headers: &mut hyper::HeaderMap,
    original_host: &str,
    client_ip: IpAddr,
    proto: &'static str,
) {
    if let Ok(v) = hyper::header::HeaderValue::from_str(original_host) {
        headers.insert("x-forwarded-host", v);
    }
    // ...
    let mut chain: Vec<String> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(str::to_string)
        .collect();
    chain.push(client_ip.to_string());
    let xff = chain.join(", ");
    if let Ok(v) = hyper::header::HeaderValue::from_str(&xff) {
        headers.insert("x-forwarded-for", v);
    }
    headers.insert(
        "x-forwarded-proto",
        hyper::header::HeaderValue::from_static(proto),
    );
}
```

Three constructions, three different type stories:

- **`from_str(original_host)` is fallible**, so it's wrapped in `if let Ok(v)`. A malformed Host produces `Err`, the `insert` is skipped, and the request still forwards — the bad bytes never reach the socket.
- **`proto` is `"http"` or `"https"`**, both compile-time-safe, so it uses `from_static` — no `Result`. That's also *why* the parameter is typed `proto: &'static str`: `from_static` demands a `&'static str`, and the accept loops thread in string literals to satisfy it.
- **Reading the existing chain uses `.to_str().ok()`.** `to_str` is the read-side `Result`; `.ok()` inside `filter_map` drops any value that isn't valid UTF-8 instead of blowing up.

One `HeaderMap` detail worth naming: `insert` *replaces* every existing value for a name, while `append` adds another line — a `HeaderMap` is a multimap that can hold several values under one key. The helper reads them all with `get_all` (not `get`, which would see only the first line), joins them, then `insert`s once to collapse the chain back to a single line.

## Why it matters

Header injection is the trap this avoids. Picture the string-concatenation version: `"X-Forwarded-Host: " + original_host` written straight to the socket. A Host of `evil\r\nX-Admin: true` smuggles in a whole extra header. `HeaderValue::from_str` rejects the `\r\n` before it can ever become a value, and the diff's `if let Ok(v)` turns that rejection into "quietly skip the header," never "forward the attack." Nobody had to remember to sanitize — the type simply refuses to hold the dangerous bytes.

## Related lessons

- PR #40 introduced the generic `serve_plain` that this PR threads `client_ip` and `proto` through — same file, same serve loop.
- PR #48 is the same "a `Result` is a value you must handle" discipline, one layer up; here the fallible values are `HeaderValue::from_str` and `.to_str()`.

## Dig deeper

- [`http::HeaderMap`](https://docs.rs/http/latest/http/header/struct.HeaderMap.html) — the multimap: `insert` vs `append`, `get` vs `get_all`
- [`http::HeaderValue`](https://docs.rs/http/latest/http/header/struct.HeaderValue.html) — `from_str` (fallible), `from_static` (infallible), `to_str` (fallible read)
