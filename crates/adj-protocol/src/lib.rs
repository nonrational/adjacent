use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Request {
    Add { path: String },
    List,
    Up { name: String },
    Down { name: String },
    Restart { name: String },
    Status { name: String },
    LogPath { name: String },
    Ping,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AppSummary {
    pub name: String,
    pub path: String,
    pub state: AppState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AppState {
    Stopped,
    Running { pid: u32 },
    Crashed { exit_code: i32 },
}

impl std::fmt::Display for AppState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AppState::Stopped => write!(f, "stopped"),
            AppState::Running { pid } => write!(f, "running (pid {pid})"),
            AppState::Crashed { exit_code } => write!(f, "crashed (exit {exit_code})"),
        }
    }
}
