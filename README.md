# Adjacent

A local dev-server harness so a developer and an AI coding agent can share one supervised server instance instead of fighting for control of the process.

When both sides need the same local server running, they evict each other. The agent takes over → the developer loses log visibility. The developer reclaims it → the agent can't validate its work. Adjacent owns the process so neither side has to.

Homepage: [adj.ac/ent](https://adj.ac/ent)

## Shape (v1)

- One CLI: `adj`. Same surface for human and agent — every read command supports `--json` ([schema](crates/adj/JSON.md)).
- One entry per app. `adj add <path>` registers; per-app config lives in `adjacent.toml`.
- Lazy-boot by default. Hit `foo.adj.ac`, the app starts, the request proxies through.
- `$PORT` injected into the boot command. Apps bind to it. Quirky apps can opt into a different variable name via `port_env = "BIND_PORT"` in `adjacent.toml`.
- Logs on disk at `~/.adjacent/logs/<name>.log`. `adj logs <name> --tail` works.
- DNS via public wildcard `*.adj.ac → 127.0.0.1`. Offline-mode resolver hook is opt-in.
- TLS via opt-in local CA.
- **Never runs as root.** Privileged ops emit reviewable commands the user runs with sudo.

## Status

Pre-alpha. v1 is being sliced into tracer-bullet issues: [github.com/nonrational/adjacent/issues](https://github.com/nonrational/adjacent/issues).

## Development

Toolchain pinned via `asdf` — see `.tool-versions`. Install the rust plugin then run `asdf install` from the repo root.

The landing page sources live in `ent/`; `just serve` runs `npx live-server` against it.
