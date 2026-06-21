# Contributing to Adjacent

## Build and test

Rust toolchain is pinned in `.tool-versions` — `asdf install` from the repo root.

- `cargo build` — workspace build (`target/debug/adj`).
- `cargo test` — full suite (unit + the integration tests under `crates/adj/tests/`).
- `cargo test -p adj --bin adj scaffold` — just the dev-command detector unit tests.

Tests sandbox their state via `ADJACENT_HOME=<tmpdir>` and start their own daemon, so they
never touch your real `~/.adjacent/`.

## Adding a dev-command detector

`adj add` scaffolds a default `adjacent.toml` and, when it recognizes the project's stack,
fills in `cmd` so the app registers in one step. Detection lives in
`crates/adj/src/scaffold.rs` as an ordered list — **first match wins** — and fires only on
**high-confidence signals** (a file that names the runner or framework). Anything it doesn't
recognize falls through to a "set `cmd` yourself" message; that's the escape hatch, so a
detector should never guess.

Current detectors, in order:

| # | Stack | Signal | Emitted `cmd` |
|---|-------|--------|---------------|
| 1 | Deno | `deno.json` / `deno.jsonc` has a `tasks.{dev,start,serve}` | `deno task <script>` |
| 2 | Node (npm/pnpm/yarn/bun) | `package.json` has `scripts.{dev,start,serve}` | `<runner> run <script>` |
| 3 | Python / Django | `manage.py` exists | `python manage.py runserver` |
| 4 | Ruby / Rails | `bin/rails` exists | `bin/rails server` |
| 5 | Ruby / Rack | `config.ru` exists (no `bin/rails`) | `bundle exec rackup` |

To add one:

1. Add a branch to `detect_cmd` (or a `detect_<stack>` helper) keyed on a file that
   unambiguously identifies the stack. Place it so more specific signals win over more
   general ones.
2. Emit the command a developer would actually run for a local dev server.
3. Add a unit test in the `tests` module of `scaffold.rs` covering the new signal.
4. Add a row to the table above.

Script-name priority across stacks is `dev` → `start` → `serve`.

## Conventions

- Commit messages: plain and descriptive, **no Conventional Commit prefixes** (`fix:`, `feat:`).
- PR bodies use `Resolves #N`.
- Comments describe **why**, not what.
