//! Server-pushed watch stream (ADR-0022): `EVENT` frames plus
//! changes to each Terminal's `phux.agent/v1` record (ADR-0040 / ADR-0046),
//! on one connection that never attaches or resizes a pane.
//!
//! A scoped watch subscribes to one Terminal's events and record; a
//! server-wide watch subscribes to lifecycle events, enumerates local
//! Terminals, and follows spawns/closes to keep the record set current. An
//! additive accelerator of the [`crate::wait`] poll floor.

use std::collections::HashMap;
use std::path::Path;

use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::FrameKind;

use crate::agent_meta::AgentRecord;
use crate::attach::AttachError;
use crate::attach::connection::Connection;

mod event;
mod fleet;
mod metadata;
mod resume;
#[cfg(test)]
mod tests;

pub use event::{WatchEvent, event_json, event_name};
pub use fleet::{FleetSubscription, subscribe_fleet, subscribe_fleet_after};
pub use metadata::{CONTROLLER_EVENT, ControllerUpdate};
pub use resume::{WatchOutcome, watch_live_async, watch_resumable};

use fleet::stream_fleet_items;
use metadata::{agent_update, controller_update, subscribe_records};

/// One observed change to a Terminal's `phux.agent/v1` record.
///
/// `record` is `None` for a deletion or an unreadable value ("no declared
/// agent"), reported so a waiter learns the agent went away. `previous` is
/// the last record this watch session saw for the Terminal, not server state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStateUpdate {
    /// The Terminal whose record changed (always `Some` in practice).
    pub terminal: Option<ResourceId>,
    /// The new record, or `None` for a deletion / unreadable value.
    pub record: Option<AgentRecord>,
    /// The record this session last saw for the same Terminal, if any.
    pub previous: Option<AgentRecord>,
}

/// One item on the watch stream, in the order the server pushed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchItem {
    /// An `EVENT` frame.
    Event(WatchEvent),
    /// A `METADATA_CHANGED` frame for the `phux.agent/v1` key.
    AgentState(AgentStateUpdate),
    /// A `METADATA_CHANGED` frame for the `phux.controller/v1` key.
    Controller(ControllerUpdate),
}

/// Subscribe to the watch stream for `terminal` (server-wide via
/// [`subscribe_fleet`] when `None`) and hand every item to `sink` until it
/// returns `false` or the server closes.
///
/// # Errors
///
/// [`AttachError`] on connect/transport/protocol failure; a clean EOF is
/// `Ok(())`.
pub async fn watch_events<F>(
    socket: &Path,
    terminal: Option<ResourceId>,
    sink: F,
) -> Result<(), AttachError>
where
    F: FnMut(WatchItem) -> bool,
{
    if terminal.is_none() {
        let mut subscription = subscribe_fleet(socket).await?;
        return stream_fleet_items(&mut subscription, sink).await;
    }
    let mut conn = subscribe(socket, terminal).await?;
    stream_items(&mut conn, sink).await
}

/// Open a connection and register the watch subscriptions, before reading
/// anything.
///
/// A level read sent next on this same connection is answered
/// after the subscription registers, so no transition between the two is
/// lost (`phux agent wait`, ADR-0076 point 5).
///
/// # Errors
///
/// Returns [`AttachError`] on connect or send failure.
pub async fn subscribe(
    socket: &Path,
    terminal: Option<ResourceId>,
) -> Result<Connection, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    send_subscriptions(&mut conn, terminal, None).await?;
    Ok(conn)
}

/// Register the watch subscriptions on `conn`: events for `terminal` (from
/// journal sequence `after_seq` when resuming), plus its `phux.agent/v1`
/// record.
async fn send_subscriptions(
    conn: &mut Connection,
    terminal: Option<ResourceId>,
    after_seq: Option<u64>,
) -> Result<(), AttachError> {
    conn.send(&FrameKind::SubscribeEvents {
        terminal: terminal.clone(),
        after_seq,
    })
    .await?;
    if let Some(id) = terminal {
        subscribe_records(conn, &id).await?;
    }
    Ok(())
}

/// Stream [`WatchItem`]s off an already-[`subscribe`]d connection until
/// `sink` returns `false` or the transport closes.
///
/// # Errors
///
/// [`AttachError`] on transport/protocol failure; a clean EOF is `Ok(())`.
pub async fn stream_items<F>(conn: &mut Connection, mut sink: F) -> Result<(), AttachError>
where
    F: FnMut(WatchItem) -> bool,
{
    let mut last_seen: HashMap<ResourceId, AgentRecord> = HashMap::new();
    loop {
        match conn.recv().await {
            Ok(FrameKind::Event {
                terminal,
                event,
                stamp,
            }) => {
                if !sink(WatchItem::Event(WatchEvent::from_frame(
                    terminal, event, stamp,
                ))) {
                    return Ok(());
                }
            }
            Ok(FrameKind::MetadataChanged {
                scope, key, value, ..
            }) => {
                let item = controller_update(&scope, &key, value.as_deref())
                    .map(WatchItem::Controller)
                    .or_else(|| {
                        agent_update(&mut last_seen, &scope, &key, value.as_deref(), |_| true)
                            .map(WatchItem::AgentState)
                    });
                if item.is_some_and(|item| !sink(item)) {
                    return Ok(());
                }
            }
            Ok(_other) => {}
            // The server closed the stream: the normal end of a watch.
            Err(AttachError::Disconnected) => return Ok(()),
            Err(err) => return Err(err),
        }
    }
}
