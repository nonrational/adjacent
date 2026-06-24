# Expose an `ADJ_*` boot environment to supervised apps

**Date:** 2026-06-22\
**Status:** Approved design, pre-implementation

## Problem

An app fronted by Adjacent has no way to learn its own external address. The supervisor
injects `$PORT` (the kernel-assigned upstream port) and nothing else, so a dev server that
generates absolute links defaults to `localhost:<port>` — wrong for every link a user
clicks through `*.adj.ac`.

Hugo is the motivating case. To emit correct links it needs `--baseURL` set to the address
the browser actually uses:

```toml
cmd = "hugo server --appendPort=false --port $PORT --baseURL $ADJ_URL_HTTP"
```

The base URL is something **Adjacent owns** — the name, the `.adj.ac` host, the listening
ports are all the daemon's facts. So the daemon should hand them to the app, not make the
app reconstruct them.

## Goal

At boot, inject a daemon-owned `ADJ_*` environment namespace so a `cmd` can reference its
own identity and external URLs. The values must be correct for plain apps, worktree
instances, the kernel-assigned-port (`port = 0`) sandbox case, and whether or not the user
has installed the port-forward or the CA.

## Design

### The six variables

Shown for app `alannorton-com` with the daemon on `:8080` / `:8443`:

| Var | Value | Notes |
|---|---|---|
| `ADJ_NAME` | `alannorton-com` | the routing key |
| `ADJ_HOST` | `alannorton-com.adj.ac` | bare hostname, no scheme/port |
| `ADJ_URL` | `https://alannorton-com.adj.ac` | canonical; prefer this |
| `ADJ_URL_HTTP` | `http://alannorton-com.adj.ac` | canonical, no TLS trust needed |
| `ADJ_URL_DIRECT` | `https://alannorton-com.adj.ac:8443` | daemon's real HTTPS port |
| `ADJ_URL_HTTP_DIRECT` | `http://alannorton-com.adj.ac:8080` | daemon's real HTTP port, always reachable |

Two axes of fallback, by deliberate design (decided in brainstorming, option (b) — four
ready-made URLs over composable primitives, so a `cmd` drops in one variable and is
correct):

