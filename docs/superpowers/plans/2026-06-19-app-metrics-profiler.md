# Stack-agnostic App Profiler Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an always-on, in-memory profiler to the Adjacent daemon that reports per-route HTTP metrics and OS process metrics for each served app, exposed via `adj stats <app>`.

**Architecture:** The proxy records one entry per completed request into a shared `Metrics` collector; a 2s sampler task records process-group CPU/RSS/threads/fds. Both feed a per-app rolling 30-minute window of per-minute buckets. `adj stats` queries a snapshot over the control socket. No app cooperation, no persistence, no new server.

**Tech Stack:** Rust (tokio, hyper, serde, nix), `/proc` on Linux + `libproc` on macOS behind a `ProcSampler` trait.

**Reference spec:** `docs/superpowers/specs/2026-06-19-app-metrics-profiler-design.md`

---

## File Structure

| File | Responsibility |
|------|----------------|
| `crates/adj-protocol/src/lib.rs` | New `Request::Stats` / `Response::Stats` + owned DTOs (`StatsDto`, `RouteStatDto`, `LatencyDto`, `RawSampleDto`, `ProcStatDto`). |
| `crates/adj/src/metrics/route.rs` | `templatize(method, path)` — collapse ID-shaped path segments to `:id`. Pure. |
| `crates/adj/src/metrics/hist.rs` | `Histogram` — log-linear latency buckets, percentiles, merge. Pure. |
| `crates/adj/src/metrics/sampler.rs` | `ProcSampler` trait, `RawProc`, `ProcSample`, Linux + macOS impls, `default_sampler()`. |
| `crates/adj/src/metrics/mod.rs` | `Metrics` collector: `record_request`, `record_sample`, `snapshot`. Owns the window. |
| `crates/adj/src/proxy.rs` | Instrument `handle()` to record each request; thread `Arc<Metrics>` through the serve loop. |
| `crates/adj/src/supervisor.rs` | Add `running_pids()` for the sampler. |
| `crates/adj/src/daemon.rs` | Construct `Arc<Metrics>`, spawn the sampler task, thread metrics into dispatch, handle `Stats`. |
| `crates/adj/src/client.rs` | `stats()` — RPC + human table / `--json`. `parse_since()`. |
| `crates/adj/src/main.rs` | `mod metrics;` + `Cmd::Stats` wiring. |
| `crates/adj/JSON.md` | Document the `StatsDto` schema. |
| `crates/adj/tests/stats.rs` | Integration test: drive requests through the proxy, assert `adj stats --json`. |

Tasks are ordered so each leaves the tree compiling and tested. Pure units (route, hist, collector, Linux sampler) come first; daemon/proxy wiring and the CLI come once their dependencies exist.

---

## Task 1: Protocol DTOs and Stats request/response

**Files:**
- Modify: `crates/adj-protocol/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Append to `crates/adj-protocol/src/lib.rs`:

```rust
#[cfg(test)]
mod stats_tests {
    use super::*;

    #[test]
    fn stats_dto_round_trips_and_omits_absent_process() {
        let dto = StatsDto {
            name: "site".into(),
            window_secs: 1800,
            total_requests: 3,
            routes: vec![RouteStatDto {
                route: "GET /users/:id".into(),
                count: 3,
                latency_ms: LatencyDto { p50: 8, p95: 128, p99: 128, max: 91 },
                status_2xx: 2,
                status_3xx: 0,
                status_4xx: 1,
                status_5xx: 0,
            }],
            slowest_raw: vec![RawSampleDto {
                method: "GET".into(),
                path: "/users/42".into(),
                status: 200,
                latency_ms: 91,
            }],
            process: None,
        };
        let json = serde_json::to_string(&dto).unwrap();
        assert!(!json.contains("process"), "absent process must be omitted: {json}");
        let back: StatsDto = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dto);
    }

    #[test]
    fn stats_request_tags_kind() {
        let req = Request::Stats { name: "site".into(), since_secs: 0 };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"kind\":\"stats\""), "got: {json}");
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p adj-protocol stats_tests`
Expected: FAIL — `StatsDto` / `Request::Stats` not defined.

- [ ] **Step 3: Add the request/response variants**

In `crates/adj-protocol/src/lib.rs`, add to `enum Request` (after `Prune`):

```rust
    /// Snapshot the in-memory metrics window for `name`. `since_secs == 0` means the full window.
    Stats {
        name: String,
        #[serde(default)]
        since_secs: u64,
    },
```

Add to `enum Response` (after `Pruned`):

```rust
    Stats { stats: StatsDto },
```

- [ ] **Step 4: Add the DTO structs**

Append to `crates/adj-protocol/src/lib.rs` (before the `#[cfg(test)]` module):

```rust
/// Stable JSON shape for `adj stats <name> --json`. Produced by the daemon's in-memory metrics
/// collector over the rolling window. See `crates/adj/JSON.md`. Unlike `StatusDto`/`ListEntryDto`
/// (borrowed views), this is owned: it carries a computed snapshot, not a borrow of daemon state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StatsDto {
    pub name: String,
    /// Seconds of history this snapshot covers (the rolling window, or `since` when narrower).
    pub window_secs: u64,
    /// Total requests recorded in the covered window, across all routes.
    pub total_requests: u64,
    pub routes: Vec<RouteStatDto>,
    /// Slowest individual raw paths in the window, for drill-down behind the templated routes.
    pub slowest_raw: Vec<RawSampleDto>,
    /// Process resource summary. Absent when the app isn't running, has no fresh sample, or the
    /// platform has no `ProcSampler`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcStatDto>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RouteStatDto {
    /// Templated route, e.g. `GET /users/:id`.
    pub route: String,
    pub count: u64,
    pub latency_ms: LatencyDto,
    pub status_2xx: u64,
    pub status_3xx: u64,
    pub status_4xx: u64,
    pub status_5xx: u64,
}

/// Latency percentiles in milliseconds. Values are histogram bucket upper bounds, so they are
/// honest over-estimates of the true percentile — never under-reported.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct LatencyDto {
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
    pub max: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawSampleDto {
    pub method: String,
    pub path: String,
    pub status: u16,
    pub latency_ms: u64,
}

/// Whole-process-group resource summary from the most recent sample. CPU is group-wide and
/// cannot be attributed to a route — the snapshot pairs it with request metrics on a shared
/// window, not as causation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProcStatDto {
    pub cpu_pct: f64,
    pub rss_bytes: u64,
    pub threads: u64,
    pub fds: u64,
    /// RFC3339 UTC timestamp of the most recent sample.
    pub sampled_at: String,
}
```

- [ ] **Step 5: Run test to verify it passes**

Run: `cargo test -p adj-protocol stats_tests`
Expected: PASS (2 tests).

- [ ] **Step 6: Commit**

```bash
git add crates/adj-protocol/src/lib.rs
git commit crates/adj-protocol/src/lib.rs -m "Add Stats request/response and profiler DTOs to the protocol"
```

---

## Task 2: Route templating

**Files:**
- Create: `crates/adj/src/metrics/route.rs`
- Modify: `crates/adj/src/metrics/mod.rs` (created here as a stub module root)
- Modify: `crates/adj/src/main.rs` (register `mod metrics;`)

- [ ] **Step 1: Create the module root and register it**

Create `crates/adj/src/metrics/mod.rs`:

```rust
pub mod route;
```

In `crates/adj/src/main.rs`, add to the `mod` list (alphabetical, after `mod installca;`):

```rust
mod metrics;
```

- [ ] **Step 2: Write the failing test**

Create `crates/adj/src/metrics/route.rs`:

