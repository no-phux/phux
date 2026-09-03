//! Thread scheduling policy for the interactive path.
//!
//! A terminal multiplexer sits between the keyboard and the glass, so its
//! hot threads belong in the same scheduling class as the terminal emulator
//! itself. Left at the default, they compete evenly with every batch job on
//! the box: measured on a 14-core laptop, a full-CPU hog took keystroke echo
//! from a p99 of 0.5 ms to 15 ms with the server itself using 0.3% CPU. On
//! macOS the fix is the per-thread `QoS` class: `USER_INTERACTIVE` raises the
//! thread's priority band and keeps it off the efficiency cores, and any
//! thread may request it for itself without privilege. Linux has no
//! unprivileged equivalent (lowering `nice` needs `CAP_SYS_NICE`), so
//! [`promote_current_thread`] is a documented no-op there.

/// Ask the OS to schedule the calling thread as user-interactive.
///
/// Returns `true` when the request was accepted. Call it once, early, on
/// each thread that carries keystrokes or their echo: the server's runtime
/// thread, the PTY reader and writer threads, the input lane, the client's
/// runtime thread and its stdout writer.
#[must_use]
pub fn promote_current_thread() -> bool {
    imp::promote_current_thread()
}

#[cfg(target_os = "macos")]
#[allow(
    unsafe_code,
    reason = "pthread QoS has no safe binding; see the SAFETY note"
)]
mod imp {
    pub(super) fn promote_current_thread() -> bool {
        // SAFETY: `pthread_set_qos_class_self_np` only reads its two scalar
        // arguments and mutates the calling thread's own scheduling
        // attributes; it touches no memory we own and cannot fail in a way
        // that leaves state half-written. `relative_priority` 0 is the
        // documented default within the class.
        let rc = unsafe {
            libc::pthread_set_qos_class_self_np(libc::qos_class_t::QOS_CLASS_USER_INTERACTIVE, 0)
        };
        rc == 0
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    pub(super) fn promote_current_thread() -> bool {
        false
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn promotion_is_accepted_on_macos_and_a_no_op_elsewhere() {
        let accepted = super::promote_current_thread();
        assert_eq!(accepted, cfg!(target_os = "macos"));
    }
}
