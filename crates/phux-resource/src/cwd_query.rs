//! Best-effort kernel query for a live PTY child's cwd.
//!
//! Serves `defaults.cwd-inheritance = inherit-focused`. OSC 7 is unreliable
//! and not exposed by the bundled libghostty, so Linux reads
//! `/proc/<pid>/cwd` and macOS uses `proc_pidinfo(PROC_PIDVNODEPATHINFO)`;
//! other targets and any failure yield `None`.

use std::path::PathBuf;

/// Best-effort cwd of process `pid`; `None` means "do not override".
#[must_use]
pub fn process_cwd(pid: u32) -> Option<PathBuf> {
    platform::process_cwd(pid)
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::PathBuf;

    pub(super) fn process_cwd(pid: u32) -> Option<PathBuf> {
        std::fs::read_link(format!("/proc/{pid}/cwd")).ok()
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::ffi::CStr;
    use std::os::raw::c_void;
    use std::path::PathBuf;

    pub(super) fn process_cwd(pid: u32) -> Option<PathBuf> {
        // Never query pid 0 (the calling process).
        let pid = i32::try_from(pid).ok().filter(|p| *p > 0)?;

        let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<libc::proc_vnodepathinfo>();
        // SAFETY: `info` is an owned, zeroed, aligned `proc_vnodepathinfo`
        // and `size` is its exact size, so the kernel cannot overrun it. A
        // short write (dead pid, EPERM) is treated as no answer.
        let size_i32 = i32::try_from(size).ok()?;
        let written = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                std::ptr::addr_of_mut!(info).cast::<c_void>(),
                size_i32,
            )
        };
        if written < size_i32 {
            return None;
        }

        // `vip_path` is a NUL-terminated path in a `[[c_char; 32]; 32]`.
        let raw = &info.pvi_cdir.vip_path;
        let bytes: &[u8] = unsafe {
            std::slice::from_raw_parts(raw.as_ptr().cast::<u8>(), std::mem::size_of_val(raw))
        };
        // SAFETY: bounded by the field's size; a missing NUL is an error,
        // not an out-of-bounds read.
        let cstr = CStr::from_bytes_until_nul(bytes).ok()?;
        let path = cstr.to_str().ok()?;
        if path.is_empty() {
            return None;
        }
        Some(PathBuf::from(path))
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
mod platform {
    use std::path::PathBuf;

    pub(super) fn process_cwd(_pid: u32) -> Option<PathBuf> {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::process_cwd;

    /// This process's cwd matches `std::env::current_dir`.
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[test]
    fn process_cwd_of_self_matches_current_dir() {
        let pid = std::process::id();
        let got = process_cwd(pid).expect("self CWD should be queryable");
        let expected = std::env::current_dir().expect("current_dir");
        let got = got.canonicalize().unwrap_or(got);
        let expected = expected.canonicalize().unwrap_or(expected);
        assert_eq!(got, expected);
    }

    /// A pid that cannot be a live child (0) yields `None`, never a
    /// bogus path.
    #[test]
    fn process_cwd_of_pid_zero_is_none() {
        assert_eq!(process_cwd(0), None);
    }

    /// An almost-certainly-dead pid yields `None` rather than panicking.
    #[test]
    fn process_cwd_of_unlikely_pid_is_none() {
        // 2^31-1 is past any real pid on the supported platforms.
        assert_eq!(process_cwd(u32::MAX >> 1), None);
    }
}
