<!-- Lesson for PR #19. Teaches one concept grounded in the real diff. -->

# PR #19 — Env loading: [env] table and env_file pointer

> **Rust lesson:** `Option<T>` is how a struct field says "this may be absent" — and serde's derive fills a missing `Option` field with `None` for you, while `#[serde(default)]` is what saves any *non*-`Option` field from erroring when its key is missing.
> **Tags:** `option` · `serde` · `deserialize` · `config`
> **Merged:** 2026-06-08 · +509/−1 · [View PR](https://github.com/nonrational/adjacent/pull/19)

## The situation

`adjacent.toml` grew two new, entirely optional fields: an `[env]` table of committed-safe `KEY = "value"` pairs, and `env_file`, a pointer to a dotenv file. Most apps set neither. The config struct has to parse a TOML file where these keys are usually just *not there* — and it must not treat "absent" as an error. That is a modeling question before it is a parsing one: how does a Rust struct field say "maybe nothing"?

## The Rust idea

Rust has no `null`. A `String` field always holds a real string; there is no back-door "empty" value that means "unset." When absence is a legitimate state, you say so in the type with `Option<T>` — an enum with exactly two variants:

```rust
enum Option<T> {
    None,       // absent
    Some(T),    // present, carrying a T
}
```

The payoff is that the compiler forces every reader to handle both cases before they can touch the inner `T`. "I forgot the field might be missing" stops being a runtime surprise and becomes a compile error.

serde bridges this to the data on disk. When you `#[derive(Deserialize)]`, serde generates code that walks the TOML and fills each struct field. Its default stance is strict: **a field whose key is missing from the input is an error** ("missing field `port`"). That is usually what you want — a required field that silently vanished is a bug.

Two things soften that strictness, and they are easy to conflate:

- `#[serde(default)]` — "if this key is missing, use the type's `Default` instead of erroring." This is what makes a field genuinely optional at the data level.
- `Option<T>` — a *special case* serde already knows about. A missing `Option<T>` field deserializes to `None` on its own, without `#[serde(default)]`, because serde special-cases the "no value" path for options.

So for the `Option` fields in this PR, the `#[serde(default)]` attribute is technically redundant — they would parse to `None` regardless. It is kept for consistency with the existing `port_env` field above them, and because the intent reads clearly at a glance. Drop `Option` for a required-but-defaulted field (say `port: u64` defaulting to `0`) and the attribute stops being optional itself: without it, a missing `port` is a hard error.

## In this PR

The whole change to the config type is four lines of field declarations:

```rust
// crates/adj/src/registry.rs
pub struct AppConfig {
    pub name: String,
    pub cmd: String,
    #[serde(default)]
    pub port_env: Option<String>,
    /// Committed-safe environment variables merged into the spawned process env.
    /// On conflict with `env_file`, this table wins. PORT injection always wins over both.
    #[serde(default)]
    pub env: Option<BTreeMap<String, String>>,
    /// Path to a dotenv-format file, resolved relative to the registered app directory.
    /// Missing files are a startup error.
    #[serde(default)]
    pub env_file: Option<String>,
}
```

`name` and `cmd` are bare `String` — required, and a manifest missing either one is rightly rejected. The two new fields are `Option`, so `None` is the normal, no-error resting state.

Notice `env` is `Option<BTreeMap<String, String>>`. A TOML `[env]` table maps straight onto a map type, and serde recurses: it deserializes each `KEY = "value"` entry into a `(String, String)` pair. `BTreeMap` (not `HashMap`) keeps the keys sorted, so iterating it later to set env vars is deterministic — the same manifest always applies its overrides in the same order.

The consumer side then reads these as ordinary `Option`s, and the `if let Some(...)` is the compiler-enforced "handle both cases":

```rust
// crates/adj/src/supervisor.rs
// Resolve env layers before any port allocation so a missing `env_file` or unreadable
// file fails fast with a clear error (and doesn't leak a port reservation).
let env_file_values = if let Some(rel) = cfg.env_file.as_deref() {
    let resolved = app_dir.join(rel);
    Some(load_env_file(&resolved)?)
} else {
    None
};
```

```rust
// crates/adj/src/supervisor.rs
if let Some(values) = &cfg.env {
    for (k, v) in values {
        command.env(k, v);
    }
}
command.env(port_env, port.to_string());
```

`None` means "no `[env]` table, do nothing"; `Some(values)` unwraps the map so you can iterate it. There is no way to iterate the map without first proving it exists — that is `Option` doing its job.

One honest footnote: `env_file` points at a *dotenv* file, which is not TOML, so serde doesn't parse its contents. The PR hand-rolls a tiny `parse_dotenv` in `crates/adj/src/env.rs`. serde's reach ends at the `adjacent.toml` boundary; the file it points to is parsed by hand.

## Why it matters

In a language with `null`, "this field is optional" and "I forgot to set this field" look identical at the type level — both are `null` — and the difference only shows up as a `NullPointerException` at 2am. Rust splits them: absence is `Option`, and the compiler won't let you read through it without checking. Pair that with serde's default strictness and you get a config loader where *required* fields are enforced for free (a missing `cmd` is rejected at parse time) and *optional* fields are spelled out deliberately, one `Option` at a time. The trap this avoids is the silent empty-string or zero-value that a laxer parser would hand you for a field the user never wrote.

## Related lessons

- PR #48 stays in this same `AppConfig` neighborhood but on the `Result` side — not swallowing a parse `Err` when a *present* field is malformed. #19 is about a field being absent; #48 is about a field being present-but-wrong.
- PR #22 covers the mirror-image direction, serde `Serialize` for the JSON DTOs; PR #75 covers *writing* an `adjacent.toml`. This lesson stays strictly on the `Deserialize` + `Option` reading side.

## Dig deeper

- [The Rust Book, ch. 6.1](https://doc.rust-lang.org/book/ch06-01-defining-an-enum.html#the-option-enum-and-its-advantages-over-null-values) — the `Option` enum and its advantages over null.
- [serde: field attributes](https://serde.rs/field-attrs.html) — `#[serde(default)]` and how missing fields are handled.
