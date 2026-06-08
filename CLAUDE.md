# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Adjacent is a local dev-server harness so a developer and a coding agent can share one supervised server instead of fighting for the process. Single Rust binary `adj`; user-facing daemon runs lazily on first request and proxies `*.adj.ac` to registered apps.

Names are locked (see `~/.claude/projects/-Users-norton-src-adjacent/memory/project_naming.md`):
- Brand: **Adjacent**, CLI: `adj`, domain: `*.adj.ac`, config file: `adjacent.toml`, home dir: `~/.adjacent/`.
- Brand split in copy/visuals: `adj.ac` + `ent` (never `adj` + `ac.ent`). The `/` is the URL path boundary.
- Public copy: no "tracer bullets", no "AI agents" (just "agents"), landing page is wordmark + tagline + status + footer — nothing else. Tagline: `Humans and agents live adjacently.`

Design contract: see `project_solocal_design.md`. Decisions to treat as load-bearing: lazy-boot + single-flight, PORT injection (override via `port_env`), JSONL logs at `~/.adjacent/logs/<name>.log`, idle shutdown default-on, **never runs as root** (privileged ops emit reviewable sudo commands).

## Commands

Rust toolchain pinned in `.tool-versions` (rust 1.92.0, nodejs 26.2.0 for the landing page) — `asdf install` from the repo root.

- `cargo build` — workspace build (binary lands at `target/debug/adj`).
- `cargo test` — full suite (unit + the four integration tests in `crates/adj/tests/`).
- `cargo test --test proxy` — run one integration test file (others: `tracer`, `readiness_idle`, `json_output`).
- `cargo test <substring>` — run individual test functions by name substring.
- `cargo run -- daemon` — run the daemon in the foreground. Logs go to stderr via `tracing` (`RUST_LOG=debug` for more).
- `cargo run -- <subcommand>` — exercise a CLI command against a running daemon (e.g. `cargo run -- list --json`).
- `just serve` — `npx live-server` for the `ent/` landing page. **Check `lsof -nP -iTCP:8080 -sTCP:LISTEN` first** — the user often already has this running; spinning up a second server demos the exact problem Adjacent solves.

Tests sandbox state via `ADJACENT_HOME=<tmpdir>` and start their own daemon — no global `~/.adjacent/` pollution. The same env var works for ad-hoc local runs.

## Architecture

Workspace, two crates:
- `crates/adj` — the binary. Subcommands in `src/main.rs` dispatch to either `daemon::run` (long-lived process) or `client::*` (one-shot Unix-socket RPC).
- `crates/adj-protocol` — wire types (`Request`/`Response`/`AppState`) and stable JSON DTOs (`ListEntryDto`, `StatusDto`, `LogRecord`). The `--json` output schema in `crates/adj/JSON.md` is the contract; the test suite asserts it.

### Daemon (`daemon.rs`)

One process hosts three concurrent tasks:
1. **Control-plane listener** on `~/.adjacent/sock` (Unix). Accepts one JSON request per connection, dispatches via `dispatch()`, writes one JSON response. Each request is line-delimited JSON.
2. **Reverse proxy** (`proxy.rs`) on `127.0.0.1:8080` (override with `ADJACENT_PROXY_PORT`). Routes by `Host: <name>.adj.ac`. On first request, lazy-boots the app via a per-name `BootGate` (single-flight: concurrent waiters serialize on the same mutex; the first one boots, the rest find `Running` on re-check). Crash during boot surfaces as `502`; timeout as `504`.
3. **Idle scanner** sweeps every 500ms. Apps whose `last_request` is older than their `idle_timeout` get SIGTERM'd via `down_if_idle`, which re-checks the timestamp **under the supervisor lock** to close the request-vs-scanner race (without that re-check, a request landing between snapshot and SIGTERM turns into a spurious 502).

### Supervisor (`supervisor.rs`)

