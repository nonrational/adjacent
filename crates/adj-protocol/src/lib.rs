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
