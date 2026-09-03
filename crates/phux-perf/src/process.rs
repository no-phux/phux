//! The process section of a report: `getrusage(2)` for the calling process.

use serde::{Deserialize, Serialize};

/// CPU, memory, and scheduling figures for one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ProcessStats {
    /// User-mode CPU time, microseconds.
    pub cpu_user_us: u64,
    /// Kernel-mode CPU time, microseconds.
    pub cpu_system_us: u64,
    /// Peak resident set size, bytes. Peak, not current: it is what
    /// `getrusage` offers portably, and a peak that keeps climbing is the
    /// leak signal anyway.
    pub max_rss_bytes: u64,
    /// Voluntary context switches: the process parked itself (idle wakeups
    /// show up here as the wake that follows each park).
    pub voluntary_ctx_switches: u64,
    /// Involuntary context switches: the scheduler took the CPU away.
    pub involuntary_ctx_switches: u64,
}

impl ProcessStats {
    /// Read the current figures; `None` if `getrusage` fails or the target
    /// has no such call (wasm).
    #[must_use]
    #[cfg(target_arch = "wasm32")]
    pub const fn capture() -> Option<Self> {
        None
    }

    /// Read the current figures; `None` if `getrusage` fails.
    #[must_use]
    #[cfg(not(target_arch = "wasm32"))]
    pub fn capture() -> Option<Self> {
        let ru = nix::sys::resource::getrusage(nix::sys::resource::UsageWho::RUSAGE_SELF).ok()?;
        let to_us = |tv: nix::sys::time::TimeVal| -> u64 {
            let secs = u64::try_from(tv.tv_sec()).unwrap_or(0);
            let usec = u64::try_from(tv.tv_usec()).unwrap_or(0);
            secs.saturating_mul(1_000_000).saturating_add(usec)
        };
        // macOS reports ru_maxrss in bytes; Linux in kibibytes.
        let maxrss = u64::try_from(ru.max_rss()).unwrap_or(0);
        let max_rss_bytes = if cfg!(target_os = "linux") {
            maxrss.saturating_mul(1024)
        } else {
            maxrss
        };
        Some(Self {
            cpu_user_us: to_us(ru.user_time()),
            cpu_system_us: to_us(ru.system_time()),
            max_rss_bytes,
            voluntary_ctx_switches: u64::try_from(ru.voluntary_context_switches()).unwrap_or(0),
            involuntary_ctx_switches: u64::try_from(ru.involuntary_context_switches()).unwrap_or(0),
        })
    }

    /// CPU and switches consumed since `prev`; RSS stays the current peak.
    #[must_use]
    pub const fn delta(&self, prev: &Self) -> Self {
        Self {
            cpu_user_us: self.cpu_user_us.saturating_sub(prev.cpu_user_us),
            cpu_system_us: self.cpu_system_us.saturating_sub(prev.cpu_system_us),
            max_rss_bytes: self.max_rss_bytes,
            voluntary_ctx_switches: self
                .voluntary_ctx_switches
                .saturating_sub(prev.voluntary_ctx_switches),
            involuntary_ctx_switches: self
                .involuntary_ctx_switches
                .saturating_sub(prev.involuntary_ctx_switches),
        }
    }

    /// Total CPU, microseconds.
    #[must_use]
    pub const fn cpu_total_us(&self) -> u64 {
        self.cpu_user_us.saturating_add(self.cpu_system_us)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn capture_reports_a_live_process() {
        let s = ProcessStats::capture().unwrap_or_default();
        assert!(
            s.max_rss_bytes > 0,
            "rss should be non-zero for a running test"
        );
        assert!(s.cpu_total_us() > 0 || s.voluntary_ctx_switches > 0);
    }

    #[test]
    fn delta_subtracts_cpu_and_keeps_rss() {
        let a = ProcessStats {
            cpu_user_us: 10,
            cpu_system_us: 5,
            max_rss_bytes: 100,
            voluntary_ctx_switches: 3,
            involuntary_ctx_switches: 1,
        };
        let b = ProcessStats {
            cpu_user_us: 30,
            cpu_system_us: 5,
            max_rss_bytes: 120,
            voluntary_ctx_switches: 10,
            involuntary_ctx_switches: 1,
        };
        let d = b.delta(&a);
        assert_eq!(d.cpu_user_us, 20);
        assert_eq!(d.cpu_system_us, 0);
        assert_eq!(d.max_rss_bytes, 120);
        assert_eq!(d.voluntary_ctx_switches, 7);
    }
}
