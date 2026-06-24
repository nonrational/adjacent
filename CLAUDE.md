# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Adjacent is a local dev-server harness so a developer and a coding agent can share one supervised server instead of fighting for the process. Single Rust binary `adj`; user-facing daemon runs lazily on first request and proxies `*.adj.ac` to registered apps.

Names are locked (see `~/.claude/projects/-Users-norton-src-adjacent/memory/project_naming.md`):
- Brand: **Adjacent**, CLI: `adj`, domain: `*.adj.ac`, config file: `adjacent.toml`, home dir: `~/.adjacent/`.
- Brand split in copy/visuals: `adj.ac` + `ent` (never `adj` + `ac.ent`). The `/` is the URL path boundary.
- Public copy: no "tracer bullets", no "AI agents" (just "agents"). Tagline: `Humans and agents live adjacently.` Landing page (revamped 2026-06-09) is a lean positioning page — wordmark + tagline, problem statement, commands, design principles, status, footer. Keep copy tight; the user cuts anything that bloats.

Design contract: see `project_solocal_design.md`. Decisions to treat as load-bearing: lazy-boot + single-flight, PORT injection (override via `port_env`), JSONL logs at `~/.adjacent/logs/<name>.log`, idle shutdown default-on, **never runs as root** (privileged ops emit reviewable sudo commands).

## Commands

Rust toolchain pinned in `.tool-versions` (rust 1.92.0, nodejs 26.2.0 for the landing page) — `asdf install` from the repo root.

- `cargo build` — workspace build (binary lands at `target/debug/adj`).
- `cargo test` — full suite (unit + the integration tests in `crates/adj/tests/`).
- `cargo test --test proxy` — run one integration test file (see `crates/adj/tests/` for the others).
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
2. **Reverse proxy** (`proxy.rs`) on `127.0.0.1:8080` (override with `ADJACENT_PROXY_PORT`; `0` = kernel-assigned, with the bound port written to `~/.adjacent/proxy.port` — `https.port` for the HTTPS listener — which is how the test sandboxes discover their daemon's ports race-free). Routes by `Host: <name>.adj.ac`. On first request, lazy-boots the app via a per-name `BootGate` (single-flight: concurrent waiters serialize on the same mutex; the first one boots, the rest find `Running` on re-check). Crash during boot surfaces as `502`; timeout as `504`.
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

`~/.adjacent/registry.toml` maps `name → path`. Registry keys may carry one structural dot: `<label>.<name>` is a worktree instance (`feature-x.site` routes at `feature-x.site.adj.ac`). `adj add` inside a linked git worktree derives the label from the branch name (`--label` overrides); `adj remove <name>` deletes one entry, `adj prune` deletes every entry whose path is gone, and `adj list` flags those as stale. App names in `adjacent.toml` therefore cannot contain dots. The TLS leaf carries a `*.<name>.adj.ac` SAN per registered base name and re-issues on registry changes.

Per-app config at `<path>/adjacent.toml`:

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

At boot the daemon injects a reserved, daemon-owned `ADJ_*` namespace into `cmd` (after
`env_file`/`[env]`, so these win): `ADJ_NAME` (routing key), `ADJ_HOST` (`<name>.adj.ac`),
and four base URLs — `ADJ_URL` (https) / `ADJ_URL_HTTP` (http), clean and assuming the
port-forward, plus `ADJ_URL_DIRECT` / `ADJ_URL_HTTP_DIRECT` carrying the daemon's real
listener ports. Lets a
`cmd` address its own origin, e.g. `hugo server --appendPort=false --port $PORT --baseURL $ADJ_URL_HTTP`.

Path canonicalization happens **client-side** in `add` — the daemon refuses non-absolute paths so it never silently resolves against its own CWD (it may have been launched by launchd from `/`).

### Privileged ops

The daemon never executes as root. Privileged work is emitted as commands the user reviews and sudos.

- `adj install-port-forward` — prints (never runs) a `pfctl` anchor with two `rdr` rules: `:80 → :8080` and `:443 → :8443`. The daemon listens on high ports always; this is how `:80` / `:443` reach it.
- `adj install-ca` — generates a local CA: cert at `~/.adjacent/ca.crt`, private key in the macOS **login keychain** under the label `Adjacent local CA`, marked **non-extractable** (`kSecAttrIsExtractable=false`) so ordinary tooling — `security export`, Keychain Access UI export, `SecItemCopyMatching(kSecReturnData=true)` — refuses to hand the bytes back. This is a software promise enforced by the Security framework, **not** a hardware boundary; a determined attacker with the user's login password and framework-level access can probably still pull the bytes out via legacy `SecKeychain` APIs. Still a meaningful step up from "PEM file at mode 0600" — no cleartext bytes on disk for backup tools or `cat ~/.adjacent/ca.key` to scoop up. The cert carries a critical RFC 5280 `nameConstraints` extension permitting `DNS:adj.ac` only, so a misused CA cannot mint trusted certs for other domains. Prints the `security add-trusted-cert` command. The daemon issues a leaf signed by the CA on next start carrying `*.adj.ac` plus `*.<name>.adj.ac` per registered base name, re-issued when the registry changes (signed through rcgen's `RemoteKeyPair` trait + `SecKeyCreateSignature`, so the CA private key never enters process memory). Rotating the CA deletes the cached leaf so a fresh one re-issues.
- `adj install-ca --reset` — removes both halves of the keychain key entry plus the on-disk cert/leaf files. Use for fresh start or test teardown. Prints the `security delete-certificate` command for the trust anchor (sudo, not run for you).

  Tried Secure Enclave first; SE requires `keychain-access-groups` entitlements that an unsigned `cargo`-built binary doesn't carry (`SecKeyCreateRandomKey` returns `errSecMissingEntitlement` / OSStatus -34018), so the pivot was non-extractable software ECDSA P-256 in the login keychain. To revisit SE later, codesign the binary with `keychain-access-groups` and add `Token::SecureEnclave` + `Location::DataProtectionKeychain` to the `GenerateKeyOptions` path in `crates/adj/src/tls/keychain.rs`.

### HTTPS listener

Alongside the HTTP proxy, the daemon opens an HTTPS listener on `:8443` (override `ADJACENT_HTTPS_PORT`). Both share the same request routing — the per-connection serve loop is generic over the stream type. Startup is best-effort: if either half of the CA is missing (no cert on disk, or no keychain entry under the install's label), the HTTPS task logs at `error!` and exits while HTTP and the control plane keep serving. Run `adj install-ca` to opt in.

## Conventions

- **Commit messages: no Conventional Commit prefixes** ("fix:", "feat:" etc.). Plain descriptive messages.
- **PR bodies use `Resolves #N`**, not `Closes #N`.
- **`git commit <path>`, never `git commit -a`** — the user keeps ambient working-tree edits intentionally dirty.
- **Comments describe WHY, not WHAT.** The codebase leans heavily on dense block comments above tricky races (boot single-flight, scanner re-check, process-group signaling, port reservation set) — match that style when adding similar code.
