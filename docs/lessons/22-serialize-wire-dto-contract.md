<!-- Lesson for PR #22. Teaches one concept grounded in the real diff. -->

# PR #22 — Agent JSON output on read commands

> **Rust lesson:** Emit your public JSON through dedicated DTO structs, not your internal types — `#[derive(Serialize)]` when the type's natural shape already matches the contract, and hand-write `impl Serialize` when you need to pin an exact shape the derive won't give you.
> **Tags:** `serde` · `Serialize` · `wire-contract`
> **Merged:** 2026-06-08 · +1071/−38 · [View PR](https://github.com/nonrational/adjacent/pull/22)

## The situation

Every read command (`adj list`, `status`, `logs`) grew a `--json` flag, and the output is a *documented, stable* schema — `crates/adj/JSON.md` is the contract, and the test suite asserts the exact keys. The catch: the internal `AppState` enum already crosses the daemon's IPC socket as serde JSON, but the shape serde gives that enum is *not* the flat shape the public `--json` contract promises. The PR had to publish an exact documented shape without letting serde's internal enum encoding leak out as the public API.

## The Rust idea

`#[derive(Serialize)]` is a procedural macro. At compile time it reads a type's fields and *writes* an `impl Serialize` for you — attribute-driven code generation. You never see the generated code, but it's ordinary Rust, and `#[serde(...)]` attributes steer what it emits:

- `#[serde(rename_all = "snake_case")]` — `Stdout` becomes `"stdout"` on the wire.
- `#[serde(tag = "kind")]` on an enum — each variant serializes as an object carrying a `kind` discriminator, e.g. `{"kind":"running","pid":…,"port":…}`.
- `#[serde(skip_serializing_if = "Option::is_none")]` — omit the field entirely when the check passes.

That derived encoding is faithful and *reversible* — it round-trips cleanly back through `Deserialize`, which is exactly what you want for a type you both send and receive (the daemon's IPC). But it is welded to the type's internal structure. Rename a variant, add a field, switch the tag representation, and the JSON moves with it. That's fine for an internal wire format both ends rebuild from the same crate. It's a landmine for a *public* contract that outside `jq` scripts depend on.

The discipline is a **DTO** — a data-transfer object, a struct whose only job is the wire shape, separate from the type it borrows from. Two cases:

1. When the DTO's natural derived shape already equals the contract, derive it and move on.
2. When it doesn't — you need a flat object, a hoisted field, an enum rendered as a bare string — remember `Serialize` is just a trait. The derive is one implementation; you can write your own and emit exactly the keys the contract documents.

This PR does both, side by side.

## In this PR

The internal type is a tagged enum. It derives `Serialize`/`Deserialize` because it round-trips over IPC, and the new `started_at` is optional so old serialized state still deserializes:

```rust
// crates/adj-protocol/src/lib.rs
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppState {
    Stopped,
    Running {
        pid: u32,
        port: u16,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
    },
    Crashed { exit_code: i32 },
}
```

Serialized directly, an `AppState::Running` is `{"kind":"running","pid":…,"port":…}` — the `kind` tag, port nested inside the state object. But `JSON.md` documents `list` entries as *flat*: `{ "name": …, "path": …, "state": "running", "port": 53412 }`. `state` is a plain string; `port` sits at the top level and appears only when running. The derive can't produce that from `AppState` — so the PR introduces a DTO with a hand-written impl:

```rust
// crates/adj-protocol/src/lib.rs
#[derive(Debug, Clone)]
pub struct ListEntryDto<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub state: &'a AppState,
}

impl<'a> Serialize for ListEntryDto<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("name", self.name)?;
        map.serialize_entry("path", self.path)?;
        map.serialize_entry("state", state_tag(self.state))?;
        if let AppState::Running { port, .. } = self.state {
            map.serialize_entry("port", port)?;
        }
        map.end()
    }
}
```

Every documented key is written by hand, in order. `state` is flattened to a bare string by a small helper instead of the enum's tagged object:

```rust
// crates/adj-protocol/src/lib.rs
fn state_tag(state: &AppState) -> &'static str {
    match state {
        AppState::Stopped => "stopped",
        AppState::Running { .. } => "running",
        AppState::Crashed { .. } => "crashed",
    }
}
```

The `if let` is where "`port` present iff running" lives — it's a fact about the *contract*, expressed in the serializer, not a property serde could infer from the enum. The DTO borrows (`&'a str`, `&'a AppState`) rather than cloning, so producing the wire view costs nothing but the JSON text. `StatusDto` does the same for `status --json`, adding `pid` / `started_at` / `exit_code` per state.

Contrast the log record. Its natural flat shape *is* the contract, so the derive is enough — no hand-written impl:

```rust
// crates/adj-protocol/src/lib.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    pub ts: String,
    pub stream: LogStream,
    pub line: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
}
```

That's the whole judgment call in one file: derive `LogRecord` (shape matches, and it round-trips back off disk), hand-write `ListEntryDto` (shape doesn't match, and it's write-only).

## Why it matters

Slap `#[derive(Serialize)]` on `AppState` and hand it to `println!` and you've quietly published serde's `{"kind":"running",…}` nesting as your public API. It works, the tests pass, everyone's happy — until you rename a variant or restructure the enum six months later and every downstream `jq` pipeline breaks, with nothing in the type to warn you the shape was load-bearing. The DTO is the seam that decouples the contract from the type: `AppState` stays free to change, `ListEntryDto`'s impl stays pinned to `JSON.md`, and `json_output.rs` asserts the exact keys so drift shows up as a red test, not a support ticket. In a dynamically-typed language you'd assemble that dict by hand anyway; Rust's derive is convenient enough to tempt you into exposing the internal shape by accident, and the DTO is the deliberate "no, this is the wire format" boundary.

## Related lessons

- **PR #16** (`Arc<Mutex<T>>`) — this same PR reuses that pattern to write the JSONL: the `LogWriter` wraps the file in `Arc<Mutex<tokio::fs::File>>` so the stdout and stderr reader tasks can append `LogRecord`s without racing.
- **PR #19** teaches the mirror image on the read side — `#[derive(Deserialize)]` and `Option` fields. Here `LogRecord` round-trips both ways; #22 is the `Serialize` half, #19 the `Deserialize` half.
- **PR #40** (generics) — the hand-written `fn serialize<S: Serializer>` is generic over the output format; that's the trait-bound machinery #40 covers, applied to serde's serializer.

## Dig deeper

- [serde.rs — Using derive](https://serde.rs/derive.html) — how `#[derive(Serialize, Deserialize)]` and the `#[serde(...)]` attributes drive codegen.
- [serde.rs — Implementing Serialize](https://serde.rs/impl-serialize.html) — writing the trait by hand with `SerializeMap`, for the shapes the derive won't give you.
