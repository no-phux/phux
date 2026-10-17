//! Peer sessions' arrangement records, read off the attach loop when the
//! server advertises `ORGANIZATION` (ADR-0161).
//!
//! The picker, fleet, and sidebar show other sessions' tabs. On an
//! `ORGANIZATION` server those tabs are each session's arrangement record,
//! read with `GET_ORGANIZATION` (for key-to-handle translation) and
//! `ARRANGEMENT_GET`. Those are round trips the attach loop must not wait
//! on, so a [`PeerReader`] task owns its own unsubscribed connection on the
//! attach's dial: the driver queues what it owes ([`PeerReads`]), the task
//! coalesces queued requests into one read, and the answer comes back on a
//! channel the driver's `select!` polls.

use std::collections::{HashMap, HashSet};

use phux_client::layout::wire::{HandleMap, from_protocol};
use phux_client::layout_ops::LayoutOpsError;
use phux_client::org::{self, OrgError};
use phux_protocol::ids::{SessionId, SessionKey};
use phux_protocol::wire::frame::OrgFilter;
use tokio::sync::mpsc;

use crate::attach::{Dial, connection::Connection};
use crate::layout::ArrangementDoc;

/// Request ids one read may use: its inventory pages, then one
/// `ARRANGEMENT_GET` per peer from [`ARRANGEMENT_GET_OFFSET`].
const REQUEST_ID_STRIDE: u32 = 4096;

/// Where a read's `ARRANGEMENT_GET`s start, past its inventory pages.
const ARRANGEMENT_GET_OFFSET: u32 = 64;

/// Which peer arrangement records are owed a read.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(in crate::attach) enum PeerReads {
    /// Nothing to read.
    #[default]
    Settled,
    /// The peers whose record changed (`ARRANGEMENT_CHANGED`).
    Sessions(HashSet<SessionKey>),
    /// Every peer: a sweep.
    All,
}

impl PeerReads {
    /// Owe a read of `session`; a pending sweep already covers it.
    pub(in crate::attach) fn add(&mut self, session: SessionKey) {
        match self {
            Self::All => {}
            Self::Sessions(sessions) => {
                sessions.insert(session);
            }
            Self::Settled => *self = Self::Sessions(HashSet::from([session])),
        }
    }

    /// Owe everything `other` owes as well.
    pub(in crate::attach) fn merge(&mut self, other: Self) {
        match other {
            Self::Settled => {}
            Self::All => *self = Self::All,
            Self::Sessions(sessions) => {
                for session in sessions {
                    self.add(session);
                }
            }
        }
    }
}

/// One finished read: what was asked, and what it found.
#[derive(Debug)]
pub(in crate::attach) struct PeerReadDone {
    /// The reads this answer covers; owed again if it failed.
    pub(in crate::attach) asked: PeerReads,
    /// Each read peer's arrangement in handle space, `None` when it has none
    /// (or this client may not read it); or why the read failed.
    pub(in crate::attach) reads: Result<HashMap<SessionId, Option<ArrangementDoc>>, String>,
}

/// The background task reading peer arrangements, and its two channels.
/// Dropping it stops the task.
pub(in crate::attach) struct PeerReader {
    requests: mpsc::UnboundedSender<PeerReads>,
    done: mpsc::UnboundedReceiver<PeerReadDone>,
    task: tokio::task::JoinHandle<()>,
}

impl std::fmt::Debug for PeerReader {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PeerReader").finish_non_exhaustive()
    }
}

impl Drop for PeerReader {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl PeerReader {
    /// Start the reader for every session but `ours`, dialing `dial` on its
    /// first read.
    pub(in crate::attach) fn spawn(dial: Dial, ours: SessionId) -> Self {
        let (requests, request_rx) = mpsc::unbounded_channel();
        let (done_tx, done) = mpsc::unbounded_channel();
        let task = tokio::spawn(run(dial, ours, request_rx, done_tx));
        Self {
            requests,
            done,
            task,
        }
    }

    /// Queue `reads`; nothing is sent for [`PeerReads::Settled`].
    pub(in crate::attach) fn request(&self, reads: PeerReads) {
        if reads != PeerReads::Settled {
            let _ = self.requests.send(reads);
        }
    }