```rust
//! Collapse ID-shaped path segments to `:id` so per-route metrics don't explode in cardinality.

/// Build a route key from an HTTP method and request path. The query string and fragment are
/// dropped (unbounded, not part of route identity); each path segment that looks like an
/// identifier — all digits, a UUID, or a long hex/hash — becomes `:id`. The method is prefixed
/// so `GET /x` and `POST /x` are distinct routes.
pub fn templatize(method: &str, path: &str) -> String {
    let path = path.split(['?', '#']).next().unwrap_or(path);
    let path = if path.is_empty() { "/" } else { path };
    let templated = path
        .split('/')
        .map(|seg| if is_id_segment(seg) { ":id" } else { seg })
        .collect::<Vec<_>>()
        .join("/");
    format!("{method} {templated}")
}

fn is_id_segment(seg: &str) -> bool {
    if seg.is_empty() {
        return false;
    }
    if seg.bytes().all(|b| b.is_ascii_digit()) {
        return true;
    }
    if is_uuid(seg) {
        return true;
    }
    // Long all-hex segments are content hashes / git shas / opaque tokens.
    seg.len() >= 16 && seg.bytes().all(|b| b.is_ascii_hexdigit())
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() != 36 {
        return false;
    }
    b.iter().enumerate().all(|(i, c)| match i {
        8 | 13 | 18 | 23 => *c == b'-',
        _ => c.is_ascii_hexdigit(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_numeric_ids() {
        assert_eq!(templatize("GET", "/users/123"), "GET /users/:id");
        assert_eq!(templatize("GET", "/users/1"), "GET /users/:id");
        assert_eq!(templatize("GET", "/users/123/posts/456"), "GET /users/:id/posts/:id");
    }

    #[test]
    fn collapses_uuid_and_long_hex() {
        assert_eq!(
            templatize("GET", "/items/550e8400-e29b-41d4-a716-446655440000"),
            "GET /items/:id"
        );
        assert_eq!(
            templatize("GET", "/blob/2f1d3a9c8b7e6f5a4d3c2b1a0998877665544332"),
            "GET /blob/:id"
        );
    }

    #[test]
    fn keeps_words_versions_and_short_segments() {
        assert_eq!(templatize("GET", "/v1/users"), "GET /v1/users");
        assert_eq!(templatize("GET", "/api/feed"), "GET /api/feed");
        // A short hex-looking word is not collapsed (< 16 chars, not all-digit, not UUID).
        assert_eq!(templatize("GET", "/face"), "GET /face");
    }

    #[test]
    fn strips_query_and_normalizes_root() {
        assert_eq!(templatize("GET", "/api/feed?page=2"), "GET /api/feed");
        assert_eq!(templatize("GET", "/"), "GET /");
        assert_eq!(templatize("GET", ""), "GET /");
    }

    #[test]
    fn method_distinguishes_routes() {
        assert_ne!(templatize("GET", "/x"), templatize("POST", "/x"));
    }
}
```

- [ ] **Step 3: Run test to verify it passes (implementation is included above)**

Run: `cargo test -p adj metrics::route`
Expected: PASS (5 tests). The implementation and tests land together because templating is a pure leaf function with no prior dependency to stub against.

- [ ] **Step 4: Commit**

```bash
git add crates/adj/src/metrics/mod.rs crates/adj/src/metrics/route.rs crates/adj/src/main.rs
git commit crates/adj/src/metrics/mod.rs crates/adj/src/metrics/route.rs crates/adj/src/main.rs -m "Add route templating for metrics cardinality control"
```

---

## Task 3: Latency histogram

**Files:**
- Create: `crates/adj/src/metrics/hist.rs`
- Modify: `crates/adj/src/metrics/mod.rs` (add `pub mod hist;`)

- [ ] **Step 1: Register the module**

In `crates/adj/src/metrics/mod.rs` add:

```rust
pub mod hist;
```

- [ ] **Step 2: Write the failing test (implementation included)**

Create `crates/adj/src/metrics/hist.rs`:

```rust
//! Fixed log-linear latency histogram. Bounded memory, cheap to record into and merge, and
//! percentiles are reported as bucket upper bounds (honest over-estimates).

/// Bucket upper bounds in milliseconds. A recorded value lands in the first bucket whose bound
/// is `>= value`; anything larger lands in an implicit overflow bucket.
const BOUNDS_MS: &[u64] = &[
    1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024, 2048, 4096, 8192, 16384, 32768, 65536,
];

#[derive(Clone, Debug)]
pub struct Histogram {
    buckets: Vec<u64>, // len = BOUNDS_MS.len() + 1 (last is the overflow bucket)
    count: u64,
    max: u64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            buckets: vec![0; BOUNDS_MS.len() + 1],
            count: 0,
            max: 0,
        }
    }
}

impl Histogram {
    pub fn record(&mut self, ms: u64) {
        let idx = BOUNDS_MS
            .iter()
            .position(|&b| ms <= b)
            .unwrap_or(BOUNDS_MS.len());
        self.buckets[idx] += 1;
        self.count += 1;
        if ms > self.max {
            self.max = ms;
        }
    }

    pub fn merge(&mut self, other: &Histogram) {
        for (i, c) in other.buckets.iter().enumerate() {
            self.buckets[i] += c;
        }
        self.count += other.count;
        if other.max > self.max {
            self.max = other.max;
        }
    }

    pub fn count(&self) -> u64 {
        self.count
    }

    pub fn max(&self) -> u64 {
        self.max
    }

    /// Percentile in milliseconds, reported as the upper bound of the bucket the rank falls in.
    /// The overflow bucket reports the exact observed `max`. Returns 0 for an empty histogram.
    pub fn percentile(&self, q: f64) -> u64 {
        if self.count == 0 {
            return 0;
        }
        let target = ((q * self.count as f64).ceil() as u64).max(1);
        let mut cum = 0u64;
        for (i, c) in self.buckets.iter().enumerate() {
            cum += c;
            if cum >= target {
                return BOUNDS_MS.get(i).copied().unwrap_or(self.max);
            }
        }
        self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_reports_zero() {
        let h = Histogram::default();
        assert_eq!(h.count(), 0);
        assert_eq!(h.percentile(0.5), 0);
        assert_eq!(h.max(), 0);
    }

    #[test]
    fn single_value_uses_bucket_bound_and_exact_max() {
        let mut h = Histogram::default();
        h.record(100); // first bound >= 100 is 128
        assert_eq!(h.count(), 1);
        assert_eq!(h.percentile(0.5), 128);
        assert_eq!(h.percentile(0.99), 128);
        assert_eq!(h.max(), 100);
    }

    #[test]
    fn skewed_distribution_percentiles() {
        let mut h = Histogram::default();
        for _ in 0..90 {
            h.record(1);
        }
        for _ in 0..10 {
            h.record(1000); // first bound >= 1000 is 1024
        }
        assert_eq!(h.count(), 100);
        assert_eq!(h.percentile(0.50), 1);
        assert_eq!(h.percentile(0.95), 1024);
        assert_eq!(h.percentile(0.99), 1024);
        assert_eq!(h.max(), 1000);
    }

    #[test]
    fn overflow_bucket_reports_max() {
        let mut h = Histogram::default();
        h.record(200_000); // larger than the last bound (65536) -> overflow bucket
        assert_eq!(h.percentile(0.99), 200_000);
        assert_eq!(h.max(), 200_000);
    }

    #[test]
    fn merge_combines_counts_and_max() {
        let mut a = Histogram::default();
        a.record(5);
        let mut b = Histogram::default();
        b.record(5000);
        a.merge(&b);
        assert_eq!(a.count(), 2);
        assert_eq!(a.max(), 5000);
    }
}
```

- [ ] **Step 3: Run test to verify it passes**

Run: `cargo test -p adj metrics::hist`
Expected: PASS (5 tests).

- [ ] **Step 4: Commit**

```bash
git add crates/adj/src/metrics/hist.rs crates/adj/src/metrics/mod.rs
git commit crates/adj/src/metrics/hist.rs crates/adj/src/metrics/mod.rs -m "Add log-linear latency histogram for the profiler"
```

---

## Task 4: Metrics collector (HTTP path)

Builds the rolling window over `route` + `hist`. Process samples come in Task 6. All time enters through explicit `now_unix` arguments so the collector is deterministic under test.

**Files:**
- Modify: `crates/adj/src/metrics/mod.rs`

- [ ] **Step 1: Write the failing test (implementation included)**

Replace the contents of `crates/adj/src/metrics/mod.rs` with:

