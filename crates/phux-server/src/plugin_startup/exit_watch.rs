//! Wake the supervisor when a sidecar exits.
//!
//! A fixed poll misses the "sleep until the child exits" requirement and
//! wakes the runtime while nothing is running. Linux waits on `pidfd`;
//! macOS waits on a kqueue `EVFILT_PROC`/`NOTE_EXIT` registration. Both
//! descriptors are nonblocking and registered with `AsyncFd`, so the
//! `LocalSet` is notified by readiness rather than a timer.

use std::os::fd::{FromRawFd, OwnedFd};
use std::sync::Arc;

use tokio::sync::Notify;

/// Aborts the wait when the child handle is dropped.
pub(super) struct ExitWatch {
    abort: tokio::task::AbortHandle,
}

impl Drop for ExitWatch {
    fn drop(&mut self) {
        self.abort.abort();
    }
}

/// Arm a one-shot wait. `None` when the kernel cannot watch `pid`; the
/// supervisor then polls on a one-second fallback instead of every tick.
pub(super) fn watch_exit(pid: i32, notify: &Arc<Notify>) -> Option<ExitWatch> {
    let fd = arm_fd(pid)?;
    // SAFETY: `fd` is an open descriptor this function owns. `AsyncFd`
    // keeps it open and returns the same raw fd until it is dropped.
    let async_fd = unsafe { tokio::io::unix::AsyncFd::register(fd) }.ok()?;
    let notify = Arc::clone(notify);
    let task = tokio::task::spawn_local(async move {
        let _ = async_fd.readable().await;
        notify.notify_one();
    });
    Some(ExitWatch {
        abort: task.abort_handle(),
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
    let ident = libc::uintptr_t::try_from(pid).ok()?;
    // `kevent` is `repr(packed(4))`. The local is aligned enough to take
    // its address; no field is borrowed individually.
    let mut change = libc::kevent {
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
            &change,
            1,
            std::ptr::null_mut(),
            0,
            std::ptr::null_mut(),
        )
    };
    if rc < 0 || set_nonblock(kq).is_none() {
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