- **port axis** — canonical (`:443`/`:80`, assumes the pfctl forward is installed) vs
  direct (the daemon's real `:8443`/`:8080`, always reachable).
- **scheme axis** — `https` (requires `adj install-ca`) vs `http` (always works, no trust).

The four ready-made URLs cover every cell so the user never assembles a URL in a TOML
string.

### Worktree instances

A worktree instance is keyed `<label>.<name>` and routes at `<label>.<name>.adj.ac`.
`ADJ_NAME` is the **routing key** (`feature-x.site`), so every value shifts together:
`ADJ_HOST=feature-x.site.adj.ac`, `ADJ_URL=https://feature-x.site.adj.ac`, etc. No label/
base-name split is exposed (deferred — see Out of scope); the routing key is the single
identity an app needs to address itself.

### Where the values come from

- `ADJ_NAME` is the `name` argument `up()` already receives. It **is** the routing key
  — correct for worktree instances with zero extra work.
- The host is `format!("{name}{HOST_SUFFIX}")`, reusing `proxy::HOST_SUFFIX` (`.adj.ac`),
  promoted to `pub` so there is one source of truth for the suffix.
- The two `_DIRECT` ports resolve as **"configured port if non-zero, else read the
  `proxy.port` / `https.port` file."** This mirrors the env-or-file discovery the test
  sandboxes already use, so the kernel-assigned (`port = 0`) case stays correct: by the
  time any app boots, the listeners have bound and written those files.
- `ADJ_URL` / `ADJ_URL_HTTP` carry no port, so they are always emittable.

**Omission rule.** If the HTTPS port can't be resolved — `port = 0` *and* the HTTPS
listener never bound (no CA installed, so `https.port` was never written) — `ADJ_URL_DIRECT`
is omitted rather than emitted pointing at a dead port. The default daemon (`:8443`
configured, non-zero) always emits all six; the https-based URLs still assume the user ran
`install-ca`, the same caveat that already applies to the canonical `ADJ_URL`.

### Injection point and precedence

Injected in `supervisor::up()` **after** the `env_file → [env]` layers, alongside the
existing `PORT` injection. `ADJ_*` is a **reserved, daemon-owned namespace**: these six win
over anything a user sets in `env_file` or `[env]`. Documented as reserved; no validation is
added to forbid user `ADJ_*` keys (YAGNI — collisions are silently overridden by the
daemon's value, non-colliding `ADJ_*` keys pass through).

This keeps the env-layering responsibility where it already lives — the supervisor owns
"the environment we inject" — and is consistent with how `PORT` is handled.

### Wiring

The supervisor builds the `ADJ_*` env itself, so the **three `up()` call sites are
unchanged** (`proxy.rs` lazy-boot, and the `Up` / `WaitReady` RPC handlers in `daemon.rs`).
Threading a pre-built env through all three was the alternative; building it inside the
supervisor avoids triplicating the call and matches the existing `PORT` pattern.

- **New type `ProxyPorts`** captures how to resolve the external ports:
  `{ http_configured: u16, https_configured: u16, http_port_file: PathBuf,
  https_port_file: PathBuf }` with `http() -> Option<u16>` / `https() -> Option<u16>`
  applying the configured-else-file rule. Built once in `daemon::run` from
  `proxy::proxy_port()`, `proxy::https_port()`, `paths::proxy_port_path()`,
  `paths::https_port_path()`.
- **`Supervisor::new(proxy_ports: ProxyPorts)`** — the supervisor holds it and reads it at
  `up()` time (not at construction: at construction the listeners haven't bound, so a
  `port = 0` actual value isn't known yet; per-boot resolution is correct and boots are
  infrequent). The two unit-test constructors (`supervisor.rs:431`, `:450`) get a
  `ProxyPorts::for_test()` helper returning fixed ports.
- **Pure helper `adj_env(name, http: Option<u16>, https: Option<u16>) -> Vec<(String,
  String)>`** builds the six (or five) pairs. Pure and deterministic → unit-tested without a
  daemon or socket. Lives next to the supervisor's env-layering code.

### Documentation

- **`adj add` scaffold** (`scaffold.rs` `render`): add the `ADJ_*` namespace to the
  `# Optional:` commented block, with the Hugo one-liner as the motivating example and a
  note that `--appendPort=false` (the `=` form) is required for Hugo.
- **`agent_docs.rs`**: document the `ADJ_*` namespace in the per-app doc so an agent reading
  `adj docs <name>` sees the available variables.
- **`CLAUDE.md`**: extend the `adjacent.toml` reference block to list the `ADJ_*` namespace
  and note it is daemon-owned / reserved.

## Testing

**Unit:**
- `adj_env`: given fixed `name` + ports, asserts the six keys and exact value formats,
  including the worktree-key case (`feature-x.site`).
- `adj_env` omission: `https = None` → `ADJ_URL_DIRECT` absent, other five present.
- `ProxyPorts` resolution: configured non-zero → returns it without touching the file;
  configured `0` + file present → returns the file value; configured `0` + file absent →
  `None`.

**Integration (`crates/adj/tests/`):**
- Register an app whose `cmd` echoes the `ADJ_*` vars into its log, boot it through the
  proxy, and assert via `adj logs --json` that the expected values reached the child. Covers
  end-to-end injection and the host/URL formatting against a real (kernel-assigned) port.

## Out of scope

- **Bare-integer port primitives** (`ADJ_HTTP_PORT` / `ADJ_HTTPS_PORT`). Deferred. The one
  thing that would reopen this: Hugo's LiveReload. Hugo injects a script that opens a
  WebSocket to the page host; behind the proxy that should work via WebSocket upgrade
  forwarding. If LiveReload turns out to need an explicit `--liveReloadPort=<bare-number>`,
  none of the six vars provides a bare integer, and we add these two back.
- **`ADJ_BASE_NAME` / `ADJ_LABEL` split** (the worktree label/base-name decomposition).
  Deferred until a concrete use (e.g. a per-branch database name) appears; plain apps make
  all three identical, so the routing key covers the common case.
- **Validation forbidding user-set `ADJ_*` keys.** The daemon-owned values simply win.