Owns process lifecycle. One `Inner` mutex covers app state and the `reserved_ports` set. `up()`:
1. Resolves env layers eagerly so a missing `env_file` fails before port reservation.
2. Allocates a free port by binding `127.0.0.1:0`, closing, retrying if the kernel reissues a port that's still in `reserved_ports` (covers the close→bind race window).
3. Spawns `sh -c <cmd>` with `process_group(0)` so SIGTERM/SIGKILL hit the whole tree — not just `sh`, which would reparent the real dev server to init.
4. Injects `$PORT` (or whatever `port_env` renames it to). Env precedence: `env_file` → `[env]` table → PORT injection (later wins).
5. Pipes stdout/stderr to per-stream readers that write JSONL records (`{ts, stream, line}`) to `~/.adjacent/logs/<name>.log`. The on-disk format **is** JSONL; `adj logs` projects `line` for humans, `adj logs --json` streams the file as-is.

State machine: `Stopped` ↔ `Running { pid, port, started_at }` ↔ `Crashed { exit_code }`. The wait task uses an `intentional_stop` flag to distinguish a `down`-driven exit from a real crash.

### Readiness (`readiness.rs`)

Polls supervisor state and either TCP-connects (default) or HTTP-GETs `health_check_url` for a 2xx. Shared by the proxy's lazy-boot path and `adj wait-ready`. A `Crashed` state mid-poll short-circuits with `ReadinessError::Crashed` so the caller doesn't sit on a timeout.

### Registry & config

`~/.adjacent/registry.toml` maps `name → path`. Per-app config at `<path>/adjacent.toml`:

```toml
name = "site"
cmd = "npm run dev"
port_env = "BIND_PORT"      # optional, defaults to PORT
env_file = ".env.local"     # optional, dotenv-format (no shell substitution)
[env]                       # optional, committed-safe overrides on top of env_file
NODE_ENV = "development"
boot_timeout = 90           # seconds, defaults to 60
health_check_url = "/healthz"
idle_timeout = "15m"        # or "30s" / "1h" / "off". default 15m
```

Path canonicalization happens **client-side** in `add` — the daemon refuses non-absolute paths so it never silently resolves against its own CWD (it may have been launched by launchd from `/`).

### Privileged ops

`adj install-port-forward` prints (never runs) a `pfctl` anchor and the sudo commands to redirect `:80 → :8080`. The daemon listens on a high port always; this is the only way `:80` reaches it. Hard rule: the daemon never executes as root.

## Conventions

- **Commit messages: no Conventional Commit prefixes** ("fix:", "feat:" etc.). Plain descriptive messages.
- **PR bodies use `Resolves #N`**, not `Closes #N`.
- **`git commit <path>`, never `git commit -a`** — the user keeps ambient working-tree edits intentionally dirty.
- **Comments describe WHY, not WHAT.** The codebase leans heavily on dense block comments above tricky races (boot single-flight, scanner re-check, process-group signaling, port reservation set) — match that style when adding similar code.

## Agent identity (`nonrational/adjacent` only)

Agents commit, push, and self-review as the GitHub user `nonreagent` — the human reviews and merges. **Read `~/.claude/projects/-Users-norton-src-adjacent/memory/project_agent_identity.md` in full before any commit / push / `gh` action.** Key points:

- `gh auth switch -u nonreagent` + `gh auth setup-git` before any `git push`. Inline `GH_TOKEN=...` does nothing for `git push`.
- Per-commit author: `git -c user.name=nonreagent -c user.email=agent@nonration.al -c commit.gpgsign=false commit ...`.
- Commit footer must include `Co-Authored-By: Claude Opus 4.7 (1M context) <noreply@anthropic.com>`.
- **Never** run `gh pr review --approve` or `gh pr merge`. Approval and merge are the human's job. The agent's loop ends at "self-review posted, awaiting your eyes."
- After the push, `gh auth switch -u nonrational` to restore the human's interactive context.
