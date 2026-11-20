//! A write past `RLIMIT_FSIZE` fails with `EFBIG` instead of killing the
//! app (#2018). The kernel also raises `SIGXFSZ`, whose default action ends
//! the process before the error returns, and any write can raise it: the
//! diagnostic log's first line, a GPU driver's shader cache.

/// Catch `SIGXFSZ` with a handler that does nothing, so the write that
/// raised it returns its error. A handler, not `SIG_IGN`: an ignored signal
/// stays ignored across `exec`, into every program this app starts.
#[cfg(unix)]
#[allow(unsafe_code)]
pub fn fail_writes_past_the_limit() {
    extern "C" fn nothing(_: libc::c_int) {}
    // SAFETY: the handler touches no state, so it is async-signal-safe, and
    // the zeroed `sigaction` is a valid empty mask with no flags.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = nothing as extern "C" fn(libc::c_int) as libc::sighandler_t;
        action.sa_flags = libc::SA_RESTART;
        libc::sigaction(libc::SIGXFSZ, &raw const action, std::ptr::null_mut());
    }
}

#[cfg(not(unix))]
pub const fn fail_writes_past_the_limit() {}

#[cfg(all(test, unix))]
mod tests {
    const CHILD: &str = "PHUX_RUNE_FILE_SIZE_CHILD";

    /// Run in a child process: the limit and the handler are process-wide,
    /// and without the handler the write kills the process.
    #[test]
    fn a_write_past_the_file_size_limit_fails_instead_of_killing_the_app() {
        if let Some(path) = std::env::var_os(CHILD) {
            super::fail_writes_past_the_limit();
            let limit = libc::rlimit {
                rlim_cur: 0,
                rlim_max: libc::RLIM_INFINITY,
            };
            #[allow(unsafe_code)]
            // SAFETY: a plain value passed by reference to setrlimit.
            let set = unsafe { libc::setrlimit(libc::RLIMIT_FSIZE, &raw const limit) };
            assert_eq!(set, 0, "setrlimit");
            let error = std::fs::write(path, "past the limit").expect_err("the write fails");
            assert_eq!(error.raw_os_error(), Some(libc::EFBIG));
            return;
        }
        let dir = tempfile::tempdir().expect("temp dir");
        // Piped: a child writing its report to an inherited log file would
        // hit its own limit.
        let child = std::process::Command::new(std::env::current_exe().expect("test binary"))
            .args([
                "--exact",
                "file_size::tests::a_write_past_the_file_size_limit_fails_instead_of_killing_the_app",
                "--test-threads=1",
            ])
            .env(CHILD, dir.path().join("log"))
            .output()
            .expect("child test");
        assert!(
            child.status.success(),
            "the write killed the child: {:?}\n{}{}",
            child.status,
            String::from_utf8_lossy(&child.stdout),
            String::from_utf8_lossy(&child.stderr),
        );
    }
}
