//! Re-adopt an existing PTY into [`portable-pty`](portable_pty)'s trait
//! objects from a bare master file descriptor and a child process id.
//!
//! # The gap this fills
//!
//! `portable-pty` can hand you a [`MasterPty`] / [`Child`] only by *creating*
//! the PTY: [`PtySystem::openpty`](portable_pty::PtySystem::openpty) mints a
//! fresh master+slave pair, and
//! [`SlavePty::spawn_command`](portable_pty::SlavePty::spawn_command) forks the
//! child.
//! The concrete Unix types are private and expose no `from_raw_fd` /
//! `from_pid` constructor. So once a PTY exists, there is no supported way to
//! rebuild those trait objects from the raw `(master_fd, child_pid)` you can
//! recover with [`MasterPty::as_raw_fd`] and [`Child::process_id`].
//!
//! That matters for **graceful, in-place restarts**. The standard reload
//! primitive (nginx, `HAProxy`, systemd socket activation) is to clear
//! `FD_CLOEXEC` on the descriptors you want to keep and `execve` the new
//! binary: open fds survive the exec, and because `execve` preserves the
//! process identity, the children stay alive and stay *your* children
//! (`waitpid` keeps working). The new image inherits the PTY master as a bare
//! fd and knows the child PID from a handoff blob — but with `portable-pty`
//! alone it cannot turn those back into the `MasterPty`/`Child` its plumbing
//! is written against.
//!
//! [`AdoptedMaster`] and [`AdoptedChild`] are that missing constructor. They
//! implement the same traits over an inherited fd / PID, so a resumed process
//! drops them into the exact code paths it already drives for freshly-spawned
//! PTYs.
//!
//! # Scope and caveats
//!
//! - **Unix only.** PTYs, `waitpid`, and `tcgetpgrp` are POSIX concepts.
//! - **You must own the descriptor.** [`AdoptedMaster`] takes an [`OwnedFd`]
//!   and closes it on drop, exactly like the real master.
//! - **You must be the child's parent.** `waitpid`/`kill` target the PID
//!   directly; this is sound after an `execve` (same process, same children)
//!   but not if the child was re-parented (e.g. across a `fork`).
//! - **`Child::kill` sends `SIGKILL`,** matching `portable-pty`'s own
//!   `std::process::Child`-backed implementation.

#![cfg(unix)]

use anyhow::Error;
use libc::winsize;
use portable_pty::{Child, ChildKiller, ExitStatus, MasterPty, PtySize};
use std::ffi::OsStr;
use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

/// The master end of an inherited PTY, exposed as a [`MasterPty`].
///
/// Construct one in a resumed process from the master fd you kept open across
/// the `execve` (see [the crate docs](crate)). It owns the descriptor and
/// closes it on drop, just like a `portable-pty`-created master.
#[derive(Debug)]
pub struct AdoptedMaster {
    fd: OwnedFd,
    tty_name: Option<PathBuf>,
}

impl AdoptedMaster {
    /// Adopt an owned PTY master descriptor.
    #[must_use]
    pub fn new(fd: OwnedFd) -> Self {
        let tty_name = tty_name(fd.as_raw_fd());
        Self { fd, tty_name }
    }

    /// Adopt a PTY master descriptor by raw number, taking ownership of it.
    ///
    /// # Safety
    ///
    /// `fd` must be an open PTY master descriptor that nothing else owns; the
    /// returned [`AdoptedMaster`] closes it on drop. Typical use: an fd
    /// inherited across `execve` whose number arrived through a handoff blob.
    #[must_use]
    pub unsafe fn from_raw_fd(fd: RawFd) -> Self {
        // SAFETY: the caller contracts that `fd` is a valid, solely-owned
        // descriptor; `OwnedFd` assumes ownership and will close it on drop.
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        Self::new(owned)
    }

    fn dup_file(&self) -> io::Result<File> {
        Ok(File::from(self.fd.try_clone()?))
    }
}

impl MasterPty for AdoptedMaster {
    fn resize(&self, size: PtySize) -> Result<(), Error> {
        let ws = winsize_from(size);
        // SAFETY: TIOCSWINSZ reads a `winsize` we fully initialise through a
        // descriptor we own; no aliasing or lifetime concerns.
        let rc = unsafe { libc::ioctl(self.fd.as_raw_fd(), libc::TIOCSWINSZ as _, &raw const ws) };
        if rc != 0 {
            anyhow::bail!("ioctl(TIOCSWINSZ) failed: {}", io::Error::last_os_error());
        }
        Ok(())
    }

