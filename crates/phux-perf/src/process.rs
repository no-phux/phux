//! Process figures: `getrusage(2)` for reports and native current RSS.

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

    /// Current resident set size of the calling process, in bytes.
    ///
    /// Unlike [`Self::max_rss_bytes`], this can decrease when memory is
    /// released. Read independently so existing report snapshots retain their
    /// peak-RSS contract. Returns `None` on native query failure or an
    /// unsupported target; never substitutes peak RSS for current RSS.
    #[must_use]
    pub fn current_rss_bytes() -> Option<u64> {
        current_rss_bytes()
    }

    /// Memory charged to this process by macOS, including compressed and GPU
    /// allocations. This is the physical footprint, not RSS or Rust heap size.
    /// Unavailable on other platforms; never substitutes a different measure.
    #[must_use]
    pub fn physical_footprint_bytes() -> Option<u64> {
        physical_footprint_bytes()
    }
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "proc_pid_rusage has no safe binding; see SAFETY notes"
)]
fn physical_footprint_bytes() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::rusage_info_v2>::uninit();
    let pid = i32::try_from(std::process::id()).ok()?;
    // SAFETY: RUSAGE_INFO_V2 writes exactly rusage_info_v2 into aligned,
    // writable storage. The kernel retains no pointer; failure is rejected.
    let result =
        unsafe { libc::proc_pid_rusage(pid, libc::RUSAGE_INFO_V2, info.as_mut_ptr().cast()) };
    if result != 0 {
        return None;
    }
    // SAFETY: a successful call initialized the complete requested structure.
    Some(unsafe { info.assume_init() }.ri_phys_footprint)
}

#[cfg(not(target_os = "macos"))]
const fn physical_footprint_bytes() -> Option<u64> {
    None
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "proc_pidinfo has no safe binding; see the SAFETY note"
)]
fn current_rss_bytes() -> Option<u64> {
    let mut info = std::mem::MaybeUninit::<libc::proc_taskinfo>::uninit();
    let size = i32::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
    let pid = i32::try_from(std::process::id()).ok()?;
    // SAFETY: `info` points to writable storage of exactly `size` bytes,
    // aligned for proc_taskinfo. PROC_PIDTASKINFO writes that type for our
    // own live process. A short/failed read is rejected before initialization
    // is assumed; the kernel retains no pointer to the storage.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // SAFETY: proc_pidinfo returned exactly the complete proc_taskinfo size.
    Some(unsafe { info.assume_init() }.pti_resident_size)
}

#[cfg(target_os = "linux")]
#[allow(
    unsafe_code,
    reason = "sysconf has no enabled safe binding; see the SAFETY note"
)]
fn current_rss_bytes() -> Option<u64> {
    use std::io::Read as _;

    // statm's second field is the current resident page count, not VmHWM.
    // A fixed buffer comfortably holds its first two native-size integers.
    let mut bytes = [0; 128];
    let mut file = std::fs::File::open("/proc/self/statm").ok()?;
    let mut len = 0;
    while len < bytes.len() {
        let read = file.read(&mut bytes[len..]).ok()?;
        if read == 0 {
            break;
        }
        len += read;
    }
    let statm = std::str::from_utf8(&bytes[..len]).ok()?;
    let resident_pages = statm.split_ascii_whitespace().nth(1)?.parse::<u64>().ok()?;
    // SAFETY: sysconf reads only the scalar selector and writes no caller
    // memory. Nonpositive results are rejected before converting to bytes.
    let page_size = unsafe { nix::libc::sysconf(nix::libc::_SC_PAGESIZE) };
    let page_size = u64::try_from(page_size).ok().filter(|size| *size > 0)?;
    resident_pages.checked_mul(page_size)
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
const fn current_rss_bytes() -> Option<u64> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn footprint_is_native_or_explicitly_unavailable() {
        let footprint = ProcessStats::physical_footprint_bytes();
        if cfg!(target_os = "macos") {
            assert!(footprint.is_some_and(|bytes| bytes > 0));
        } else {
            assert_eq!(footprint, None);
        }
    }

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

    #[test]
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    #[allow(
        unsafe_code,
        reason = "explicit VM mapping avoids allocator retention; see SAFETY notes"
    )]
    fn current_rss_tracks_resident_allocations_not_the_high_water_mark() {
        const ALLOCATION: usize = 64 * 1024 * 1024;
        const MIN_CHANGE: u64 = 32 * 1024 * 1024;

        let before = ProcessStats::current_rss_bytes().expect("native current RSS");
        assert!(before > 0);
        // The system allocator may retain freed heap blocks. Map pages directly
        // so this checks resident memory, not an allocator's release policy.
        // SAFETY: anonymous private mapping, no file or requested fixed address.
        let memory = unsafe {
            nix::libc::mmap(
                std::ptr::null_mut(),
                ALLOCATION,
                nix::libc::PROT_READ | nix::libc::PROT_WRITE,
                nix::libc::MAP_PRIVATE | nix::libc::MAP_ANON,
                -1,
                0,
            )
        };
        assert_ne!(memory, nix::libc::MAP_FAILED);
        // SAFETY: the successful mapping owns ALLOCATION writable bytes.
        unsafe { std::ptr::write_bytes(memory.cast::<u8>(), 0x5a, ALLOCATION) };
        let during = ProcessStats::current_rss_bytes();
        let peak_during = ProcessStats::capture();
        // SAFETY: release this exact mapping once; no pointers are used afterward.
        assert_eq!(unsafe { nix::libc::munmap(memory, ALLOCATION) }, 0);
        let after = ProcessStats::current_rss_bytes().expect("RSS after release");
        let during = during.expect("RSS during mapping");
        let peak_during = peak_during.expect("peak RSS").max_rss_bytes;
        let peak_after = ProcessStats::capture()
            .expect("peak RSS after release")
            .max_rss_bytes;
        assert!(
            during >= before.saturating_add(MIN_CHANGE),
            "RSS must measure resident bytes: before={before}, during={during}"
        );
        assert!(
            after.saturating_add(MIN_CHANGE) <= during,
            "current RSS must fall after release: during={during}, after={after}"
        );
        assert!(peak_after >= peak_during, "peak RSS must not fall");
    }

    #[test]
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    fn current_rss_is_unavailable_on_unsupported_targets() {
        assert_eq!(ProcessStats::current_rss_bytes(), None);
    }
}
