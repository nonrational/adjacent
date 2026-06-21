# Stack-agnostic app profiler

**Status:** design approved, pre-implementation\
**Date:** 2026-06-19

## Goal

Give a developer and their agent a shared, always-on view of how each
Adjacent-served app is performing at runtime — without instrumenting the app,
knowing its language, or running a second tool. The same "one log, both of you
read it" model, applied to performance.

## The stack-agnostic ceiling (why this scope)

Adjacent is a reverse proxy that also owns the app process. That gives two
vantage points that need no cooperation from the app and work for any language:

1. **HTTP layer** (the proxy sees every request/response): per-route latency,
   status, payload size, throughput, concurrency.
2. **OS process layer** (the supervisor owns the PID + process group): CPU, RSS,
   thread count, file-descriptor count.

Function-level profiling (flame graphs, in-app hot paths) is explicitly **out of
scope**: native PID sampling returns interpreter C-frames on Node/Python/Ruby,
so making it useful requires per-runtime symbolication — which stops being
stack-agnostic and becomes a set of language adapters. This spec covers tiers 1
and 2 only.

## Decisions (load-bearing)

- **Depth:** HTTP + process metrics. No function-level profiling.
- **Collection model:** always-on, in-memory rolling window (fixed 30 min).
  Mirrors the logs/idle-shutdown ethos — nothing to remember to turn on.
- **Route grouping:** auto-template path segments that look like IDs
  (numeric, UUID, long hex/hash) into `:id`; group by templated route. Surface
  the slowest *raw* paths separately for drill-down.
- **Storage:** in-memory only. Metrics reset on daemon restart (rare — the
  daemon is long-lived and outlives individual app restarts, so a window can
  span an app crash/restart). Persistence is deferred.
- **Correlation, not causation:** whole-process CPU cannot be attributed to a
  route. Request metrics and process samples share a time axis; the tool shows
  correlation and never claims per-route resource attribution.

## Architecture

```
crates/adj-protocol/        new DTOs: StatsDto, RouteStatDto, ProcStatDto, LatencyDto
                            new Request::Stats / Response::Stats variants
crates/adj/src/metrics/
  mod.rs       Metrics collector (Arc-shared): record_request, record_sample, snapshot
  route.rs     path -> template (:id collapsing), cardinality cap + overflow bucket
  hist.rs      log-linear latency histogram + per-minute window buckets
  sampler.rs   ProcSampler trait + macos/linux impls + the sampler task
crates/adj/src/proxy.rs     instrument forward() to emit one record per completed request
crates/adj/src/daemon.rs    spawn the sampler task, thread Arc<Metrics>, dispatch Stats
crates/adj/src/client.rs    `adj stats` subcommand
crates/adj/src/main.rs      subcommand wiring
crates/adj/JSON.md          document StatsDto schema (asserted in tests)
```

The collector is one owned unit with a three-method interface
(`record_request`, `record_sample`, `snapshot`). Producers (proxy, sampler) and
the consumer (`adj stats`) are blind to its internals.

### Data model & collector (`metrics/mod.rs`, `hist.rs`)

Per app, a ring of **per-minute buckets** retained for a fixed **30-minute
window**. Each bucket holds, keyed by templated route:

- a **log-linear latency histogram** (bounded fixed buckets, e.g. ~1ms..60s),
- counters: request count, status-class split (2xx/3xx/4xx/5xx), bytes in/out.

Plus a short series of process samples per bucket. `snapshot(app, window)` merges
the buckets in range into a `StatsDto` (percentiles computed from merged
histograms).

- **Cardinality cap:** at most ~200 templated routes per app; further routes fold
  into an `other` bucket. Keeps memory bounded by `routes × hist-buckets ×
  minutes` (low single-digit MB worst case).
- **Slowest raw paths:** a separate bounded top-N (by latency) of actual raw
  paths, for drill-down behind the templated view.
- **Hot-path cost:** per-app `Mutex<AppMetrics>` in a name-keyed map (the same
  per-name locking shape the boot gates already use). Recording a request is one
  lock acquire + a histogram increment.

### Route templating (`metrics/route.rs`)

Normalize the request path: split on `/`, replace any segment matching an ID
shape — all-digits, UUID, or long hex/hash — with `:id`. Key the route as
`<METHOD> <templated-path>`. Templating is heuristic and may occasionally
mis-collapse; the raw-outlier list is the escape hatch.

### Request instrumentation (`proxy.rs`)

In `forward()`: stamp an `Instant` at request start and capture method + raw
path; on the **response head**, capture status, latency (time-to-first-byte),
and byte counts, then call `record_request`. Recording at the response head —
not body completion — keeps long-lived streaming responses from skewing latency,
consistent with the proxy's existing streaming carve-out.

### Process sampler (`metrics/sampler.rs`)

A new daemon task modeled on the idle scanner, ticking every **2s**. For each
`Running` app it samples the whole **process group** (apps are spawned with
`process_group(0)`, so the pgid equals the `sh` pid). Behind:

```rust
trait ProcSampler { fn sample(&mut self, pgid: Pid) -> Option<ProcSample>; }
// ProcSample { cpu_pct, rss_bytes, threads, fds, ts }
```

- **macOS:** `libproc` — `proc_pid_rusage` per pid, enumerated across the group.
- **Linux:** `/proc/<pid>/{stat,status,task,fd}` summed across the group.

CPU% is a delta between ticks, so the sampler retains previous CPU counters per
app. A platform with no impl reports the process section as `unsupported` while
HTTP metrics keep working.

### Output surface (`client.rs`, `main.rs`)

New `adj stats <app>`:

- **default:** human table — top routes (p50/p95, count, err%), slowest raw
  outliers, and a process summary with a compact recent timeline.
- **`--json`:** stable `StatsDto`, documented in `JSON.md` and asserted in tests.
- **`--since <dur>`:** narrow the window (e.g. `5m`); defaults to the full 30 min.

`list` and `status` are left untouched so their asserted JSON contracts do not
churn.

## Data flow

```
request  -> proxy::forward -> on response head: record_request(app, {template, method, status, ttfb, bytes})
sampler  -> every 2s, per Running app: ProcSampler.sample(pgid) -> record_sample(app, {cpu, rss, threads, fds, ts})
adj stats <app> -> daemon dispatch -> Metrics.snapshot(app, window) -> StatsDto -> CLI renders table or --json
```

## Error handling

- App not running / no data yet -> empty `StatsDto` with the window span, **not**
  an error.
- Sampler can't read a pid (app exited mid-tick) -> skip that sample, never crash
  the task.
- No `ProcSampler` for the platform -> process section `unsupported`; HTTP
  metrics unaffected.

## Testing

- **Unit:** route templating (numeric/UUID/hash collapse, cap + overflow),
  histogram percentile accuracy, window aging/eviction.
- **Integration:** drive requests through the proxy at a test app, assert
  `adj stats --json` counts, latency, and status buckets. The **Linux**
  `ProcSampler` is exercised in CI (reads `/proc` of the test app) — assert
  cpu/rss/threads/fds are present and plausible.
- **Contract:** extend `JSON.md` with the `StatsDto` schema and assert it like the
  other DTOs.

## Out of scope (v1)

- Persistence across daemon restarts.
- `[metrics]` config in `adjacent.toml` (window length, disable, sample rate).
- `adj stats --watch` (live refresh loop).
- Anything function-level / flame graphs.
