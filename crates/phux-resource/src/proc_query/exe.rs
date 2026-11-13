//! Whether a process still runs the binary installed at its path.

use std::path::Path;

/// Whether `pid` runs an older image than the file now at `installed`,
/// because `phux update` or a package manager replaced that file after the
/// process started. `None` when either cannot be read.
///
/// Linux compares the inode `/proc/<pid>/exe` names (a replaced image's
/// link still resolves) with the file's. Elsewhere there is no such link,
/// so the file's `ctime` (which install tools cannot backdate, unlike
/// `mtime`) after the process's start time is the answer.
#[must_use]
pub fn runs_replaced_executable(pid: i32, installed: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt as _;
    let now = std::fs::metadata(installed).ok()?;
    if cfg!(target_os = "linux") {
        let running = std::fs::metadata(format!("/proc/{pid}/exe")).ok()?;
        return Some((running.dev(), running.ino()) != (now.dev(), now.ino()));
    }
    let started = super::process_start_ms(pid)?;
    let changed_ms = now.ctime().checked_mul(1000)? + now.ctime_nsec() / 1_000_000;
    Some(u64::try_from(changed_ms).ok()? > started)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A process keeps its image when the file at its path is replaced by
    /// rename, as `phux update` does; until then it runs the installed one.
    /// macOS kills a copied system binary, so the "installed" file starts
    /// as a symlink to `sleep` and is replaced by a fresh script.
    #[test]
    fn a_replaced_binary_is_seen_after_the_swap_and_not_before() {
        let dir = tempfile::tempdir().expect("tempdir");
        let exe = dir.path().join("sleeper");
        let sleep = ["/bin/sleep", "/usr/bin/sleep"]
            .into_iter()
            .find(|p| Path::new(p).exists())
            .expect("sleep");
        std::os::unix::fs::symlink(sleep, &exe).expect("link");
        let mut child = std::process::Command::new(&exe)
            .arg("30")
            .spawn()
            .expect("spawn");
        let pid = i32::try_from(child.id()).expect("pid");
        std::thread::sleep(std::time::Duration::from_millis(50));
        assert_eq!(runs_replaced_executable(pid, &exe), Some(false));
        let next = dir.path().join("sleeper.new");
        std::fs::write(&next, "#!/bin/sh\n").expect("write");
        std::fs::rename(&next, &exe).expect("swap");
        assert_eq!(runs_replaced_executable(pid, &exe), Some(true));
        assert_eq!(
            runs_replaced_executable(pid, &dir.path().join("gone")),
            None
        );
        assert!(matches!(child.try_wait(), Ok(None)), "still running");
        let _ = child.kill();
        let _ = child.wait();
    }
}
