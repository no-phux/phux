//! Keeping a spawned plugin from inheriting the server's descriptors.

/// Mark every descriptor above stderr `FD_CLOEXEC` so a successful `exec`
/// drops them. Do not close them: std's exec-error pipe lives above stderr,
/// and closing it makes `spawn` return `Ok` when the program is missing.
/// The child then dies with `SIGABRT` and a typo becomes a crash loop.
pub(super) fn mark_cloexec_above_stderr() {
    #[cfg(target_os = "linux")]
    {
        // SAFETY: `close_range` is one syscall and async-signal-safe.
        // `CLOSE_RANGE_CLOEXEC` sets the flag; it does not close. This runs
        // in the child, so it does not flip the server's own descriptor
        // flags (that would race the upgrade handoff; see
        // `daemon::withhold_inherited_descriptors`). A non-zero return is
        // `ENOSYS` or `EINVAL` on kernels before 5.9, and the loop below
        // covers that.
        // Called by number: musl has no `close_range` wrapper, and glibc
        // only gained one in 2.34.
        let flags = libc::CLOSE_RANGE_CLOEXEC;
        if unsafe { libc::syscall(libc::SYS_close_range, 3_u32, u32::MAX, flags) } == 0 {
            return;
        }
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    mark_cloexec_loop();
}

/// `FD_CLOEXEC` from 3 up to `getdtablesize()`. No allocation. Async-signal-safe.
#[cfg(any(target_os = "linux", target_os = "macos"))]
fn mark_cloexec_loop() {
    // SAFETY: `getdtablesize` and `fcntl` are async-signal-safe. Missing
    // descriptors return `EBADF` and are skipped.
    let limit = unsafe { libc::getdtablesize() };
    if limit <= 3 {
        return;
    }
    for fd in 3..limit {
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        if flags < 0 {
            continue;
        }
        unsafe {
            libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC);
        }
    }
}
