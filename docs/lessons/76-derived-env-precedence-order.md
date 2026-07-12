<!-- Lesson for PR #76. Teaches one concept grounded in the real diff. -->

# PR #76 — Inject an ADJ_* boot environment into supervised apps

> **Rust lesson:** Compute a namespace of derived values into a `Vec<(String, String)>` with `format!`, then express precedence as insertion *order* — inject that layer last so `Command::env`'s last-writer-wins overwrites anything the user set.
> **Tags:** `format!` · `last-writer-wins` · `derived-data`
> **Merged:** 2026-06-24 · +1303/−12 · [View PR](https://github.com/nonrational/adjacent/pull/76)

## The situation

PR #17 injected one variable: `$PORT`. That let an app *bind*, but not *address itself*. A dev server behind the proxy still defaults its own links to `localhost:<port>` — wrong for every URL a user clicks through `*.adj.ac`. The daemon knows the real facts (the name, the `.adj.ac` host, the listener ports); the app can't reconstruct them. So the daemon now hands the app a whole reserved namespace — `ADJ_NAME`, `ADJ_HOST`, and four base URLs — computed at boot. And because they're *authoritative*, they have to beat anything the user set in `env_file` or `[env]`.

## The Rust idea

Two ideas, tied together.

**Deriving data with `format!`.** `format!` is `println!`'s sibling that returns instead of prints: it allocates a brand-new owned `String` from a template. `format!("https://{host}")` interpolates the `host` variable directly (Rust's inline captures — the name in the braces is a real variable in scope). Owned is the key word: the returned `String` carries its own bytes, so a function can *return* it or push it into a `Vec` without any borrow outliving the stack frame it came from. That's what makes a pure "build the values" helper possible — feed it a name and two ports, get back owned pairs, no daemon or socket required to test it.

**Precedence as insertion order.** `Command::env(key, value)` inserts-or-overrides a single entry (see #17). When you apply several layers to the same command, *the order you apply them in is the precedence policy* — the last write for a given key wins. There's no merge function, no priority flag, no "if not already set" check. To make one layer authoritative, you run it last. Precedence stops being a rule buried in a helper and becomes a visible fact about the sequence of statements.

## In this PR

A pure helper builds the derived pairs. Note it returns `Vec<(String, String)>` — an *ordered* list of owned key/value tuples, not a map — because the caller only iterates it, and order is exactly what the precedence step needs:

```rust
// crates/adj/src/supervisor.rs
fn adj_env(name: &str, http: Option<u16>, https: Option<u16>) -> Vec<(String, String)> {
    let host = format!("{name}{}", crate::proxy::HOST_SUFFIX);
    let mut vars = vec![
        ("ADJ_NAME".to_string(), name.to_string()),
        ("ADJ_HOST".to_string(), host.clone()),
        ("ADJ_URL".to_string(), format!("https://{host}")),
        ("ADJ_URL_HTTP".to_string(), format!("http://{host}")),
    ];
    if let Some(p) = https {
        vars.push(("ADJ_URL_DIRECT".to_string(), format!("https://{host}:{p}")));
    }
    if let Some(p) = http {
        vars.push((
            "ADJ_URL_HTTP_DIRECT".to_string(),
            format!("http://{host}:{p}"),
        ));
    }
    vars
}
```

- `host` is computed once, then reused. `host.clone()` hands one owned copy to the `ADJ_HOST` pair while the original stays live for the three `format!` calls below it — each of those *borrows* `host` (`&host` under the hood) to read its bytes, so `host` must still exist.
- The four unconditional entries go in a `vec![…]` literal. The two `_DIRECT` URLs are **conditional**: `if let Some(p) = https` unwraps the port only when it resolved, and `.push`es a pair carrying it. A `None` port means that URL is simply absent — no entry pointing at a dead port. Optionality is expressed by *whether the pair exists*, not by an empty string.
- Everything is an owned `String`. `"ADJ_NAME".to_string()` and each `format!` produce data the `Vec` owns outright, so the whole thing can be returned by value.

Then `up()` applies it as the final env layer:

```rust
// crates/adj/src/supervisor.rs
command.env(port_env, port.to_string());
// Daemon-owned ADJ_* namespace: the app's own external identity and URLs. Injected
// after env_file/[env] so these authoritative values win over anything a user set.
for (k, v) in adj_env(&name, self.proxy_ports.http(), self.proxy_ports.https()) {
    command.env(k, v);
}
```

The full precedence chain, top to bottom, is `env_file` → `[env]` → `PORT` → `ADJ_*`. Each `command.env` call overwrites the same key from an earlier layer. The `ADJ_*` loop runs *last*, so if a user set `ADJ_HOST` in their `[env]` table, the daemon's value silently overwrites it. That's the entire enforcement of "reserved, daemon-owned namespace" — no validation, no rejection, just position in the sequence.

Because `adj_env` is pure and returns plain data, the unit tests skip the daemon entirely and collect the pairs into a `HashMap` for random-access lookup by key:

```rust
// crates/adj/src/supervisor.rs
let vars: std::collections::HashMap<String, String> =
    adj_env("alannorton-com", Some(8080), Some(8443))
        .into_iter()
        .collect();
assert_eq!(vars["ADJ_URL_DIRECT"], "https://alannorton-com.adj.ac:8443");
```

The builder returns a `Vec` (order matters, for precedence and for iteration); the test `.collect()`s it into a `HashMap` because *it* only wants "give me the value for this key." Same pairs, two container shapes, each chosen for what the code at hand does with them.

## Why it matters

In a language where you mutate a shared, global environment or reach for a `merge(defaults, overrides)` helper, precedence is implicit and easy to get backwards. Someone reorders two blocks, or flips an argument, and user config silently starts winning over values that were supposed to be authoritative — a bug that type-checks and passes most tests. Here the policy is one readable fact: the `ADJ_*` loop is the last thing that touches the environment before spawn, so it wins by construction. Move it above the `[env]` layer and the precedence inverts — but you'd *see* that in the diff, because it's a line order, not a hidden flag.

The derived-data half pays off in testing. `format!` returning owned `String`s lets `adj_env` be a pure function of `(name, http, https)` — no I/O, no clock, no socket. Every value format, the worktree-key case, and the "port unresolved → drop the URL" rule get asserted in plain unit tests that run in microseconds, with the end-to-end test left to prove only that the wiring reaches a real child.

## Related lessons

- PR #17 is the one-variable version of this exact mechanic: `Command::env` as insert-or-override, and why the value must be an owned `String`. #17 injects `PORT` alone; #76 generalizes it into a computed namespace and makes the *layer order* load-bearing. Read #17 first for the builder and inheritance model; this lesson is about deriving a set of values and using order as precedence.
- PR #45 builds rows the cousin way — `map` + `collect` into a collection — where order doesn't carry meaning the way it does here.

## Dig deeper

- [`std::fmt` / the `format!` macro](https://doc.rust-lang.org/std/macro.format.html) — the template syntax, inline argument captures (`{host}`), and that it returns an owned `String`.
- [`std::collections::HashMap`](https://doc.rust-lang.org/std/collections/struct.HashMap.html) — the "Which collection?" preamble at the top of the [`std::collections`](https://doc.rust-lang.org/std/collections/index.html) module is the canonical guide to `Vec` vs `HashMap`: sequence-with-order vs lookup-by-key, exactly the choice this PR makes twice.
