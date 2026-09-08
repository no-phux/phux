//! The outer terminal's input handle, read on reactor readiness.
//!
//! `tokio::io::stdin()` is backed by the blocking pool: every `read` hands the
//! call to a worker thread, blocks there, and hands the bytes back through a
//! channel. On the attach loop that costs a cross-thread wake-up per keystroke
//! — the loop is parked on `select!`, the pool thread returns from `read(2)`,
//! the runtime is woken, and only then does the byte reach the parser. The
//! wake-up is pure latency: nothing about a 1-byte tty read needs a thread.
//!
//! [`TtyInput::Ready`] instead registers a private, non-blocking handle to the
//! controlling terminal with the reactor, so the `select!` arm wakes straight
//! off kqueue/epoll readiness and reads on the runtime thread. There is no
//! second thread, no channel, and no handoff.
//!
//! The handle is a *fresh* open of the terminal rather than fd 0 (or a `dup`
//! of it): `O_NONBLOCK` lives on the open file description, and fd 0's
//! description is shared with whatever launched us, so flipping it there would
//! leave the parent shell reading a non-blocking stdin after `phux attach`
//! exits — the classic way a crashed full-screen program wedges the shell that
//! ran it. A fresh open gets its own description; termios (raw mode,
//! `VMIN`/`VTIME`) is per *device*, so [`super::driver::RawModeGuard`] still
//! governs this handle exactly as it governs fd 0.
//!
//! It opens the terminal by its own device path (`ttyname` of fd 0) rather
//! than through `/dev/tty`. `/dev/tty` is a redirector, and on macOS the fd it
//! returns is not kqueue-registrable at all: `EVFILT_READ` on it is `EINVAL`,
//! while the same registration on the underlying pty succeeds. Going through
//! `/dev/tty` therefore demoted every attach on this platform straight back to
//! the blocking fallback, silently and with the fast path fully written.
//!
//! [`TtyInput::Blocking`] is the fallback for every environment where the
//! readiness path cannot be established — stdin is not a tty, the device will
//! not open, it resolves to a different device than fd 0, or the reactor
//! refuses the registration. It is the pre-existing behavior, unchanged.

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
    /// EOF has been reported once. Reads never complete again.
    ///
    /// A terminal that reached EOF stays at EOF, so a `read` on it returns
    /// `Ok(0)` immediately, forever. The attach loop's `select!` is `biased`
    /// with stdin first, so an arm that completes on every poll starves every
    /// arm below it — inbound frames, the paint deadline, and the
    /// SIGINT/SIGTERM/SIGHUP handlers that restore the terminal. The observed
    /// failure was a client spinning a core at 100% and ignoring `kill`,
    /// because the signal arms were never polled to see it.
    ///
    /// Latching here rather than in the driver keeps the invariant with the
    /// thing that owns it: whatever the loop does with the `Ok(0)` it is
    /// handed, it cannot re-arm an exhausted terminal by forgetting a flag.
    Closed,
}

