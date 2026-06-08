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

Coming soon. Work in progress: [github.com/nonrational/adjacent/issues](https://github.com/nonrational/adjacent/issues).

## Build and run locally

Toolchain pinned via `asdf` — see `.tool-versions` (rust 1.92.0, nodejs 26.2.0). Install the asdf plugins then run `asdf install` from the repo root.

```sh
cargo build                  # workspace build, binary at target/debug/adj
cargo test                   # unit + integration tests
cargo run -- daemon          # run the daemon in the foreground (Ctrl-C to stop)
```

In another shell, against the running daemon:

```sh
cd path/to/your/app          # must contain adjacent.toml
cargo run --manifest-path /path/to/adjacent/Cargo.toml -- add .
cargo run --manifest-path /path/to/adjacent/Cargo.toml -- list
```

State lives in `~/.adjacent/`. Override with `ADJACENT_HOME=/tmp/adj-sandbox` to keep ad-hoc experiments out of the real home. Proxy port defaults to `8080`; override with `ADJACENT_PROXY_PORT=...`.

Minimal `adjacent.toml`:

```toml
name = "site"
cmd = "npm run dev"          # must bind to $PORT
```

Then `curl -H 'Host: site.adj.ac' http://127.0.0.1:8080/` lazy-boots the app and proxies through. Full `--json` output schema in [`crates/adj/JSON.md`](crates/adj/JSON.md).

The landing page sources live in `ent/`; `just serve` runs `npx live-server` against it.
