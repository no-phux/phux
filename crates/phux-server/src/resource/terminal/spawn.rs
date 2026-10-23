//! PTY spawn/adopt, the reader and writer bridge threads, and pane command
//! construction.

use super::{EncodedInputRequest, TerminalActorError, WriteCompletion};
use nix::sys::termios::{InputFlags, LocalFlags};
use nix::unistd::{PathconfVar, fpathconf};
use phux_core::process::ExitOutcome;
use portable_pty::{Child, CommandBuilder, MasterPty, PtySize, native_pty_system};
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use tokio::sync::mpsc;
use tracing::{debug, error, warn};

/// PTY-side resources of a [`TerminalActor`](crate::terminal_actor::TerminalActor).
///
/// Drop order is the reverse of declaration: writer thread (VEOF), reader
/// dup, child, then the master. Dropping the reader hangs up a master the
/// poller was still holding.
pub(crate) struct PtyOwned {
    /// Master handle, kept for resize ioctls and the writer's termios checks.
    pub(crate) master: Arc<Mutex<Box<dyn MasterPty + Send>>>,
    /// Child on the slave side, reaped in `TerminalActor::shutdown_pty`.
    pub(crate) child: Box<dyn Child + Send + Sync>,
    /// Quiet-poller registration, or a dedicated reader when the pane is hot.
    pub(super) reader: super::park::PtyReader,
    /// Writer thread; exits when its receiver closes.
    pub(crate) writer_thread: Option<JoinHandle<()>>,
    /// Inline-write gate shared with the receive task.
    pub(super) fast: Arc<super::fast_pty::PtyFastPath>,
}

impl std::fmt::Debug for PtyOwned {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PtyOwned")
            .field("child", &self.child)
            .finish_non_exhaustive()
    }
}

/// Events flowing from the PTY reader thread into the actor.
#[derive(Debug)]
pub(crate) enum PtyEvent {
    /// A chunk read from the master, refcounted for broadcast. `read_at`
    /// feeds the `pty.queue_wait` metric.
    Bytes {
        /// The bytes read.
        chunk: bytes::Bytes,
        /// When the read completed.
        read_at: std::time::Instant,
    },
    /// The PTY hit EOF or errored. Either way: the child is going away.
    Eof,
}

/// Map a `portable_pty::ExitStatus` to an [`ExitOutcome`].
///
/// Exit code, signal number, or neither when unknown. portable-pty reports signals by name
/// (`strsignal(3)` text, or `"signal N"` for adopted children), which
/// [`signal_number`] maps back.
pub(crate) fn exit_outcome(status: &portable_pty::ExitStatus) -> ExitOutcome {
    status.signal().map_or_else(
        || ExitOutcome::exited(i32::try_from(status.exit_code()).unwrap_or(i32::MAX)),
        |name| ExitOutcome {
            status: None,
            signal: signal_number(name),
        },
    )
}

/// The signal number behind a `portable_pty` signal name; `None` for a
/// non-signal name, never a guess.
fn signal_number(name: &str) -> Option<i32> {
    adopted_signal_number(name).or_else(|| {
        strsignal_names()
            .iter()
            .find(|(described, _)| described == name)
            .map(|(_, signal)| *signal)
    })
}

/// `portable-pty-adopt` renders a signal death as `"signal N"`.
fn adopted_signal_number(name: &str) -> Option<i32> {
    name.strip_prefix("signal ")?.parse().ok()
}

/// `portable_pty`'s name for each signal number, built by running its own
/// `ExitStatus` conversion over synthetic signal deaths, so names match this
/// platform exactly without a hand-kept table.
fn strsignal_names() -> &'static [(String, i32)] {
    use std::os::unix::process::ExitStatusExt;
    /// Covers the classic signals and Linux's real-time range.
    const HIGHEST_SIGNAL: i32 = 64;
    static NAMES: std::sync::OnceLock<Vec<(String, i32)>> = std::sync::OnceLock::new();
    NAMES.get_or_init(|| {
        (1..=HIGHEST_SIGNAL)
            .filter_map(|signal| {
                // Low seven bits = the terminating signal (WIFSIGNALED).
                let status = std::process::ExitStatus::from_raw(signal);
                let converted = portable_pty::ExitStatus::from(status);
                converted.signal().map(|name| (name.to_owned(), signal))
            })
            .collect()
    })
}

#[cfg(test)]
mod exit_outcome_tests;

#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod writer_tests;

#[cfg(test)]
mod tests;

/// Canonical-mode write guard (phux-mjmc): the pure predicate, then real-PTY
/// tests through the production writer thread.
#[cfg(test)]
#[allow(clippy::expect_used, reason = "tests")]
mod canonical_guard_tests;

#[cfg(test)]
mod drain_tests;

pub mod writer;
pub(super) use writer::*;
pub mod command;
pub use command::*;
pub mod bridge;
pub(crate) use bridge::*;