impl TtyInput {
    /// Prefer the readiness path, falling back to blocking stdin.
    ///
    /// Must be called from inside the attach runtime: [`AsyncFd`] registers
    /// with the current reactor.
    pub(super) fn open() -> Self {
        if readiness_disabled() {
            tracing::debug!("PHUX_TTY_READINESS=0: using blocking stdin");
            return Self::Blocking(Box::new(tokio::io::stdin()));
        }
        match controlling_tty() {
            // Read interest only. `AsyncFd::new` would also register write
            // readiness, which this handle is opened `O_RDONLY` against and
            // never uses.
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

    /// Read one burst of input.
    ///
    /// Cancel-safe on every variant, which is what lets it sit in the attach
    /// loop's `select!`: the readiness path only awaits readiness (dropping
    /// the future loses no bytes, the fd stays readable), tokio's stdin
    /// buffers a blocking read that outlives its future, and a pending future
    /// carries no state at all.
    ///
    /// `Ok(0)` is EOF, exactly as for `Read::read` — reported EXACTLY ONCE.
    /// The caller still gets its one chance to detach cleanly; every later
    /// call parks forever so the exhausted terminal cannot spin the loop (see
    /// [`Self::Closed`]).
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

/// `PHUX_TTY_READINESS=0` forces the blocking fallback.
///
/// The readiness path depends on the platform's poller accepting a terminal
/// fd, which is exactly the kind of thing that is true until it is not — the
/// first cut of this module was silently demoted on macOS by a `/dev/tty` fd
/// kqueue refuses. The switch keeps the old path one environment variable
/// away, both as a support answer and as the control arm for measuring this
/// one against it in a single binary.
fn readiness_disabled() -> bool {
    std::env::var_os("PHUX_TTY_READINESS").is_some_and(|v| v == "0")
}

/// Park on readiness, then read on the runtime thread.
///
/// The loop re-parks on the one case `try_io` reports as spurious: the reactor
/// said readable and the read still answered `EWOULDBLOCK` (another reader
/// drained the queue first). `try_io` has cleared the readiness bit by then, so
/// the next `readable()` waits for a genuine event rather than spinning.
async fn read_when_ready(fd: &mut AsyncFd<OwnedFd>, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        let mut guard = fd.readable_mut().await?;
        match guard.try_io(|inner| read_uninterrupted(inner.get_ref(), buf)) {
            Ok(result) => return result,
            Err(_would_block) => {}
        }
    }
}

/// `read(2)` with `EINTR` retried.
///
/// The attach loop installs SIGWINCH/SIGINT/SIGTERM/SIGHUP handlers, so a
/// signal landing mid-read is routine; tokio's blocking stdin retries the same
/// way, and the interrupted read carried no bytes.
fn read_uninterrupted(fd: &OwnedFd, buf: &mut [u8]) -> io::Result<usize> {
    loop {
        match rustix::io::read(fd.as_fd(), buf) {
            Ok(n) => return Ok(n),
            Err(rustix::io::Errno::INTR) => {}
            Err(err) => return Err(io::Error::from(err)),
        }
    }
}

/// Open a private non-blocking handle to stdin's terminal, proving it really is
/// the same device.
///
/// `O_NOCTTY` because this open must never change which terminal controls the
/// process — it is a second view of the one we already have, not an
/// acquisition. The `st_rdev` check then closes the gap between naming the
/// device and opening it: raw mode was installed on fd 0's device, so reading
/// a *different* terminal would read a cooked, echoing one. Any mismatch — or
/// any error along the way — falls back to stdin.
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

    /// Under `cargo test` stdin is not a terminal, so the constructor must
    /// take the documented fallback rather than erroring or panicking. This is
    /// also the shape every non-tty embedder sees.
    #[tokio::test]
    async fn falls_back_to_blocking_stdin_without_a_tty() {
        assert!(matches!(TtyInput::open(), TtyInput::Blocking(_)));
    }

    /// The regression that motivated [`TtyInput::Closed`]: a terminal at EOF
    /// answers `Ok(0)` instantly and forever, and the attach loop's `select!`
    /// polls stdin FIRST (`biased`), so an arm that keeps completing starves
    /// the inbound-frame, paint-deadline and signal arms below it. A real
    /// client was found spinning a core at 100% and deaf to `kill` for an
    /// hour, because the SIGTERM arm was never reached.
    ///
    /// EOF must therefore be reported exactly once — the caller needs that one
    /// `Ok(0)` to send its `Detach` — and every read after it must park. The
    /// timeout is the assertion: before the latch this call returned `Ok(0)`
    /// immediately, which is precisely the spin.
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

    /// The device-identity guard is what makes a fresh `/dev/tty` open safe to
    /// substitute for fd 0. With stdin redirected away from a terminal there is
    /// nothing to match, and the helper must say so instead of handing back a
    /// handle to some unrelated terminal.
    #[test]
    fn no_private_handle_when_stdin_is_not_a_terminal() {
        assert!(controlling_tty().is_err());
    }
}
