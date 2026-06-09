# Adjacent

A local dev-server harness so a human developer and an agent developer can share one supervised server instance instead of fighting for control of the process.

When both sides need the same local server running, they evict each other. The agent takes over → the developer loses log visibility. The developer reclaims it → the agent can't validate its work. Adjacent owns the process so neither side has to.

Homepage: [adj.ac/ent](https://adj.ac/ent)

## Shape (v1)

- One CLI: `adj`. Same surface for human and agent — every read command supports `--json` ([schema](crates/adj/JSON.md)).
- One entry per app. `adj add <path>` registers; per-app config lives in `adjacent.toml`.
- Lazy-boot by default. Hit `foo.adj.ac`, the app starts, the request proxies through.
- Readiness probing. Default is TCP-connect; set `health_check_url = "/healthz"` and the proxy waits for a 2xx before forwarding.
- Idle shutdown. Apps stop after no proxied requests for `idle_timeout` (default `"15m"`, accepts `"30s"` / `"1h"` / `"off"`).
- `adj wait-ready <name>` blocks until the app reports ready — handy in agent workflows after `adj restart`.
- `$PORT` injected into the boot command. Apps bind to it. Quirky apps can opt into a different variable name via `port_env = "BIND_PORT"` in `adjacent.toml`.
- Logs on disk at `~/.adjacent/logs/<name>.log`. `adj logs <name> --tail` works.
- DNS via public wildcard `*.adj.ac → 127.0.0.1`. Offline-mode resolver hook is opt-in.
- TLS via opt-in local CA. `adj install-ca` provisions a non-extractable ECDSA key in the macOS login keychain (no private key on disk), name-constrained to `*.adj.ac` so the CA cannot mint trusted certs for other domains.
- **Never runs as root.** Privileged ops emit reviewable commands the user runs with sudo.

## Status

Coming soon. Work in progress: [github.com/nonrational/adjacent/issues](https://github.com/nonrational/adjacent/issues).

## Usage

Run `adj <command> --help` for flags. Every read command supports `--json`.

```zsh
Usage: adj <COMMAND>

Commands:
  daemon                Run the Adjacent daemon in the foreground
  add                   Register an app from a directory containing adjacent.toml
  list                  List registered apps and their state
  up                    Boot a registered app
  down                  Stop a running app (SIGTERM, then SIGKILL after a grace period)
  restart               Restart an app (down then up)
  status                Report the current state of an app
  logs                  Print the log file for an app
  wait-ready            Block until an app reports ready (TCP-open or 2xx from health_check_url)
  agent-instructions    Print a markdown steering doc telling AI coding agents how to interact with the Adjacent-supervised app in the target directory
  install-port-forward  Print the pf anchor and the sudo command to redirect :80 to the proxy port
  install-ca            Generate the local HTTPS CA (if missing) and print the sudo command to trust it
  doctor                Verify the local install end-to-end: pf port-forward rule, daemon reachability, and the local CA (on-disk cert, keychain key, signing ACL, system trust). All checks are rootless. Exit status is 0 when everything passes, 2 when any check fails
  help                  Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

## Agent Integration

When an agent runs in a directory with `adjacent.toml`, it should delegate server management to `adj`. `adj agent-instructions` prints a markdown steering doc. Redirect it to the agent's instructions file:

```sh
cd path/to/your/app
adj agent-instructions >> CLAUDE.md   # or AGENTS.md
```

The doc names the app, names the dev command the agent should _not_ run, and lists the `adj` subcommands the agent should use to read state, restart, and verify changes.

The landing page sources live in `ent/`; `just serve` runs `npx live-server` against it.

## Local Development

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
cmd = "npm run dev"           # must bind to $PORT

# Optional:
health_check_url = "/healthz" # poll for 2xx instead of TCP-open
idle_timeout = "30m"          # stop after no requests (default "15m", or "off")
```

Then `curl -H 'Host: site.adj.ac' http://127.0.0.1:8080/` lazy-boots the app and proxies through. Full `--json` output schema in [`crates/adj/JSON.md`](crates/adj/JSON.md).
