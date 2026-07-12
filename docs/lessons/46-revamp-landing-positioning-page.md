<!--
Lesson for PR #46. Honest entry: this PR ships no Rust. It teaches how a
single-binary Rust project carries a non-Rust web frontend alongside its crates,
plus the one real Rust-ecosystem detail the diff does touch (dual licensing).
-->

# PR #46 — Revamp landing page into a positioning page

> **Rust lesson:** None — this PR has no Rust code. It's the `ent/` landing page (HTML/CSS). The lesson is how a single-binary Rust project carries a web frontend *beside* the Cargo workspace so the two never touch, and the one Rust-ecosystem convention the diff does hit: dual `MIT OR Apache-2.0` licensing.
> **Tags:** `repo-layout` · `web-frontend` · `dual-license`
> **Merged:** 2026-06-09 · +341/−24 · [View PR](https://github.com/nonrational/adjacent/pull/46)

## The situation

Adjacent needed a real landing page — not just a wordmark, but a *positioning
page*: hero, problem statement, the four-command flow, design principles, status.
A "positioning page" answers "what is this and why do I care" for a first-time
visitor, versus a "coming soon" placeholder. The whole change lives in `ent/`,
the project's static web frontend. No crate was rebuilt.

## The idea (no Rust this time)

Adjacent is one Rust binary, `adj`. But the project also ships a website. The
question every polyglot repo faces: *where do the web assets live so they don't
leak into the build of the thing you actually compile?*

Cargo's answer is the **workspace membership list**. Cargo only sees, compiles,
and lints what `members` names. Anything else in the tree is invisible to it —
a directory of HTML is just files on disk as far as `cargo build` is concerned.
So the convention is: put the frontend in a **sibling directory** to `crates/`,
and leave it out of `members`. It gets served or deployed by something else
(here, eventually Cloudflare Pages), never bundled into the binary.

## In this PR

The workspace lists two crates and nothing web-facing:

```toml
# Cargo.toml
[workspace]
members = [
    "crates/adj",
    "crates/adj-protocol",
]
```

The site lives one level up from those crates, entirely outside that list:

```
adjacent/
├── crates/          # the Cargo workspace — what `adj` compiles
│   ├── adj/
│   └── adj-protocol/
├── ent/             # the web frontend — Cargo never sees this
│   ├── index.html
│   └── favicon.svg
└── index.html       # root redirect → /ent/
```

`ent/index.html` is where the 341 lines land — plain HTML with an inline
`<style>` block, no build step. The positioning content is the new hero
category line and the two-column problem statement:

```html
<!-- ent/index.html -->
<p class="category">Adjacent is a local dev-server harness &mdash; one supervised
  dev server that you and your agent share, behind a real URL.</p>

<section class="statement">
  <h2>Your dev server has two developers now.</h2>
  <div>
    <p>When you and your agent need the same local server, you evict each other.
      The agent takes the process &mdash; you lose the logs. You take it back
      &mdash; the agent can't verify its work.</p>
    <p><strong>Adjacent supervises the server so neither of you has to own
      it.</strong> One daemon boots, routes and watches every app; both of you
      talk to it through the same CLI.</p>
  </div>
</section>
```

The root-level `index.html` is a one-line redirect so `adj.ac/` lands on the
real page at `adj.ac/ent/` — the `/ent` path split is the brand pun (`adj.ac`
+ `ent`), enforced by URL structure:

```html
<!-- index.html -->
<meta http-equiv="refresh" content="0; URL='/ent/'">
<link rel="canonical" href="https://adj.ac/ent/">
```

### The one Rust-ecosystem thing in the diff

`Cargo.toml` flips the license, and this *is* a Rust convention worth knowing:

```toml
# Cargo.toml — before / after
-license = "MIT"
+license = "MIT OR Apache-2.0"
```

Rust itself, and the overwhelming majority of crates on crates.io, dual-license
under **`MIT OR Apache-2.0`**. The SPDX `OR` means a downstream user picks
either license — MIT for simplicity, Apache-2.0 for its explicit patent grant.
It's the community default; publishing a crate under bare MIT reads as slightly
off-convention. The PR added `LICENSE-MIT` and `LICENSE-APACHE` files to match,
which is the expected on-disk layout for a dual-licensed Rust project.

## Why it matters

Keep the frontend out of `members` and Cargo stays fast and honest: `cargo build`
compiles only Rust, `cargo test` runs only Rust tests, and a broken bit of CSS
can never fail a `cargo` command. Pull the site *into* the workspace — say, as a
crate with a `build.rs` that embeds the HTML — and now every landing-page tweak
recompiles the binary and CI treats a copy edit like a code change. The sibling
directory is the cheap, correct boundary.

On licensing: `MIT OR Apache-2.0` isn't cosmetic. The Apache-2.0 half carries an
explicit patent grant that bare MIT lacks, and matching the ecosystem default is
what lets other Rust projects depend on yours without a legal review.

## Related lessons

- The `justfile` here also bumped `just serve` to `--port=8081` so live-server
  wouldn't collide with the proxy's default `:8080` — a small nod to the exact
  port-contention problem Adjacent exists to solve.

## Dig deeper

- [The Cargo Book — Workspaces](https://doc.rust-lang.org/cargo/reference/workspaces.html) — how `members` defines what Cargo compiles, and why a sibling directory stays invisible.
- [Rust API Guidelines — Necessities](https://rust-lang.github.io/api-guidelines/necessities.html#crate-and-its-dependencies-have-a-permissive-license-c-permissive) — the `MIT OR Apache-2.0` dual-license convention, stated as a guideline.
