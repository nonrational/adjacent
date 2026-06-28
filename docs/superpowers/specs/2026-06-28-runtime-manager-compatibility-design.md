# Runtime-manager compatibility characterization

**Status:** design approved, pre-implementation\
**Date:** 2026-06-28

## Goal

Empirically characterize how Adjacent resolves language-runtime version managers
(mise, asdf, rbenv, uv, …) when it boots an app — across the launch contexts a
real user actually ends up in — so we can see which setups silently fall back to
the wrong toolchain and then decide, per case, **fix vs document**.

This is a *characterization*, not a contract. We build the matrix to discover
the truth, then act on it. No behavior change to `adj` is in scope for this work.

## How `adj` boots an app (the ground truth)

`supervisor.rs:100` spawns `sh -c <cmd>` with `current_dir` set to the app dir,
and the child **inherits the daemon's environment**. Three consequences drive
the entire design:

1. **The interactive-shell axis is moot.** `adj` never invokes the user's login
   shell, so `.zshrc` / `.bashrc` / `config.fish` never run. bash-vs-zsh-vs-fish
   does not propagate. What matters is the *environment the daemon inherited*,
   not the shell the user types in.
2. **Version managers split into two camps:**
   - **Shim / PATH-based** (rbenv, pyenv, nodenv, asdf-classic, mise shim mode,
     uv): drop a shims dir on `PATH`, read `.tool-versions` / `.ruby-version` /
     `.python-version` from the CWD. Since `adj` sets `current_dir` to the app
     and inherits `PATH`, these resolve **iff the shims are on the inherited
     PATH**.
   - **Activation-hook-based** (`mise activate`, direnv, nvm): rely on shell
     hooks (`chpwd`/`precmd`/`--on-variable PWD`) that fire only in interactive
     shells, or (nvm) are shell *functions* with no binary. `sh -c` never
     triggers them — these **silently don't work** unless rewritten in
     shim/exec form.
3. **The inherited env depends on who launched the daemon** — the single biggest
   determinant of success, and orthogonal to shells and managers (see axis B).

## Decisions (load-bearing)

- **Goal is characterization** — discover what works today and what silently
  fails; no `adj` code change in this work.
- **Success signal: non-default pinned version, asserted.** The test app's
  health endpoint returns its own resolved runtime version (`ruby -v`,
  `node --version`, `python --version`). The fixture pins a version that
  **deliberately differs from the CI runner's system default**. A silent
  fallback therefore reports the system version, mismatches the pin, and **fails
  loudly**. "Booted + served 2xx" alone is rejected — it green-lights the exact
  silent-fallback we're hunting.
- **Two launch contexts (axis B), every manager runs in both:**
  - **inherited-shell** — full `PATH` with the manager's shims/bins, as if the
    user launched `adj` from their terminal.
  - **launchd-minimal** — `env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin`, modeling
    the always-on launchd-started daemon that gets none of the user's shims.
    This is the context real users land in, and where shim managers fall back.
- **Platform: simulate contexts, run the matrix on Linux, one macOS launchd
  smoke test.** Version resolution is a pure function of env + CWD, so the full
  managers × contexts grid runs on fast/cheap `ubuntu-latest`. A single
  `macos-14` job boots `adj` under a real launchd plist to confirm the shipped
  deployment path end-to-end. (GHA macOS runners bill ~10× Linux; we don't pay
  that to re-confirm what `env -i` already reproduces.)

## The matrix (A × B)

Axis A — manager / mode. Axis B — {inherited-shell, launchd-minimal}.

| # | Manager / mode | Camp | Runtime | Expected: inherited-shell | Expected: launchd-minimal |
|---|---|---|---|---|---|
| 1 | **rbenv** | shim | Ruby | ✅ resolves pin | ❌ system fallback |
| 2 | **asdf** (classic, nodejs plugin) | shim | Node | ✅ | ❌ |
| 3 | **mise** (shim mode) | shim | Python | ✅ | ❌ |
| 4 | **mise** (`mise activate` only) — *Berkopec profile* | activation | Ruby | ❌ no shell hook | ❌ |
| 5a | **mise exec** (`cmd = "mise exec -- …"`) | exec-wrapped | Node | ✅ | ❓ iff `mise` on bare PATH |
| 5b | **mise run** (`cmd = "mise run dev"`) — *Berkopec remedy* | exec-wrapped | Ruby | ✅ | ❓ iff `mise` on bare PATH |
| 6 | **uv** (`uv run`, `.python-version`) | shim-ish | Python | ✅ | ❓ iff `uv` on bare PATH |
| 7 | **nvm** | activation | Node | ❌ shell function, no binary | ❌ |

7 managers, 8 cells (mise split into 5a/5b), × 2 contexts = **16 runs**.

