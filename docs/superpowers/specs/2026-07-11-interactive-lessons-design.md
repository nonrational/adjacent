# Interactive surface for the Rust lessons series

**Date:** 2026-07-11\
**Status:** Approved design, pre-implementation

## Problem

`docs/lessons/` holds a 31-file teaching series: one Rust lesson per merged PR, each
grounded in the real diff. The files are excellent as prose, but a newcomer hits
foundational terms they don't yet own. A lesson says "hand the task an `Arc<Mutex<T>>`
clone" and assumes the reader can already climb ownership → move → `Send`/`Sync` to
appreciate why. Today that climb is on the reader.

We want an interactive surface where a reader can click a foundational term and get a
plain-language definition, why it matters, and — the distinctive ask — **what they'd need
to learn first**. The prerequisite chain is the novel part, not an afterthought.

## Goal

Generate an interactive HTML page per lesson, served from `ent/lessons/`, where:

- Foundational terms in the prose are clickable.
- Clicking a term opens a side panel with a definition, a "why it matters" line, and a
  row of clickable **prerequisite** terms ("learn these first").
- The lesson markdown in `docs/lessons/` stays the single source of truth. The interactive
  layer is generated *from* it, never a fork of it.

The ultimate goal (see "Where it's headed") is a reusable skill that does this for any
repo, annotating its PRs with lesson plans and building this same interactive site. This
v1 builds the concrete Adjacent instance, with the code seamed so that extraction is a
lift, not a rewrite.

## Design

### Data model — the glossary is the "learn more" layer

One checked-in file, `docs/lessons/glossary.json`, sits next to the lessons it explains.
Each entry:

```json
{
  "arc": {
    "term": "Arc<T>",
    "aliases": ["Arc", "Arc<Mutex<T>>", "Arc<Mutex<Inner>>"],
    "short": "Atomically reference-counted shared pointer. Cloning it bumps a refcount and hands back a second owner of the same value, not a copy.",
    "why": "Lets two threads or tasks own one value at once. It is how a detached task can touch state the spawner still holds.",
    "prereqs": ["ownership", "move", "send-sync"],
    "link": { "label": "Rust Book 16.3", "url": "https://doc.rust-lang.org/book/ch16-03-shared-state.html" }
  }
}
```

- **`aliases`** are the exact strings that appear in the prose (mostly inside backticks)
  that resolve to this entry.
- **`prereqs`** are other glossary keys. This array *is* the dependency graph; there is no
  separate graph file. It is a directed graph encoded as adjacency lists.
- The side panel shows a term's **direct** prereqs (one level) as clickable chips. Clicking
  one swaps the panel to that term. One level plus click-to-descend gives the "climb the
  ladder" experience with zero cycle risk and no rendered node diagram.

The glossary is an authored data file, drafted by the agent and reviewed by a human before
publish, because teaching accuracy is the whole point. Framing note for reusability: the
glossary is an **output** the tooling produces for a repo, not an asset baked into the
tooling. A Python project would get a Python glossary.

### How a prose term becomes clickable

The lessons already wrap Rust terms in backticks (inline code). The generator walks each
lesson's inline-code spans; any span whose text matches a glossary alias becomes an
annotated, clickable term. The **Tags** line in each lesson header (already a controlled
vocabulary) also becomes clickable into the glossary.

Two consequences, stated plainly:

- The lesson markdown is never modified. It stays the single source of truth.
- Only backticked code spans and tag chips are annotation candidates, so an English prose
  word never false-matches. The word "move" in a sentence stays plain; only `move` in
  backticks lights up. The cost: a multi-word prose phrase that is not backticked (for
  example "automatic reference counting" written out) does not annotate in v1. This is the
  honest limitation, recorded in Out of scope and dialed up later.

Every matching code span is annotated (not just the first per page); code spans are already
visually distinct, so a subtle dotted underline on the glossary matches is a light touch.
If it reads as noisy in review, the dial is "first occurrence per page."

### Generation pipeline — `ent/lessons/build.mjs`

A dependency-free Node ESM script. Because every lesson has the identical template
structure, it parses with targeted logic instead of pulling in a CommonMark library, so
`ent/` stays dependency-free.

