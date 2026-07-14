<!-- Lesson for PR #65. Non-Rust: community health files; teaches OSS hygiene + Cargo.toml crates.io metadata. -->

# PR #65 — Add community health files

> **Rust lesson:** None — this PR is seven Markdown/YAML files under `.github/` (+160/−0), zero Rust. The lesson is open-source project hygiene, and the Rust-ecosystem analog worth knowing: the way GitHub reads your `.github/` files and renders them into the repo UI is exactly how **crates.io reads your `Cargo.toml` package metadata** — `description`, `repository`, `readme`, `keywords`, `categories`, `license`, `rust-version` — and renders it into your crate page.
> **Tags:** `oss-hygiene` · `crates.io-metadata`
> **Merged:** 2026-06-14 · +160/−0 · [View PR](https://github.com/nonrational/adjacent/pull/65)

## The situation

The repo had code and a README but none of the paperwork a public project is
judged by: no code of conduct, no contribution guide, no security policy, no
issue or PR templates. This PR adds the whole set — `CODE_OF_CONDUCT.md`,
`CONTRIBUTING.md`, `SECURITY.md`, two issue templates plus a `config.yml`, and a
`PULL_REQUEST_TEMPLATE.md` — all under `.github/`.

## The idea (no Rust this time)

These are **community health files**, and the reason they live in `.github/` is
that GitHub reads them by convention and wires them into the UI. Drop
`CONTRIBUTING.md` there and the "New pull request" screen sprouts a link to it.
Add `ISSUE_TEMPLATE/*.md` and the "New issue" button becomes a menu of your
templates. Add `SECURITY.md` and a "Report a vulnerability" affordance appears.
You're not writing docs a human has to go find — you're filling in slots a
platform already knows how to render.

The trick is that the file's **location and name are the contract**. GitHub
doesn't parse your prose; it keys off the path (`.github/SECURITY.md`) and, for
templates, off the YAML front-matter at the top of the file. Get the path right
and the feature lights up; get it wrong and the file is just dead Markdown.

## In this PR

The issue templates lead with YAML front-matter — that block *is* what GitHub
reads to build the template menu (the `name` and `about` become the menu entry):

```yaml
# .github/ISSUE_TEMPLATE/feature_request.md
---
name: Feature Request
about: Propose an enhancement or new capability for Adjacent
labels: enhancement
---
```

`config.yml` isn't a template — it configures the template *chooser*, keeping the
blank-issue escape hatch open and routing open-ended questions to Discussions
instead of the issue tracker:

```yaml
# .github/ISSUE_TEMPLATE/config.yml
blank_issues_enabled: true
contact_links:
  - name: General questions
    url: https://github.com/nonrational/adjacent/discussions
    about: Ask questions and discuss Adjacent in Discussions
```

`SECURITY.md` states a support window and points reporters at GitHub's private
reporting flow rather than a public issue or an email address:

```markdown
<!-- .github/SECURITY.md -->
For security vulnerabilities, please use GitHub's [private vulnerability
reporting feature](https://github.com/nonrational/adjacent/security/advisories/new)
rather than opening a public issue.
```

And `CONTRIBUTING.md` closes with the standard Rust dual-license inbound clause —
every contribution comes in under both licenses, no extra paperwork:

```markdown
<!-- .github/CONTRIBUTING.md -->
Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you ... shall be dual licensed under the Apache
License, Version 2.0 and the MIT license, without any additional terms or
conditions.
```

That clause isn't free-floating — it matches a field already in the manifest,
which is the bridge to the Rust angle.

## Why it matters — the crates.io angle

crates.io is to a published crate what GitHub's repo page is to a repo: it reads
structured metadata and renders it. The source of that metadata is the
`[package]` table in `Cargo.toml`. The repo's workspace manifest already carries
two of the important fields, and the `license` here is the exact counterpart to
the CONTRIBUTING clause above:

```toml
# Cargo.toml — [workspace.package] (already in the repo, not added by this PR)
license = "MIT OR Apache-2.0"
repository = "https://github.com/nonrational/adjacent"
```

If Adjacent were ever `cargo publish`ed, crates.io would surface `license` as a
badge and `repository` as the "Repository" link — the same "platform renders
your metadata" move as GitHub and `.github/`. But the metadata slots for a
polished crate page go further, and they're worth knowing even before you
publish:

- **`description`** — the one-liner under the crate name in search results. Required to publish.
- **`readme`** — path to the file crates.io renders as the crate's front page (defaults to `README.md`).
- **`keywords`** / **`categories`** — how people find you in search and browse; `categories` must come from crates.io's fixed slug list.
- **`rust-version`** — your MSRV (minimum supported Rust version). Cargo refuses to build with an older toolchain and prints a clear error instead of a confusing mid-compile failure.

The lesson mirrors the health files exactly: don't write a paragraph telling
people your MSRV or where to file bugs — put the value in the slot the tooling
reads, and the tooling shows it for you, consistently, everywhere.

## Related lessons

- PR #46 covers the `MIT OR Apache-2.0` dual-license convention itself and the on-disk `LICENSE-*` layout — the CONTRIBUTING clause here is the inbound-contribution half of that same decision.
- PR #36 makes the "the file's path/format *is* the contract" point from the docs side.

## Dig deeper

- [The Cargo Book — The Manifest Format](https://doc.rust-lang.org/cargo/reference/manifest.html) — every `[package]` field crates.io reads: `description`, `repository`, `readme`, `keywords`, `categories`, `license`, and `rust-version`.
