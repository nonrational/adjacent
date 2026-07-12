<!-- Lesson for PR #68. Non-Rust: pure docs deletion; modeling honesty when there's no lesson. -->

# PR #68 — Remove the agent-identity section from CLAUDE.md

> **Rust lesson:** None — this is a pure documentation deletion (+0/−10), no Rust and no technical concept to teach.
> **Tags:** `docs-hygiene` · `honest-series`
> **Merged:** 2026-06-16 · +0/−10 · [View PR](https://github.com/nonrational/adjacent/pull/68)

## What happened

This PR deleted the `## Agent identity` section from `CLAUDE.md` — ten lines of personal operational detail (the `nonreagent`/`nonrational` GitHub accounts, the `gh auth switch` dance, the per-commit author line) that don't belong in an open-source repo. The durable principle it encoded still lives, machine-agnostically, in each contributor's own profile:

```markdown
## Agent identity (`nonrational/adjacent` only)

Agents commit, push, and self-review as the GitHub user `nonreagent` — the human reviews and merges.
- `gh auth switch -u nonreagent` + `gh auth setup-git` before any `git push`. Inline `GH_TOKEN=...` does nothing for `git push`.
- **Never** run `gh pr review --approve` or `gh pr merge`. Approval and merge are the human's job.
```

The meta-note worth keeping: **agent-instruction files (`CLAUDE.md`, `AGENTS.md`) earn their keep by staying lean.** Every line is context an agent re-reads on each task, and anything personal or repo-account-specific is noise for other contributors plus a small information leak. Prune aggressively.

And this file is the honesty principle in action: a good teaching series says so plainly when a PR carries no lesson, rather than inventing one.
