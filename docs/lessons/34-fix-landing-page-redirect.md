<!-- Lesson for PR #34. Teaches one concept grounded in the real diff. -->

# PR #34 — Fix landing-page redirect (adj.ac/ent)

> **Rust lesson:** None — this PR ships no Rust. It fixes a broken client-side redirect on the static apex page (`/cent` → `/ent/`) and teaches how a zero-config `<meta http-equiv="refresh">` bounce works, plus why the `/` in `adj.ac/ent` is a real URL path boundary rather than decoration.
> **Tags:** `redirects` · `static-site` · `html`
> **Merged:** 2026-06-08 · +96/−5 · [View PR](https://github.com/nonrational/adjacent/pull/34)

## The situation

The apex domain `adj.ac` serves a bare `index.html` whose only job is to bounce
visitors to the real landing page. It redirected to `/cent` — a path that exists
nowhere in the repo. One stray `c`, and the bounce dead-ended on a 404. This PR
retargets the redirect to `/ent/` and fills in the real page underneath it.

## The idea (no Rust this time)

A static site has no server code at the apex to emit an HTTP `301`/`302`
redirect — issuing a real 3xx needs host-specific config (`.htaccess`, a
Cloudflare `_redirects` file, etc.). The option that works on *any* dumb static
host with zero config is a **meta refresh**: a tag in the HTML `<head>` that the
browser parses and acts on.

```html
<!-- index.html (apex) -->
<meta http-equiv="refresh" content="0; URL='/ent/'">
<link rel="canonical" href="https://adj.ac/ent/">
```

`content="0; URL='/ent/'"` reads as *wait 0 seconds, then navigate to `/ent/`*.
The `0` makes it immediate. Two details carry weight:

- **Root-relative, trailing slash.** The leading `/` resolves from the domain
  root regardless of where the visitor entered. The trailing slash points at the
  *directory* `/ent/`, whose index (`ent/index.html`) the host serves. The old
  `/cent` was just a typo for `/ent` — but a redirect target is a plain string,
  so nothing flagged that the path didn't exist.
- **`rel="canonical"`.** Meta-refresh shells look like duplicate content or a
  sneaky redirect to a crawler. The canonical link tells search engines "the
  real address is `/ent/` — index *that*, not this empty bouncer."

## The pun is load-bearing on the path

`adj.ac/ent` read aloud is "adjacent." That only works because it is a *real*
URL: `adj.ac` is the registered domain, `/` is the literal path separator, and
`ent` is a genuine path beneath it. So the landing content has to live at
`/ent/` for the joke to resolve — the brand depends on the file layout. The
wordmark markup encodes the same three-part split:

```html
<!-- ent/index.html -->
<h1 class="wordmark"><span class="a">adj.ac</span><span class="slash">/</span><span class="b">ent</span></h1>
```

Domain in paper-white, slash in rule-gray, `ent` in accent — the styling makes
the `/` read as a boundary, not a hyphen. (CLAUDE.md locks this: the split is
`adj.ac` + `ent`, *never* `adj` + `ac.ent`, and "the `/` is the URL path
boundary.")

## The one tooling detail

`.tool-versions` gains a line:

```
# .tool-versions
rust 1.92.0
nodejs 26.2.0
```

The site is tooled through Node (`npx live-server`, per the `justfile`), so asdf
pins a Node version alongside the Rust toolchain. Anyone building the landing
page now gets the same runtime — the pin lives beside the compiler pin even
though Cargo never touches the web frontend.

## Why it matters

A meta refresh is the cheapest redirect that runs on any static host — no server,
no config file. The cost is an extra round trip (fetch the shell, then fetch the
target) and a weaker signal to search engines than a real `301`, which the
canonical link partially repairs.

The failure this PR fixes is the one meta refresh makes easy: the target is a
*string*, not a checked reference. `/cent` ships fine and silently dead-ends —
nothing validates that the path exists until a human loads the page. Contrast a
router written in code, where a route pointing at a missing handler often won't
compile or will trip a test. A redirect string has no such safety net, so the
typo survives all the way to production.

## Related lessons

- **PR #46** also ships no Rust and covers this same `ent/` landing page — but
  from the repo-layout angle: why the site sits *beside* the Cargo workspace so
  `cargo build` never sees it. This lesson is the redirect that *points at* that
  directory; #46 is where the directory lives.

## Dig deeper

- [MDN — `<meta http-equiv>`, the `refresh` value](https://developer.mozilla.org/en-US/docs/Web/HTML/Element/meta#http-equiv) — syntax, and why the spec discourages non-zero timers for accessibility.
- [Google Search Central — Consolidate duplicate URLs (`rel=canonical`)](https://developers.google.com/search/docs/crawling-indexing/consolidate-duplicate-urls) — what the canonical link buys you on a bouncer page.
