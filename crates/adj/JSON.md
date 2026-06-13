# `adj --json` output schema

Every read command on `adj` accepts `--json` and emits a stable, machine-parseable shape.
This document is the contract — fields documented here will not change without a deprecation
notice, and the test suite asserts the shapes below.

Write commands (`add`, `up`, `down`, `restart`, `remove`, `prune`) do not accept `--json` in v1.

## States

A registered app is in one of three states:

- `"stopped"` — registered, not currently supervised.
- `"running"` — supervised process is alive and bound to a port.
- `"crashed"` — last supervised run exited non-zero and was not intentionally stopped.

## `adj list --json`

A JSON array of entries, one per registered app. Optional fields are present only when meaningful.

```json
[
  { "name": "site",           "path": "/Users/me/code/site",    "state": "running", "port": 53412 },
  { "name": "feature-x.site", "path": "/Users/me/code/deleted", "state": "stopped", "stale": true },
  { "name": "backend",        "path": "/Users/me/code/api",     "state": "stopped" },
  { "name": "worker",         "path": "/Users/me/code/work",    "state": "crashed" }
]
```

| Field   | Type    | When                                 |
|---------|---------|--------------------------------------|
| `name`  | string  | always                               |
| `path`  | string  | always (absolute, canonical)         |
| `state` | string  | always; one of `stopped` / `running` / `crashed` |
| `port`  | number  | present iff `state == "running"`     |
| `stale` | boolean | present iff the registered path no longer exists on disk |

An empty registry returns `[]`.

## `adj status <name> --json`

A single JSON object describing one app. Optional fields are present only when meaningful.

Running:

```json
{
  "name": "site",
  "path": "/Users/me/code/site",
  "state": "running",
  "pid": 84212,
  "port": 53412,
  "started_at": "2026-06-07T18:23:11.401234Z"
}
```

Stopped:

```json
{
  "name": "site",
  "path": "/Users/me/code/site",
  "state": "stopped"
}
```

Crashed:

```json
{
  "name": "worker",
  "path": "/Users/me/code/work",
  "state": "crashed",
  "exit_code": 1
}
```

| Field        | Type   | When                                       |
|--------------|--------|--------------------------------------------|
| `name`       | string | always                                     |
| `path`       | string | always                                     |
| `state`      | string | always; one of `stopped` / `running` / `crashed` |
| `pid`        | number | present iff running                        |
| `port`       | number | present iff running                        |
| `started_at` | string | present iff running; RFC3339 UTC           |
| `exit_code`  | number | present iff crashed                        |

## `adj logs <name> --json`

JSONL: one JSON object per line. Each record is one line from the supervised process's
`stdout` or `stderr`, tagged at capture time.

```jsonl
{"ts":"2026-06-07T18:23:11.402Z","stream":"stdout","line":"listening on :53412"}
{"ts":"2026-06-07T18:23:12.118Z","stream":"stderr","line":"deprecation: foo() is unused"}
```

| Field    | Type   | Notes                                          |
|----------|--------|------------------------------------------------|
| `ts`     | string | RFC3339 UTC, recorded when the supervisor read the line |
| `stream` | string | `"stdout"` or `"stderr"`                       |
| `line`   | string | the raw line with the trailing newline stripped |

The on-disk log file at `~/.adjacent/logs/<name>.log` is itself JSONL in this exact shape;
`--json` streams the file as-is. The plain (non-`--json`) view projects the `line` field.

## `adj logs <name> --tail --json`

Same shape as above, streamed. New records are emitted as they're written by the supervisor.
The process keeps reading until interrupted (`Ctrl-C`), exactly like `--tail` without `--json`.

```sh
adj logs site --tail --json | jq -r 'select(.stream == "stderr") | .line'
```

## Versioning

This schema is the v1 contract. Additions (new optional fields) are non-breaking. Removals
or type changes require a deprecation cycle.
