//! Runtime events, classified once into the two product families a binding
//! actually encodes.
//!
//! A [`TerminalSignal`] is something one terminal did: it rang, it retitled,
//! it moved, a command started or finished, its history or its replica
//! changed state. A [`Lifecycle`] is something that happened to a terminal or
//! to the session as a whole: a pane appeared, a request was answered, a
//! process exited, the server detached or errored.
//!
//! Both classifiers take the event by value and hand it back unchanged when
//! it belongs to neither family, so a caller keeps its own dispatch order and
//! pays no clone. An encoder is free to ignore variants it has no vocabulary
//! for — the C ABI has no burst notion, the `UniFFI` surface has no replica
//! tombstones — but neither gets to decide what the runtime value *means*.

use phux_client_core::history::HistoryStatus;
use phux_client_core::session::HistoryUnavailableReason;
use phux_client_runtime::control::Event;
use phux_protocol::ResourceId;
use phux_protocol::wire::frame::{CloseReason, DetachReason, ErrorCode, TombstoneReason};

/// Something one terminal did.
#[derive(Debug, Clone)]
pub enum TerminalSignal {
    /// The terminal rang its bell.
    Bell {
        /// The terminal.
        terminal_id: ResourceId,
    },
    /// The terminal's title changed (OSC 0/2).
    TitleChanged {
        /// The terminal.
        terminal_id: ResourceId,
        /// The new title.
        title: String,
    },
    /// The terminal's working directory changed.
    CwdChanged {
        /// The terminal.
        terminal_id: ResourceId,
        /// The new working directory.
        cwd: String,
    },
    /// An output burst began (the server's coalesced `Dirty`).
    OutputStarted {
        /// The terminal.
        terminal_id: ResourceId,
    },
    /// Output settled (the server's coalesced `Idle`).
    OutputSettled {
        /// The terminal.
        terminal_id: ResourceId,
    },
    /// A shell command began (OSC 133 C).
    CommandStarted {
        /// The terminal.
        terminal_id: ResourceId,
    },
    /// A shell command finished (OSC 133 D).
    CommandFinished {
        /// The terminal.
        terminal_id: ResourceId,
        /// The exit code the shell integration reported, if any.
        exit_code: Option<i32>,
    },
    /// The replica generation was invalidated; fresh snapshots are needed.
    Resync {
        /// The terminal.
        terminal_id: ResourceId,
        /// The tombstone reason.
        reason: TombstoneReason,
    },
    /// Progressive-history loading state changed.
    History {
        /// The terminal.
        terminal_id: ResourceId,
        /// The cache's presentation state.
        status: HistoryStatus,
    },
    /// One history cursor chain ended; live state stays valid.
    HistoryUnavailable {
        /// The terminal.
        terminal_id: ResourceId,
        /// Why.
        reason: HistoryUnavailableReason,
    },
}

/// Something that happened to a terminal's existence or to the session.
#[derive(Debug, Clone)]
pub enum Lifecycle {
    /// A pane appeared mid-session; a topology refresh follows.
    PaneSpawned {
        /// The new terminal.
        terminal_id: ResourceId,
    },
    /// The server answered one of this client's spawns. Exactly one of
    /// `terminal_id` and `error` is set.
    SpawnAnswered {
        /// The spawn correlation.
        request_id: u32,
        /// The spawned terminal.
        terminal_id: Option<ResourceId>,
        /// Why the spawn failed.
        error: Option<String>,
    },
    /// The server answered one of this client's per-terminal attaches.
    AttachAnswered {
        /// The command correlation.
        request_id: u32,
        /// The terminal.
        terminal_id: ResourceId,
        /// Why the attach failed, if it did.
        error: Option<String>,
    },
    /// The server answered one of this client's per-terminal detaches.
    DetachAnswered {
        /// The command correlation.
        request_id: u32,
        /// The terminal.
        terminal_id: ResourceId,
        /// Why the detach failed, if it did.
        error: Option<String>,
    },
    /// The terminal is gone: its process ended, it was killed, or it left a
    /// fresh topology.
    Closed {
        /// The terminal.
        terminal_id: ResourceId,
        /// The process exit code, `None` for signals and unknown.
        exit_status: Option<i32>,
        /// The terminating signal, if any.
        signal: Option<i32>,
        /// Why it closed.
        reason: CloseReason,
    },
    /// The terminal's process exited; the resource may be retained
    /// (ADR-0124), so this is not [`Lifecycle::Closed`].
    Exited {
        /// The terminal.
        terminal_id: ResourceId,
        /// The process exit code, `None` for signals and unknown.
        exit_status: Option<i32>,
        /// The terminating signal, if any.
        signal: Option<i32>,
        /// Why it exited.
        reason: CloseReason,
    },
    /// The server ended the attach.
    Detached {
        /// The stated reason; `None` when unstated.
        reason: Option<DetachReason>,
        /// The message.
        message: String,
    },
    /// The server sent an `ERROR` frame.
    ServerError {
        /// The code.
        code: ErrorCode,
        /// The message.
        message: String,
        /// The request the error answers, if any.
        request_id: Option<u32>,
    },
}

