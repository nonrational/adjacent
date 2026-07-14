<!-- Lesson for PR #47. Teaches one concept grounded in the real diff. -->

# PR #47 — Document and test the docker run container convention

> **Rust lesson:** Every file under `crates/adj/tests/` is compiled as its *own* separate crate that links your code as an outside dependency — so it can only touch the public API, and for a binary-only crate that means driving the compiled executable via `CARGO_BIN_EXE_adj`.
> **Tags:** `integration-tests` · `CARGO_BIN_EXE`
> **Merged:** 2026-06-10 · +283/−0 · [View PR](https://github.com/nonrational/adjacent/pull/47)

## The situation

The PR is honest about being two things: a README section documenting how to run a
containerized app under Adjacent, plus one new integration test that proves the
convention actually works. The test — `crates/adj/tests/docker.rs` — lazy-boots
`traefik/whoami` through the proxy, asserts a 200, checks `docker ps` sees the
container, then runs `adj down` and confirms the container is gone. That last
assertion is the one that would catch a signal-forwarding regression.

To do all that, the test has to start a *real* `adj` daemon and talk to it exactly
like a user would. That constraint isn't incidental — it falls straight out of how
Rust organizes tests.

## The Rust idea

Rust has two places tests live, and they are not interchangeable:

1. **Unit tests** — a `#[cfg(test)] mod tests { ... }` block *inside* a source file
   (e.g. `src/supervisor.rs`). This module is compiled as part of the same crate, so
   it can reach *private* functions, structs, and fields. Use it to test internals.

2. **Integration tests** — each file in the top-level `tests/` directory. Here's the
   part that surprises people coming from other languages: **Cargo compiles each of
   these files as its own independent crate**, and that crate depends on yours the way
   any outside consumer would. It can only see `pub` items. Private internals are
   invisible to it, on purpose — an integration test proves the *public surface* works.

Now the twist that makes `adj` interesting. `crates/adj` is a **binary-only** crate:
its `Cargo.toml` declares a `[[bin]]` and there's no `lib.rs`. A binary exposes no
library API to an external crate at all — there are no `pub fn`s to call. So the
integration test can't link against `adj` and call functions. Instead it drives the
compiled `adj` executable as a subprocess, the same as a shell would.

Cargo makes that ergonomic: when it builds the test, it sets an environment variable
`CARGO_BIN_EXE_<name>` to the absolute path of the compiled binary. `env!(...)` reads
it *at compile time*, so the path is baked in — no guessing at `target/debug/`, no
`cargo build` ordering to manage.

## In this PR

The test resolves the binary through that Cargo-provided variable:

```rust
// crates/adj/tests/docker.rs
fn adj_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}
```

Because it's driving a real binary, it also stands up a real daemon — sandboxed into
a throwaway `TempDir` via `ADJACENT_HOME`, on a random port via `ADJACENT_PROXY_PORT`,
so the test never touches your actual `~/.adjacent/`:

```rust
// crates/adj/tests/docker.rs
fn cmd(&self) -> Command {
    let mut c = Command::new(adj_bin());
    c.env("ADJACENT_HOME", &self.home_path);
    c.env("ADJACENT_PROXY_PORT", self.proxy_port.to_string());
    c.env("RUST_LOG", "warn");
    c.env_remove("PORT");
    c.env_remove("BIND_PORT");
    c
}
```

The test itself is `async`, so it's annotated `#[tokio::test]` — the async cousin of
`#[test]`, which spins up a Tokio runtime around the test body so you can `.await`
inside it (here: awaiting daemon startup, subprocess output, and the container
teardown poll):

```rust
// crates/adj/tests/docker.rs
#[tokio::test]
async fn docker_run_lazy_boots_and_down_stops_the_container() {
    if !docker_ready().await {
        eprintln!("skipping: no reachable Docker daemon (or {IMAGE} unavailable)");
        return;
    }
    // ... boot through the proxy, assert 200, then `adj down` and confirm gone
}
```

One more integration-test detail worth naming: this file uses `tempfile` and
`tokio`, and both are available because `Cargo.toml` lists them under
`[dev-dependencies]` — dependencies compiled only for tests and examples, never
shipped in the release binary.

## Why it matters

The unit-vs-integration split is a real design decision, not boilerplate. Reaching
into private state from a `#[cfg(test)] mod tests` is convenient, but it tests the
code you *wrote*, not the code your users *touch*. The separate-crate rule forces the
integration layer to go through the front door: for a library that means calling only
`pub` items; for a binary like `adj` it means shelling out to the actual executable.
That's why this test can honestly claim to prove the documented container convention —
it exercises the same `adj add` / proxy / `adj down` path a developer runs by hand,
with nothing mocked and no private shortcut.

Skip the `CARGO_BIN_EXE_<name>` variable and you'd hardcode `target/debug/adj`, which
breaks under `--release`, breaks on the CI runner's target dir, and silently runs a
*stale* binary if you forget to rebuild. Letting Cargo hand you the path removes all
three traps.

## Related lessons

- PR #38 works inside this same `tests/` directory but teaches a different axis:
  making an async integration test *deterministic* (killing flakiness). This lesson is
  the layout — where tests live and what they can see; #38 is how to make one reliable
  once it's there.
- PR #16 stood up the supervisor this test ultimately drives; the daemon it boots is
  the `Arc<Mutex<...>>` machinery taught there, exercised end-to-end from outside.

## Dig deeper

- [The Rust Book, ch. 11.3](https://doc.rust-lang.org/book/ch11-03-test-organization.html) — Test Organization (the unit-vs-integration split, the `tests/` directory, and testing binary crates)
