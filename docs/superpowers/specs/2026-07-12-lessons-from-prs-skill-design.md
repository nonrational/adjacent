# `lessons-from-prs` skill design

**Date:** 2026-07-12\
**Status:** Approved design, pre-implementation (uncommitted by request)

## Problem

We built a concrete "learn by example, one PR at a time" teaching series for this
repo: one lesson per merged PR, drafted from the real diff, plus an interactive site
with clickable glossary terms and prerequisite ladders. The *process* that produced
those lessons is reusable across any repo, but it currently lives only as prose in
`docs/lessons/README.md` ("Adding a lesson for a new PR") and as tacit judgment. There
is no tool to point at another repo and get the same teaching notes.

## Goal

A reusable skill, **`lessons-from-prs`**, that drafts per-PR teaching lessons from real
diffs in any repo. It packages the authoring judgment (pick one concept, honesty rule,
real snippets, fixed template) into a repeatable, language-agnostic flow, and it keeps a
running vocabulary index that seeds a later glossary.

This is the "build one, then extract" step named in the interactive-lessons spec
(`docs/superpowers/specs/2026-07-11-interactive-lessons-design.md`, section "Where it's
headed — the reusable skill"). This v1 is **authoring-first**: the interactive site and
the full glossary are deferred (see Out of scope).

## The reference instance (extract from, don't duplicate)

- Authoring process + honesty rule + "Adding a lesson": `docs/lessons/README.md`.
- The fixed lesson template: `docs/lessons/_TEMPLATE.md`.
- 31 worked examples: `docs/lessons/<NN>-<slug>.md`.
- The (deferred) site engine and glossary format: `ent/lessons/build.mjs`,
  `docs/lessons/glossary.json` — these ride with the deferred site skill, NOT this one.

## Design

### Where it lives

`~/.claude/skills/lessons-from-prs/`. Note `~/.claude` is a **public** repo, so the
skill is a published, shareable artifact: the portable-tooling rule
(`~/.dotfiles/.claude/rules/skill-authoring.md`) applies in full. No project-specific
identifiers may appear in the skill files; any example uses a neutral invented domain
(support tickets, invoices — varied), and a grep gate confirms it before done. The word
"Adjacent" / "adj.ac" / "Rust"-as-the-only-language must not leak into the skill.

### Two invocation modes, one drafting logic

- **Per-PR (default):** run in the target repo, pass a PR number. Read the diff, draft
  the lesson, present it for review/edit, save it. The maintenance loop, run when a PR
  merges.
- **Backfill:** pass a PR range or `--all`. Draft lessons for many PRs into files for
  batch review. Bootstraps an existing repo (how the 31 reference lessons were made).

The per-PR drafting is identical in both; backfill is that logic in a loop with
batched, not inline, review.

### The authoring process it encodes (the reusable core)

1. **Read the real diff** (`gh pr diff <N>` / `gh pr view <N>`), never the title alone.
   The diff wins over any preconception; verify the chosen angle against the code.
2. **One concept per lesson.** Pick the single most instructive idea the PR demonstrates.
3. **Honesty rule.** If a PR has little/no language content (docs, a deletion, config),
   say so plainly and teach what it *does* contain (release engineering, project
   hygiene, docs-as-contract), marked as such. Never fabricate a lesson.
4. **Real snippets only,** lifted from the diff with the file path as a leading comment;
   elisions marked. Never invent code.
5. **Fixed template** (bundled generic `_TEMPLATE.md`): the `<!-- -->` provenance
   comment, `# PR #NN — <title>`, a three-line blockquote (lesson sentence / Tags /
   Merged with +A/−D and a View-PR link), then sections `The situation` / `The idea` /
   `In this PR` / `Why it matters` / `Related lessons` / `Dig deeper`. Cross-links to
   sibling lessons use relative `NN-slug.md` links. The uniform structure is a contract
   a future site parses without heuristics.
6. **Language-parametric voice.** Infer the repo's primary language/domain and adapt the
   teaching voice and the "Dig deeper" links (the language's book/std docs) accordingly.
   The template and the five rules above are language-neutral.

### Artifacts it writes

- `docs/lessons/<NN>-<slug>.md` per PR (slug = 3–5 kebab-case words from the title).
- A **running vocabulary index** aggregating each lesson's `Tags` across the series (the
  seed vocabulary for the future glossary). Definitions and prerequisite edges are NOT
  produced here — they are deferred with the site. Exact filename/format is an
  implementation detail for the plan (e.g. a simple `docs/lessons/VOCABULARY.md` table of
  term → lessons that use it), kept trivially regenerable from the lessons' Tags lines.

### Posting is opt-in and gated

A lesson is written to double as a `gh pr comment --body-file` body. Posting sends
content on the user's behalf (outward-facing), so the skill **drafts and saves locally
by default** and posts a PR comment only with the user's explicit per-PR go-ahead. It
never posts automatically, and never in backfill mode without confirmation.

### Bundled resources

- A **generic `_TEMPLATE.md`** (the reference template with all project nouns removed).
- The **authoring checklist** (the six rules above) as the skill's core guidance, with
  progressive disclosure per the write-a-skill conventions.

## Out of scope for v1 (deferred, recorded)

- **Interactive site generation** (the drawer, term annotations, prereq ladders). Rides
  with a separate future site-build skill that wraps the existing `build.mjs` engine.
- **Full glossary** (plain-language definitions + prerequisite graph). Deferred with the
  site; v1 only tracks the vocabulary (Tags), not definitions.
- **Auto-posting** PR comments without per-PR confirmation.
- **Non-GitHub forges.** v1 assumes `gh` / GitHub PRs.

## Testing / acceptance

- Point the skill at this repo and re-draft the lesson for a known PR (e.g. #16); the
  output should match the reference lesson's shape and pick a defensible concept, with
  real snippets from the actual diff.
- Run the honesty rule against a no-lesson PR (e.g. #68, a ten-line docs deletion) and
  confirm it produces an honest "little/no lesson here" entry, not a fabricated concept.
- Backfill a small range and confirm batch drafting + a coherent vocabulary index.
- **Portability gate:** `grep -rniE '<project nouns>' ~/.claude/skills/lessons-from-prs`
  returns only neutral examples — no reference-instance identifiers survive.