Parser scope (the bounded set the lessons actually use, confirmed by survey):

- ATX headings `#` / `##` / `###`
- The three-line blockquote header (lesson / tags / merged)
- Paragraphs, bold `**...**`, italic `*...*`, inline code `` `...` ``, links `[t](u)`
- Fenced code blocks ` ```lang ` for any language (bash, html, make, markdown, rust, sh,
  toml, yaml). **Fenced content is literal**: a ` ```markdown ` block is rendered as code,
  never re-parsed, and inline-code annotation does not descend into fences.
- Unordered lists with one level of nesting; ordered lists (`1.`)

Explicitly **not** in the lesson bodies (so not in scope): tables, images, raw HTML in
prose. (The one table and the reading-path lists live in `README.md`; see the index below.)

Steps:

1. Read and validate `glossary.json`: every `prereqs` key resolves to a real entry, aliases
   are unique across entries, and emit a **coverage report** of backticked terms found
   across all lessons that are not in the glossary (so gaps are visible).
2. For each `docs/lessons/<NN>-<slug>.md`: parse the leading comment, `# PR #NN — title`,
   the three-line blockquote, and the `##` sections; render the bounded markdown to HTML;
   annotate glossary matches in inline-code spans and the tags line; rewrite cross-lesson
   links (`40-...md` → `40-...html`).
3. Emit `ent/lessons/<NN>-<slug>.html` per lesson, plus `ent/lessons/index.html`.
4. Every generated file carries a `<!-- generated by build.mjs; edit docs/lessons/*.md -->`
   header comment.

Shared assets, emitted once into `ent/lessons/` and referenced by every page:
`lessons.css`, `drawer.js`, and `glossary.js` (which assigns `window.__GLOSSARY__ = {...}`
so the drawer needs no `fetch` and works over `file://`). A multi-page set shares assets
rather than inlining 31 copies; this is a deliberate departure from the single landing
page's fully-inlined approach, chosen for maintainability. (If fully self-contained pages
are preferred, the generator can inline instead.)

The index page is built by parsing `README.md`'s table rows and reading-path lines with
targeted regex (same known-structure philosophy as the lesson parser, no general table
renderer). This keeps `README.md` the source of truth for the curated takeaways, the ◦
honesty markers, and the themed reading paths.

### The engine / theme seam (for reusability)

`build.mjs` is structured as a neutral **engine** plus a thin **theme + config** layer, so
the eventual skill reuses the engine and swaps the theme:

- **Engine (portable, no Adjacent identifiers):** parse a lesson of the known template
  shape; render the bounded markdown; match and annotate glossary aliases; build the drawer
  data; emit a page and the shared assets; parse an index from a README of the known shape.
- **Theme + config (repo-specific):** a single `THEME` object at the top of the file —
  palette, font stack, wordmark parts, site title, repo URL, and the input/output paths.
  v1 fills it with Adjacent's values (`--ink`/`--paper`/`--accent #d4a574`, JetBrains Mono,
  the `adj.ac/ent` wordmark, `docs/lessons` → `ent/lessons`).

This is one file with a clear internal boundary, not a premature multi-package split
(YAGNI). The boundary is the documented extraction line.

### Page anatomy and interaction

Reuse `ent/index.html`'s design language exactly: `--ink` background, `--paper` text,
`--accent` `#d4a574`, JetBrains Mono, the dotted-underline link treatment. Dark-only, to
match the landing page.

- **Reading column** centered, roughly 72ch, article-style.
- **Term affordance:** glossary-matched code spans get a dotted underline in the accent
  color and a pointer cursor. Plain code stays plain.
- **Side panel** slides in from the right (about 360px) on term click, showing: the term in
  monospace accent, the short definition, "Why it matters", a "Learn first" row of clickable
  prereq chips, and the external link. Descending into a prereq swaps the content with a
  back affordance. Closes on Esc or click-away.
- **Progressive enhancement:** like the landing page's copy buttons, the drawer is built in
  JS. With JS off the lesson is fully readable, code stays code, links still work.
- **Index page** carries the chronological table, the ◦ markers, and the themed reading
  paths, each linking into the generated pages.