```rust
pub mod hist;
pub mod route;
pub mod sampler; // added in Task 5; declared now so the module tree is stable

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use adj_protocol::{LatencyDto, ProcStatDto, RawSampleDto, RouteStatDto, StatsDto};

use self::hist::Histogram;
use self::sampler::ProcSample;

/// Rolling window length. Buckets older than this are evicted on write.
const WINDOW_SECS: u64 = 30 * 60;
/// Per-bucket distinct-route cap. Routes beyond this fold into `other` so a high-cardinality app
/// (or a templating miss) can't grow a bucket without bound.
const MAX_ROUTES_PER_BUCKET: usize = 200;
/// How many slowest raw paths to retain per app.
const MAX_RAW: usize = 10;
/// A process sample older than this (relative to the snapshot) is considered stale and omitted —
/// e.g. the app stopped, so the sampler no longer refreshes it. Three sample intervals.
const PROC_FRESH_SECS: u64 = 6;

/// One request as seen by the proxy, before templating.
pub struct RequestRecord {
    pub method: String,
    pub raw_path: String,
    pub status: u16,
    pub latency_ms: u64,
}

#[derive(Default)]
struct RouteAgg {
    hist: Histogram,
    count: u64,
    s2xx: u64,
    s3xx: u64,
    s4xx: u64,
    s5xx: u64,
}

impl RouteAgg {
    fn record(&mut self, status: u16, latency_ms: u64) {
        self.hist.record(latency_ms);
        self.count += 1;
        match status / 100 {
            2 => self.s2xx += 1,
            3 => self.s3xx += 1,
            4 => self.s4xx += 1,
            5 => self.s5xx += 1,
            _ => {}
        }
    }

    fn merge(&mut self, other: &RouteAgg) {
        self.hist.merge(&other.hist);
        self.count += other.count;
        self.s2xx += other.s2xx;
        self.s3xx += other.s3xx;
        self.s4xx += other.s4xx;
        self.s5xx += other.s5xx;
    }
}

struct Bucket {
    minute: u64,
    routes: HashMap<String, RouteAgg>,
}

struct RawSample {
    method: String,
    path: String,
    status: u16,
    latency_ms: u64,
    minute: u64,
}

#[derive(Default)]
struct AppMetrics {
    buckets: VecDeque<Bucket>, // ascending by minute; current is the back
    slowest_raw: Vec<RawSample>,
    last_proc: Option<(ProcSample, u64)>, // (sample, unix seconds it was taken)
}

/// In-memory, always-on metrics window shared by the proxy (writer), the sampler task (writer),
/// and the `adj stats` dispatch path (reader). The inner `Mutex` is `std`, held only for short
/// synchronous critical sections — never across an `.await`.
#[derive(Default)]
pub struct Metrics {
    apps: Mutex<HashMap<String, AppMetrics>>,
}

impl Metrics {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a completed request. `now_unix` is wall-clock seconds; production callers pass
    /// `unix_now()`.
    pub fn record_request_at(&self, app: &str, rec: RequestRecord, now_unix: u64) {
        let route = route::templatize(&rec.method, &rec.raw_path);
        let minute = now_unix / 60;
        let mut apps = self.apps.lock().unwrap();
        let am = apps.entry(app.to_string()).or_default();
        evict_old(&mut am.buckets, minute);

        if am.buckets.back().map(|b| b.minute) != Some(minute) {
            am.buckets.push_back(Bucket {
                minute,
                routes: HashMap::new(),
            });
        }
        let bucket = am.buckets.back_mut().unwrap();
        let key = if bucket.routes.contains_key(&route) || bucket.routes.len() < MAX_ROUTES_PER_BUCKET
        {
            route
        } else {
            "other".to_string()
        };
        bucket.routes.entry(key).or_default().record(rec.status, rec.latency_ms);

        am.slowest_raw.push(RawSample {
            method: rec.method,
            path: rec.raw_path,
            status: rec.status,
            latency_ms: rec.latency_ms,
            minute,
        });
        am.slowest_raw.sort_by(|a, b| b.latency_ms.cmp(&a.latency_ms));
        am.slowest_raw.truncate(MAX_RAW);
    }

    pub fn record_request(&self, app: &str, rec: RequestRecord) {
        self.record_request_at(app, rec, unix_now());
    }

    /// Snapshot the window for `app`. `since_secs == 0` covers the whole window; otherwise the
    /// most recent `since_secs`. Returns an empty (but valid) snapshot for an unknown app.
    pub fn snapshot_at(&self, app: &str, since_secs: u64, now_unix: u64) -> StatsDto {
        let window = if since_secs == 0 { WINDOW_SECS } else { since_secs.min(WINDOW_SECS) };
        let now_min = now_unix / 60;
        let cutoff_min = now_min.saturating_sub(window / 60);

        let apps = self.apps.lock().unwrap();
        let Some(am) = apps.get(app) else {
            return StatsDto {
                name: app.to_string(),
                window_secs: window,
                total_requests: 0,
                routes: Vec::new(),
                slowest_raw: Vec::new(),
                process: None,
            };
        };

        let mut merged: HashMap<String, RouteAgg> = HashMap::new();
        for bucket in am.buckets.iter().filter(|b| b.minute >= cutoff_min) {
            for (route, agg) in &bucket.routes {
                merged.entry(route.clone()).or_default().merge(agg);
            }
        }

        let mut routes: Vec<RouteStatDto> = merged
            .into_iter()
            .map(|(route, agg)| RouteStatDto {
                route,
                count: agg.count,
                latency_ms: LatencyDto {
                    p50: agg.hist.percentile(0.50),
                    p95: agg.hist.percentile(0.95),
                    p99: agg.hist.percentile(0.99),
                    max: agg.hist.max(),
                },
                status_2xx: agg.s2xx,
                status_3xx: agg.s3xx,
                status_4xx: agg.s4xx,
                status_5xx: agg.s5xx,
            })
            .collect();
        routes.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.route.cmp(&b.route)));
        let total_requests = routes.iter().map(|r| r.count).sum();

        let slowest_raw = am
            .slowest_raw
            .iter()
            .filter(|s| s.minute >= cutoff_min)
            .map(|s| RawSampleDto {
                method: s.method.clone(),
                path: s.path.clone(),
                status: s.status,
                latency_ms: s.latency_ms,
            })
            .collect();

        let process = am.last_proc.as_ref().and_then(|(sample, taken)| {
            if now_unix.saturating_sub(*taken) <= PROC_FRESH_SECS {
                Some(ProcStatDto {
                    cpu_pct: sample.cpu_pct,
                    rss_bytes: sample.rss_bytes,
                    threads: sample.threads,
                    fds: sample.fds,
                    sampled_at: rfc3339(*taken),
                })
            } else {
                None
            }
        });

        StatsDto {
            name: app.to_string(),
            window_secs: window,
            total_requests,
            routes,
            slowest_raw,
            process,
        }
    }

    pub fn snapshot(&self, app: &str, since_secs: u64) -> StatsDto {
        self.snapshot_at(app, since_secs, unix_now())
    }

    /// Store the latest process sample for `app`. Wired in Task 6.
    pub fn record_sample_at(&self, app: &str, sample: ProcSample, now_unix: u64) {
        let mut apps = self.apps.lock().unwrap();
        let am = apps.entry(app.to_string()).or_default();
        am.last_proc = Some((sample, now_unix));
    }

    pub fn record_sample(&self, app: &str, sample: ProcSample) {
        self.record_sample_at(app, sample, unix_now());
    }
}

fn evict_old(buckets: &mut VecDeque<Bucket>, now_min: u64) {
    let cutoff = now_min.saturating_sub(WINDOW_SECS / 60);
    while buckets.front().is_some_and(|b| b.minute < cutoff) {
        buckets.pop_front();
    }
}

fn unix_now() -> u64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn rfc3339(unix_secs: u64) -> String {
    use time::format_description::well_known::Rfc3339;
    use time::OffsetDateTime;
    OffsetDateTime::from_unix_timestamp(unix_secs as i64)
        .ok()
        .and_then(|t| t.format(&Rfc3339).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(path: &str, status: u16, ms: u64) -> RequestRecord {
        RequestRecord {
            method: "GET".into(),
            raw_path: path.into(),
            status,
            latency_ms: ms,
        }
    }

    #[test]
    fn aggregates_by_templated_route() {
        let m = Metrics::new();
        let t = 1_000_000u64;
        m.record_request_at("site", rec("/users/1", 200, 10), t);
        m.record_request_at("site", rec("/users/2", 200, 20), t);
        m.record_request_at("site", rec("/users/3", 404, 30), t);

        let snap = m.snapshot_at("site", 0, t);
        assert_eq!(snap.total_requests, 3);
        assert_eq!(snap.routes.len(), 1);
        let r = &snap.routes[0];
        assert_eq!(r.route, "GET /users/:id");
        assert_eq!(r.count, 3);
        assert_eq!(r.status_2xx, 2);
        assert_eq!(r.status_4xx, 1);
        assert!(snap.process.is_none());
    }

    #[test]
    fn unknown_app_returns_empty_snapshot() {
        let m = Metrics::new();
        let snap = m.snapshot_at("ghost", 0, 1_000_000);
        assert_eq!(snap.total_requests, 0);
        assert!(snap.routes.is_empty());
    }

    #[test]
    fn since_window_excludes_old_buckets() {
        let m = Metrics::new();
        let base = 1_000_000u64;
        m.record_request_at("site", rec("/old", 200, 5), base);
        // 10 minutes later
        let later = base + 600;
        m.record_request_at("site", rec("/new", 200, 5), later);

        // since = 5 minutes: only the recent request is in range.
        let snap = m.snapshot_at("site", 300, later);
        assert_eq!(snap.total_requests, 1);
        assert_eq!(snap.routes[0].route, "GET /new");
    }

    #[test]
    fn evicts_buckets_older_than_window() {
        let m = Metrics::new();
        let base = 1_000_000u64;
        m.record_request_at("site", rec("/old", 200, 5), base);
        // 31 minutes later — base bucket is past the 30m window and must be evicted on write.
        let later = base + 31 * 60;
        m.record_request_at("site", rec("/new", 200, 5), later);

        let snap = m.snapshot_at("site", 0, later);
        assert_eq!(snap.total_requests, 1);
        assert_eq!(snap.routes[0].route, "GET /new");
    }

    #[test]
    fn slowest_raw_keeps_top_n_by_latency() {
        let m = Metrics::new();
        let t = 1_000_000u64;
        for i in 0..20u64 {
            m.record_request_at("site", rec("/users/9", 200, i), t);
        }
        let snap = m.snapshot_at("site", 0, t);
        assert_eq!(snap.slowest_raw.len(), MAX_RAW);
        assert_eq!(snap.slowest_raw[0].latency_ms, 19);
        assert_eq!(snap.slowest_raw[0].path, "/users/9");
    }
}
```

