//! Journal cursors (ADR-0123): the position a resumable observer reached.
//!
//! A cursor is `(HELLO_OK.server_id, seq)`, spelled `server_id_hex:seq`. It
//! is meaningful only to the server incarnation that issued it: a cursor
//! from another `server_id` is void, and the observer falls back to a level
//! read (`docs/spec/L1.md` §7.3). A cursor never replaces that level read; it
//! only lets a reconnecting observer replay events the journal still holds,
//! such as the close of a pane that was not retained.

use std::fmt;
use std::str::FromStr;

use phux_protocol::caps::ServerFeature;

use crate::attach::connection::Connection;

/// `SUBSCRIBE_EVENTS.after_seq` asking for journal semantics with no replay
/// (L1 §7.3): a server never assigns it.
pub const NO_REPLAY: u64 = u64::MAX;

/// A position in one server incarnation's event journal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    server_id: Vec<u8>,
    seq: u64,
}

impl Cursor {
    /// A cursor at `seq` in the journal of the incarnation `server_id`.
    /// `None` for an empty `server_id`: a server that names no incarnation
    /// issues no cursor.
    #[must_use]
    pub fn new(server_id: Vec<u8>, seq: u64) -> Option<Self> {
        (!server_id.is_empty()).then_some(Self { server_id, seq })
    }

    /// The server incarnation this cursor belongs to.
    #[must_use]
    pub fn server_id(&self) -> &[u8] {
        &self.server_id
    }

    /// The last journal sequence the observer accounted for.
    #[must_use]
    pub const fn seq(&self) -> u64 {
        self.seq
    }
}

impl fmt::Display for Cursor {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in &self.server_id {
            write!(f, "{byte:02x}")?;
        }
        write!(f, ":{}", self.seq)
    }
}

/// Why a string is not a cursor.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("`{input}` is not a cursor (want SERVER_ID_HEX:SEQ, as printed by a previous run)")]
pub struct CursorParseError {
    input: String,
}

impl FromStr for Cursor {
    type Err = CursorParseError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let invalid = || CursorParseError {
            input: input.to_owned(),
        };
        let (id_hex, seq) = input.rsplit_once(':').ok_or_else(invalid)?;
        let seq = seq.parse::<u64>().map_err(|_| invalid())?;
        let server_id = decode_hex(id_hex).ok_or_else(invalid)?;
        Self::new(server_id, seq).ok_or_else(invalid)
    }
}

fn decode_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) || !text.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).ok())
        .collect()
}

/// What a subscription asks the journal for, given the caller's cursor and
/// the connection it will subscribe on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ResumePlan {
    /// The `SUBSCRIBE_EVENTS.after_seq` to send: the cursor's `seq` to
    /// replay after it, [`NO_REPLAY`] for journal semantics with no replay,
    /// or `None` (live only) on a server with no journal.
    pub after_seq: Option<u64>,
    /// The cursor's `seq` when the subscription resumes from it.
    pub position: Option<u64>,
    /// The caller passed a cursor this connection cannot honor: another
    /// incarnation, or a server without `EVENT_JOURNAL`. The observer falls
    /// back to its level read, which is still correct.
    pub void: bool,
}

/// Plan a resume of `cursor` on `conn`.
#[must_use]
pub fn plan_resume(cursor: Option<&Cursor>, conn: &Connection) -> ResumePlan {
    let journal = journal_advertised(conn);
    let live = ResumePlan {
        after_seq: journal.then_some(NO_REPLAY),
        position: None,
        void: false,
    };
    let Some(cursor) = cursor else {
        return live;
    };
    if journal && conn.server_id() == Some(cursor.server_id()) {
        ResumePlan {
            after_seq: Some(cursor.seq()),
            position: Some(cursor.seq()),
            void: false,
        }
    } else {
        ResumePlan { void: true, ..live }
    }
}

/// The cursor an observer on `conn` holds after journal sequence `last_seq`.
///
/// A `last_seq` of `None` means nothing accounted for yet, so the cursor
/// replays whatever the journal still holds. `None` when the server keeps no
/// journal or names no incarnation.
#[must_use]
pub fn cursor_after(conn: &Connection, last_seq: Option<u64>) -> Option<Cursor> {
    if !journal_advertised(conn) {
        return None;
    }
    Cursor::new(conn.server_id()?.to_vec(), last_seq.unwrap_or(0))
}

