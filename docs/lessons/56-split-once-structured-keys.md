<!-- Lesson for PR #56. Teaches one concept grounded in the real diff. -->

# PR #56 — Worktree instances: per-label registry keys, remove/prune, wildcard TLS SANs

> **Rust lesson:** `str::split_once` cuts a string at the *first* delimiter into two borrowed `&str` slices and returns an `Option`, so "there was no delimiter" is a case you handle instead of a bug you forget — the clean way to parse a structured key.
> **Tags:** `string-slices` · `split-once` · `borrowing`
> **Merged:** 2026-06-13 · +3601/−112 · [View PR](https://github.com/nonrational/adjacent/pull/56)

## The situation

The registry maps a key to a path. This PR lets one base app register under several keys: a bare `site` is the app itself, and `feature-x.site` is a *worktree instance* that routes at `feature-x.site.adj.ac`. The key encodes structure — one dot separates the instance label from the base name. Every routed request has to pull those two pieces back out of a `&str` key, and it has to be cheap.

## The Rust idea

`str::split_once(delimiter)` splits at the **first** match and hands back `Option<(&str, &str)>`. Two things are worth internalizing.

**It returns `Option`, so the "no delimiter" case can't be forgotten.** In many languages you write `"feature-x.site".split(".")`, get an array, and reach for `[0]` and `[1]` — hoping there was a dot. When there isn't, `[1]` is `undefined`, an out-of-bounds panic, or a silent empty string, depending on the language. That missing-delimiter path is a landmine. `split_once` turns it into a value: `Some((before, after))` when the delimiter is present, `None` when it isn't. A bare key like `"site"` isn't an edge case to defend against — it's simply the `None` arm of a `match`.

**The two pieces are borrowed slices — no allocation.** A `&str` is a pointer + length: a *window* into bytes someone else owns. The pair `split_once` returns points straight into the original string, so splitting a key costs two integer offsets, not two heap allocations. (Contrast a `String`, which owns its bytes on the heap. `&str` borrows; `String` owns.) The price of that speed is a lifetime: the slices are valid only while the input lives — which is exactly why the helper below *returns* borrows and lets the caller decide whether to `.to_string()` them.

(`split_once` is the precise cousin of `split`, which returns a lazy iterator over *every* piece. When a string has a known two-part shape, you want the one that gives you exactly two.)

## In this PR

The parser is six lines. `split_key` maps a registry key to `(Option<label>, base)`:

```rust
// crates/adj/src/registry.rs
/// Split a registry key into `(label, base)`. Keys are either a bare app name (`site`) or a
/// worktree-instance key (`feature-x.site`). `add` enforces at most one dot, so `split_once`
/// is total here.
pub fn split_key(key: &str) -> (Option<&str>, &str) {
    match key.split_once('.') {
        Some((label, base)) => (Some(label), base),
        None => (None, key),
    }
}

/// The app name a registry key resolves config against: the part after the instance label,
/// or the whole key when there is no label.
pub fn base_name(key: &str) -> &str {
    split_key(key).1
}
```

Read the `match`: the `Some` arm is an instance key — wrap the label in `Some`, keep the base. The `None` arm is a bare app name — there's no label, and the whole key *is* the base. `base_name` just projects the second field (`.1`). Every return value is a borrow into `key`; nothing is copied.

Now the load-bearing detail in that doc comment: "so `split_once` is total here." `split_once` always cuts at the *first* dot. On `"a.b.c"` it yields `("a", "b.c")` — everything after the first delimiter lands in the second slice. So if a dotted app name ever slipped through, `feature.my.app` would misparse as label `feature`, base `my.app`. The claim that "the first dot is *the* structural boundary" only holds because of an invariant enforced somewhere else: app names can't contain dots. That check lives at the point where a config enters the system:

```rust
// crates/adj/src/registry.rs
// Dots are structural in registry keys (`<label>.<name>` is a worktree instance), so a
// dotted app name would make `feature-x.site` ambiguous. Checked before the general
// DNS-label check so the dot case gets its own targeted message.
if cfg.name.contains('.') {
    return Err(anyhow!(
        "app name `{}` contains `.` — dots are reserved for worktree instances (`<label>.<name>`)",
        cfg.name
    ));
}
```

That's the other half of the design. Splitting downstream gets to be a one-liner *because* the boundary code guarantees at most one dot per key. Reject the malformed input where it enters (see #49), and the parser never has to cope with it.

The test pins both shapes — bare key and instance key — in one place:

```rust
// crates/adj/src/registry.rs
#[test]
fn split_key_handles_bare_and_instance_keys() {
    assert_eq!(split_key("site"), (None, "site"));
    assert_eq!(split_key("feature-x.site"), (Some("feature-x"), "site"));
    assert_eq!(base_name("site"), "site");
    assert_eq!(base_name("feature-x.site"), "site");
}
```

## Why it matters

The trap this sidesteps is the array-index split: grab `[0]`/`[1]`, forget the no-delimiter path, and ship a parser that panics or silently mangles a bare key. `split_once` makes "no dot" a branch the compiler forces you to write, and pairing it with the upstream `contains('.')` reject turns "the first dot is the only dot" from a hope into a guarantee. Because the two pieces are borrowed slices, this runs on every routed request without allocating a thing — the invariant lives in code, and reading it back out is free.

## Related lessons

- **PR #49** is the same file and the same move from the other end: the dotted-name reject above is a parse-don't-validate boundary check, refusing bad input at `read_app_config` so nothing downstream has to re-check it.
- **PR #45** leans on the same borrowing trick — its `DashboardEntry<'a>` holds `&str` views into each app summary instead of cloning strings, exactly like `split_key` returns windows into `key`.

## Dig deeper

- [`str::split_once`](https://doc.rust-lang.org/std/primitive.str.html#method.split_once) — the signature (`Option<(&str, &str)>`), plus `rsplit_once` for splitting on the *last* delimiter instead of the first.
- [The Rust Book, ch. 4.3](https://doc.rust-lang.org/book/ch04-03-slices.html) — The Slice Type: why a `&str` is a borrowed view, and how its lifetime is tied to the data it points into.