- [ ] **Step 2: Run test to verify it fails to compile**

Run: `cargo test -p adj metrics::tests`
Expected: FAIL — `self::sampler` and `ProcSample` don't exist yet (added in Task 5). This confirms the module wiring is in place; the compile error names `sampler`.

- [ ] **Step 3: Add a minimal `sampler` stub so the collector compiles**

Create `crates/adj/src/metrics/sampler.rs` with just the type the collector needs (the real impls land in Task 5):

```rust
//! Process-group resource sampling. Full impls land in the next task; this defines the sample
//! type the collector stores.

/// A computed, ready-to-report process sample (CPU already converted to a percentage).
#[derive(Clone, Debug, PartialEq)]
pub struct ProcSample {
    pub cpu_pct: f64,
    pub rss_bytes: u64,
    pub threads: u64,
    pub fds: u64,
}
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p adj metrics::tests`
Expected: PASS (5 collector tests).

- [ ] **Step 5: Commit**

```bash
git add crates/adj/src/metrics/mod.rs crates/adj/src/metrics/sampler.rs
git commit crates/adj/src/metrics/mod.rs crates/adj/src/metrics/sampler.rs -m "Add in-memory metrics collector with rolling window"
```

---

## Task 5: ProcSampler trait + Linux sampler

CI's test matrix is `[ubuntu-latest, macos-14]` (see `.github/workflows/ci.yml`), so the Linux sampler is exercised on the Ubuntu leg and the macOS sampler (Task 5b) on the macOS leg — both are CI-covered, no manual verification gap.

**Files:**
- Modify: `crates/adj/src/metrics/sampler.rs`

- [ ] **Step 1: Write the failing test (implementation included)**

Replace `crates/adj/src/metrics/sampler.rs` with:

```rust
//! Process-group resource sampling behind a platform-agnostic trait. The Linux impl reads
//! `/proc`; the macOS impl (see below) uses `libproc`. The sampler returns *cumulative* CPU time
//! so the caller can derive a percentage from the delta between ticks.

/// A computed, ready-to-report process sample (CPU already converted to a percentage).
#[derive(Clone, Debug, PartialEq)]
pub struct ProcSample {
    pub cpu_pct: f64,
    pub rss_bytes: u64,
    pub threads: u64,
    pub fds: u64,
}

/// A raw reading of a process group at one instant. `cpu_time_ms` is cumulative across the group
/// since process start; the caller turns successive readings into `ProcSample::cpu_pct`.
#[derive(Clone, Debug, PartialEq)]
pub struct RawProc {
    pub cpu_time_ms: u64,
    pub rss_bytes: u64,
    pub threads: u64,
    pub fds: u64,
}

/// Sample the whole process group led by `pgid`. Returns `None` when the group is gone or the
/// platform can't be read.
pub trait ProcSampler: Send {
    fn sample(&mut self, pgid: i32) -> Option<RawProc>;
}

/// The platform default sampler, or `None` on an unsupported platform.
pub fn default_sampler() -> Option<Box<dyn ProcSampler>> {
    #[cfg(target_os = "linux")]
    {
        Some(Box::new(linux::LinuxSampler))
    }
    #[cfg(target_os = "macos")]
    {
        Some(Box::new(macos::MacSampler))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        None
    }
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{ProcSampler, RawProc};
    use std::fs;

    pub struct LinuxSampler;

    impl ProcSampler for LinuxSampler {
        fn sample(&mut self, pgid: i32) -> Option<RawProc> {
            let clk_tck = clk_tck();
            let page_size = page_size();
            let mut acc = RawProc {
                cpu_time_ms: 0,
                rss_bytes: 0,
                threads: 0,
                fds: 0,
            };
            let mut found = false;
            for entry in fs::read_dir("/proc").ok()? {
                let Ok(entry) = entry else { continue };
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let Ok(pid) = name.parse::<i32>() else { continue };
                let Some(stat) = read_stat(pid) else { continue };
                if stat.pgrp != pgid {
                    continue;
                }
                found = true;
                acc.cpu_time_ms += (stat.utime + stat.stime) * 1000 / clk_tck;
                acc.rss_bytes += stat.rss_pages * page_size;
                acc.threads += stat.num_threads.max(0) as u64;
                acc.fds += count_fds(pid);
            }
            found.then_some(acc)
        }
    }

    struct Stat {
        pgrp: i32,
        utime: u64,
        stime: u64,
        num_threads: i64,
        rss_pages: u64,
    }

    /// Parse the numeric fields we need from `/proc/<pid>/stat`. `comm` (field 2) may contain
    /// spaces and parentheses, so we split on the *last* ')': the remaining whitespace-separated
    /// tokens start at field 3 (state). Field N is therefore `tokens[N - 3]`.
    fn read_stat(pid: i32) -> Option<Stat> {
        let raw = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let rparen = raw.rfind(')')?;
        let rest = raw.get(rparen + 1..)?.trim();
        let f: Vec<&str> = rest.split_whitespace().collect();
        // pgrp=5, utime=14, stime=15, num_threads=20, rss=24  ->  index = field - 3
        Some(Stat {
            pgrp: f.get(2)?.parse().ok()?,
            utime: f.get(11)?.parse().ok()?,
            stime: f.get(12)?.parse().ok()?,
            num_threads: f.get(17)?.parse().ok()?,
            rss_pages: f.get(21)?.parse().ok()?,
        })
    }

    fn count_fds(pid: i32) -> u64 {
        fs::read_dir(format!("/proc/{pid}/fd"))
            .map(|d| d.count() as u64)
            .unwrap_or(0)
    }

    fn clk_tck() -> u64 {
        nix::unistd::sysconf(nix::unistd::SysconfVar::CLK_TCK)
            .ok()
            .flatten()
            .map(|v| v as u64)
            .filter(|v| *v > 0)
            .unwrap_or(100)
    }

    fn page_size() -> u64 {
        nix::unistd::sysconf(nix::unistd::SysconfVar::PAGE_SIZE)
            .ok()
            .flatten()
            .map(|v| v as u64)
            .filter(|v| *v > 0)
            .unwrap_or(4096)
    }
}

#[cfg(target_os = "linux")]
#[cfg(test)]
mod linux_tests {
    use super::*;

    #[test]
    fn samples_own_process_group() {
        // The test process is in its own group; sampling it must see this process: at least one
        // thread, non-zero RSS, and at least the fds for stdout/stderr.
        let pgid = nix::unistd::getpgrp().as_raw();
        let mut sampler = linux::LinuxSampler;
        let raw = sampler.sample(pgid).expect("own group must be sampleable");
        assert!(raw.rss_bytes > 0, "rss should be non-zero: {raw:?}");
        assert!(raw.threads >= 1, "at least one thread: {raw:?}");
        assert!(raw.fds >= 1, "at least one fd: {raw:?}");
    }

    #[test]
    fn absent_group_returns_none() {
        let mut sampler = linux::LinuxSampler;
        // pgid 0 means "the caller's group" to many syscalls, but as a literal /proc pgrp match
        // no process reports pgrp 0, so this reads as an absent group.
        assert!(sampler.sample(0).is_none());
    }
}
```

- [ ] **Step 2: Run the Linux sampler tests**

Run: `cargo test -p adj metrics::sampler`
Expected: PASS (2 tests on Linux). The collector tests from Task 4 still pass (`ProcSample` is unchanged).

- [ ] **Step 3: Commit**

```bash
git add crates/adj/src/metrics/sampler.rs
git commit crates/adj/src/metrics/sampler.rs -m "Add ProcSampler trait and Linux /proc sampler"
```

---

## Task 5b: macOS sampler

