# Scaffold a default adjacent.toml in `adj add`

**Date:** 2026-06-21\
**Status:** Approved design, pre-implementation

## Problem

`adj add <path>` registers an app by reading `<path>/adjacent.toml`. When that file is
missing, the command dead-ends:

```
$ adj add .
adj: no adjacent.toml found at /Users/norton/src/lizzie/adjacent.toml
```

The user has no on-ramp — they must hand-author the manifest before `adj` is any use,
and nothing tells them the file's shape or the available fields. The first contact with
the tool is a hard error.

## Goal

`adj add .` produces a usable `adjacent.toml` and, when it can confidently detect the
dev command, registers the app in a single step. When it can't detect the command, it
still scaffolds the file (so the user has a starting point) and tells them what to fill
in — plus how to teach `adj` about their stack.

## Design

### Surface: enhance `adj add`, no new subcommand

No `adj init`. Generation lives in `client::add`, next to the path canonicalization that
already runs client-side. The daemon and the wire protocol are unchanged — the daemon
still just reads `adjacent.toml` and registers `name → path`. Generation is a pure
client-side concern because the file is written into the user's working tree, in the
user's CWD context (the daemon may have been launched by launchd from `/`).

### Behavior

`adj add <path>` (which covers `adj add .`):

1. Canonicalize the directory (unchanged; fails if the dir doesn't exist).
2. If `<dir>/adjacent.toml` **exists** → today's flow, untouched. **We never overwrite.**
3. If **missing** → scaffold:
   - Derive `name` from the directory basename.
   - Detect `cmd` (see detector table below).
   - **cmd detected** → write the file, then fall through into the normal register path.
     One step.

     ```
     generated adjacent.toml (cmd = "npm run dev")
     registered `lizzie` at /Users/norton/src/lizzie
     ```
   - **cmd not detected** → write the file with an empty `cmd` + a TODO comment, and
     **stop without registering** (an app whose `cmd` can't boot should not enter the
     registry). Print the action+contribution message:

     ```
     adj: couldn't detect a dev command for this project.
       set `cmd` in adjacent.toml (e.g. "npm run dev"), then run `adj add .` again.
       know the command for this stack? add a detector:
       https://github.com/nonrational/adjacent (see CONTRIBUTING)
     ```

   On re-run after the user fills in `cmd`, the file now exists, so step 2 applies: the
   existing `read_app_config` validation enforces a non-empty `cmd` and a valid name.

### Detection: an ordered, table-driven detector list

Detection is a list of `(predicate over the directory, cmd to emit)` rules; **first match
wins**. Table-driven so adding a stack is appending one row — which is exactly what the
not-detected message points contributors at. **Rules fire only on high-confidence
signals** (a file that names the runner or framework). Low-confidence stacks fall through
to the not-detected message; that message is the documented escape hatch, so the table
does not need to cover the long tail.

| # | Stack | Signal | Emitted `cmd` |
|---|-------|--------|---------------|
| 1 | Deno | `deno.json` / `deno.jsonc` has a `tasks.{dev,start,serve}` entry | `deno task <script>` |
| 2 | Node (npm/pnpm/yarn/bun) | `package.json` has `scripts.{dev,start,serve}` | `<runner> run <script>` |
| 3 | Python / Django | `manage.py` exists | `python manage.py runserver` |
| 4 | Ruby / Rails | `bin/rails` exists | `bin/rails server` |
| 5 | Ruby / Rack | `config.ru` exists (and no `bin/rails`) | `bundle exec rackup` |

- **Script priority** is `dev` → `start` → `serve` for both Deno tasks and Node scripts —
  the first one present is chosen.
- **Node runner** is selected from the lockfile present in the directory:
  `pnpm-lock.yaml` → `pnpm`, `bun.lock` / `bun.lockb` → `bun`, `yarn.lock` → `yarn`,
  else (`package-lock.json` or no lockfile) → `npm`. `<runner> run <script>` is valid for
  all four. This covers npm/pnpm/yarn/bun.
- **Order resolves ambiguity.** A repo carrying both `deno.json` tasks and `package.json`
  scripts resolves to Deno (rule 1 before rule 2). This is documented behavior, not an
  accident.
- **Two guesses emitted as-is** (the user edits if wrong): `python` (not `python3`) for
  Django, and `bundle exec rackup` for Rack.

### `name` derivation

`name` comes from the directory basename, sanitized into a DNS label to satisfy the
existing `validate_dns_label` (the name becomes `<name>.adj.ac` and a TLS SAN):

- lowercase
- any char not in `[a-z0-9-]` → `-`
- collapse repeated `-`
- trim leading/trailing `-`
- cap at 63 chars

The re-read in `read_app_config` validates the result. If sanitization yields an empty
string (pathological basename), write a placeholder `name` with a TODO and do not
register — the same non-registering path as the not-detected `cmd` case.

### Generated template (lean)

Committed-safe and intentionally minimal — `name` + `cmd`, then a short commented block
of the highest-value optional fields. The detected case:

```toml
# Generated by `adj add` — edit and re-run if needed.
name = "lizzie"
cmd = "npm run dev"

# Optional:
# port_env = "PORT"
# env_file = ".env.local"
# idle_timeout = "15m"          # "30s" / "1h" / "off"
# health_check_url = "/healthz"
```

The not-detected case is identical except `cmd = ""` with an inline TODO comment.

### Module layout

A new client-side module — `crates/adj/src/scaffold.rs` — owns detection, name
sanitization, and template rendering. Its public surface is small and pure (directory in,
`(name, Option<cmd>, rendered_toml)` out), so it is unit-testable without a daemon, a
filesystem write, or a socket. `client::add` calls it, performs the single file write, and
branches on whether a `cmd` was detected.

### Documentation deliverable

Create `CONTRIBUTING.md` with a section documenting the detector table and how to add a
row (the signal predicate + the cmd to emit + where it slots in the ordered list). The
not-detected message links here, so it must exist.

## Testing

**Unit (`scaffold.rs`):**
- Name sanitization: uppercase, underscores, dots, leading/trailing junk, over-63-char,
  empty-after-sanitize.
- Detection per stack: Deno tasks; Node with each lockfile (npm/pnpm/yarn/bun) and the
  no-lockfile default; Django `manage.py`; Rails `bin/rails`; Rack `config.ru`.
- Script priority `dev` > `start` > `serve`.
- Ambiguity ordering: `deno.json` + `package.json` present → Deno wins.
- No detectable signal → returns "not detected".

**Integration (`crates/adj/tests/`):**
- `adj add .` in a dir with a detectable dev command → file written with expected
  contents **and** the app registered.
- `adj add .` in a dir with no detectable command → file written, app **not** registered,
  correct message on stderr.
- `adj add .` where `adjacent.toml` already exists → file untouched (byte-identical),
  existing behavior preserved.

## Out of scope

- A standalone `adj init` command.
- Detecting frameworks beyond the table above (Flask, FastAPI, Phoenix, Go, etc.) — these
  fall through to the not-detected message by design; contributors extend the table.
- Any daemon or wire-protocol change.