    fn get_size(&self) -> Result<PtySize, Error> {
        // SAFETY: zeroed `winsize` is a valid initial value; the ioctl fills
        // it through a descriptor we own.
        let mut ws: winsize = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::ioctl(self.fd.as_raw_fd(), libc::TIOCGWINSZ as _, &raw mut ws) };
        if rc != 0 {
            anyhow::bail!("ioctl(TIOCGWINSZ) failed: {}", io::Error::last_os_error());
        }
        Ok(PtySize {
            rows: ws.ws_row,
            cols: ws.ws_col,
            pixel_width: ws.ws_xpixel,
            pixel_height: ws.ws_ypixel,
        })
    }

    fn try_clone_reader(&self) -> Result<Box<dyn Read + Send>, Error> {
        Ok(Box::new(PtyReader(self.dup_file()?)))
    }

    fn take_writer(&self) -> Result<Box<dyn Write + Send>, Error> {
        Ok(Box::new(self.dup_file()?))
    }

    fn process_group_leader(&self) -> Option<libc::pid_t> {
        // SAFETY: tcgetpgrp on a descriptor we own; returns -1 / sets errno on
        // failure, which we map to `None`.
        match unsafe { libc::tcgetpgrp(self.fd.as_raw_fd()) } {
            pid if pid > 0 => Some(pid),
            _ => None,
        }
    }

    fn as_raw_fd(&self) -> Option<RawFd> {
        Some(self.fd.as_raw_fd())
    }

    fn tty_name(&self) -> Option<PathBuf> {
        self.tty_name.clone()
    }
}

/// A reader over a PTY master that maps the slave-closed `EIO` to clean EOF.
///
/// Mirrors `portable-pty`'s own master reader: once the last slave fd closes,
/// `read(2)` on the master returns `EIO`, which we translate to `Ok(0)` so
/// `read`-loop callers terminate gracefully instead of erroring.
struct PtyReader(File);

impl Read for PtyReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.0.read(buf) {
            Err(ref e) if e.raw_os_error() == Some(libc::EIO) => Ok(0),
            other => other,
        }
    }
}

/// Signal name carried by the [`ExitStatus`] an [`AdoptedChild`] reports when
/// `waitpid` fails with `ECHILD`: the process is gone from our view, but how it
/// ended is unknown.
///
/// It is not a real signal name, so a consumer mapping names to signal numbers
/// finds no match and reports "unknown" — exactly the truth. `success()` is
/// `false` for it, and `exit_code()` is `portable-pty`'s signal placeholder
/// (`1`), which callers must not read as an exit code.
pub const ECHILD_EXIT_SIGNAL_NAME: &str = "unknown (ECHILD: not our child)";

/// A child process re-adopted by PID, exposed as a [`Child`].
///
/// Sound only when the current process is the child's parent — true across an
/// `execve` (the process and its children are preserved). `waitpid` results
/// are cached so repeated `try_wait`/`wait` after exit keep returning the
/// status instead of failing with `ECHILD`.
///
/// # Reaping and zombies
///
/// Like [`std::process::Child`], this does **not** reap on `Drop`: a child
/// dropped without a prior [`Child::try_wait`]/[`Child::wait`] that observed
/// its exit stays a zombie until this process exits. Call `wait` on teardown.
/// Reaping is by explicit poll only — there is deliberately no `SIGCHLD`
/// handler involved (and note `SIG_IGN` on `SIGCHLD` survives `execve` and
/// would make every `waitpid` here return `ECHILD`, so a resuming process must
/// not set it).
///
/// `ECHILD` (the PID is not our child — already reaped, or never ours) is
/// reported as a terminal status so callers stop polling, but as an
/// *unknown* one: an [`ExitStatus`] whose signal name is
/// [`ECHILD_EXIT_SIGNAL_NAME`], never a fabricated `exit 0`. Nothing was
/// observed about how the process ended, so claiming success would be a lie
/// an exit-code reader cannot detect. The corollary is a caveat for `execve`
/// handoffs: only adopt PIDs you captured as *live* in the same process
/// lineage. A PID that already exited and was reaped before the exec could in
/// principle be recycled by the OS; carry per-child liveness in the handoff
/// blob rather than blindly adopting every recorded PID.
#[derive(Debug)]
pub struct AdoptedChild {
    pid: libc::pid_t,
    exited: Option<ExitStatus>,
}

impl AdoptedChild {
    /// Adopt a child by process id.
    #[must_use]
    pub const fn new(pid: libc::pid_t) -> Self {
        Self { pid, exited: None }
    }

    /// `waitpid(self.pid, _, flags)`, retrying on `EINTR`.
    ///
    /// Returns `Ok(None)` when `WNOHANG` finds the child still running,
    /// `Ok(Some(status))` once it is reaped, and treats `ECHILD` (not our
    /// child / already reaped elsewhere) as an unknown terminal status
    /// ([`ECHILD_EXIT_SIGNAL_NAME`]) so callers stop polling rather than spin
    /// on an error, without claiming the child exited cleanly.
    fn waitpid(&self, flags: libc::c_int) -> io::Result<Option<ExitStatus>> {
        loop {
            let mut status: libc::c_int = 0;
            // SAFETY: `waitpid` writes the wait status through a valid
            // out-pointer; it has no memory-safety preconditions.
            let rc = unsafe { libc::waitpid(self.pid, &raw mut status, flags) };
            if rc == -1 {
                let err = io::Error::last_os_error();
                return match err.raw_os_error() {
                    Some(libc::EINTR) => continue,
                    Some(libc::ECHILD) => {
                        Ok(Some(ExitStatus::with_signal(ECHILD_EXIT_SIGNAL_NAME)))
                    }
                    _ => Err(err),
                };
            }
            if rc == 0 {
                return Ok(None);
            }
            return Ok(Some(raw_status_to_exit(status)));
        }
    }
}