**Files:**
- Modify: `crates/adj/Cargo.toml`
- Modify: `crates/adj/src/metrics/sampler.rs`

> The CI matrix includes `macos-14`, so this module is compiled and run on macOS in CI — the `libproc` calls and the integration test's process assertions are verified automatically. Write the two flagged lines correctly; the macOS CI leg catches mistakes. The trait contract and the collector's use of it are already covered by Tasks 4–5 on both legs.

- [ ] **Step 1: Add the macOS dependency**

In `crates/adj/Cargo.toml`, under the existing `[target.'cfg(target_os = "macos")'.dependencies]` block, add:

```toml
libproc = "0.14"
```

- [ ] **Step 2: Add the macOS sampler module**

In `crates/adj/src/metrics/sampler.rs`, append:

```rust
#[cfg(target_os = "macos")]
mod macos {
    use super::{ProcSampler, RawProc};
    use libproc::libproc::bsd_info::BSDInfo;
    use libproc::libproc::pid_rusage::{pidrusage, RUsageInfoV2};
    use libproc::libproc::proc_pid::{listpids, pidinfo, ProcType};

    pub struct MacSampler;

    impl ProcSampler for MacSampler {
        fn sample(&mut self, pgid: i32) -> Option<RawProc> {
            let mut acc = RawProc {
                cpu_time_ms: 0,
                rss_bytes: 0,
                threads: 0,
                fds: 0,
            };
            let mut found = false;
            // Enumerate every pid, keep those whose BSD info reports our target process group.
            let pids = listpids(ProcType::ProcAllPIDS).ok()?;
            for pid in pids {
                let pid = pid as i32;
                let Ok(info) = pidinfo::<BSDInfo>(pid, 0) else { continue };
                if info.pbi_pgid as i32 != pgid {
                    continue;
                }
                found = true;
                if let Ok(ru) = pidrusage::<RUsageInfoV2>(pid) {
                    // ri_user_time / ri_system_time are nanoseconds; RSS is bytes.
                    acc.cpu_time_ms += (ru.ri_user_time + ru.ri_system_time) / 1_000_000;
                    acc.rss_bytes += ru.ri_resident_size;
                }
                acc.threads += info.pbi_nfiles_dummy_unused_keep_zero(); // see Step 4 note
            }
            found.then_some(acc)
        }
    }
}
```

> **Step 4 note / known confirm-on-Mac points:** the exact `BSDInfo` field for thread count and the call for fd count vary across `libproc` versions. On macOS, confirm and finish two lines: (a) thread count — `BSDInfo` does not expose threads directly; use `pidinfo::<libproc::libproc::task_info::TaskInfo>(pid, 0)?.pti_threadnum`; (b) fd count — use `libproc::libproc::proc_pid::listpidinfo::<libproc::libproc::file_info::ListFDs>(pid, max)` and take its length. Replace the `acc.threads += ...` line accordingly and add the fd accumulation. These are real, bounded edits with concrete APIs — make them while the compiler and a Mac are in front of you.

- [ ] **Step 3: Build on the dev host (confirms macOS code is gated out when not on macOS)**

Run: `cargo build -p adj`
Expected: PASS. On a non-macOS host the `macos` module is `#[cfg]`-gated out, so this proves the gating compiles; on a Mac it compiles the real module.

- [ ] **Step 4: Confirm on macOS via CI (or a local Mac)**

The `macos-14` CI leg compiles this module and runs Task 9's integration test, whose process-section assertions exercise the sampler. After finishing the two flagged lines, confirm the macOS CI leg is green (or, on a local Mac: `cargo test -p adj --test stats` plus `cargo run -- daemon` → boot an app → `adj stats <app>` shows non-zero RSS).

- [ ] **Step 5: Commit**

```bash
git add crates/adj/Cargo.toml crates/adj/src/metrics/sampler.rs
git commit crates/adj/Cargo.toml crates/adj/src/metrics/sampler.rs -m "Add macOS libproc process sampler"
```

---

## Task 6: Supervisor running_pids + sampler task

Wires the sampler into the daemon: a 2s loop reads running apps' pids, samples each group, derives CPU% from the delta, and records the sample.

**Files:**
- Modify: `crates/adj/src/supervisor.rs`
- Modify: `crates/adj/src/daemon.rs`

- [ ] **Step 1: Write the failing test for `running_pids`**

In `crates/adj/src/supervisor.rs`, inside `mod tests`, add:

```rust
    #[tokio::test]
    async fn running_pids_lists_only_running_apps() {
        let sup = Supervisor::new();
        sup.insert_fake_running("up", std::time::Instant::now()).await;
        let pids = sup.running_pids().await;
        assert_eq!(pids, vec![("up".to_string(), 1u32)]);
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p adj running_pids_lists_only_running_apps`
Expected: FAIL — no method `running_pids`.

- [ ] **Step 3: Implement `running_pids`**

In `crates/adj/src/supervisor.rs`, add to `impl Supervisor` (after `idle_candidates`):

```rust
    /// Snapshot every running app's `(name, pid)`. The pid is the process-group leader (apps are
    /// spawned with `process_group(0)`, so pgid == pid). Used by the metrics sampler.
    pub async fn running_pids(&self) -> Vec<(String, u32)> {
        let inner = self.inner.lock().await;
        inner
            .apps
            .iter()
            .filter_map(|(name, rt)| match rt.state {
                AppState::Running { pid, .. } => Some((name.clone(), pid)),
                _ => None,
            })
            .collect()
    }
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p adj running_pids_lists_only_running_apps`
Expected: PASS.

- [ ] **Step 5: Add the sampler task to the daemon**

In `crates/adj/src/daemon.rs`, add imports near the top:

```rust
use crate::metrics::sampler::{default_sampler, ProcSample};
use crate::metrics::Metrics;
```

Add the interval constant near `IDLE_SCAN_INTERVAL`:

```rust
/// How often the metrics sampler reads each running app's process group. Matches the spec's 2s
/// cadence; CPU% is derived from the delta between consecutive ticks.
const METRICS_SAMPLE_INTERVAL: Duration = Duration::from_secs(2);
```

Add the sampler loop as a free function at the end of `daemon.rs`:

```rust
/// Periodic process sampler: every tick, read each running app's process-group resource usage
/// and record it. CPU% is `(delta cpu_time) / (delta wall_time)`, so the loop keeps each app's
/// previous cumulative CPU time and the tick timestamp. Apps that stop simply drop out of
/// `running_pids`, and their stale sample ages out of the snapshot (see `PROC_FRESH_SECS`).
async fn metrics_sampler(supervisor: Arc<Supervisor>, metrics: Arc<Metrics>) {
    let Some(mut sampler) = default_sampler() else {
        tracing::info!("process sampling unsupported on this platform; HTTP metrics only");
        return;
    };
    // name -> (prev cumulative cpu_ms, prev wall_ms)
    let mut prev: std::collections::HashMap<String, (u64, u128)> = std::collections::HashMap::new();
    loop {
        tokio::time::sleep(METRICS_SAMPLE_INTERVAL).await;
        let now_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0);
        let running = supervisor.running_pids().await;
        let live: std::collections::HashSet<String> =
            running.iter().map(|(n, _)| n.clone()).collect();
        for (name, pid) in running {
            // pgid == pid because apps are spawned as their own process-group leader.
            let Some(raw) = sampler.sample(pid as i32) else {
                continue;
            };
            let cpu_pct = match prev.get(&name) {
                Some((prev_cpu, prev_wall)) if now_ms > *prev_wall => {
                    let d_cpu = raw.cpu_time_ms.saturating_sub(*prev_cpu) as f64;
                    let d_wall = (now_ms - *prev_wall) as f64;
                    (d_cpu / d_wall) * 100.0
                }
                _ => 0.0,
            };
            prev.insert(name.clone(), (raw.cpu_time_ms, now_ms));
            metrics.record_sample(
                &name,
                ProcSample {
                    cpu_pct,
                    rss_bytes: raw.rss_bytes,
                    threads: raw.threads,
                    fds: raw.fds,
                },
            );
        }
        // Forget apps that are no longer running so their prev-CPU baseline doesn't leak.
        prev.retain(|name, _| live.contains(name));
    }
}
```

- [ ] **Step 6: Construct metrics and spawn the task in `run()`**

In `crates/adj/src/daemon.rs`, in `run()`, right after `let supervisor = Arc::new(Supervisor::new());`:

```rust
    let metrics = Arc::new(Metrics::new());
```

After the idle-scanner spawn block, add:

```rust
    let sampler_supervisor = supervisor.clone();
    let sampler_metrics = metrics.clone();
    tokio::spawn(async move {
        metrics_sampler(sampler_supervisor, sampler_metrics).await;
    });
```