/// Classify `event` as a terminal signal, or hand it back untouched.
///
/// # Errors
///
/// Returns the original event when it is not a terminal signal.
#[allow(
    clippy::result_large_err,
    reason = "the Err payload is the caller's own event handed back, not an error; boxing it would allocate once per drained event"
)]
pub fn terminal_signal(event: Event) -> Result<TerminalSignal, Event> {
    Ok(match event {
        Event::Bell { terminal_id } => TerminalSignal::Bell { terminal_id },
        Event::TitleChanged { terminal_id, title } => {
            TerminalSignal::TitleChanged { terminal_id, title }
        }
        Event::CwdChanged { terminal_id, cwd } => TerminalSignal::CwdChanged { terminal_id, cwd },
        Event::OutputStarted { terminal_id } => TerminalSignal::OutputStarted { terminal_id },
        Event::OutputSettled { terminal_id } => TerminalSignal::OutputSettled { terminal_id },
        Event::CommandStarted { terminal_id } => TerminalSignal::CommandStarted { terminal_id },
        Event::CommandFinished {
            terminal_id,
            exit_code,
        } => TerminalSignal::CommandFinished {
            terminal_id,
            exit_code,
        },
        Event::ResyncRequired {
            terminal_id,
            reason,
        } => TerminalSignal::Resync {
            terminal_id,
            reason,
        },
        Event::History {
            terminal_id,
            status,
        } => TerminalSignal::History {
            terminal_id,
            status,
        },
        Event::HistoryUnavailable {
            terminal_id,
            reason,
        } => TerminalSignal::HistoryUnavailable {
            terminal_id,
            reason,
        },
        other => return Err(other),
    })
}

/// Classify `event` as a lifecycle fact, or hand it back untouched.
///
/// # Errors
///
/// Returns the original event when it is not a lifecycle fact.
#[allow(
    clippy::result_large_err,
    reason = "the Err payload is the caller's own event handed back, not an error; boxing it would allocate once per drained event"
)]
pub fn lifecycle(event: Event) -> Result<Lifecycle, Event> {
    Ok(match event {
        Event::PaneSpawned { terminal_id } => Lifecycle::PaneSpawned { terminal_id },
        Event::TerminalSpawned {
            request_id,
            terminal_id,
            error,
        } => Lifecycle::SpawnAnswered {
            request_id,
            terminal_id,
            error,
        },
        Event::TerminalAttached {
            request_id,
            terminal_id,
            error,
        } => Lifecycle::AttachAnswered {
            request_id,
            terminal_id,
            error,
        },
        Event::TerminalDetached {
            request_id,
            terminal_id,
            error,
        } => Lifecycle::DetachAnswered {
            request_id,
            terminal_id,
            error,
        },
        Event::TerminalClosed {
            terminal_id,
            exit_status,
            signal,
            reason,
        } => Lifecycle::Closed {
            terminal_id,
            exit_status,
            signal,
            reason,
        },
        Event::Exited {
            terminal_id,
            exit_status,
            signal,
            reason,
        } => Lifecycle::Exited {
            terminal_id,
            exit_status,
            signal,
            reason,
        },
        Event::Detached { reason, message } => Lifecycle::Detached { reason, message },
        Event::ServerError {
            code,
            message,
            request_id,
        } => Lifecycle::ServerError {
            code,
            message,
            request_id,
        },
        other => return Err(other),
    })
}

