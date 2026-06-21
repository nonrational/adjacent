# Adjacent

A local dev-server harness so a human developer and an agent developer can share one supervised server instance instead of fighting for control of the process.

When both sides need the same local server running, they evict each other. The agent takes over → the developer loses log visibility. The developer reclaims it → the agent can't validate its work. Adjacent owns the process so neither side has to.

Homepage: [adj.ac/ent](https://adj.ac/ent)

## Agentic by Design

- **One CLI for humans & agents.** One surface, `adj`, with `--json` on every read command ([schema](crates/adj/JSON.md)). No separate agent mode — parity keeps the contract simple and stops the two sides drifting apart.

- **Config lives in code.** `adjacent.toml` sits in the app directory and registers via `adj add <path>`. The boot command and idle timeout are part of the repo, not buried in a global registry. Agents make config maintenance cheap.

- **A URL per worktree.** Several agents in parallel git worktrees of one repo can all register. `adj add` names each instance after its branch — `feature-x.site.adj.ac` — each with its own process, port and logs. See [Worktrees](#worktrees).

- **Boots on demand, stops when idle.** Apps don't run until a request hits `<name>.adj.ac`, and concurrent requests during boot wait on the same start. They stop after `idle_timeout` (default `"15m"`, accepts `"30s"` / `"1h"` / `"off"`) with no proxied traffic, so long sessions don't accumulate orphaned servers.

- **Readiness before forwarding.** Default is TCP-connect; opt into HTTP probing with `health_check_url = "/healthz"`. The proxy never forwards to a half-booted process. Agents can block on this explicitly with `adj wait-ready`.

- **The daemon owns ports.** `$PORT` is injected into the boot command; the app binds where it's told. Apps that need a different variable name set `port_env = "BIND_PORT"`.

- **Logs are JSONL on disk.** `~/.adjacent/logs/<name>.log` is the source of truth. `adj logs` projects them for humans; `adj logs --json` streams them as-is.

- **DNS is real, not `/etc/hosts`.** `*.adj.ac` resolves to `127.0.0.1` via a public wildcard A record. No hosts editing, no local resolver, nothing to install for DNS to work.

- **TLS without a private key on disk.** `adj install-ca` provisions an ECDSA key in the macOS login keychain marked non-extractable; the cert is name-constrained to `*.adj.ac` so the CA cannot mint trusted certs for other domains.

- **Never runs as root.** Privileged ops — pf port-forward, CA trust — emit reviewable commands the user runs with `sudo`.

## Status

In development. See [github.com/nonrational/adjacent/issues](https://github.com/nonrational/adjacent/issues).

## Install (alpha)

Apple Silicon, unsigned alpha build, straight from this repo (no separate tap repo):

```sh
brew tap nonrational/adjacent https://github.com/nonrational/adjacent
brew install adjacent
```

Then `adj daemon` (or install it as a login service via a launchd LaunchAgent) and `adj add .` in an
app directory. HTTPS is opt-in via `adj install-ca`; because each `brew upgrade adjacent` replaces the
binary, repair the CA afterward with `adj install-ca --reset && adj install-ca`. Building from source
instead? See [Local Development](#local-development).

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
  remove                Remove an app from the registry (stopping it first if running)
  prune                 Remove every registry entry whose directory no longer exists on disk
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

## Worktrees

Four agents in four git worktrees of the same repo can all register. `adj add` inside a
linked worktree names the instance after its branch: the worktree of `site` on branch
`feature-x` serves at `feature-x.site.adj.ac`, while the main checkout keeps `site.adj.ac`.
No flags needed (`--label` overrides the branch name); each worktree gets its own process,
port and logs. When a worktree is deleted, `adj list` flags leftover entries as stale and
`adj prune` clears them.

## Agent Integration

When an agent runs in a directory with `adjacent.toml`, it should delegate server management to `adj`. `adj agent-instructions` prints a markdown steering doc. Redirect it to the agent's instructions file:

```sh
cd path/to/your/app
adj agent-instructions >> CLAUDE.md   # or AGENTS.md
```

The doc names the app, names the dev command the agent should _not_ run, and lists the `adj` subcommands the agent should use to read state, restart, and verify changes.

The landing page sources live in `ent/`; `just serve` runs `npx live-server` against it.

## Containers

`cmd` can be a `docker run` as easily as an `npm run dev`. The command runs through a shell, so `$PORT` expands — map it to the container's port:

```toml
name = "whoami"
cmd = "docker run --rm --init --name adj-whoami -p 127.0.0.1:$PORT:80 traefik/whoami"
health_check_url = "/"
boot_timeout = 120            # first boot may need to pull the image
```

Three things make this the shape that works:

- **Run attached.** No `-d`, no `compose up -d`. Adjacent supervises the process it spawned; a detached `docker run` exits immediately and reads as a crash. Attached, the docker client forwards SIGTERM to the container on `adj down` and idle shutdown.
- **Set `health_check_url`.** Docker binds the host port before the app inside is listening, so the default TCP-open probe reports ready too early. An HTTP check polls through to the app itself.
- **`--rm` and `--init`.** `--rm` keeps stopped containers from piling up. `--init` makes SIGTERM reach the app even when the image's entrypoint doesn't forward signals.

One caveat: if a container ignores SIGTERM through the grace window, the follow-up SIGKILL kills the docker _client_ — the container keeps running under the Docker daemon. Naming it (`--name`) makes a leaked one easy to spot and `docker stop`.

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

## License

Licensed under either of the [Apache License, Version 2.0](LICENSE-APACHE) or the [MIT license](LICENSE-MIT) at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in this work by you, as defined in the Apache-2.0 license, shall be dual licensed as above, without any additional terms or conditions.
