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
