<!-- Lesson for PR #17. Teaches one concept grounded in the real diff. -->

# PR #17 — Inject PORT into supervised processes

> **Rust lesson:** `tokio::process::Command` is a builder that inherits the parent process's environment by default; each `&mut self` method chains, and `.env(key, value)` layers one variable on top before `.spawn()` runs anything.
> **Tags:** `tokio::process::Command` · `builder-pattern` · `env-vars`
> **Merged:** 2026-06-08 · +322/−12 · [View PR](https://github.com/nonrational/adjacent/pull/17)

## The situation

A supervised dev server has to bind *somewhere*, and the daemon — not the app — picks the port (it allocates a free one via a `:0` bind probe). So the daemon must hand that number to the child before it starts. The classic Unix mechanism is an environment variable: inject `PORT=54321` into the child's environment, and the app reads `$PORT` and binds it. Some apps insist on a different name, so `adjacent.toml` gains a `port_env` knob to rename the injected variable.

## The Rust idea

Rust has no keyword arguments, no default parameters, and no function overloading. When a thing has many optional knobs and one final "go" step — spawning a process is the textbook case — the idiomatic shape is the **builder pattern**.

`Command::new("sh")` returns a mutable `Command`. Every configuration method (`.arg`, `.current_dir`, `.env`, `.stdout`, `.process_group`) takes `&mut self` and returns `&mut Self`. Returning the same mutable borrow is what lets calls chain, and because nothing is *moved*, you can also configure across several statements on the same binding. Nothing actually runs until a terminal method — here `.spawn()`.

The environment piece has one detail that trips people up: **a freshly built `Command` inherits the parent process's entire environment.** You start with a full copy of whatever the daemon itself was launched with. The env methods then edit that inherited set:

- `.env(key, value)` — insert or **override** one entry.
- `.envs(iter)` — the same, for many at once.
- `.env_remove(key)` — drop one inherited entry.
- `.env_clear()` — throw the whole inheritance away and start empty.

"Inherit, then override" is a precedence decision baked into the default: a later `.env` wins over whatever the child would have inherited under the same name.

## In this PR

The core is a handful of chained builder calls in `up()`. The port name is resolved first, then layered onto the command:

```rust
// crates/adj/src/supervisor.rs
let port_env = cfg.port_env.as_deref().unwrap_or("PORT");
let mut command = Command::new("sh");
command
    .arg("-c")
    .arg(&cfg.cmd)
    .current_dir(&app_dir)
    .env(port_env, port.to_string())
    .stdin(Stdio::null())
    .stdout(Stdio::from(log_file))
    .stderr(Stdio::from(stderr_file));
```

- `cfg.port_env.as_deref().unwrap_or("PORT")` collapses `Option<String>` into a plain `&str`: `as_deref()` borrows the inner `String` as `&str` if present, and `unwrap_or("PORT")` supplies the default when it's `None`. One expression, one default, no branch.
- `.env(port_env, port.to_string())` layers exactly one variable onto the inherited environment. The value has to be an *owned* `String` (`port` is a `u16`), because `.env` keeps the bytes for the eventual spawn — a borrowed slice tied to this stack frame wouldn't outlive it.

The config field that feeds `port_env` is optional by construction:

```rust
// crates/adj/src/registry.rs
/// Override the env var name used to inject the assigned port.
/// When unset, Adjacent exports `PORT`. When set, it exports the named variable instead.
#[serde(default)]
pub port_env: Option<String>,
```

`#[serde(default)]` means an absent key deserializes to `None` instead of erroring — that's what makes `port_env` opt-in.

The load-bearing proof that inheritance is the default lives in the test harness:

```rust
// crates/adj/tests/tracer.rs
// Scrub port-related env vars from the parent shell so the daemon (and any child it
// spawns) starts from a known-clean slate. Without this, `PORT=3000 cargo test` would
// pass `PORT` through to the supervised child and silently break the rename test.
c.env_remove("PORT");
c.env_remove("BIND_PORT");
```

Read that comment as a demonstration of the semantics: because `Command` copies the parent's environment and `.env` only touches the keys you name, a stray `PORT` in the test runner's own environment flows straight through to the child. The `rename` test asserts the child sees `BIND_PORT` set and `PORT` *unset* — which is only true if the parent isn't leaking a `PORT` of its own.

## Why it matters

In a language where you mutate a shared, global `process.env`, injecting a variable is order-dependent and easy to leak between spawns. The `Command` builder gives you an isolated, per-child edit list assembled on a `&mut Command`, so the process can't be spawned half-configured — there's no window where the child exists but `PORT` hasn't been set yet, because `.spawn()` is the last thing you call.

The trap the builder's default hides is the inheritance itself. It's convenient (the child gets `PATH`, `HOME`, and the rest for free) but it's also a silent channel: if the daemon happened to run with `PORT` in its own environment, every child would inherit it, and injection would be masking rather than setting. That's exactly why the test scrubs `PORT`/`BIND_PORT` before starting — not paranoia, but a direct consequence of "inherit, then override" being the default you signed up for.

## Related lessons

- PR #16 stands up the `Supervisor` and the `up()` method this `.env()` call lives inside — the `Arc<Mutex<Inner>>` state and the wait-for-exit task are the foundation this port injection slots into.
- PR #76 generalizes this single-variable mechanic into a whole daemon-owned `ADJ_*` namespace (`ADJ_NAME`, `ADJ_HOST`, `ADJ_URL`, …) injected *after* `env_file` and `[env]` so the daemon's values win — that's where the full env-layering-and-precedence story gets taught. #17 is the one-variable version; #76 is the layered namespace.

## Dig deeper

- [`std::process::Command`](https://doc.rust-lang.org/std/process/struct.Command.html) — tokio's `Command` mirrors this API. The docs for `.env`, `.envs`, `.env_remove`, and `.env_clear` spell out the inherit-then-edit model, and the struct-level intro is a clean example of the builder pattern.