#[cfg(test)]
mod tests {
    use super::{Lifecycle, TerminalSignal, lifecycle, terminal_signal};
    use phux_client_runtime::control::Event;
    use phux_protocol::ResourceId;
    use phux_protocol::wire::frame::CloseReason;

    #[test]
    fn bursts_are_terminal_signals() {
        let signal = terminal_signal(Event::OutputStarted {
            terminal_id: ResourceId::local(1),
        })
        .expect("output start is a signal");
        assert!(matches!(
            signal,
            TerminalSignal::OutputStarted { terminal_id } if terminal_id == ResourceId::local(1)
        ));
    }

    #[test]
    fn titles_and_cwds_keep_their_payload() {
        let signal = terminal_signal(Event::TitleChanged {
            terminal_id: ResourceId::local(2),
            title: "vim".to_owned(),
        })
        .expect("title is a signal");
        let TerminalSignal::TitleChanged { title, .. } = signal else {
            panic!("expected a title signal");
        };
        assert_eq!(title, "vim");

        let signal = terminal_signal(Event::CwdChanged {
            terminal_id: ResourceId::local(2),
            cwd: "/tmp".to_owned(),
        })
        .expect("cwd is a signal");
        let TerminalSignal::CwdChanged { cwd, .. } = signal else {
            panic!("expected a cwd signal");
        };
        assert_eq!(cwd, "/tmp");
    }

    #[test]
    fn an_exit_is_not_a_close() {
        let exited = lifecycle(Event::Exited {
            terminal_id: ResourceId::local(3),
            exit_status: Some(2),
            signal: None,
            reason: CloseReason::Exited,
        })
        .expect("exit is a lifecycle fact");
        assert!(matches!(exited, Lifecycle::Exited { .. }));

        let closed = lifecycle(Event::TerminalClosed {
            terminal_id: ResourceId::local(3),
            exit_status: Some(2),
            signal: None,
            reason: CloseReason::Exited,
        })
        .expect("close is a lifecycle fact");
        assert!(matches!(closed, Lifecycle::Closed { .. }));
    }

    #[test]
    fn spawn_answers_carry_their_correlation() {
        let answered = lifecycle(Event::TerminalSpawned {
            request_id: 9,
            terminal_id: Some(ResourceId::local(4)),
            error: None,
        })
        .expect("spawn answer is a lifecycle fact");
        let Lifecycle::SpawnAnswered {
            request_id,
            terminal_id,
            error,
        } = answered
        else {
            panic!("expected a spawn answer");
        };
        assert_eq!(request_id, 9);
        assert_eq!(terminal_id, Some(ResourceId::local(4)));
        assert_eq!(error, None);
    }

    #[test]
    fn unrelated_events_are_handed_back() {
        let event = Event::TerminalChanged {
            terminal_id: ResourceId::local(5),
        };
        let event = terminal_signal(event).expect_err("not a signal");
        let event = lifecycle(event).expect_err("not a lifecycle fact");
        assert!(matches!(event, Event::TerminalChanged { .. }));
    }

    #[test]
    fn the_two_families_do_not_overlap() {
        let signals = [
            Event::Bell {
                terminal_id: ResourceId::local(6),
            },
            Event::CommandStarted {
                terminal_id: ResourceId::local(6),
            },
        ];
        for event in signals {
            let event = lifecycle(event).expect_err("a signal is not a lifecycle fact");
            assert!(terminal_signal(event).is_ok());
        }
    }
}