(The proxy and dispatch wiring for `metrics` is Task 7; for now `metrics` is used only by the sampler. Add `#[allow(unused)]`-free usage by completing Task 7 in the same session, or temporarily prefix with `let _ = &metrics;` — Task 7 removes the need.)

- [ ] **Step 7: Verify it builds and tests pass**

Run: `cargo test -p adj supervisor`
Expected: PASS. `cargo build -p adj` succeeds.

- [ ] **Step 8: Commit**

```bash
git add crates/adj/src/supervisor.rs crates/adj/src/daemon.rs
git commit crates/adj/src/supervisor.rs crates/adj/src/daemon.rs -m "Add process sampler task and supervisor running_pids"
```

---

## Task 7: Thread metrics through the proxy + Stats dispatch

**Files:**
- Modify: `crates/adj/src/proxy.rs`
- Modify: `crates/adj/src/daemon.rs`

- [ ] **Step 1: Thread `Arc<Metrics>` into the proxy serve loop**

In `crates/adj/src/proxy.rs`:

Add the import:

```rust
use crate::metrics::{Metrics, RequestRecord};
```

Change `run` and `run_https` to take metrics. Update `run`'s signature:

```rust
pub async fn run(supervisor: Arc<Supervisor>, metrics: Arc<Metrics>) -> Result<()> {
```

In `run`'s accept loop, clone and pass it:

```rust
        let sup = supervisor.clone();
        let gate = gate.clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            serve_plain(stream, sup, gate, metrics, peer.ip(), "http").await;
        });
```

Update `run_https`'s signature:

```rust
pub async fn run_https(
    supervisor: Arc<Supervisor>,
    metrics: Arc<Metrics>,
    resolver: Arc<tls::LeafResolver>,
) -> Result<()> {
```

In `run_https`'s accept loop, clone and pass it the same way:

```rust
            let sup = supervisor.clone();
            let gate = gate.clone();
            let metrics = metrics.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let tls_stream = match acceptor.accept(stream).await {
                    Ok(s) => s,
                    Err(err) => {
                        tracing::debug!("tls handshake failed: {err}");
                        return;
                    }
                };
                serve_plain(tls_stream, sup, gate, metrics, peer.ip(), "https").await;
            });
```

- [ ] **Step 2: Pass metrics through `serve_plain` into `handle`**

Update `serve_plain`'s signature and the `service_fn` closure:

```rust
async fn serve_plain<S>(
    stream: S,
    sup: Arc<Supervisor>,
    gate: Arc<BootGate>,
    metrics: Arc<Metrics>,
    client_ip: IpAddr,
    proto: &'static str,
) where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let io = TokioIo::new(stream);
    let service = service_fn(move |req: Request<Incoming>| {
        let sup = sup.clone();
        let gate = gate.clone();
        let metrics = metrics.clone();
        async move { Ok::<_, Infallible>(handle(req, sup, gate, metrics, client_ip, proto).await) }
    });
    if let Err(err) = server_http1::Builder::new()
        .serve_connection(io, service)
        .with_upgrades()
        .await
    {
        tracing::debug!("proxy connection ended: {err}");
    }
}
```

- [ ] **Step 3: Instrument `handle` to record the request**

Update `handle`'s signature:

```rust
async fn handle(
    req: Request<Incoming>,
    supervisor: Arc<Supervisor>,
    gate: Arc<BootGate>,
    metrics: Arc<Metrics>,
    client_ip: IpAddr,
    proto: &'static str,
) -> Response<BoxBody<Bytes, hyper::Error>> {
```

Replace the final `match forward(...)` block (the one after `supervisor.touch_idle(&name).await;`) with a timed, recorded version:

```rust
    // Capture method + path before `forward` consumes the request, and time to the response
    // head (TTFB) — not body completion, so long-lived streams don't skew latency. Bytes are
    // best-effort from Content-Length.
    let method = req.method().as_str().to_string();
    let raw_path = req.uri().path().to_string();
    let started = std::time::Instant::now();

    let result = forward(req, upstream_port, &raw_host, client_ip, proto).await;
    let latency_ms = started.elapsed().as_millis() as u64;
    match result {
        Ok(resp) => {
            metrics.record_request(
                &name,
                RequestRecord {
                    method,
                    raw_path,
                    status: resp.status().as_u16(),
                    latency_ms,
                },
            );
            resp
        }
        Err(err) => {
            tracing::warn!("proxy forward to `{name}:{upstream_port}` failed: {err}");
            metrics.record_request(
                &name,
                RequestRecord {
                    method,
                    raw_path,
                    status: 502,
                    latency_ms,
                },
            );
            error_response(
                StatusCode::BAD_GATEWAY,
                &format!("upstream `{name}` error: {err}"),
            )
        }
    }
```

- [ ] **Step 4: Update the proxy's internal unit tests for the new `handle`/`serve_plain` arity**

The existing proxy unit tests (`strips_optional_port_suffix`, `boot_gate_*`, `forwarded_*`, `extracts_name_*`) do not call `handle`/`serve_plain` directly, so they need no change. Confirm by running them in Step 7.

- [ ] **Step 5: Pass metrics from the daemon into the proxy tasks + dispatch**

In `crates/adj/src/daemon.rs`:

The proxy spawn becomes:

```rust
    let proxy_supervisor = supervisor.clone();
    let proxy_metrics = metrics.clone();
    tokio::spawn(async move {
        if let Err(err) = proxy::run(proxy_supervisor, proxy_metrics).await {
            tracing::error!("proxy listener exited: {err}");
        }
    });
```

The HTTPS spawn passes metrics too:

```rust
        let cell = resolver.clone();
        let https_supervisor = supervisor.clone();
        let https_metrics = metrics.clone();
        tokio::spawn(async move {
            match tls::LeafResolver::new() {
                Ok(r) => {
                    let _ = cell.set(r.clone());
                    if let Err(err) = proxy::run_https(https_supervisor, https_metrics, r).await {
                        tracing::error!("https listener exited: {err}");
                    }
                }
                Err(err) => tracing::error!("https listener disabled: {err}"),
            }
        });
```

- [ ] **Step 6: Thread metrics into `handle_client` → `dispatch` and handle `Stats`**

In `crates/adj/src/daemon.rs`, the accept loop passes metrics to `handle_client`:

```rust
        let sup = supervisor.clone();
        let reg_lock = registry_lock.clone();
        let resolver = resolver.clone();
        let metrics = metrics.clone();
        tokio::spawn(async move {
            if let Err(err) = handle_client(stream, sup, reg_lock, resolver, metrics).await {
                tracing::warn!("client handler error: {err}");
            }
        });
```

Update `handle_client`'s signature and its `dispatch` call:

```rust
async fn handle_client(
    stream: UnixStream,
    supervisor: Arc<Supervisor>,
    registry_lock: Arc<Mutex<()>>,
    resolver: Arc<std::sync::OnceLock<Arc<tls::LeafResolver>>>,
    metrics: Arc<Metrics>,
) -> Result<()> {
```

```rust
    let response = match dispatch(req, supervisor, registry_lock, resolver, metrics).await {
```

Update `dispatch`'s signature, add the `Stats` arm. (Task 1 left a temporary `Request::Stats { .. } => Err(anyhow!("stats: not yet implemented"))` stub arm here so the crate kept compiling — replace the whole `match req` block below, which removes the stub.)

```rust
async fn dispatch(
    req: Request,
    supervisor: Arc<Supervisor>,
    registry_lock: Arc<Mutex<()>>,
    resolver: Arc<std::sync::OnceLock<Arc<tls::LeafResolver>>>,
    metrics: Arc<Metrics>,
) -> Result<Response> {
    match req {
        Request::Ping => Ok(Response::Ok),
        Request::Add { path, label } => add(path, label, registry_lock, resolver).await,
        Request::List => list(supervisor).await,
        Request::Up { name } => up(name, supervisor).await,
        Request::Down { name } => down(name, supervisor).await,
        Request::Restart { name } => restart(name, supervisor).await,
        Request::Status { name } => status(name, supervisor).await,
        Request::LogPath { name } => log_path(name).await,
        Request::WaitReady { name, timeout_secs } => {
            wait_ready(name, timeout_secs, supervisor).await
        }
        Request::Remove { name } => remove(name, supervisor, registry_lock, resolver).await,
        Request::Prune => prune(supervisor, registry_lock, resolver).await,
        Request::Stats { name, since_secs } => stats(name, since_secs, metrics).await,
    }
}
```

Add the `stats` handler near `status`:

