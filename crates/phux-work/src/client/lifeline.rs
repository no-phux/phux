//! A wait bound to its caller's life.
//!
//! A harness runs `phux mail wait` with a stdin pipe it never writes. When
//! the harness dies, however abruptly, the pipe's write end closes and this
//! end turns readable. The wait then hangs up on the keeper before any mail
//! is taken, so an orphaned wait cannot take mail meant for the next one.

use std::io;
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::net::UnixStream;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags, Timespec};

use crate::error::Error;

/// How long a reply may take, and the caller whose exit abandons it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Patience<'a> {
    pub(super) reply: Duration,
    pub(super) lifeline: Option<BorrowedFd<'a>>,
}

/// Block until `stream` has a reply to read, or fail once `lifeline`
/// closes first or `reply` passes. A reply already waiting wins over a
/// closed lifeline: the caller then decides what to do with taken mail.
pub(super) fn await_reply(stream: &UnixStream, patience: Patience<'_>) -> Result<(), Error> {
    let Some(lifeline) = patience.lifeline else {
        return Ok(());
    };
    let deadline = Instant::now() + patience.reply;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(Error::Keeper("no reply before the deadline".to_owned()));
        }
        let mut fds = [
            PollFd::new(stream, PollFlags::IN),
            PollFd::from_borrowed_fd(lifeline.as_fd(), PollFlags::IN),
        ];
        match rustix::event::poll(&mut fds, Some(&timespec(remaining))) {
            Ok(0) | Err(rustix::io::Errno::INTR) => continue,
            Ok(_) => {}
            Err(err) => return Err(Error::Keeper(format!("wait: {}", io::Error::from(err)))),
        }
        if !fds[0].revents().is_empty() {
            return Ok(());
        }
        return Err(Error::Keeper(
            "the waiting caller exited; nothing was taken".to_owned(),
        ));
    }
}

fn timespec(duration: Duration) -> Timespec {
    Timespec {
        tv_sec: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
        tv_nsec: i64::from(duration.subsec_nanos()),
    }
}
