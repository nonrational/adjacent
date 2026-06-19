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

## `adj stats <name> --json`

A single JSON object describing the rolling in-memory metrics window for one app. The window is
30 minutes; `--since <dur>` narrows it. The `process` field is present only when the app is
running, has a fresh sample, and the platform supports process sampling.

```json
{
  "name": "site",
  "window_secs": 1800,
  "total_requests": 1240,
  "routes": [
    {
      "route": "GET /users/:id",
      "count": 980,
      "latency_ms": { "p50": 8, "p95": 128, "p99": 256, "max": 412 },
      "status_2xx": 970,
      "status_3xx": 0,
      "status_4xx": 10,
      "status_5xx": 0
    }
  ],
  "slowest_raw": [
    { "method": "GET", "path": "/users/42", "status": 200, "latency_ms": 412 }
  ],
  "process": {
    "cpu_pct": 38.0,
    "rss_bytes": 536870912,
    "threads": 24,
    "fds": 180,
    "sampled_at": "2026-06-19T18:23:11Z"
  }
}
```

| Field            | Type   | When                                                        |
|------------------|--------|-------------------------------------------------------------|
| `name`           | string | always                                                      |
| `window_secs`    | number | always; seconds of history covered                          |
| `total_requests` | number | always; sum of route counts in the window                   |
| `routes`         | array  | always; one entry per templated route, busiest first        |
| `slowest_raw`    | array  | always; slowest individual raw paths in the window (≤ 10)    |
| `process`        | object | present iff a fresh process sample exists for a running app  |

Each route entry carries `route` (string), `count` (number), `latency_ms` (object with `p50` /
`p95` / `p99` / `max` in milliseconds), and `status_2xx` / `status_3xx` / `status_4xx` /
`status_5xx` (numbers). The `process` object carries `cpu_pct` (number), `rss_bytes` (number),
`threads` (number), `fds` (number), and `sampled_at` (RFC3339 UTC string).

Route latency values are histogram bucket upper bounds — honest over-estimates, never under-
reported. Path segments that look like IDs (digits, UUIDs, long hashes) collapse to `:id` so
route cardinality stays bounded; the original paths survive in `slowest_raw`. `process.cpu_pct`
is whole-process-group CPU and is not attributable to any single route.

## Versioning

This schema is the v1 contract. Additions (new optional fields) are non-breaking. Removals
or type changes require a deprecation cycle.
