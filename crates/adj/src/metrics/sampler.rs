//! Process-group resource sampling behind a platform-agnostic trait. The Linux impl reads
//! `/proc`; the macOS impl uses `libproc`. The sampler returns
//! *cumulative* CPU time so the caller can derive a percentage from the delta between ticks.

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
                let Ok(pid) = name.parse::<i32>() else {
                    continue;
                };
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

// macOS path. Not compiled on Linux (so it's invisible to the Linux build and to local
// `cargo build`/`clippy`); it is compiled and exercised on the `macos-14` CI leg, which is the
// verifier for this module. Sums resource usage across every pid in the target process group.
#[cfg(target_os = "macos")]
mod macos {
    use super::{ProcSampler, RawProc};
    use libproc::libproc::bsd_info::BSDInfo;
    use libproc::libproc::file_info::ListFDs;
    use libproc::libproc::pid_rusage::{pidrusage, RUsageInfoV2};
    use libproc::libproc::proc_pid::{listpidinfo, listpids, pidinfo, ProcType};
    use libproc::libproc::task_info::TaskInfo;

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
                let Ok(bsd) = pidinfo::<BSDInfo>(pid, 0) else {
                    continue;
                };
                if bsd.pbi_pgid as i32 != pgid {
                    continue;
                }
                found = true;
                if let Ok(ru) = pidrusage::<RUsageInfoV2>(pid) {
                    // ri_user_time / ri_system_time are nanoseconds; ri_resident_size is bytes.
                    acc.cpu_time_ms += (ru.ri_user_time + ru.ri_system_time) / 1_000_000;
                    acc.rss_bytes += ru.ri_resident_size;
                }
                if let Ok(task) = pidinfo::<TaskInfo>(pid, 0) {
                    acc.threads += task.pti_threadnum.max(0) as u64;
                }
                if let Ok(fds) = listpidinfo::<ListFDs>(pid, 4096) {
                    acc.fds += fds.len() as u64;
                }
            }
            found.then_some(acc)
        }
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
        // i32::MAX is not a real process-group id, so no /proc entry reports it as its pgrp and
        // the scan finds nothing. (pgid 0 would be wrong here: kernel threads report pgrp 0.)
        assert!(sampler.sample(i32::MAX).is_none());
    }
}
