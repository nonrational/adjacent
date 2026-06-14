# Worktree instances

**Date:** 2026-06-11\
**Status:** Approved design, pre-implementation

## Problem

Four agents working in four git worktrees of the same repo cannot all register with
Adjacent. Every worktree carries the same committed `adjacent.toml`, so they collide on
`name`: the registry keys on it, `adj add` rejects duplicates, and the proxy routes
`<name>.adj.ac` to exactly one path. Worktree #2 simply can't register.

## Goal

Each worktree of an app gets its own URL, its own supervised process, its own port and
its own logs — registered by the agent itself with zero ceremony — plus a cleanup story
for worktrees that have been deleted.

## Design

### Naming and routing: dotted registry keys

Instances are not a new concept. The registry stays one flat `name → path` map; a
worktree instance is an entry whose key contains one dot: `feature-x.site`.

- URL shape: `<label>.<name>.adj.ac` → worktree; bare `<name>.adj.ac` keeps pointing at
  the main checkout.
- `name_from_host` in `proxy.rs` accepts at most one dot in the prefix (two labels).
  Deeper hosts are still rejected.
- Supervisor, boot gate, idle scanner and JSONL logs are string-keyed and need no
  changes: `feature-x.site` gets its own port, its own `~/.adjacent/logs/feature-x.site.log`,
  its own idle timer.
- `RESERVED_NAMES` validation applies to both the label and the base name.
- No structural parent/child relationship. A worktree whose branch carries a different
  `adjacent.toml` behaves independently, which is correct. `adj remove site` does not
  cascade to `*.site` entries; `adj prune` is the all-in-one cleanup (below).

### Registration: worktree detection in `adj add`

Detection happens client-side — the client has the CWD and git context; the daemon
receives a final name + path, consistent with the existing canonicalize-client-side rule.

- `adj add` in a linked worktree (`.git` is a file, confirmed via `git rev-parse`)
  derives the label from `git rev-parse --abbrev-ref HEAD`, sanitized to a DNS label:
  lowercase; `/` and `_` become `-`; any other non-`[a-z0-9-]` character is stripped;
  empty result is an error directing the user to `--label`.
- Registered key: `<label>.<cfg.name>`.
- `adj add --label demo` overrides the derived label, and also works outside a worktree
  (a plain second clone can register as `demo.site`).
- A main checkout registers exactly as today. Nothing requires the parent name to be
  registered first.
- Duplicate key (same branch label twice) is rejected with the existing
  already-registered error, suggesting `--label`.

Wire change: `Request::Add` gains the instance name. The daemon keeps final say on
validation (reserved names, dot count, label charset).

### TLS: per-app wildcard SANs on the leaf

A single-label wildcard `*.adj.ac` does not match `feature-x.site.adj.ac`. The CA's
`nameConstraints` (`adj.ac`) permits any depth, so this is a leaf-issuance problem only —
no CA or trust-store changes.

- Leaf SAN set: `adj.ac`, `*.adj.ac`, plus `*.<name>.adj.ac` for every registered
  top-level base name.
- The daemon re-issues the leaf when a registry change alters the SAN set (add/remove/
  prune), signing through the keychain as it already does at startup.
- The HTTPS listener serves the new leaf without restart via a
  `rustls::server::ResolvesServerCert` impl reading an `ArcSwap`'d certified key.
- No CA installed → HTTPS stays opted-out; SAN regeneration is a no-op.

### Lifecycle: remove, prune, stale

New control-plane requests `Remove { name }` and `Prune`, both serialized under the
existing registry lock.

- `adj remove <name>` — downs the app if running, deletes the registry entry.
- `adj prune` — removes every entry whose registered path no longer exists (downing any
  that still run), and reports each removed name. This is the all-in-one cleanup for
  deleted worktrees and deleted folders alike.
- `adj list` marks entries whose path is missing as `stale`.
- A request routed to a stale entry returns 502 with a message naming the cause:
  path deleted — run `adj prune`.
- No auto-pruning. The registry never mutates behind the user's back; a temporarily
  unmounted volume must not silently deregister real apps.

## Error handling

- Worktree detected but branch is detached HEAD → error directing to `--label`.
- Label sanitizes to empty → error directing to `--label`.
- `--label` with invalid characters after sanitization rules → rejected by the daemon.
- Boot/readiness/idle failures for instances are identical to base apps (same code paths).

## Testing

- New integration test `crates/adj/tests/worktree.rs`: build a real git repo plus a
  linked worktree in a tmpdir; `adj add` from both; assert `site.adj.ac` and
  `<branch>.site.adj.ac` route to their respective processes; delete the worktree
  directory; assert `adj list` shows `stale`; assert `adj prune` removes it and reports it.
- Unit tests: label sanitization, two-label `name_from_host` parsing, SAN-set
  computation from a registry snapshot.
- `crates/adj/JSON.md` contract gains the `stale` field on list entries and the
  remove/prune response shapes; the JSON test suite asserts them.

## Out of scope

- Structural parent/child registry model (instances cascade-deleting with their parent).
- Ephemeral, registry-less instances (`adj up --here`): breaks lazy-boot after daemon
  restart, which is the tool's core promise.
- Hosts deeper than two labels.
- Auto-pruning on detection.

## Alternatives considered

1. **Flat suffixed names** (`site-feature-x.adj.ac`) — zero TLS work, but the URL doesn't
   express the instance relationship and collides with legitimately-hyphenated app names.
2. **First-class instance model** — registry entries grow an `instances` sub-map; stronger
   invariants but touches the wire protocol, every dispatch arm and the JSON contract for
   a structural distinction nothing consumes yet.
3. **Ephemeral instances** — self-cleaning lifecycle but incompatible with lazy-boot
   (see Out of scope).
