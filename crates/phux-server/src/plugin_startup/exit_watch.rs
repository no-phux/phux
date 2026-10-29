//! Wake the supervisor when a sidecar exits.
//!
//! A fixed poll misses the "sleep until the child exits" requirement and
//! wakes the runtime while nothing is running. Linux waits on `pidfd`;
//! macOS waits on a kqueue `EVFILT_PROC`/`NOTE_EXIT` registration. Both
//! descriptors are nonblocking and registered with `AsyncFd` for
//! `Interest::READABLE` only. mio's default also adds `EVFILT_WRITE`, and
//! XNU rejects that on a kqueue descriptor with `EINVAL`.

use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::io::{Interest, unix::AsyncFd};
use tokio::sync::Notify;

/// Aborts the wait when the child handle is dropped.
pub(super) struct ExitWatch {
    abort: tokio::task::AbortHandle,
    /// Set when the descriptor becomes readable. The process may not be a
    /// zombie yet (XNU posts `NOTE_EXIT` first; a pidfd can race the same
    /// way). A fired watch is spent: `try_wait` may still return `None`,
    /// and the supervisor must poll on a deadline instead of trusting the
    /// one-shot registration.
    fired: Arc<AtomicBool>,
}

impl ExitWatch {
    pub(super) fn fired(&self) -> bool {
        self.fired.load(Ordering::Acquire)
    }
}

impl Drop for ExitWatch {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

/// Arm a wait. `None` when the kernel cannot watch `pid`; the supervisor
/// then polls on a one-second fallback instead of every tick. Registration
/// failure is logged; it is not discarded quietly.
pub(super) fn watch_exit(pid: i32, notify: &Arc<Notify>) -> Option<ExitWatch> {
    let Some(fd) = arm_fd(pid) else {
        tracing::debug!(pid, "startup: could not arm an exit watch");
        return None;
    };
    let fired = Arc::new(AtomicBool::new(false));
    // SAFETY: `fd` is an open descriptor this function owns. `AsyncFd`
    // keeps it open and returns the same raw fd until it is dropped.
    // `register_with_interest` is the non-deprecated form of
    // `AsyncFd::with_interest`. `READABLE` alone: registering `WRITABLE`
    // makes mio add `EVFILT_WRITE`, which XNU rejects on a kqueue fd.
    let async_fd = match unsafe { AsyncFd::register_with_interest(fd, Interest::READABLE) } {
        Ok(async_fd) => async_fd,
        Err(err) => {
            tracing::warn!(
                pid,
                error = %err,
                "startup: exit watch registration failed"
            );
            return None;
        }
    };
    let notify = Arc::clone(notify);
    let flag = Arc::clone(&fired);
    let task = tokio::task::spawn_local(async move {
        let _ = async_fd.readable().await;
        flag.store(true, Ordering::Release);
        notify.notify_one();
    });
    Some(ExitWatch {
        abort: task.abort_handle(),
        fired,
    })
}

fn set_nonblock(fd: i32) -> Option<()> {
    // SAFETY: `fd` is a descriptor this function just opened. `F_GETFL` /
    // `F_SETFL` only change its blocking mode.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 {
        return None;
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return None;
    }
    Some(())
}

#[cfg(target_os = "macos")]
fn set_cloexec(fd: i32) -> Option<()> {
    // SAFETY: `fd` is a descriptor this function just opened. `F_GETFD` /
    // `F_SETFD` only change its close-on-exec flag.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFD) };
    if flags < 0 {
        return None;
    }
    if unsafe { libc::fcntl(fd, libc::F_SETFD, flags | libc::FD_CLOEXEC) } < 0 {
        return None;
    }
    Some(())
}

#[cfg(target_os = "linux")]
fn arm_fd(pid: i32) -> Option<OwnedFd> {
    // SAFETY: `pidfd_open` returns a new descriptor for `pid`, or -1.
    // Flags are 0; nonblocking is set below so `AsyncFd` can poll it.
    let opened = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
    let fd = i32::try_from(opened).ok()?;
    if fd < 0 {
        return None;
    }
    if set_nonblock(fd).is_none() {
        unsafe { libc::close(fd) };
        return None;
    }
    // SAFETY: `fd` is open and not owned elsewhere. `OwnedFd` closes it.
    Some(unsafe { OwnedFd::from_raw_fd(fd) })
}

#[cfg(target_os = "macos")]
fn arm_fd(pid: i32) -> Option<OwnedFd> {
    // SAFETY: `kqueue` opens a new queue descriptor, or -1.
    let kq = unsafe { libc::kqueue() };
    if kq < 0 {
        return None;
    }
    if set_cloexec(kq).is_none() || set_nonblock(kq).is_none() {
        unsafe { libc::close(kq) };
        return None;
    }
    let Some(ident) = libc::uintptr_t::try_from(pid).ok() else {
        unsafe { libc::close(kq) };
        return None;
    };
    // `kevent` is `repr(packed(4))`. `addr_of!` does not borrow a field,
    // so the pointer passed to `kevent` is not an unaligned reference.
    let change = libc::kevent {
        ident,
        filter: libc::EVFILT_PROC,
        flags: libc::EV_ADD | libc::EV_ONESHOT,
        fflags: libc::NOTE_EXIT,
        data: 0,
        udata: std::ptr::null_mut(),
    };
    // SAFETY: `change` is a one-item changelist. A null event list only
    // registers; it does not write.
    let rc = unsafe {
        libc::kevent(
            kq,
            std::ptr::addr_of!(change),
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
        )
    };
    if rc < 0 {
        unsafe { libc::close(kq) };
        return None;
    }
    // SAFETY: `kq` is open and not owned elsewhere. `OwnedFd` closes it.
    Some(unsafe { OwnedFd::from_raw_fd(kq) })
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn arm_fd(_pid: i32) -> Option<OwnedFd> {
    None
}
