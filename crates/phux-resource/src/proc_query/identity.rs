//! Process identity: start time, group, and this boot's id.

#[allow(
    unused_imports,
    clippy::wildcard_imports,
    reason = "the parent's namespace is shared across this split"
)]
use super::*;

/// The identity of the process `pid`, recorded when an activation starts
/// (R12).
///
/// `None` when its start time cannot be read. The process group is read
/// once; a spawned child leads its own group, and a mismatch is logged and
/// recorded as found.
#[must_use]
pub fn proc_identity(pid: i32) -> Option<phux_core::ProcIdentity> {
    let start_raw = process_start_time(pid)?;
    let boot = boot_id()?;
    let group = nix::unistd::getpgid(Some(nix::unistd::Pid::from_raw(pid)))
        .map_or(pid, nix::unistd::Pid::as_raw);
    if group != pid {
        tracing::warn!(pid, group, "child does not lead its process group");
    }
    Some(phux_core::ProcIdentity {
        pid,
        pgid: group,
        start_raw,
        boot_id: boot,
    })
}

/// This boot's identity: Linux `/proc/sys/kernel/random/boot_id`, macOS
/// `kern.boottime` (seconds and microseconds, big-endian).
#[must_use]
pub fn boot_id() -> Option<[u8; 16]> {
    platform_boot_id()
}

#[cfg(target_os = "linux")]
pub(super) fn platform_boot_id() -> Option<[u8; 16]> {
    let text = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
    parse_boot_id(&text)
}

/// A UUID's 32 hex digits, dashes ignored.
#[cfg_attr(
    not(target_os = "linux"),
    allow(
        dead_code,
        reason = "compiled everywhere so the parser is tested everywhere"
    )
)]
pub(super) fn parse_boot_id(text: &str) -> Option<[u8; 16]> {
    let hex: String = text.trim().chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    let mut out = [0_u8; 16];
    for (i, slot) in out.iter_mut().enumerate() {
        *slot = u8::from_str_radix(hex.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}

#[cfg(target_os = "macos")]
pub(super) fn platform_boot_id() -> Option<[u8; 16]> {
    let mut tv = libc::timeval {
        tv_sec: 0,
        tv_usec: 0,
    };
    let mut len = std::mem::size_of::<libc::timeval>();
    let name = c"kern.boottime";
    // SAFETY: `name` is a NUL-terminated C string; `tv` and `len` point at
    // a `timeval` and its size, which `kern.boottime` writes; no new value
    // is set (null, 0).
    let rc = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            std::ptr::from_mut(&mut tv).cast(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 || len != std::mem::size_of::<libc::timeval>() {
        return None;
    }
    let mut out = [0_u8; 16];
    out[..8].copy_from_slice(&tv.tv_sec.to_be_bytes());
    out[8..].copy_from_slice(&i64::from(tv.tv_usec).to_be_bytes());
    Some(out)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(super) const fn platform_boot_id() -> Option<[u8; 16]> {
    None
}
