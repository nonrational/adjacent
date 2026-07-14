<!-- Lesson for PR #45. Teaches one concept grounded in the real diff. -->

# PR #45 — Add built-in dashboard at status.adj.ac

> **Rust lesson:** `iter().map(f).collect()` is how you turn one collection into another — a lazy pipeline that does nothing until `collect` drives it.
> **Tags:** `iterators` · `map-collect`
> **Merged:** 2026-06-09 · +817/−0 · [View PR](https://github.com/nonrational/adjacent/pull/45)

## The situation

The dashboard at `status.adj.ac` polls `GET /apps.json` and gets back one JSON
object per registered app. The daemon holds each app as an `AppSummary` (name,
path, live state). The handler's job: take that list and produce a list of
wire-shaped `DashboardEntry` objects to serialize. Classic "transform a
collection into rendered rows."

## The Rust idea

In many languages you'd write a `for` loop, build an empty array, and `push`
each transformed item. Rust can do that too — but the idiomatic move is an
**iterator chain**: `.iter()` to walk the collection, `.map(f)` to transform
each element, `.collect()` to gather the results into a new collection.

Two things make this worth learning:

**Iterators are lazy.** `.map(f)` doesn't run `f` on anything. It returns a
`Map` value that *remembers* what to do. Nothing happens until something pulls
items through — here, `.collect()`. That's why you can chain ten adapters and
pay for zero of them until the final consumer runs. (In a test below, `.find()`
is a consumer that stops early — it walks only as far as the first match.)

**`.collect()` builds the target type.** The type you're collecting *into*
drives it. Annotate the binding as `Vec<DashboardEntry>` and `collect` fills a
`Vec`. The compiler infers the rest.

## In this PR

The transform is one line — a pure, synchronous mapping:

```rust
// crates/adj/src/status.rs
let dtos: Vec<DashboardEntry> = entries.iter().map(dashboard_entry).collect();
// ... then serde_json::to_vec(&dtos) turns the Vec into the JSON body
```

`entries.iter()` yields `&AppSummary`. `dashboard_entry` takes exactly that and
returns a `DashboardEntry`, so it drops straight in as the map function — no
closure needed. `.collect()` gathers them into the `Vec` the annotation asks
for. Read it left to right: "for each summary, make a dashboard entry, gather
them up."

Contrast that with the function that *builds* `entries` a few lines down:

```rust
// crates/adj/src/status.rs
async fn snapshot(supervisor: Arc<Supervisor>) -> anyhow::Result<Vec<AppSummary>> {
    let reg = Registry::load()?;
    let mut entries = Vec::with_capacity(reg.apps.len());
    for (name, entry) in &reg.apps {
        let state = supervisor.state(name).await;
        entries.push(AppSummary {
            name: name.clone(),
            path: entry.path.display().to_string(),
            state,
        });
    }
    Ok(entries)
}
```

Why a plain `for` loop here and a `.map().collect()` there? Because this loop
`.await`s `supervisor.state(name)` on every pass. A plain `.map()` closure can't
`await` — its output would be a future, not a value — so an async transform
falls back to the loop. The rule of thumb: **synchronous, side-effect-free
transform → iterator chain; anything that awaits or fails per-item → loop.**
The two idioms sit side by side in the same file, each used where it fits.

One efficiency note in the loop: `Vec::with_capacity(reg.apps.len())`
pre-allocates for the known count, so the `push`es never trigger a reallocation
+ copy as the vector grows.

## Why it matters

`.iter().map(f).collect()` reads as a single intention — "these become those" —
where a hand-rolled loop makes you track a mutable accumulator and read three
lines to confirm it only ever appends. The laziness means chains stay cheap: no
intermediate collection is materialized between adapters. And because
`dashboard_entry` returns a `DashboardEntry<'_>` that *borrows* (`&'a str`) from
each `AppSummary` rather than cloning strings, the whole render is references
until `serde_json::to_vec` walks them — the `entries` vector just has to outlive
the serialize call.

The trap the chain sidesteps: reaching for it when the per-item work can fail or
await. A `.map()` that returns `Result` leaves you holding a
`Vec<Result<T, E>>` and no clean error path; that's exactly why `snapshot` stays
a loop with `?`. Match the tool to the shape of the work.

## Related lessons

- PR #41 owns `include_str!` — the static HTML shell (`const HTML`) is embedded
  that way; this lesson is only about the *dynamic* JSON the iterator chain builds.
- PR #16 — the `Arc<Supervisor>` threaded through `snapshot` and `handle` is the
  `Arc<Mutex>` sharing pattern.

## Dig deeper

- [The Rust Book, ch. 13.2](https://doc.rust-lang.org/book/ch13-02-iterators.html) — Processing a Series of Items with Iterators (laziness, `map`, `collect`).
- [`std::fmt::Write`](https://doc.rust-lang.org/std/fmt/trait.Write.html) — the string-building cousin: when you *do* assemble text by hand, `write!`/`writeln!` append into one `String` instead of allocating a fresh string per `format!` + `+`.
