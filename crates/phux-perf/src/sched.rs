//! Thread scheduling policy for the interactive path.
//!
//! Hot threads belong in the terminal emulator's scheduling class; left at
//! the default, a CPU hog took keystroke echo p99 from 0.5 ms to 15 ms. On
//! macOS the unprivileged per-thread `USER_INTERACTIVE` `QoS` class fixes that.
//! Linux has no unprivileged equivalent, so [`promote_current_thread`] is a
//! no-op there.

/// Ask the OS to schedule the calling thread as user-interactive.
///
/// Returns `true` when the request was accepted. Call it once, early, on
/// each thread that carries keystrokes or their echo: the server's runtime
/// thread, the PTY reader and writer threads, the input lane, the client's
/// runtime thread and its stdout writer.
#[must_use]
#[cfg_attr(
    not(target_os = "macos"),
    allow(
        clippy::missing_const_for_fn,
        reason = "a no-op on this target; an FFI call on macOS"
    )
)]
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
    pub(super) const fn promote_current_thread() -> bool {
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