/// A resumable observer's cursor bookkeeping: what it asked the journal for,
/// what it has accounted for since, and the cursor that resumes it.
///
/// Held by the caller, outside the future that streams, so a run cut short
/// (a deadline, Ctrl-C, an error) still reports the position it reached. A
/// second [`Self::bind`] (a reconnect) resumes from that position.
#[derive(Debug, Clone, Default)]
pub struct ResumeState {
    /// The caller's cursor, echoed back if the observer never connects.
    after: Option<Cursor>,
    incarnation: Incarnation,
    last_seq: Option<u64>,
    void: bool,
}

/// Which journal a resumable observer's position belongs to.
#[derive(Debug, Clone, Default)]
enum Incarnation {
    /// Not connected yet: the caller's cursor is still the answer.
    #[default]
    Unbound,
    /// The server keeps no journal, or names no incarnation: no cursor.
    NoJournal,
    /// The journal of the server incarnation with this id.
    Journal(Vec<u8>),
}

impl ResumeState {
    /// Start from the caller's cursor, if any.
    #[must_use]
    pub const fn new(after: Option<Cursor>) -> Self {
        Self {
            after,
            incarnation: Incarnation::Unbound,
            last_seq: None,
            void: false,
        }
    }

    /// Plan the subscription on `conn` and return its `after_seq`. The
    /// first bind resumes from the caller's cursor; a later one (a
    /// reconnect) from the position reached since.
    pub fn bind(&mut self, conn: &Connection) -> Option<u64> {
        let first = matches!(self.incarnation, Incarnation::Unbound);
        let from = if first {
            self.after.clone()
        } else {
            self.cursor()
        };
        let plan = plan_resume(from.as_ref(), conn);
        if first {
            self.void = plan.void;
        }
        if plan.void {
            // Another incarnation's position means nothing here.
            self.last_seq = None;
        }
        if let Some(position) = plan.position {
            self.note(position);
        }
        self.incarnation = cursor_after(conn, None).map_or(Incarnation::NoJournal, |cursor| {
            Incarnation::Journal(cursor.server_id().to_vec())
        });
        plan.after_seq
    }

    /// Record that everything through journal sequence `seq` is accounted
    /// for: delivered, reported missing, or covered by a level read.
    pub fn note(&mut self, seq: u64) {
        self.last_seq = Some(self.last_seq.map_or(seq, |seen| seen.max(seq)));
    }

    /// The cursor that resumes this observer. `None` on a server with no
    /// journal.
    #[must_use]
    pub fn cursor(&self) -> Option<Cursor> {
        match &self.incarnation {
            Incarnation::Unbound => self.after.clone(),
            Incarnation::NoJournal => None,
            Incarnation::Journal(server_id) => {
                Cursor::new(server_id.clone(), self.last_seq.unwrap_or(0))
            }
        }
    }

    /// Whether the caller's cursor could not be honored (another server
    /// incarnation, or no journal), so the observer started live.
    #[must_use]
    pub const fn cursor_void(&self) -> bool {
        self.void
    }
}

fn journal_advertised(conn: &Connection) -> bool {
    conn.negotiated_bootstrap().is_some_and(|negotiated| {
        negotiated
            .server_features
            .contains(ServerFeature::EventJournal)
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    #[test]
    fn a_cursor_round_trips_through_its_printed_form() {
        let cursor = Cursor::new(vec![0x00, 0xab, 0x10], 42).unwrap();
        assert_eq!(cursor.to_string(), "00ab10:42");
        assert_eq!("00ab10:42".parse::<Cursor>().unwrap(), cursor);
    }

    #[test]
    fn malformed_cursors_are_refused() {
        for bad in ["", "abc", ":4", "zz:1", "abc:1", "ab:", "ab:-1", "ab:x"] {
            assert!(bad.parse::<Cursor>().is_err(), "{bad:?} parsed");
        }
        assert!(Cursor::new(Vec::new(), 1).is_none());
    }

    #[test]
    fn noting_keeps_the_furthest_position() {
        let mut state = ResumeState::new(None);
        state.note(9);
        state.note(4);
        assert_eq!(state.last_seq, Some(9));
    }
}