    /// The next finished read. Pending forever once the task is gone.
    pub(in crate::attach) async fn next(&mut self) -> PeerReadDone {
        match self.done.recv().await {
            Some(done) => done,
            None => std::future::pending().await,
        }
    }
}

/// The next finished read of `reader`, or never when there is none.
pub(in crate::attach) async fn next_done(reader: Option<&mut PeerReader>) -> PeerReadDone {
    match reader {
        Some(reader) => reader.next().await,
        None => std::future::pending().await,
    }
}

async fn run(
    dial: Dial,
    ours: SessionId,
    mut requests: mpsc::UnboundedReceiver<PeerReads>,
    done: mpsc::UnboundedSender<PeerReadDone>,
) {
    let mut conn: Option<Connection> = None;
    let mut first_request_id: u32 = 1;
    while let Some(mut asked) = requests.recv().await {
        // Everything queued while the last read ran is answered by one read.
        while let Ok(more) = requests.try_recv() {
            asked.merge(more);
        }
        let only = match &asked {
            PeerReads::Settled => continue,
            PeerReads::All => None,
            PeerReads::Sessions(sessions) => Some(sessions.clone()),
        };
        let first = first_request_id;
        first_request_id = first_request_id.wrapping_add(REQUEST_ID_STRIDE);
        let reads = read_on(&dial, &mut conn, ours, only.as_ref(), first).await;
        if done.send(PeerReadDone { asked, reads }).is_err() {
            return;
        }
    }
}

/// One read on `conn`, connecting first when needed and dropping a
/// connection a transport failure broke.
async fn read_on(
    dial: &Dial,
    conn: &mut Option<Connection>,
    ours: SessionId,
    only: Option<&HashSet<SessionKey>>,
    first: u32,
) -> Result<HashMap<SessionId, Option<ArrangementDoc>>, String> {
    if conn.is_none() {
        *conn = Some(
            Connection::connect_dial(dial)
                .await
                .map_err(|err| err.to_string())?,
        );
    }
    let Some(live) = conn.as_mut() else {
        return Err("no control connection".to_owned());
    };
    let result = read_peers(live, ours, only, first).await;
    if matches!(
        result,
        Err(LayoutOpsError::Transport(_)
            | LayoutOpsError::Org(OrgError::Attach(_) | OrgError::OutcomeUnknown { .. }))
    ) {
        *conn = None;
    }
    result.map_err(|err| err.to_string())
}

/// Read the arrangements of every listed session but `ours`, in handle
/// space: one `GET_ORGANIZATION` (narrowed to the one session when `only`
/// names exactly one), then one `ARRANGEMENT_GET` per session. `only`, when
/// given, limits the reads to those keys. A session with no arrangement, or
/// one whose read the server refuses, maps to `None`; a session without a
/// live id is skipped.
///
/// # Errors
///
/// Transport failures, a refused inventory read, or a server error.
pub(in crate::attach) async fn read_peers(
    conn: &mut Connection,
    ours: SessionId,
    only: Option<&HashSet<SessionKey>>,
    first: u32,
) -> Result<HashMap<SessionId, Option<ArrangementDoc>>, LayoutOpsError> {
    let filter = match only.map(|only| only.iter().collect::<Vec<_>>()).as_deref() {
        Some([one]) => OrgFilter::Session(**one),
        _ => OrgFilter::All,
    };
    let inventory = org::fetch(conn, filter, first).await?;
    let mut request_id = first.wrapping_add(ARRANGEMENT_GET_OFFSET);
    let mut out = HashMap::new();
    for session in &inventory.sessions {
        let Some(live) = session.live.filter(|live| *live != ours) else {
            continue;
        };
        if only.is_some_and(|only| !only.contains(&session.key)) {
            continue;
        }
        let Some(key) = session.arrangement else {
            out.insert(live, None);
            continue;
        };
        request_id = request_id.wrapping_add(1);
        let document = match org::arrangement(conn, request_id, key).await {
            Ok(value) => {
                let handles = HandleMap::from_pairs(
                    inventory
                        .members(&session.key)
                        .map(|record| (record.key, record.live.clone())),
                );
                Some(from_protocol(&value.document, &handles).0)
            }
            // A refusal is about this one peer: it shows no cached tabs.
            Err(OrgError::Refused(_)) => None,
            Err(err) => return Err(err.into()),
        };
        out.insert(live, document);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

    use super::*;

    fn key(byte: u8) -> SessionKey {
        SessionKey::new([byte; 16]).unwrap()
    }

    #[test]
    fn owed_reads_coalesce_and_a_sweep_covers_every_peer() {
        let mut owed = PeerReads::default();
        owed.add(key(1));
        owed.merge(PeerReads::Sessions(HashSet::from([key(2)])));
        assert_eq!(owed, PeerReads::Sessions(HashSet::from([key(1), key(2)])));
        owed.merge(PeerReads::All);
        owed.add(key(3));
        assert_eq!(owed, PeerReads::All);
        owed.merge(PeerReads::Settled);
        assert_eq!(owed, PeerReads::All);
    }
}
