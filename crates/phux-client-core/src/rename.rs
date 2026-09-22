//! One session-rename policy for every frontend.
//!
//! A rename is a `SET_METADATA` of `phux.session.name/v1` (`current\0new`).
//! The server applies it and broadcasts nothing when it refuses, and
//! `SET_METADATA` has no reply. Every client therefore does the same three
//! steps: judge the request against the session list it already trusts, send
//! the write only when that list allows it, then read `GET_STATE` as an
//! ordering barrier whose snapshot is the outcome.
//!
//! This module is synchronous on purpose. `phux_client::session::rename_checked`
//! performs the round trip for the CLI and MCP, on a connection that can
//! block for the reply. The TUI attach loop and the FFI bridge cannot: the
//! attach connection is full-duplex, and the bridge queues frames. Both call
//! these functions and correlate the barrier themselves. `phux-client`
//! re-exports this module as `phux_client::rename`.

use phux_protocol::ids::SessionId;
use phux_protocol::wire::frame::{
    Command, FrameKind, SESSION_NAME_KEY, Scope, StateScope, encode_session_rename,
};

/// A session the rename policy can see: its stable id and its current name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedSession<'a> {
    /// Stable session id. The barrier matches on this, not on the old name.
    pub id: SessionId,
    /// Human-readable name from the snapshot or the local roster.
    pub name: &'a str,
}

/// Why a rename must not be sent.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RenameRefusal {
    /// `current` is not in the session list. Session names are hub-local.
    #[error("no such session")]
    NoSuchSession,
    /// `new_name` is already held by another session.
    #[error("{new_name:?} already exists")]
    AlreadyExists {
        /// The name that collided.
        new_name: String,
    },
}

/// What to do with a rename, judged before any write is sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RenamePlan {
    /// `current` already has `new_name`. Nothing to send.
    Unchanged {
        /// The session that already carries the name.
        session_id: SessionId,
    },
    /// Do not send the write.
    Refused(RenameRefusal),
    /// Send the write, then a `GET_STATE` barrier.
    Send {
        /// The session the write names. The barrier looks this id up.
        session_id: SessionId,
    },
}

/// Judge `current` → `new_name` against `sessions`.
///
/// An unknown current name is refused. A new name another session already
/// holds is refused. Renaming a session to the name it already has does not
/// send. Anything else may be written.
#[must_use]
pub fn plan_rename(sessions: &[NamedSession<'_>], current: &str, new_name: &str) -> RenamePlan {
    let Some(session) = sessions.iter().find(|session| session.name == current) else {
        return RenamePlan::Refused(RenameRefusal::NoSuchSession);
    };
    if current == new_name {
        return RenamePlan::Unchanged {
            session_id: session.id,
        };
    }
    if sessions
        .iter()
        .any(|other| other.name == new_name && other.id != session.id)
    {
        return RenamePlan::Refused(RenameRefusal::AlreadyExists {
            new_name: new_name.to_owned(),
        });
    }
    RenamePlan::Send {
        session_id: session.id,
    }
}

/// The write, then the ordering-barrier read, in that order.
///
/// Callers that own a request-id space pass the two ids they will correlate.
/// `phux_client::session::rename_checked` sends the same two frames through
/// its connection helper, which allocates its own ids.
#[must_use]
pub fn rename_frames(
    write_id: u32,
    barrier_id: u32,
    current: &str,
    new_name: &str,
) -> (FrameKind, FrameKind) {
    (
        write_frame(write_id, current, new_name),
        FrameKind::Command {
            request_id: barrier_id,
            command: Command::GetState {
                scope: StateScope::Server,
            },
        },
    )
}

/// The `SET_METADATA` write alone: `current\0new` under [`SESSION_NAME_KEY`].
#[must_use]
pub fn write_frame(request_id: u32, current: &str, new_name: &str) -> FrameKind {
    FrameKind::SetMetadata {
        request_id,
        scope: Scope::Global,
        key: SESSION_NAME_KEY.to_owned(),
        value: encode_session_rename(current, new_name),
    }
}

/// What the barrier snapshot says about the session that was renamed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BarrierVerdict {
    /// That session's name is now `new_name`.
    Applied,
    /// That session id is no longer in the snapshot.
    Gone,
    /// The session is still there, under some other name.
    NotApplied,
}