impl ChildKiller for AdoptedChild {
    fn kill(&mut self) -> io::Result<()> {
        kill_pid(self.pid)
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(AdoptedChildKiller { pid: self.pid })
    }
}

impl Child for AdoptedChild {
    fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        if let Some(status) = &self.exited {
            return Ok(Some(status.clone()));
        }
        let result = self.waitpid(libc::WNOHANG)?;
        if let Some(status) = &result {
            self.exited = Some(status.clone());
        }
        Ok(result)
    }

    fn wait(&mut self) -> io::Result<ExitStatus> {
        if let Some(status) = &self.exited {
            return Ok(status.clone());
        }
        let status = self
            .waitpid(0)?
            .unwrap_or_else(|| ExitStatus::with_exit_code(0));
        self.exited = Some(status.clone());
        Ok(status)
    }

    fn process_id(&self) -> Option<u32> {
        u32::try_from(self.pid).ok()
    }
}

/// A detached killer for an [`AdoptedChild`], as returned by
/// [`ChildKiller::clone_killer`]. Sends signals to the PID without holding the
/// `Child`, so a thread blocked in `wait` can still be interrupted.
#[derive(Debug)]
struct AdoptedChildKiller {
    pid: libc::pid_t,
}

impl ChildKiller for AdoptedChildKiller {
    fn kill(&mut self) -> io::Result<()> {
        kill_pid(self.pid)
    }

    fn clone_killer(&self) -> Box<dyn ChildKiller + Send + Sync> {
        Box::new(Self { pid: self.pid })
    }
}

fn kill_pid(pid: libc::pid_t) -> io::Result<()> {
    // SAFETY: `kill(2)` with a PID and signal number; no memory-safety
    // preconditions. A failure (e.g. ESRCH if already gone) surfaces as Err.
    let rc = unsafe { libc::kill(pid, libc::SIGKILL) };
    if rc == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

const fn winsize_from(size: PtySize) -> winsize {
    winsize {
        ws_row: size.rows,
        ws_col: size.cols,
        ws_xpixel: size.pixel_width,
        ws_ypixel: size.pixel_height,
    }
}

fn raw_status_to_exit(status: libc::c_int) -> ExitStatus {
    if libc::WIFEXITED(status) {
        ExitStatus::with_exit_code(u32::try_from(libc::WEXITSTATUS(status)).unwrap_or(1))
    } else if libc::WIFSIGNALED(status) {
        ExitStatus::with_signal(&format!("signal {}", libc::WTERMSIG(status)))
    } else {
        // Stopped/continued — not requested via our flags, so treat as a
        // benign terminal status rather than inventing a code.
        ExitStatus::with_exit_code(0)
    }
}

/// Resolve the path of the slave tty for a master fd, mirroring
/// `portable-pty`'s `tty_name`. Returns `None` on any error, including the
/// macOS quirk where `ttyname_r` reports `ERANGE` for an oversized buffer.
fn tty_name(fd: RawFd) -> Option<PathBuf> {
    let mut buf = vec![0 as std::ffi::c_char; 128];
    loop {
        // SAFETY: `ttyname_r` writes at most `buf.len()` bytes into a buffer
        // we own; we pass its true length.
        let rc = unsafe { libc::ttyname_r(fd, buf.as_mut_ptr(), buf.len()) };
        if rc == libc::ERANGE {
            if buf.len() > 64 * 1024 {
                return None;
            }
            buf.resize(buf.len() * 2, 0 as std::ffi::c_char);
            continue;
        }
        if rc != 0 {
            return None;
        }
        // SAFETY: on success `ttyname_r` null-terminated the buffer.
        let cstr = unsafe { std::ffi::CStr::from_ptr(buf.as_ptr()) };
        return Some(PathBuf::from(OsStr::from_bytes(cstr.to_bytes())));
    }
}

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod tests {
    use super::{AdoptedChild, Child, ECHILD_EXIT_SIGNAL_NAME};

    /// pid 1 is never this test process's child, so `waitpid` fails with
    /// `ECHILD`. That must read as "gone, cause unknown" — not as a clean
    /// `exit 0` an exit-code reader would believe.
    #[test]
    fn echild_is_unknown_status_not_zero() {
        let mut child = AdoptedChild::new(1);
        let status = child
            .try_wait()
            .expect("ECHILD is a terminal status, not an error")
            .expect("ECHILD stops polling");
        assert!(!status.success(), "ECHILD must not claim success");
        assert_eq!(status.signal(), Some(ECHILD_EXIT_SIGNAL_NAME));
        // Cached: a second poll reports the same unknown status.
        let again = child.try_wait().expect("cached").expect("cached");
        assert_eq!(again.signal(), Some(ECHILD_EXIT_SIGNAL_NAME));
    }
}