```rust
async fn stats(name: String, since_secs: u64, metrics: Arc<Metrics>) -> Result<Response> {
    // Require registration so an unknown name is an error, consistent with `status`. An app with
    // no traffic yet returns a valid empty snapshot rather than an error.
    let reg = Registry::load()?;
    if reg.get(&name).is_none() {
        return Err(anyhow!("no app named `{}`", name));
    }
    let stats = metrics.snapshot(&name, since_secs);
    Ok(Response::Stats { stats })
}
```

If Task 6 added a temporary `let _ = &metrics;` in `run()`, remove it now — metrics is genuinely used.

- [ ] **Step 7: Verify the whole crate builds and existing tests pass**

Run: `cargo test -p adj`
Expected: PASS — all existing unit + integration tests, plus the metrics unit tests. `cargo build` clean.

- [ ] **Step 8: Commit**

```bash
git add crates/adj/src/proxy.rs crates/adj/src/daemon.rs
git commit crates/adj/src/proxy.rs crates/adj/src/daemon.rs -m "Record proxied requests into the metrics collector and serve Stats"
```

---

## Task 8: `adj stats` CLI

**Files:**
- Modify: `crates/adj/src/client.rs`
- Modify: `crates/adj/src/main.rs`

- [ ] **Step 1: Write the failing test for `parse_since`**

In `crates/adj/src/client.rs`, add at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_since_supports_units_and_bare_seconds() {
        assert_eq!(parse_since("0").unwrap(), 0);
        assert_eq!(parse_since("30s").unwrap(), 30);
        assert_eq!(parse_since("5m").unwrap(), 300);
        assert_eq!(parse_since("1h").unwrap(), 3600);
        assert_eq!(parse_since("90").unwrap(), 90);
        assert!(parse_since("nonsense").is_err());
        assert!(parse_since("5d").is_err());
    }
}
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p adj client::tests::parse_since_supports_units_and_bare_seconds`
Expected: FAIL — `parse_since` undefined.

- [ ] **Step 3: Implement `parse_since` and `stats`**

In `crates/adj/src/client.rs`, update the protocol import to include the stats types:

```rust
use adj_protocol::{ListEntryDto, LogRecord, Request, Response, StatsDto, StatusDto};
```

Add `parse_since`:

```rust
/// Parse a `--since` value into seconds. Accepts bare seconds (`90`) or a single unit suffix:
/// `s`, `m`, `h`. `0` means "the full window".
pub fn parse_since(s: &str) -> Result<u64> {
    let s = s.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    let (num, mult) = match s.chars().last() {
        Some('s') => (&s[..s.len() - 1], 1),
        Some('m') => (&s[..s.len() - 1], 60),
        Some('h') => (&s[..s.len() - 1], 3600),
        _ => return Err(anyhow!("invalid --since `{s}` (use e.g. 30s, 5m, 1h)")),
    };
    let n: u64 = num
        .parse()
        .map_err(|_| anyhow!("invalid --since `{s}` (use e.g. 30s, 5m, 1h)"))?;
    Ok(n * mult)
}
```

Add the `stats` client function:

```rust
pub async fn stats(name: String, json: bool, since: String) -> Result<()> {
    let since_secs = parse_since(&since)?;
    let resp = into_error(
        request(Request::Stats {
            name: name.clone(),
            since_secs,
        })
        .await?,
    )?;
    let Response::Stats { stats } = resp else {
        return Err(anyhow!("unexpected response from daemon"));
    };
    if json {
        println!("{}", serde_json::to_string(&stats)?);
        return Ok(());
    }
    render_stats_table(&stats);
    Ok(())
}

fn render_stats_table(s: &StatsDto) {
    let mins = s.window_secs / 60;
    println!("{} — last {mins}m, {} requests", s.name, s.total_requests);
    if let Some(p) = &s.process {
        println!(
            "  process: cpu {:.0}%  rss {}  threads {}  fds {}",
            p.cpu_pct,
            human_bytes(p.rss_bytes),
            p.threads,
            p.fds
        );
    }
    if s.routes.is_empty() {
        println!("  (no requests in window)");
        return;
    }
    println!("  {:<34} {:>6} {:>7} {:>7} {:>6}", "route", "count", "p50", "p95", "err%");
    for r in &s.routes {
        let errs = r.status_4xx + r.status_5xx;
        let err_pct = if r.count > 0 { errs as f64 * 100.0 / r.count as f64 } else { 0.0 };
        println!(
            "  {:<34} {:>6} {:>5}ms {:>5}ms {:>5.0}%",
            truncate(&r.route, 34),
            r.count,
            r.latency_ms.p50,
            r.latency_ms.p95,
            err_pct
        );
    }
    if !s.slowest_raw.is_empty() {
        println!("  slowest:");
        for raw in &s.slowest_raw {
            println!(
                "    {:>5}ms  {} {} ({})",
                raw.latency_ms, raw.method, raw.path, raw.status
            );
        }
    }
}

fn human_bytes(n: u64) -> String {
    const UNITS: &[&str] = &["B", "KB", "MB", "GB"];
    let mut v = n as f64;
    let mut u = 0;
    while v >= 1024.0 && u < UNITS.len() - 1 {
        v /= 1024.0;
        u += 1;
    }
    format!("{v:.0}{}", UNITS[u])
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max - 1])
    }
}
```

- [ ] **Step 4: Run it to verify it passes**

Run: `cargo test -p adj client::tests::parse_since_supports_units_and_bare_seconds`
Expected: PASS.

- [ ] **Step 5: Wire the `Cmd::Stats` subcommand**

In `crates/adj/src/main.rs`, add to `enum Cmd` (after `Status`):

```rust
    /// Report runtime metrics for an app: per-route latency/throughput/errors and a process
    /// resource summary, over a rolling in-memory window.
    Stats {
        name: String,
        /// Emit the stable `StatsDto` JSON object instead of the human table.
        #[arg(long)]
        json: bool,
        /// Narrow the window, e.g. `30s`, `5m`, `1h`. Defaults to the full window.
        #[arg(long, default_value = "0")]
        since: String,
    },
```

Add to the `match cli.cmd` dispatch (after `Cmd::Status`):

```rust
        Cmd::Stats { name, json, since } => client::stats(name, json, since).await,
```

- [ ] **Step 6: Verify the crate builds and the new tests pass**

Run: `cargo test -p adj`
Expected: PASS. `cargo run -- stats --help` shows the new subcommand.

- [ ] **Step 7: Commit**

```bash
git add crates/adj/src/client.rs crates/adj/src/main.rs
git commit crates/adj/src/client.rs crates/adj/src/main.rs -m "Add adj stats subcommand with table and --json output"
```

---

## Task 9: End-to-end integration test

**Files:**
- Create: `crates/adj/tests/stats.rs`

This mirrors the `tests/proxy.rs` harness (sandbox daemon + raw HTTP through the proxy) and the `tests/json_output.rs` assertions.

- [ ] **Step 1: Write the integration test**

Create `crates/adj/tests/stats.rs`:

```rust
// End-to-end: drive requests through the proxy at a real app, then assert `adj stats --json`
// reports per-route metrics and (on Linux) a process section.

use std::io::{Read, Write};
use std::net::TcpStream;
use std::path::Path;
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::Value;
use tempfile::TempDir;
use tokio::process::{Child, Command};

fn adj_bin() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_BIN_EXE_adj"))
}

fn read_port_file(path: &Path) -> Option<u16> {
    std::fs::read_to_string(path).ok()?.trim().parse().ok()
}

struct Sandbox {
    _home: TempDir,
    home_path: std::path::PathBuf,
    proxy_port: u16,
    daemon: Option<Child>,
}

impl Sandbox {
    fn new() -> Self {
        let home = TempDir::new().expect("tempdir");
        let home_path = home.path().to_path_buf();
        Self { _home: home, home_path, proxy_port: 0, daemon: None }
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(adj_bin());
        c.env("ADJACENT_HOME", &self.home_path);
        c.env("ADJACENT_PROXY_PORT", self.proxy_port.to_string());
        c.env("RUST_LOG", "warn");
        c.env_remove("PORT");
        c.env_remove("BIND_PORT");
        c
    }

