<!-- Lesson for PR #41. Teaches one concept grounded in the real diff. -->

# PR #41 — Add `adj agent-instructions` command

> **Rust lesson:** A raw string literal (`r#"…"#`) bakes a whole multi-line document into the binary verbatim — no escaping, no external file — and `format!`'s in-scope capture (`{name}`) splices in the dynamic fields, so the template ships *inside* the binary and only the two live values are read at run time.
> **Tags:** `raw-string-literals` · `format-macro` · `string-templating`
> **Merged:** 2026-06-08 · +783/−0 · [View PR](https://github.com/nonrational/adjacent/pull/41)

## The situation

`adj agent-instructions` prints a markdown steering doc telling a coding agent to
drive the app through `adj` instead of running the dev command itself. The doc is
mostly fixed prose with two holes: the app's `name` and its `cmd`, both read from
`adjacent.toml`. The whole thing prints to stdout — no daemon, no template file on
disk. So where does the boilerplate live, and how do the two fields get spliced in?

## The Rust idea

**Raw string literals.** An ordinary `"..."` string processes escapes: `\n` is a
newline, `\t` a tab, and to put a literal `"` or `\` inside you have to escape it.
That gets painful fast for a multi-line markdown doc full of backticks and `#`
headers. A *raw* string turns escape processing off — `r"..."` treats every
character between the quotes literally, backslashes and newlines included.

The `#` delimiters guard the ends: `r#"..."#` only terminates at the `"#` sequence,
so a bare `"` inside the content won't close it early. Need to embed the sequence
`"#` itself? Add more hashes — `r##"..."##`. This is the same tool you'd reach for
with a regex or a Windows path, here holding a page of markdown exactly as it
renders.

Because the literal is written directly in the source, it compiles into the
binary's read-only data. The boilerplate is *self-contained* — there is no
`template.md` to ship alongside `adj` and no file read at run time for the fixed
text.

**Templating with `format!`.** The two dynamic fields are `{name}` and `{cmd}`.
Since Rust 2021, `format!` captures identifiers that are already in scope: `{name}`
means "the local variable `name`," with no trailing `name = name` argument. That's
why `render`'s parameters are *named* `name` and `cmd` to match the placeholders,
and the `format!` call has no arguments after the string.

## In this PR

The template is a raw string literal; `format!` fills the two holes (trimmed):

```rust
// crates/adj/src/agent_docs.rs
fn render(name: &str, cmd: &str) -> String {
    format!(
        r#"# Working with `{name}` via Adjacent

This project's dev server is supervised by **Adjacent** (`adj`). The agent does not
start the server directly — `adj` lazy-boots it on the first proxied request, captures
stdout/stderr to `~/.adjacent/logs/{name}.log`, and stops it on idle.

## Don't run the dev command yourself

Don't run `{cmd}` directly. Adjacent owns the process. Running it directly
double-binds the port and Adjacent loses visibility into the log stream.
// ... (adj status / logs / restart / wait-ready, verify loop) ...
"#
    )
}
```

The only thing read from disk at run time is the manifest, and only to pull `name`
and `cmd` out of it — everything else is already in the binary:

```rust
// crates/adj/src/agent_docs.rs
pub fn emit(path: Option<String>) -> Result<()> {
    let dir: PathBuf = match path {
        Some(p) => PathBuf::from(p),
        None => std::env::current_dir().context("resolving current directory")?,
    };
    let cfg = registry::read_app_config(&dir)?;
    print!("{}", render(&cfg.name, &cfg.cmd));
    Ok(())
}
```

One subtlety: `format!` still treats `{` and `}` specially even inside a raw string,
so a *literal* brace in the doc would have to be written `{{` or `}}`. This template
has none, which is part of why the plain `format!` stays readable.

## Why it matters

Reach for a run-time read instead — `std::fs::read_to_string("template.md")` — and
the binary grows a hidden dependency: the file has to exist at the right path when
`adj` runs. Ship the binary on its own and the command breaks at run time with a
"file not found," not at build time. Baking the text into the source removes that
whole failure mode: if it compiled, the content is there.

The sibling technique for the *same* self-contained benefit is `include_str!`, which
pulls an entire separate file's bytes in **at compile time** — best when the asset is
large or edited by someone who shouldn't touch `.rs` files. This repo does exactly
that for the dashboard HTML (`const HTML: &str = include_str!("../assets/status.html")`
in `status.rs`). For a two-field template a few dozen lines long, an inline raw string
keeps the text next to the one function that formats it — no second file to keep in
sync. Same "it's in the binary" guarantee, different call on where the source text
lives.

## Related lessons

- PR #48 covers the `Result` / `?` / `anyhow::Context` machinery visible in `emit`
  here — the `.context(...)?` that turns a failed `current_dir()` into a labeled
  error instead of a silent one.
- The `include_str!` route to a self-contained binary — embedding a *whole file* at
  compile time — shows up in the dashboard work (PR #45); this PR deliberately inlines
  a literal instead, so the two make a clean before/after on "when is the asset big
  enough to earn its own file?"

## Dig deeper

- [Rust Reference — Raw string literals](https://doc.rust-lang.org/reference/tokens.html#raw-string-literals) — the `r#"…"#` grammar and the multi-hash rule.
- [`std::fmt`](https://doc.rust-lang.org/std/fmt/) — `format!` syntax, including captured identifiers (`{name}`) and escaping braces with `{{` / `}}`.
- [`std::include_str!`](https://doc.rust-lang.org/std/macro.include_str.html) — the sibling: embed an external file's contents into the binary at compile time.