### The two surfaces (postable markdown vs hosted drawer)

The source markdown was written to double as a `gh pr comment` body. A GitHub comment
strips JS and scoped CSS, so the interactive drawer cannot live inside a PR comment. The
tooling therefore produces two distinct surfaces from one source: the plain-markdown lesson
(postable to the PR) and the hosted interactive site (where the drawer lives). The design
keeps these separate and never assumes the annotations survive inside a comment.

### justfile

Add a `build-lessons` recipe and make `serve` depend on it, so the site is always fresh in
dev:

```make
build-lessons:
  node ent/lessons/build.mjs

serve: build-lessons
  npx live-server --port=8081
```

Generated pages are committed (consistent with the repo committing other generated
artifacts, and so `ent/` stays directly serveable with no build step for a future `adj.ac`
deploy). The `generated by build.mjs` header makes provenance clear. This is the one small
choice worth a human nod at review time.

## Testing

Every normal build runs the validations below and fails fast (non-zero exit) on an
integrity error, so a broken glossary or dangling link can never emit a page. `--check`
runs the same validations without emitting files, for use as a pre-commit or CI gate.

**Generator self-check (`node ent/lessons/build.mjs --check`):**

- Glossary integrity: every `prereqs` key resolves; aliases are unique across entries.
  Exits non-zero on failure so it can gate.
- Link integrity: every rewritten cross-lesson link resolves to an emitted file.
- Coverage report: backticked terms across all lessons that are absent from the glossary,
  printed (not fatal) so the gap is visible.

**Manual verification (per the `verify` skill at implementation time):**

- `just serve` (after checking `lsof -nP -iTCP:8080 -sTCP:LISTEN` and `:8081`), open a
  lesson, click `Arc<Mutex<T>>`, confirm the drawer content, click a prereq chip to descend,
  Esc to close.
- Load a page over `file://` to confirm `glossary.js` works without a server.
- View a page with JS disabled to confirm the lesson is fully readable.

## Where it's headed — the reusable skill

The end state is a skill that runs against any repo, drafts a per-PR lesson plan and a
per-repo glossary, and builds this interactive site. Four disciplines in this v1 keep that
path a lift rather than a rewrite:

1. **Engine / theme seam.** The generic engine carries no Adjacent identifiers; everything
   repo-specific lives in the `THEME` object.
2. **Glossary and lessons are generated outputs**, not assets baked into the tooling. The
   skill's job is to produce them per repo.
3. **Two surfaces, kept separate:** postable markdown lesson vs hosted interactive site.
4. **Build one, then extract.** Ship the concrete Adjacent instance first; you cannot design
   good portable seams against repos that do not exist yet. Extraction happens once this
   instance is real, and must obey the portable-tooling rule (no project-specific
   identifiers survive into the skill).

## Out of scope (v1)

- **Highlight-any-phrase "go deeper."** The north-star interaction: select arbitrary prose
  and ask for a deeper explanation. The side-panel drawer is deliberately the surface it
  would plug into later.
- **Rendered visual dependency-graph diagram.** v1 surfaces the graph as one-level prereq
  chains with click-to-descend, not as a drawn node/edge view.
- **Multi-word prose-phrase annotation.** Only backticked code spans and tag chips annotate
  in v1.
- **Full-text search across lessons.**
- **Light/dark toggle.** Dark-only, matching the landing page.

## Seed glossary terms

Drafted by the agent, reviewed by a human before publish. From the tags and concepts across
the 31 lessons: ownership, borrowing, move, lifetime, `'static`, reference counting / `Arc`,
`Rc`, `Weak`, `Mutex`, interior mutability, `Send`/`Sync`, trait, trait bound, `dyn` vs
generic (dynamic vs static dispatch), monomorphization, `Option`, `Result`, `?`,
combinators, closure, `Drop`/RAII, iterator / laziness, `collect`, newtype, derive macro,
`Serialize`/`Deserialize`, FFI / `unsafe`, `Instant` vs `SystemTime`, `HashMap::retain`,
`&str` vs `String`, `split_once`, `include_str!`, `build.rs`, `env!` / `option_env!`.
