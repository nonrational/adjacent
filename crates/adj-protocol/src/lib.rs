use serde::{Deserialize, Serialize, Serializer};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Add {
        path: String,
        /// Register as a named instance `<label>.<name>` (routes at `<label>.<name>.adj.ac`).
        /// `None` registers under the bare app name as before.
        #[serde(default)]
        label: Option<String>,
    },
    List,
    Up {
        name: String,
    },
    Down {
        name: String,
    },
    Restart {
        name: String,
    },
    Status {
        name: String,
    },
    LogPath {
        name: String,
    },
    /// Block on the daemon until `name` reports ready (TCP-open, or 2xx from
    /// `health_check_url` when configured). `timeout_secs == 0` means use the app's configured
    /// `boot_timeout`.
    WaitReady {
        name: String,
        timeout_secs: u64,
    },
    Ping,
    /// Delete one registry entry, stopping the app first if it is running.
    Remove {
        name: String,
    },
    /// Delete every registry entry whose registered path no longer exists on disk.
    Prune,
    /// Snapshot the in-memory metrics window for `name`. `since_secs == 0` means the full window.
    Stats {
        name: String,
        #[serde(default)]
        since_secs: u64,
    },
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Response {
    Ok,
    Added { name: String, path: String },
    List { entries: Vec<AppSummary> },
    Status { name: String, state: AppState },
    LogPath { path: String },
    Error { message: String },
    Removed { name: String },
    Pruned { removed: Vec<String> },
    Stats { stats: StatsDto },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSummary {
    pub name: String,
    pub path: String,
    pub state: AppState,
    /// True when the registered path no longer exists on disk (e.g. a deleted worktree).
    /// Skipped on the wire when false so pre-stale daemons and clients interoperate.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppState {
    Stopped,
    Running {
        pid: u32,
        port: u16,
        /// RFC3339 timestamp recorded when the process was spawned. Optional for backward
        /// compatibility with any serialized state that predates the field.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        started_at: Option<String>,
    },
    Crashed {
        exit_code: i32,
    },
}

impl std::fmt::Display for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppState::Stopped => write!(f, "stopped"),
            AppState::Running { pid, port, .. } => write!(f, "running (pid {pid}, port {port})"),
            AppState::Crashed { exit_code } => write!(f, "crashed (exit {exit_code})"),
        }
    }
}

/// Stable JSON shape for `adj list --json`. One entry per registered app.
///
/// The shape is intentionally flat (no nested `state` object) to match the documented
/// schema. `port` is present only when the app is running. `stale` is present only when true.
#[derive(Debug, Clone)]
pub struct ListEntryDto<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub state: &'a AppState,
    pub stale: bool,
}

impl<'a> Serialize for ListEntryDto<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("name", self.name)?;
        map.serialize_entry("path", self.path)?;
        map.serialize_entry("state", state_tag(self.state))?;
        if let AppState::Running { port, .. } = self.state {
            map.serialize_entry("port", port)?;
        }
        if self.stale {
            map.serialize_entry("stale", &true)?;
        }
        map.end()
    }
}

/// Stable JSON shape for `adj status <name> --json`. Optional fields are present
/// only when meaningful for the current state.
#[derive(Debug, Clone)]
pub struct StatusDto<'a> {
    pub name: &'a str,
    pub path: &'a str,
    pub state: &'a AppState,
}

impl<'a> Serialize for StatusDto<'a> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeMap;
        let mut map = serializer.serialize_map(None)?;
        map.serialize_entry("name", self.name)?;
        map.serialize_entry("path", self.path)?;
        map.serialize_entry("state", state_tag(self.state))?;
        match self.state {
            AppState::Running {
                pid,
                port,
                started_at,
            } => {
                map.serialize_entry("pid", pid)?;
                map.serialize_entry("port", port)?;
                if let Some(ts) = started_at {
                    map.serialize_entry("started_at", ts)?;
                }
            }
            AppState::Crashed { exit_code } => {
                map.serialize_entry("exit_code", exit_code)?;
            }
            AppState::Stopped => {}
        }
        map.end()
    }
}

fn state_tag(state: &AppState) -> &'static str {
    match state {
        AppState::Stopped => "stopped",
        AppState::Running { .. } => "running",
        AppState::Crashed { .. } => "crashed",
    }
}

/// One record in the JSONL log file. Each supervised line of stdout/stderr becomes
/// one of these on disk; `adj logs --json` streams them verbatim, and the plain-text
/// view projects only the `line` field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogRecord {
    /// RFC3339 timestamp captured when the supervisor read the line.
    pub ts: String,
    pub stream: LogStream,
    pub line: String,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LogStream {
    Stdout,
    Stderr,
}

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
                latency_ms: LatencyDto {
                    p50: 8,
                    p95: 128,
                    p99: 128,
                    max: 91,
                },
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
        assert!(
            !json.contains("process"),
            "absent process must be omitted: {json}"
        );
        let back: StatsDto = serde_json::from_str(&json).unwrap();
        assert_eq!(back, dto);
    }

    #[test]
    fn stats_request_tags_kind() {
        let req = Request::Stats {
            name: "site".into(),
            since_secs: 0,
        };
        let json = serde_json::to_string(&req).unwrap();
        assert!(json.contains("\"kind\":\"stats\""), "got: {json}");
    }
}
