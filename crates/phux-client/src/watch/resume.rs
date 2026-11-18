//! Bounded, cursor-aware and live-only scoped watches.

use std::path::Path;
use std::time::Duration;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::{FrameKind, ResourceLifecycle};

use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::deadline::Deadline;
use crate::resource::cursor::{NO_REPLAY, ResumeState};
use crate::selector::format_terminal_id;
use crate::state::get_state_on_with_interleaved;
use crate::wait::FIRST_READ_FLOOR;

use super::{WatchEvent, WatchItem, send_subscriptions, stream_items};

/// How a [`watch_resumable`] run ended: the gate was met, it had not been
/// met yet, or it can no longer be met on this stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchOutcome {
    /// The sink returned `false` (after being handed the matching item).
    Stopped,
    /// `timeout` elapsed with the sink still asking for more.
    TimedOut,
    /// The server closed the stream before the sink asked to stop.
    Ended,
}

/// A Terminal-scoped watch under an optional deadline that resumes from a
/// journal cursor and records the one it reaches (ADR-0123).
///
/// The deadline starts before the connect and bounds the whole run, but
/// setting up the subscription (connect, cursor anchor, subscribe) always
/// gets [`FIRST_READ_FLOOR`] from that start, the floor `wait` gives its
/// first read: a zero timeout means "subscribe once, now" and still reports
/// a cursor, instead of racing its own connect, while a wedged server still
/// times out.
///
/// `resume` lives outside the future, so a caller that drops it
/// on Ctrl-C can still print where the stream stopped; a foreign cursor
/// starts the watch live and sets `resume.cursor_void()`.
///
/// # Errors
///
/// [`AttachError`] on connect/transport/protocol failure; a clean EOF is
/// [`WatchOutcome::Ended`].
pub async fn watch_resumable<F>(
    socket: &Path,
    terminal: ResourceId,
    resume: &mut ResumeState,
    timeout: Option<Duration>,
    mut sink: F,
) -> Result<WatchOutcome, AttachError>
where
    F: FnMut(WatchItem) -> bool,
{
    let deadline = Deadline::new(timeout);
    let subscribing = subscribe_resumable(socket, terminal, resume, false);
    let Some(subscribed) = deadline.floored(FIRST_READ_FLOOR).run(subscribing).await else {
        return Ok(WatchOutcome::TimedOut);
    };
    let mut conn = subscribed?;
    let mut stopped = false;
    let stream = stream_items(&mut conn, |item| {
        if let Some(seq) = event_seq(&item) {
            resume.note(seq);
        }
        let keep_going = sink(item);
        stopped |= !keep_going;
        keep_going
    });
    let finished = deadline.run(stream).await.transpose()?;
    // The connection is the subscription: it ends with the stream.
    drop(conn);
    Ok(watch_outcome(finished.is_none(), stopped))
}

/// A live-only Terminal watch for journal events, not lossy metadata
/// notifications, for a consumer with side effects. The start
/// snapshot must still contain the Terminal and a journal head to cover
/// the race between that snapshot and subscribing. This
/// never resumes a previous run, reconnects, or exposes a resume cursor.
/// The sink is awaited before reading the next item.
///
/// # Errors
///
/// [`AttachError`] on connect/transport/protocol failure or a vanished or
/// non-Terminal target.
pub async fn watch_live_async(
    socket: &Path,
    terminal: ResourceId,
    mut sink: impl AsyncFnMut(WatchEvent) -> bool,
) -> Result<WatchOutcome, AttachError> {
    let mut resume = ResumeState::default();
    let mut conn = subscribe_resumable(socket, terminal, &mut resume, true).await?;
    loop {
        match conn.recv().await {
            Ok(FrameKind::Event {
                terminal,
                event,
                stamp,
            }) => {
                if !sink(WatchEvent::from_frame(terminal, event, stamp)).await {
                    return Ok(WatchOutcome::Stopped);
                }
            }
            Ok(_) => {}
            Err(AttachError::Disconnected) => return Ok(WatchOutcome::Ended),
            Err(err) => return Err(err),
        }
    }
}

/// Connect and subscribe from `resume`'s cursor, anchoring a live start at
/// the journal head so even a run that sees no event has a cursor.
async fn subscribe_resumable(
    socket: &Path,
    terminal: ResourceId,
    resume: &mut ResumeState,
    require_live: bool,
) -> Result<Connection, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    let mut after_seq = resume.bind(&conn);
    if after_seq == Some(NO_REPLAY) {
        let check_target = require_live.then_some(&terminal);
        after_seq = anchor_live_start(&mut conn, resume, check_target)
            .await?
            .or(after_seq);
    }
    if require_live {
        conn.send(&FrameKind::SubscribeEvents {
            terminal: Some(terminal),
            after_seq,
        })
        .await?;
    } else {
        send_subscriptions(&mut conn, Some(terminal), after_seq).await?;
    }
    Ok(conn)
}

/// Anchor a live start at the journal head, as `resource wait` does: a run
/// that sees no event then reports the head as its cursor rather than
/// `SERVER_ID:0`, whose resume would replay every event from before the
/// watch began. The subscription replays from that head, so an event landing
/// between this read and the subscribe is delivered, not skipped. `None`
/// (subscribe live, as before) when the server names no head.
async fn anchor_live_start(
    conn: &mut Connection,
    resume: &mut ResumeState,
    check_target: Option<&ResourceId>,
) -> Result<Option<u64>, AttachError> {
    let (view, _interleaved) = match get_state_on_with_interleaved(conn).await {
        Ok(result) => result,
        Err(err) if check_target.is_some() => return Err(err),
        Err(_) => return Ok(None),
    };
    let snapshot = view.snapshot();
    let head = snapshot.journal_head();
    if let Some(id) = check_target {
        ensure_live_target(snapshot, id)?;
        if head.is_none() {
            return Err(AttachError::Refused(
                "live reactions require a server event journal".to_owned(),
            ));
        }
    }
    if let Some(head) = head {
        resume.note(head);
    }
    Ok(head)
}

fn ensure_live_target(
    snapshot: &phux_protocol::wire::info::SessionSnapshot,
    id: &ResourceId,
) -> Result<(), AttachError> {
    if snapshot.resources.iter().any(|entry| {
        entry.id == *id && entry.kind.is_terminal() && entry.lifecycle != ResourceLifecycle::Exited
    }) {
        return Ok(());
    }
    Err(AttachError::Refused(format!(
        "{} is not a live pane",
        format_terminal_id(id)
    )))
}

/// The journal sequence of a stamped event item.
const fn event_seq(item: &WatchItem) -> Option<u64> {
    match item {
        WatchItem::Event(WatchEvent {
            stamp: Some(stamp), ..
        }) => Some(stamp.seq),
        _ => None,
    }
}

/// Which of the three endings a bounded run reached.
const fn watch_outcome(expired: bool, stopped: bool) -> WatchOutcome {
    if expired {
        WatchOutcome::TimedOut
    } else if stopped {
        WatchOutcome::Stopped
    } else {
        WatchOutcome::Ended
    }
}