    async fn start_daemon(&mut self) {
        let mut c = self.cmd();
        c.arg("daemon").stdout(Stdio::null()).stderr(Stdio::null());
        self.daemon = Some(c.spawn().expect("spawn daemon"));

        let deadline = Instant::now() + Duration::from_secs(5);
        let sock = self.home_path.join("sock");
        let port_file = self.home_path.join("proxy.port");
        loop {
            let sock_ready = sock.exists();
            if self.proxy_port == 0 {
                if let Some(p) = read_port_file(&port_file) {
                    self.proxy_port = p;
                }
            }
            if sock_ready && self.proxy_port != 0 {
                return;
            }
            if Instant::now() >= deadline {
                panic!("daemon did not come up within 5s");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    async fn stop_daemon(&mut self) {
        if let Some(mut child) = self.daemon.take() {
            let _ = child.start_kill();
            let _ = child.wait().await;
        }
    }
}

async fn write_echo_server(dir: &Path, name: &str) {
    // A tiny HTTP server that 200s every request. Mirrors tests/proxy.rs: python3 stdlib, so no
    // node/npm dependency — /usr/bin/python3 is present on both the ubuntu-latest and macos-14
    // runners.
    let py = r#"import os
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
class H(BaseHTTPRequestHandler):
    def do_GET(self):
        body = b"ok"
        self.send_response(200)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *a, **kw):
        pass
ThreadingHTTPServer(("127.0.0.1", int(os.environ["PORT"])), H).serve_forever()
"#;
    let script = dir.join("server.py");
    tokio::fs::write(&script, py).await.expect("write server.py");
    let body = format!(
        "name = \"{name}\"\ncmd = \"exec /usr/bin/python3 {}\"\n",
        script.display()
    );
    tokio::fs::write(dir.join("adjacent.toml"), body).await.expect("write toml");
}

fn http_get(proxy_port: u16, host: &str, path: &str) -> Result<u16, String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", proxy_port)).map_err(|e| format!("connect: {e}"))?;
    let req = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream.write_all(req.as_bytes()).map_err(|e| format!("write: {e}"))?;
    let mut buf = String::new();
    stream.read_to_string(&mut buf).map_err(|e| format!("read: {e}"))?;
    let status = buf
        .lines()
        .next()
        .and_then(|l| l.split_whitespace().nth(1))
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("no status line in: {buf}"))?;
    Ok(status)
}

#[tokio::test]
async fn stats_json_reports_routes_and_process() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let app_dir = TempDir::new().expect("app dir");
    write_echo_server(app_dir.path(), "st-app").await;
    let _ = sandbox.cmd().arg("add").arg(app_dir.path()).output().await.expect("add");

    // First request lazy-boots the app; drive a handful across two templated routes.
    let proxy_port = sandbox.proxy_port;
    for path in ["/users/1", "/users/2", "/users/3", "/health"] {
        let status =
            tokio::task::spawn_blocking(move || http_get(proxy_port, "st-app.adj.ac", path))
                .await
                .unwrap()
                .expect("http_get");
        assert_eq!(status, 200, "request to {path} should 200");
    }

    // Give the 2s sampler at least one tick so the process section is populated.
    tokio::time::sleep(Duration::from_secs(3)).await;

    let out = sandbox
        .cmd()
        .arg("stats")
        .arg("st-app")
        .arg("--json")
        .output()
        .await
        .expect("stats");
    assert!(out.status.success(), "stats --json failed: {:?}", out);
    let v: Value = serde_json::from_slice(&out.stdout).expect("parse stats json");

    assert_eq!(v["name"], "st-app");
    assert_eq!(v["total_requests"], 4);
    let routes = v["routes"].as_array().expect("routes array");
    let route_names: Vec<&str> = routes.iter().map(|r| r["route"].as_str().unwrap()).collect();
    assert!(route_names.contains(&"GET /users/:id"), "templated route missing: {route_names:?}");
    assert!(route_names.contains(&"GET /health"), "health route missing: {route_names:?}");
    let users = routes.iter().find(|r| r["route"] == "GET /users/:id").unwrap();
    assert_eq!(users["count"], 3);
    assert!(users["latency_ms"]["p95"].is_u64());

    // The process section is present on platforms with a sampler (Linux CI included), absent
    // otherwise — assert the running app surfaced one where supported.
    if cfg!(any(target_os = "linux", target_os = "macos")) {
        let proc = v.get("process").expect("process present on supported platform");
        assert!(proc["rss_bytes"].as_u64().unwrap() > 0, "rss should be non-zero");
        assert!(proc["threads"].as_u64().unwrap() >= 1);
    }

    let _ = sandbox.cmd().arg("down").arg("st-app").output().await.expect("down");
    sandbox.stop_daemon().await;
}

#[tokio::test]
async fn stats_json_unknown_app_errors() {
    let mut sandbox = Sandbox::new();
    sandbox.start_daemon().await;

    let out = sandbox
        .cmd()
        .arg("stats")
        .arg("nope")
        .arg("--json")
        .output()
        .await
        .expect("stats");
    assert!(!out.status.success(), "unknown app must be an error");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stderr.contains("no app named"), "got: {stderr}");

    sandbox.stop_daemon().await;
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p adj --test stats`
Expected: PASS (2 tests). Uses `/usr/bin/python3` like the other integration tests — present on both CI runners and on macOS by default. On the `macos-14` leg, the `process` assertions also exercise the `libproc` sampler.

- [ ] **Step 3: Commit**

```bash
git add crates/adj/tests/stats.rs
git commit crates/adj/tests/stats.rs -m "Add end-to-end test for adj stats"
```

---

## Task 10: Document the JSON contract

**Files:**
- Modify: `crates/adj/JSON.md`

- [ ] **Step 1: Add the `adj stats` schema section**

In `crates/adj/JSON.md`, before the `## Versioning` section, add:

```markdown
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

Route latency values are histogram bucket upper bounds — honest over-estimates, never under-
reported. Path segments that look like IDs (digits, UUIDs, long hashes) collapse to `:id` so
route cardinality stays bounded; the original paths survive in `slowest_raw`. `process.cpu_pct`
is whole-process-group CPU and is not attributable to any single route.
```

Also update the line near the top that lists write commands without `--json` — no change needed there, but verify the intro paragraph still reads correctly with `stats` added as a read command.

- [ ] **Step 2: Verify formatting**

Run: `cargo test -p adj --test json_output`
Expected: PASS — existing JSON tests are unaffected; this step just confirms nothing regressed.

- [ ] **Step 3: Commit**

```bash
git add crates/adj/JSON.md
git commit crates/adj/JSON.md -m "Document the adj stats --json schema"
```

---

## Final verification

- [ ] **Run the full suite + lints**

Run:
```bash
cargo test
cargo fmt --check
cargo clippy --all-targets -- -D warnings
```
Expected: all green. (CI runs the same fmt + clippy job.)

- [ ] **Manual smoke (optional, macOS for the process path)**

```bash
cargo run -- daemon &        # in one shell
# register + hit an app, then:
cargo run -- stats <app>
cargo run -- stats <app> --json --since 5m
```
Expected: per-route table with latency + a process line showing CPU/RSS.

---

## Self-Review

**Spec coverage:**
- HTTP metrics (per-route latency/status/bytes) → Tasks 4, 7. Bytes: best-effort via Content-Length, noted in Task 7 (a documented v1 limitation; latency + status + counts are exact).
- Process metrics (CPU/RSS/threads/fds) → Tasks 5, 5b, 6.
- Always-on rolling 30m window → Task 4 (`WINDOW_SECS`, eviction).
- Auto-templated routes + raw outliers → Tasks 2, 4.
- `ProcSampler` trait split (macOS/Linux) → Tasks 5, 5b.
- `adj stats` + `--json` + `--since` → Task 8.
- Correlation-not-causation framing → carried in DTO docs (Task 1) and JSON.md (Task 10).
- Error handling: unknown app non-error empty snapshot (Task 4 `snapshot_at`), registered-but-no-traffic (Task 7 `stats`), sampler skips dead pids (Task 5), unsupported platform (Task 5 `default_sampler` → None, Task 6 logs and returns), stale sample omitted (Task 4 `PROC_FRESH_SECS`).
- Tests: route/hist/collector/Linux-sampler unit tests; end-to-end `tests/stats.rs`; JSON.md contract.

**Deviations from spec (intentional, YAGNI):**
- Process "recent timeline / sparkline" from the design's illustrative output is dropped from v1 — the DTO reports the latest sample summary only. Noted here; not in the spec's hard requirements.
- Per-request byte accounting is best-effort (Content-Length), not exact, to avoid wrapping the streaming response body. Acceptable for v1; revisit if exact egress bytes are needed.

**Placeholder scan:** none — every code step is complete except the two explicitly-flagged macOS FFI lines in Task 5b Step 2, which carry concrete APIs and are verified by the `macos-14` CI leg (the only platform where they compile and run).

**Type consistency:** `Metrics`, `RequestRecord`, `ProcSample`, `RawProc`, `ProcSampler`, `StatsDto`/`RouteStatDto`/`LatencyDto`/`RawSampleDto`/`ProcStatDto`, `templatize`, `Histogram`, `running_pids`, `record_request`/`record_sample`/`snapshot` are used identically across tasks.
