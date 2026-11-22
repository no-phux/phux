//! Fleet subscriptions follow local Terminal spawns and closes.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;

use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::wire::frame::{AgentEvent, FrameKind};

use crate::agent_meta::AgentRecord;
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::get_state_on_with_interleaved;

use super::metadata::{agent_update, controller_update, subscribe_records};
use super::{WatchEvent, WatchItem};

/// A server-wide event subscription plus one agent-record subscription per
/// local Terminal. Lifecycle events interleaved with the enumeration are
/// retained and replayed, closing the enumerate/follow race.
#[derive(Debug)]
pub struct FleetSubscription {
    pub(crate) conn: Connection,
    pub(crate) terminals: HashSet<ResourceId>,
    pending: VecDeque<FrameKind>,
    last_seen: HashMap<ResourceId, AgentRecord>,
}

impl FleetSubscription {
    pub(crate) async fn subscribe_terminal(
        &mut self,
        terminal: ResourceId,
    ) -> Result<bool, AttachError> {
        if !terminal.is_local() || !self.terminals.insert(terminal.clone()) {
            return Ok(false);
        }
        subscribe_records(&mut self.conn, &terminal).await?;
        Ok(true)
    }

    pub(crate) fn remove_terminal(&mut self, terminal: &ResourceId) {
        self.terminals.remove(terminal);
    }

    pub(crate) fn take_pending(&mut self) -> VecDeque<FrameKind> {
        std::mem::take(&mut self.pending)
    }

    /// The next watch item, or `None` when the server closes the stream.
    ///
    /// # Errors
    ///
    /// [`AttachError`] on a transport or protocol failure.
    pub async fn recv_item(&mut self) -> Result<Option<WatchItem>, AttachError> {
        loop {
            let frame = match self.pending.pop_front() {
                Some(frame) => Ok(frame),
                None => self.conn.recv().await,
            };
            match frame {
                Ok(FrameKind::Event {
                    terminal,
                    event,
                    stamp,
                }) => {
                    if let Some((id, spawned)) = event_lifecycle(&event, terminal.as_ref()) {
                        let id = id.clone();
                        if spawned {
                            self.subscribe_terminal(id).await?;
                        } else {
                            self.remove_terminal(&id);
                            self.last_seen.remove(&id);
                        }
                    }
                    return Ok(Some(WatchItem::Event(WatchEvent::from_frame(
                        terminal, event, stamp,
                    ))));
                }
                Ok(FrameKind::MetadataChanged {
                    scope, key, value, ..
                }) => {
                    if let Some(update) = controller_update(&scope, &key, value.as_deref()) {
                        return Ok(Some(WatchItem::Controller(update)));
                    }
                    let tracked = |id: &ResourceId| self.terminals.contains(id);
                    if let Some(update) =
                        agent_update(&mut self.last_seen, &scope, &key, value.as_deref(), tracked)
                    {
                        return Ok(Some(WatchItem::AgentState(update)));
                    }
                }
                Ok(_) => {}
                Err(AttachError::Disconnected) => return Ok(None),
                Err(err) => return Err(err),
            }
        }
    }
}

/// `(terminal, spawned)` when `event` spawns or closes a Terminal.
const fn event_lifecycle<'a>(
    event: &AgentEvent,
    terminal: Option<&'a ResourceId>,
) -> Option<(&'a ResourceId, bool)> {
    let Some(terminal) = terminal else {
        return None;
    };
    match event {
        AgentEvent::ResourceSpawned {
            kind: ResourceKind::Terminal,
            ..
        } => Some((terminal, true)),
        AgentEvent::ResourceClosed { .. } => Some((terminal, false)),
        _ => None,
    }
}

const fn lifecycle_change(frame: &FrameKind) -> Option<(&ResourceId, bool)> {
    match frame {
        FrameKind::Event {
            terminal, event, ..
        } => event_lifecycle(event, terminal.as_ref()),
        _ => None,
    }
}

/// Establish a race-free fleet-wide agent-state subscription on one ordered
/// connection: `SUBSCRIBE_EVENTS(Server)`, then `GET_STATE`, then one
/// `SUBSCRIBE_METADATA` per local Terminal.
///
/// # Errors
///
/// Returns [`AttachError`] on connect, enumeration, or subscription failure.
pub async fn subscribe_fleet(socket: &Path) -> Result<FleetSubscription, AttachError> {
    subscribe_fleet_after(socket, None).await
}

/// [`subscribe_fleet`] starting at journal sequence `after_seq`.
///
/// `None` follows live events. A cursor the server has already evicted is
/// answered with `journal_gap` before anything else.
///
/// # Errors
///
/// Returns [`AttachError`] on connect, enumeration, or subscription failure.
pub async fn subscribe_fleet_after(
    socket: &Path,
    after_seq: Option<u64>,
) -> Result<FleetSubscription, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    conn.send(&FrameKind::SubscribeEvents {
        terminal: None,
        after_seq,
    })
    .await?;
    let (view, interleaved) = get_state_on_with_interleaved(&mut conn).await?;
    let mut terminals: HashSet<ResourceId> = view
        .snapshot()
        .resources
        .iter()
        .filter(|resource| resource.kind == ResourceKind::Terminal && resource.id.is_local())
        .map(|resource| resource.id.clone())
        .collect();
    for frame in &interleaved {
        if let Some((terminal, spawned)) = lifecycle_change(frame) {
            if spawned && terminal.is_local() {
                terminals.insert(terminal.clone());
            } else if !spawned {
                terminals.remove(terminal);
            }
        }
    }
    let mut ordered: Vec<ResourceId> = terminals.iter().cloned().collect();
    ordered.sort();
    for terminal in ordered {
        subscribe_records(&mut conn, &terminal).await?;
    }
    Ok(FleetSubscription {
        conn,
        terminals,
        pending: interleaved.into(),
        last_seen: HashMap::new(),
    })
}

/// Stream a fleet subscription, extending it as local Terminals spawn and
/// pruning closed ones. A clean EOF is `Ok(())`.
pub(super) async fn stream_fleet_items<F>(
    subscription: &mut FleetSubscription,
    mut sink: F,
) -> Result<(), AttachError>
where
    F: FnMut(WatchItem) -> bool,
{
    loop {
        match subscription.recv_item().await? {
            Some(item) => {
                if !sink(item) {
                    return Ok(());
                }
            }
            None => return Ok(()),
        }
    }
}
