<!--
Lesson template. One file per merged PR: docs/lessons/<NN>-<slug>.md
Goal: teach one practical Rust concept, grounded in real code this PR shipped.
Keep it tight. Real snippets from the diff, not invented ones. Honest labels:
if a PR has little or no Rust, say so and teach the tooling/ecosystem lesson instead.
This file is also meant to be postable as a post-merge teaching comment on the PR itself.
-->

# PR #NN — <PR title>

> **Rust lesson:** <one sentence — the single concept a reader should walk away with>
> **Tags:** `concept-one` · `concept-two`
> **Merged:** YYYY-MM-DD · +A/−D · [View PR](https://github.com/nonrational/adjacent/pull/NN)

## The situation

One or two sentences: what the PR set out to do, in plain terms. The practical
problem that forced the Rust concept to show up.

## The Rust idea

Teach the concept. Assume the reader knows another language but is new to Rust.
Explain *why* Rust works this way, not just the syntax.

## In this PR

Real code from the diff, with the file path. Walk through the lines that matter.

```rust
// crates/adj/src/<file>.rs
<snippet lifted from the actual PR>
```

## Why it matters

The trap this avoids, or what a non-Rust language would have let you get wrong.

## Related lessons

- PR #NN also leans on this.

## Dig deeper

- [The Rust Book, ch. N.N](https://doc.rust-lang.org/book/) — <chapter name>