The **❓ cells (5a/5b/6) are the payoff** — they tell us whether the workaround
we'd hand a user ("wrap your cmd in `mise exec` / `mise run`") actually survives
launchd. That advice is only worth giving if these pass under launchd-minimal.

**Deferred** (noted, not in the matrix): **pyenv** (same shim family as rbenv —
rbenv represents it), **direnv** (an env-loader, not a version manager —
different problem), other rbenv/pyenv siblings.

## The Berkopec profile (named fixture)

[Nate Berkopec's published dotfiles](https://github.com/nateberkopec/dotfiles)
are a real-world instance of the matrix's most important finding. He runs
**fish + mise wired via the activation hook** (`config.fish`):

```fish
set -gx MISE_FISH_AUTO_ACTIVATE 0
mise activate fish --no-hook-env | source
mise hook-env -s fish | source
function __mise_refresh_on_cd --on-variable PWD
  mise hook-env -s fish | source
end
```

His ADR 0002 ("Prefer mise as the tool owner") standardizes runtimes, CLIs,
tasks, and system packages on mise, and he drives projects through `mise run`
tasks. That `--on-variable PWD` hook is the whole point: mise swaps resolved tool
paths into `PATH` on every `cd`, **inside the interactive shell only**. Under
`adj`'s `sh -c` it never fires.

So his opinionated, widely-read setup **is cell #4** — the activation case `adj`
silently doesn't support. We encode it two ways:

- **Cell #4 fixture** mirrors his pattern to *demonstrate the failure*: a
  `.mise.toml` pinning a non-default Ruby, launched the natural way
  (`cmd = "ruby app.rb"`). Expected: silent fallback in both contexts.
- **Cell #5b fixture** is his *native remedy*: same `.mise.toml`, but
  `cmd = "mise run dev"`. Expected: resolves, and the launchd-minimal result
  tells us if the remedy holds where he'd actually deploy it.

## Harness design

**Test app (shared, runtime-agnostic shape).** A minimal HTTP server per runtime
(Ruby/Node/Python, ~10 lines each) that binds `$PORT` and answers a health route
with its own interpreter version string. The assertion compares that string to
the fixture's pinned version.

**Fixture = a directory per cell** containing: the version-pin file the manager
reads (`.tool-versions` / `.mise.toml` / `.python-version` / `.nvmrc`), an
`adjacent.toml` with the cell's `cmd`, and the tiny server. Pinned versions are
chosen to differ from the runner's system default.

**Context realization.** Each fixture runs twice:
- *inherited-shell* — install the manager, ensure its shims/bins are on `PATH`,
  start the daemon with that `PATH`.
- *launchd-minimal* — start the daemon under
  `env -i PATH=/usr/bin:/bin:/usr/sbin:/sbin` (plus the irreducible `HOME` and
  `ADJACENT_HOME` the daemon needs). Faithfully reproduces the bare launchd env.

**Test flow per cell × context** (reuses existing test scaffolding):
`ADJACENT_HOME=<tmpdir>` → start daemon (it writes `proxy.port` for race-free
discovery) → `adj add` the fixture → issue a request through the proxy →
parse the version from the response → assert against the pin (or assert the
expected *mismatch* for the ❌ cells). Recording the *observed* version for every
cell — pass or fail — is the characterization output.

**Where it lives.** A dedicated CI job (its own `.github/workflows` matrix over
the 16 cells, or a single job iterating fixtures — TBD in the plan), separate
from the Rust unit/integration suite so manager installs don't slow `cargo
test`. Plus one `macos-14` launchd smoke job: install mise, generate a launchd
plist for the daemon, boot one fixture, confirm it serves.

**Output.** The job's artifact is the filled-in matrix: for each of the 16 cells,
the pinned version, the observed version, and pass/fail. That table is the
deliverable that drives the fix-vs-document decision.

## Out of scope

- Any change to how `adj` spawns apps or resolves environments. If the matrix
  argues for a fix (e.g. a `mise exec` shim, a PATH-augmentation knob, docs), it
  is **separate follow-up work** with its own spec.
- pyenv, direnv, rvm, chruby, Windows, function-level anything.

## Risks / open questions (resolve in the plan)

- **CI install time.** Installing 4+ managers and multiple runtimes per run is
  the main cost. Mitigations: cache manager installs (`~/.rbenv`, `~/.asdf`,
  `~/.local/share/mise`, uv cache); pick already-cached or fast-to-install
  runtime versions that still differ from the system default.
- **Non-default version availability.** The pinned version must (a) differ from
  the runner default and (b) install reliably on `ubuntu-latest`. The plan pins
  exact versions and verifies availability.
- **launchd-minimal floor.** The daemon needs *some* env (`HOME`,
  `ADJACENT_HOME`, possibly `USER`). The plan enumerates the irreducible set so
  "minimal" stays honest without being broken.
- **Job topology.** One workflow job iterating fixtures vs a GHA matrix per cell
  — decide in the plan based on caching and log readability.
