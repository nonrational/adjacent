pub mod hist;
pub mod route;
pub mod sampler;

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

// Per-route aggregate. The request count is the histogram's count (every record bumps the
// histogram), so we don't track a redundant counter — `hist.count()` is the single source.
#[derive(Default)]
struct RouteAgg {
    hist: Histogram,
    s2xx: u64,
    s3xx: u64,
    s4xx: u64,
    s5xx: u64,
}

impl RouteAgg {
    fn record(&mut self, status: u16, latency_ms: u64) {
        self.hist.record(latency_ms);
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
        let key =
            if bucket.routes.contains_key(&route) || bucket.routes.len() < MAX_ROUTES_PER_BUCKET {
                route
            } else {
                "other".to_string()
            };
        bucket
            .routes
            .entry(key)
            .or_default()
            .record(rec.status, rec.latency_ms);

        am.slowest_raw.push(RawSample {
            method: rec.method,
            path: rec.raw_path,
            status: rec.status,
            latency_ms: rec.latency_ms,
            minute,
        });
        am.slowest_raw
            .sort_by(|a, b| b.latency_ms.cmp(&a.latency_ms));
        am.slowest_raw.truncate(MAX_RAW);
    }

    pub fn record_request(&self, app: &str, rec: RequestRecord) {
        self.record_request_at(app, rec, unix_now());
    }

    /// Snapshot the window for `app`. `since_secs == 0` covers the whole window; otherwise the
    /// most recent `since_secs`. Returns an empty (but valid) snapshot for an unknown app.
    pub fn snapshot_at(&self, app: &str, since_secs: u64, now_unix: u64) -> StatsDto {
        let window = if since_secs == 0 {
            WINDOW_SECS
        } else {
            since_secs.min(WINDOW_SECS)
        };
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
                count: agg.hist.count(),
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

    /// Store the latest process sample for `app`. Called by the sampler task in `daemon.rs`.
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
