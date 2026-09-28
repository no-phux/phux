//! The outer terminal's input handle, read on reactor readiness.
//!
//! `tokio::io::stdin()` reads on the blocking pool, costing a cross-thread
//! wake-up per keystroke. [`TtyInput::Ready`] registers a private
//! non-blocking handle to the controlling terminal with the reactor instead.
//!
//! The handle is a fresh open of the terminal's device path (`ttyname` of fd
//! 0): `O_NONBLOCK` on fd 0's shared description would leak to the parent
//! shell after exit, and a `/dev/tty` fd is not kqueue-registrable on macOS.
//! Termios is per device, so the raw-mode guard still governs it.
//! [`TtyInput::Blocking`] is the fallback whenever that path cannot be
//! established.

use std::io;
use std::io::IsTerminal;
use std::os::fd::{AsFd, OwnedFd};

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncReadExt, Interest};

/// How the attach loop reads the outer terminal.
#[derive(Debug)]
pub(super) enum TtyInput {
    /// Reactor readiness on a private non-blocking handle to the controlling
    /// terminal. The fast path; see the module docs.
    Ready(Box<AsyncFd<OwnedFd>>),
    /// Blocking-pool-backed stdin. Correct everywhere, slower by one
    /// cross-thread wake-up per read.
    Blocking(Box<tokio::io::Stdin>),
    /// EOF has been reported once; reads never complete again. A terminal at
    /// EOF answers `Ok(0)` forever, and stdin is polled first in the biased
    /// `select!`, so re-reporting it starved the signal arms (a client once
    /// spun a core and ignored `kill`).
    Closed,
}

impl TtyInput {
    /// Prefer the readiness path, falling back to blocking stdin. Must run
    /// inside the attach runtime ([`AsyncFd`] registers with its reactor).
    pub(super) fn open() -> Self {
        if readiness_disabled() {
            tracing::debug!("PHUX_TTY_READINESS=0: using blocking stdin");
            return Self::Blocking(Box::new(tokio::io::stdin()));
        }
        match controlling_tty() {
            // Read interest only; the handle is `O_RDONLY`.
            Ok(fd) => match AsyncFd::with_interest(fd, Interest::READABLE) {
                Ok(registered) => {
                    tracing::debug!("reading the outer terminal on reactor readiness");
                    return Self::Ready(Box::new(registered));
                }
                Err(err) => {
                    tracing::debug!(error = %err, "tty not pollable; using blocking stdin");
                }
            },
            Err(err) => {
                tracing::debug!(error = %err, "no private tty handle; using blocking stdin");
            }
        }
        Self::Blocking(Box::new(tokio::io::stdin()))
    }

    /// Read one burst of input. Cancel-safe on every variant, so it can sit
    /// in `select!`. `Ok(0)` (EOF) is reported exactly once; later calls park
    /// (see [`Self::Closed`]).
    pub(super) async fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let result = match self {
            Self::Ready(fd) => read_when_ready(fd, buf).await,
            Self::Blocking(stdin) => stdin.read(buf).await,
            Self::Closed => std::future::pending().await,
        };
        if matches!(result, Ok(0)) {
            *self = Self::Closed;
        }
        result
    }
}

/// `PHUX_TTY_READINESS=0` forces the blocking fallback (a support switch and
/// a measurement control).
fn readiness_disabled() -> bool {
    std::env::var_os("PHUX_TTY_READINESS").is_some_and(|v| v == "0")
}

/// Park on readiness, then read on the runtime thread; re-park when the read
/// still answers `EWOULDBLOCK`.
async fn read_when_ready(fd: &mut AsyncFd<OwnedFd>, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        let mut guard = fd.readable_mut().await?;
        match guard.try_io(|inner| read_uninterrupted(inner.get_ref(), buf)) {
            Ok(result) => return result,
            Err(_would_block) => {}
        }
    }
}

/// `read(2)` with `EINTR` retried (the loop's signal handlers interrupt
/// reads routinely).
fn read_uninterrupted(fd: &OwnedFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match rustix::io::read(fd.as_fd(), &mut *buf) {
            Ok(n) => return Ok(n),
            Err(rustix::io::Errno::INTR) => {}
            Err(err) => return Err(io::Error::from(err)),
        }
    }
}

/// Open a private non-blocking handle to stdin's terminal (`O_NOCTTY`: never
/// acquire a controlling terminal) and prove via `st_rdev` it is the same
/// device raw mode was set on; any mismatch or error falls back to stdin.
fn controlling_tty() -> io::Result<OwnedFd> {
    let stdin = io::stdin();
    if !stdin.is_terminal() {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "stdin is not a terminal",
        ));
    }
    let device = rustix::termios::ttyname(stdin.as_fd(), Vec::new())?;
    let fd = rustix::fs::open(
        device.as_c_str(),
        rustix::fs::OFlags::RDONLY
            | rustix::fs::OFlags::NONBLOCK
            | rustix::fs::OFlags::CLOEXEC
            | rustix::fs::OFlags::NOCTTY,
        rustix::fs::Mode::empty(),
    )?;
    if rustix::fs::fstat(&fd)?.st_rdev != rustix::fs::fstat(stdin.as_fd())?.st_rdev {
        return Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "the named terminal is a different device than stdin",
        ));
    }
    Ok(fd)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Under `cargo test` stdin is not a terminal: the fallback, not an error.
    #[tokio::test]
    async fn falls_back_to_blocking_stdin_without_a_tty() {
        assert!(matches!(TtyInput::open(), TtyInput::Blocking(_)));
    }

    /// EOF is reported exactly once and every later read parks; the timeout
    /// is the assertion (the old code returned `Ok(0)` immediately: the spin).
    #[tokio::test]
    async fn eof_is_reported_once_and_then_parks_forever() {
        // A closed pipe is a fd that is readable and at EOF, which is exactly
        // the shape a hung-up terminal presents.
        let (reader, writer) = std::io::pipe().expect("pipe");
        drop(writer);
        let mut input = TtyInput::Ready(Box::new(
            AsyncFd::with_interest(OwnedFd::from(reader), Interest::READABLE).expect("register"),
        ));

        let mut buf = [0u8; 16];
        assert_eq!(
            input.read(&mut buf).await.expect("first read"),
            0,
            "EOF must be reported once so the caller can detach",
        );
        assert!(matches!(input, TtyInput::Closed), "EOF must latch");

        let again =
            tokio::time::timeout(std::time::Duration::from_millis(200), input.read(&mut buf)).await;
        assert!(
            again.is_err(),
            "a read after EOF must never complete; it completed with {again:?}",
        );
    }

    /// With stdin not a terminal the device guard refuses, rather than
    /// returning some unrelated terminal.
    #[test]
    fn no_private_handle_when_stdin_is_not_a_terminal() {
        assert!(controlling_tty().is_err());
    }
}