impl BarrierVerdict {
    /// The refusal sentence a frontend shows when the barrier did not apply.
    ///
    /// `None` when the rename [applied](Self::Applied).
    #[must_use]
    pub const fn refusal_reason(self) -> Option<&'static str> {
        match self {
            Self::Applied => None,
            Self::Gone => Some("the session no longer exists"),
            Self::NotApplied => Some(
                "the server did not rename the session (the name may have been taken meanwhile)",
            ),
        }
    }
}

/// Read the barrier snapshot: the session id from [`RenamePlan::Send`] either
/// carries `new_name`, is missing, or still has another name.
#[must_use]
pub fn barrier_verdict(
    sessions: &[NamedSession<'_>],
    session_id: SessionId,
    new_name: &str,
) -> BarrierVerdict {
    let Some(session) = sessions.iter().find(|session| session.id == session_id) else {
        return BarrierVerdict::Gone;
    };
    if session.name == new_name {
        BarrierVerdict::Applied
    } else {
        BarrierVerdict::NotApplied
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roster<'a>(rows: &'a [(u32, &'a str)]) -> Vec<NamedSession<'a>> {
        rows.iter()
            .map(|(id, name)| NamedSession {
                id: SessionId::new(*id),
                name,
            })
            .collect()
    }

    #[test]
    fn plan_refuses_an_unknown_session_and_a_taken_name() {
        let sessions = roster(&[(1, "work"), (2, "play")]);
        assert_eq!(
            plan_rename(&sessions, "gone", "x"),
            RenamePlan::Refused(RenameRefusal::NoSuchSession)
        );
        assert_eq!(
            plan_rename(&sessions, "work", "play"),
            RenamePlan::Refused(RenameRefusal::AlreadyExists {
                new_name: "play".to_owned()
            })
        );
        assert_eq!(
            RenameRefusal::AlreadyExists {
                new_name: "play".to_owned()
            }
            .to_string(),
            "\"play\" already exists"
        );
        assert_eq!(RenameRefusal::NoSuchSession.to_string(), "no such session");
    }

    #[test]
    fn plan_sends_a_free_name_and_skips_a_no_op() {
        let sessions = roster(&[(7, "work"), (8, "play")]);
        assert_eq!(
            plan_rename(&sessions, "work", "notes"),
            RenamePlan::Send {
                session_id: SessionId::new(7)
            }
        );
        assert_eq!(
            plan_rename(&sessions, "work", "work"),
            RenamePlan::Unchanged {
                session_id: SessionId::new(7)
            }
        );
    }

    #[test]
    fn frames_are_the_write_then_a_server_get_state() {
        let (write, barrier) = rename_frames(5, 6, "work", "notes");
        assert_eq!(write, write_frame(5, "work", "notes"));
        assert_eq!(
            write,
            FrameKind::SetMetadata {
                request_id: 5,
                scope: Scope::Global,
                key: SESSION_NAME_KEY.to_owned(),
                value: b"work\0notes".to_vec(),
            }
        );
        assert_eq!(
            barrier,
            FrameKind::Command {
                request_id: 6,
                command: Command::GetState {
                    scope: StateScope::Server,
                },
            }
        );
    }

    #[test]
    fn barrier_names_applied_missing_and_unchanged() {
        let applied = roster(&[(7, "notes"), (8, "play")]);
        assert_eq!(
            barrier_verdict(&applied, SessionId::new(7), "notes"),
            BarrierVerdict::Applied
        );
        assert_eq!(
            barrier_verdict(&applied, SessionId::new(7), "notes").refusal_reason(),
            None
        );

        let unchanged = roster(&[(7, "work"), (8, "play")]);
        assert_eq!(
            barrier_verdict(&unchanged, SessionId::new(7), "notes"),
            BarrierVerdict::NotApplied
        );
        assert!(
            barrier_verdict(&unchanged, SessionId::new(7), "notes")
                .refusal_reason()
                .unwrap_or_default()
                .contains("did not rename")
        );

        let gone = roster(&[(8, "play")]);
        assert_eq!(
            barrier_verdict(&gone, SessionId::new(7), "notes"),
            BarrierVerdict::Gone
        );
        assert_eq!(
            barrier_verdict(&gone, SessionId::new(7), "notes").refusal_reason(),
            Some("the session no longer exists")
        );
    }
}
