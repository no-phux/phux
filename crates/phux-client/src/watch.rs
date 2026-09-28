//! Server-pushed watch stream (SPEC §7.5, ADR-0022): `EVENT` frames plus
//! changes to each Terminal's `phux.agent/v1` record (ADR-0040 / ADR-0046),
//! on one connection that never attaches or resizes a pane.
//!
//! A scoped watch subscribes to one Terminal's events and record; a
//! server-wide watch subscribes to lifecycle events, enumerates local
//! Terminals, and follows spawns/closes to keep the record set current. An
//! additive accelerator of the [`crate::wait`] poll floor.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::Duration;

use phux_protocol::ids::{ClientId, ResourceId, ResourceKind};
use phux_protocol::wire::frame::{AgentEvent, EventStamp, FrameKind, Scope};
use serde_json::{Map, Value, json};

use crate::agent_meta::{AgentRecord, RESOURCE_AGENT_KEY, parse_agent_record};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::resource::cursor::ResumeState;
use crate::selector::format_terminal_id;
use crate::state::get_state_on_with_interleaved;

/// One streamed agent event plus the Terminal it concerns (`None` for a
/// server-scoped event).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    /// The Terminal the event concerns, or `None` if server-scoped.
    pub terminal: Option<ResourceId>,
    /// The event payload.
    pub event: AgentEvent,
    /// The journal stamp (ADR-0123); `None` without `EVENT_JOURNAL` and on a
    /// `journal_gap` notice.
    pub stamp: Option<EventStamp>,
}

impl WatchEvent {
    /// Unwrap one `EVENT` frame's fields.
    fn from_frame(
        terminal: Option<ResourceId>,
        event: AgentEvent,
        stamp: Option<Box<EventStamp>>,
    ) -> Self {
        Self {
            terminal,
            event,
            stamp: stamp.map(|stamp| *stamp),
        }
    }
}

/// The stable `event` name `phux watch --json`, `--until`, and MCP use; a
/// tag this build predates is `unknown`.
#[must_use]
pub const fn event_name(event: &AgentEvent) -> &'static str {
    match event {
        AgentEvent::CommandStarted => "command_started",
        AgentEvent::CommandFinished { .. } => "command_finished",
        AgentEvent::TitleChanged { .. } => "title_changed",
        AgentEvent::Bell => "bell",
        AgentEvent::ResourceSpawned { .. } => "pane_spawned",
        AgentEvent::ResourceClosed { .. } => "pane_closed",
        AgentEvent::Dirty => "dirty",
        AgentEvent::Idle => "idle",
        AgentEvent::Asked { .. } => "asked",
        AgentEvent::TerminalControl { .. } => "terminal_control",
        AgentEvent::CwdChanged { .. } => "cwd_changed",
        AgentEvent::JournalGap { .. } => "journal_gap",
        AgentEvent::SourceGap { .. } => "source_gap",
        AgentEvent::ApprovalRequested { .. } => "approval_requested",
        AgentEvent::ApprovalDecided { .. } => "approval_decided",
        _ => "unknown",
    }
}

/// One watch event as the JSON object `phux watch --json` and MCP print:
/// `event`, `terminal`, the payload fields, then the journal stamp.
#[must_use]
pub fn event_json(ev: &WatchEvent) -> Value {
    let mut obj = Map::new();
    obj.insert("event".to_owned(), Value::from(event_name(&ev.event)));
    if let Some(terminal) = &ev.terminal {
        obj.insert(
            "terminal".to_owned(),
            Value::from(format_terminal_id(terminal)),
        );
    }
    if let Value::Object(payload) = payload_json(&ev.event) {
        obj.extend(payload);
    }
    if let Some(stamp) = &ev.stamp {
        insert_stamp(&mut obj, stamp);
    }
    Value::Object(obj)
}

/// The payload fields of `event`, as a JSON object.
fn payload_json(event: &AgentEvent) -> Value {
    match event {
        AgentEvent::TitleChanged { title } => json!({ "title": title }),
        AgentEvent::CommandFinished { exit_code } => json!({ "exit_code": exit_code }),
        AgentEvent::ResourceClosed { exit_status } => json!({ "exit_status": exit_status }),
        // Additive (ADR-0102): the spawned resource's kind and parent, so a
        // consumer can tell a new pane from a new agent session bound to one.
        AgentEvent::ResourceSpawned { kind, parent } => json!({
            "kind": kind.as_str(),
            "parent": parent.as_ref().map(format_terminal_id),
        }),
        AgentEvent::Asked {
            id,
            question,
            suggestions,
            elapsed_seconds,
        } => json!({
            "id": id,
            "question": question,
            "suggestions": suggestions,
            "elapsed_seconds": elapsed_seconds,
        }),
        AgentEvent::TerminalControl { .. } => terminal_control_json(event),
        AgentEvent::CwdChanged { cwd } => json!({ "cwd": cwd }),
        AgentEvent::JournalGap {
            first_missing,
            last_missing,
        } => json!({ "first_missing": first_missing, "last_missing": last_missing }),
        AgentEvent::SourceGap { dropped } => json!({ "dropped": dropped }),
        // ADR-0128: the id names the `phux.approval/v1/<id>` record, which
        // holds the details while the action is pending.
        AgentEvent::ApprovalRequested { id } => json!({ "id": id.to_string() }),
        AgentEvent::ApprovalDecided { id, outcome } => {
            json!({ "id": id.to_string(), "outcome": outcome.as_str() })
        }
        AgentEvent::Unknown { tag, .. } => json!({ "tag": tag }),
        _ => json!({}),
    }
}

/// `terminal_control`'s payload. Its own connection id rides as
/// `actor_client`, so it cannot collide with the journal stamp's `actor`.
fn terminal_control_json(event: &AgentEvent) -> Value {
    let AgentEvent::TerminalControl {
        lifecycle,
        exit_status,
        input_holder,
        action,
        actor,
    } = event
    else {
        return json!({});
    };
    json!({
        "lifecycle": crate::resource::lifecycle_name(*lifecycle),
        "action": crate::resource::control_action_name(*action),
        "exit_status": exit_status,
        "input_holder": input_holder.map(ClientId::get),
        "actor_client": actor.map(ClientId::get),
    })
}

fn insert_stamp(obj: &mut Map<String, Value>, stamp: &EventStamp) {
    obj.insert("seq".to_owned(), Value::from(stamp.seq));
    obj.insert("ts_ms".to_owned(), Value::from(stamp.ts_ms));
    if let Some(actor) = &stamp.actor {
        obj.insert(
            "actor".to_owned(),
            json!({
                "client": actor.client.get(),
                "credential_id": actor.credential_id,
                "client_name": actor.client_name,
            }),
        );
    }
}

/// One observed change to a Terminal's `phux.agent/v1` record (L3 §3.7).
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
    /// An `EVENT` frame (SPEC §7.5).
    Event(WatchEvent),
    /// A `METADATA_CHANGED` frame for the `phux.agent/v1` key.
    AgentState(AgentStateUpdate),
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

/// A server-wide event subscription plus one agent-record subscription per
/// local Terminal. Lifecycle events interleaved with the enumeration are
/// retained and replayed, closing the enumerate/follow race.
#[derive(Debug)]
pub struct FleetSubscription {
    pub(crate) conn: Connection,
    pub(crate) terminals: HashSet<ResourceId>,
    pending: VecDeque<FrameKind>,
}

impl FleetSubscription {
    pub(crate) async fn subscribe_terminal(
        &mut self,
        terminal: ResourceId,
    ) -> Result<bool, AttachError> {
        if !terminal.is_local() || !self.terminals.insert(terminal.clone()) {
            return Ok(false);
        }
        self.conn
            .send(&FrameKind::SubscribeMetadata {
                scope: Scope::Resource(terminal),
                key: RESOURCE_AGENT_KEY.to_owned(),
            })
            .await?;
        Ok(true)
    }

    pub(crate) fn remove_terminal(&mut self, terminal: &ResourceId) {
        self.terminals.remove(terminal);
    }

    pub(crate) fn take_pending(&mut self) -> VecDeque<FrameKind> {
        std::mem::take(&mut self.pending)
    }
}

const fn lifecycle_change(frame: &FrameKind) -> Option<(&ResourceId, bool)> {
    let FrameKind::Event {
        terminal: Some(terminal),
        event,
        ..
    } = frame
    else {
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

/// Establish a race-free fleet-wide agent-state subscription on one ordered
/// connection: `SUBSCRIBE_EVENTS(Server)`, then `GET_STATE`, then one
/// `SUBSCRIBE_METADATA` per local Terminal.
///
/// # Errors
///
/// Returns [`AttachError`] on connect, enumeration, or subscription failure.
pub async fn subscribe_fleet(socket: &Path) -> Result<FleetSubscription, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    conn.send(&FrameKind::SubscribeEvents {
        terminal: None,
        after_seq: None,
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
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Resource(terminal),
            key: RESOURCE_AGENT_KEY.to_owned(),
        })
        .await?;
    }
    Ok(FleetSubscription {
        conn,
        terminals,
        pending: interleaved.into(),
    })
}

/// Stream a fleet subscription, extending it as local Terminals spawn and
/// pruning closed ones. A clean EOF is `Ok(())`.
async fn stream_fleet_items<F>(
    subscription: &mut FleetSubscription,
    mut sink: F,
) -> Result<(), AttachError>
where
    F: FnMut(WatchItem) -> bool,
{
    let mut last_seen: HashMap<ResourceId, AgentRecord> = HashMap::new();
    let mut pending = subscription.take_pending();
    loop {
        let frame = match pending.pop_front() {
            Some(frame) => Ok(frame),
            None => subscription.conn.recv().await,
        };
        match frame {
            Ok(frame @ FrameKind::Event { .. }) => {
                if let Some((terminal, spawned)) = lifecycle_change(&frame) {
                    let terminal = terminal.clone();
                    if spawned {
                        subscription.subscribe_terminal(terminal).await?;
                    } else {
                        subscription.remove_terminal(&terminal);
                        last_seen.remove(&terminal);
                    }
                }
                let FrameKind::Event {
                    terminal,
                    event,
                    stamp,
                } = frame
                else {
                    unreachable!();
                };
                if !sink(WatchItem::Event(WatchEvent::from_frame(
                    terminal, event, stamp,
                ))) {
                    return Ok(());
                }
            }
            Ok(FrameKind::MetadataChanged {
                scope, key, value, ..
            }) => {
                let tracked = |id: &ResourceId| subscription.terminals.contains(id);
                if let Some(update) =
                    agent_update(&mut last_seen, &scope, &key, value.as_deref(), tracked)
                    && !sink(WatchItem::AgentState(update))
                {
                    return Ok(());
                }
            }
            Ok(_) => {}
            Err(AttachError::Disconnected) => return Ok(()),
            Err(err) => return Err(err),
        }
    }
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
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Resource(id),
            key: RESOURCE_AGENT_KEY.to_owned(),
        })
        .await?;
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
                if let Some(update) =
                    agent_update(&mut last_seen, &scope, &key, value.as_deref(), |_| true)
                    && !sink(WatchItem::AgentState(update))
                {
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

/// Fold one `METADATA_CHANGED` into an [`AgentStateUpdate`] when it is a
/// Terminal-scoped `phux.agent/v1` record for a `tracked` Terminal,
/// remembering it in `last_seen` so the next update carries `previous`.
fn agent_update(
    last_seen: &mut HashMap<ResourceId, AgentRecord>,
    scope: &Scope,
    key: &str,
    value: Option<&[u8]>,
    tracked: impl FnOnce(&ResourceId) -> bool,
) -> Option<AgentStateUpdate> {
    if key != RESOURCE_AGENT_KEY {
        return None;
    }
    let Scope::Resource(id) = scope else {
        return None;
    };
    if !tracked(id) {
        return None;
    }
    let record = value.and_then(parse_agent_record);
    let previous = match &record {
        Some(new) => last_seen.insert(id.clone(), new.clone()),
        None => last_seen.remove(id),
    };
    Some(AgentStateUpdate {
        terminal: Some(id.clone()),
        record,
        previous,
    })
}

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

/// A Terminal-scoped watch under an optional deadline (covering the connect
/// too) that resumes from a journal cursor and records the one it reaches
/// (ADR-0123).
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
    let mut stopped = false;
    let stream = stream_resumable(socket, terminal, resume, |item| {
        let keep_going = sink(item);
        stopped |= !keep_going;
        keep_going
    });
    let expired = run_within(timeout, stream).await?;
    Ok(watch_outcome(expired, stopped))
}

/// Connect, subscribe from `resume`'s cursor, and stream items into `sink`,
/// noting each stamped event's journal sequence in `resume`.
#[allow(
    clippy::significant_drop_tightening,
    reason = "the connection is the subscription: it lives exactly as long as the stream"
)]
async fn stream_resumable<F>(
    socket: &Path,
    terminal: ResourceId,
    resume: &mut ResumeState,
    mut sink: F,
) -> Result<(), AttachError>
where
    F: FnMut(WatchItem) -> bool,
{
    let mut conn = Connection::connect(socket).await?;
    let after_seq = resume.bind(&conn);
    send_subscriptions(&mut conn, Some(terminal), after_seq).await?;
    stream_items(&mut conn, |item| {
        if let Some(seq) = event_seq(&item) {
            resume.note(seq);
        }
        sink(item)
    })
    .await
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

/// Run `stream` under an optional deadline: `Ok(true)` when the deadline
/// fired first, `Ok(false)` when the stream finished.
async fn run_within(
    timeout: Option<Duration>,
    stream: impl Future<Output = Result<(), AttachError>>,
) -> Result<bool, AttachError> {
    let Some(limit) = timeout else {
        return stream.await.map(|()| false);
    };
    match tokio::time::timeout(limit, stream).await {
        Ok(result) => result.map(|()| false),
        // Dropping the stream drops the connection and its subscription.
        Err(_elapsed) => Ok(true),
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

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests"
    )]

    use crate::agent_meta::AgentMetaState;
    use crate::testkit::{EndOfScript, ScriptSpec, serve_one};
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

    use super::*;

    fn agent_record(scope_terminal: &ResourceId, json: &str) -> FrameKind {
        FrameKind::MetadataChanged {
            scope: Scope::Resource(scope_terminal.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: Some(json.as_bytes().to_vec()),
            actor: None,
        }
    }

    /// Drive `watch_events` against the shared scripted server, collecting
    /// every streamed item and returning it alongside the frames the client
    /// actually sent.
    async fn drive(
        terminal: Option<ResourceId>,
        script: Vec<FrameKind>,
    ) -> (Vec<WatchItem>, Vec<FrameKind>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) = serve_one(
            dir.path(),
            ScriptSpec::new().extend(script).end(EndOfScript::HangUp),
        );
        let mut items = Vec::new();
        watch_events(&socket, terminal, |item| {
            items.push(item);
            true
        })
        .await
        .expect("a scripted hang-up is a clean EOF, not an error");
        let seen = server.await.expect("scripted server task");
        (items, seen)
    }

    /// A terminal-scoped watch asks for events and the `phux.agent/v1` key,
    /// and never attaches.
    #[tokio::test]
    async fn terminal_scoped_watch_subscribes_to_events_and_the_agent_key() {
        let pane = ResourceId::local(7);
        let (_items, seen) = drive(
            Some(pane.clone()),
            vec![agent_record(&pane, r#"{"name":"reviewer"}"#)],
        )
        .await;

        assert!(
            seen.iter().any(|f| matches!(
                f,
                FrameKind::SubscribeEvents { terminal: Some(id), .. } if *id == pane
            )),
            "watch must subscribe to events for the pane; sent {seen:?}"
        );
        assert!(
            seen.iter().any(|f| matches!(
                f,
                FrameKind::SubscribeMetadata { scope: Scope::Resource(id), key }
                    if *id == pane && key == RESOURCE_AGENT_KEY
            )),
            "watch must subscribe to the pane's phux.agent/v1 record; sent {seen:?}"
        );
        assert!(
            !seen.iter().any(|f| matches!(
                f,
                FrameKind::Attach { .. } | FrameKind::ViewportResize { .. }
            )),
            "watch must not attach or report a viewport; sent {seen:?}"
        );
    }

    #[tokio::test]
    async fn agent_records_stream_as_agent_state_items_carrying_the_previous_record() {
        let pane = ResourceId::local(7);
        let (items, _seen) = drive(
            Some(pane.clone()),
            vec![
                agent_record(
                    &pane,
                    r#"{"name":"reviewer","kind":"claude","state":"working"}"#,
                ),
                agent_record(
                    &pane,
                    r#"{"name":"reviewer","kind":"claude","state":"blocked"}"#,
                ),
            ],
        )
        .await;

        assert_eq!(items.len(), 2, "one item per record: {items:?}");
        let WatchItem::AgentState(first) = &items[0] else {
            panic!("expected an agent-state item, got {:?}", items[0]);
        };
        assert_eq!(first.terminal.as_ref(), Some(&pane));
        assert_eq!(
            first.record.as_ref().unwrap().state,
            AgentMetaState::Working
        );
        assert!(
            first.previous.is_none(),
            "the first record for a Terminal has nothing to transition from"
        );

        let WatchItem::AgentState(second) = &items[1] else {
            panic!("expected an agent-state item, got {:?}", items[1]);
        };
        assert_eq!(
            second.record.as_ref().unwrap().state,
            AgentMetaState::Blocked
        );
        assert_eq!(
            second.previous.as_ref().unwrap().state,
            AgentMetaState::Working,
            "the transition the consumer is waiting on"
        );
    }

    /// A tombstone and an unreadable value both stream as a cleared record
    /// (L3 §3.7), other keys are ignored, and events interleave in push order.
    #[tokio::test]
    async fn clears_other_keys_and_ordering_on_one_stream() {
        let pane = ResourceId::local(3);
        let (items, _seen) = drive(
            Some(pane.clone()),
            vec![
                agent_record(&pane, r#"{"name":"reviewer","state":"blocked"}"#),
                FrameKind::MetadataChanged {
                    scope: Scope::Resource(pane.clone()),
                    key: "phux.tui.layout/v1".to_owned(),
                    value: Some(b"{}".to_vec()),
                    actor: None,
                },
                FrameKind::Event {
                    terminal: Some(pane.clone()),
                    event: AgentEvent::Bell,
                    stamp: None,
                },
                FrameKind::MetadataChanged {
                    scope: Scope::Resource(pane.clone()),
                    key: RESOURCE_AGENT_KEY.to_owned(),
                    value: None,
                    actor: None,
                },
                agent_record(&pane, "not a record"),
            ],
        )
        .await;

        assert_eq!(items.len(), 4, "the foreign key is ignored: {items:?}");
        assert!(matches!(items[1], WatchItem::Event(_)));
        let WatchItem::AgentState(tombstone) = &items[2] else {
            panic!("expected an agent-state item, got {:?}", items[2]);
        };
        assert!(tombstone.record.is_none());
        assert_eq!(
            tombstone.previous.as_ref().unwrap().state,
            AgentMetaState::Blocked
        );
        let WatchItem::AgentState(unreadable) = &items[3] else {
            panic!("expected an agent-state item, got {:?}", items[3]);
        };
        assert!(unreadable.record.is_none());
    }

    // -- bounded gate ------------------------------------------------------

    /// Drive [`watch_resumable`] (no cursor) against the scripted server,
    /// stopping on the first item `accept` takes.
    async fn drive_bounded<P>(
        script: Vec<FrameKind>,
        end: EndOfScript,
        timeout: Option<Duration>,
        mut accept: P,
    ) -> (WatchOutcome, Vec<WatchItem>)
    where
        P: FnMut(&WatchItem) -> bool,
    {
        let dir = tempfile::tempdir().expect("temp dir");
        let (socket, server) = serve_one(dir.path(), ScriptSpec::new().extend(script).end(end));
        let mut items = Vec::new();
        let mut resume = ResumeState::new(None);
        let outcome = watch_resumable(
            &socket,
            ResourceId::local(7),
            &mut resume,
            timeout,
            |item| {
                let stop = accept(&item);
                items.push(item);
                !stop
            },
        )
        .await
        .expect("scripted transport");
        server.await.expect("scripted server task");
        (outcome, items)
    }

    fn event(event: AgentEvent) -> FrameKind {
        FrameKind::Event {
            terminal: Some(ResourceId::local(7)),
            event,
            stamp: None,
        }
    }

    fn is_bell(item: &WatchItem) -> bool {
        matches!(item, WatchItem::Event(ev) if ev.event == AgentEvent::Bell)
    }

    /// `phux watch --until`: the matching item is delivered before the stream
    /// stops; an agent-state item can satisfy the gate too.
    #[tokio::test]
    async fn a_bounded_watch_stops_on_the_first_matching_item_and_delivers_it() {
        let (outcome, items) = drive_bounded(
            vec![
                event(AgentEvent::Dirty),
                event(AgentEvent::Bell),
                event(AgentEvent::Idle),
            ],
            EndOfScript::HangUp,
            None,
            is_bell,
        )
        .await;
        assert_eq!(outcome, WatchOutcome::Stopped);
        assert_eq!(items.len(), 2, "{items:?}");
        assert!(is_bell(&items[1]));

        let pane = ResourceId::local(7);
        let (outcome, items) = drive_bounded(
            vec![agent_record(
                &pane,
                r#"{"name":"reviewer","state":"blocked"}"#,
            )],
            EndOfScript::HangUp,
            None,
            |item| matches!(item, WatchItem::AgentState(_)),
        )
        .await;

        assert_eq!(outcome, WatchOutcome::Stopped);
        assert_eq!(items.len(), 1);
    }

    /// A silent connected server ends in `TimedOut` (the prefix still
    /// delivered); a server EOF before the match is `Ended`, never a
    /// satisfied gate.
    #[tokio::test]
    async fn a_bounded_watch_tells_a_deadline_from_a_server_eof() {
        let (outcome, items) = drive_bounded(
            vec![event(AgentEvent::Dirty)],
            EndOfScript::ServeUntilDetach,
            Some(Duration::from_millis(150)),
            is_bell,
        )
        .await;
        assert_eq!(outcome, WatchOutcome::TimedOut);
        assert_eq!(items.len(), 1, "{items:?}");

        let (outcome, _items) = drive_bounded(
            vec![event(AgentEvent::Dirty)],
            EndOfScript::HangUp,
            Some(Duration::from_secs(30)),
            is_bell,
        )
        .await;
        assert_eq!(outcome, WatchOutcome::Ended);
    }

    fn snapshot(resources: Vec<ResourceInfo>) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_resources(resources)
    }

    /// A server-wide watch subscribes each local Terminal's record, excluding
    /// satellite Terminals and non-Terminal resources.
    #[tokio::test]
    async fn a_server_wide_watch_streams_multiple_local_agents_and_filters_resources() {
        let first = ResourceId::local(7);
        let second = ResourceId::local(8);
        let child = ResourceId::local(9);
        let satellite = ResourceId::satellite("edge", 10);
        let dir = tempfile::tempdir().expect("temp dir");
        let spec = ScriptSpec::new()
            .state(snapshot(vec![
                ResourceInfo::new(first.clone(), WindowId::new(1), 80, 24),
                ResourceInfo::new(second.clone(), WindowId::new(1), 80, 24),
                ResourceInfo::resource(child.clone(), ResourceKind::AgentSession),
                ResourceInfo::new(satellite.clone(), WindowId::new(1), 80, 24),
            ]))
            .push(FrameKind::MetadataChanged {
                scope: Scope::Resource(first.clone()),
                key: "phux.other/v1".to_owned(),
                value: Some(b"not an agent record".to_vec()),
                actor: None,
            })
            .push_after_subscribe(
                Scope::Resource(first.clone()),
                RESOURCE_AGENT_KEY,
                vec![agent_record(
                    &first,
                    r#"{"name":"first","state":"working"}"#,
                )],
            )
            .push_after_subscribe(
                Scope::Resource(second.clone()),
                RESOURCE_AGENT_KEY,
                vec![agent_record(
                    &second,
                    r#"{"name":"second","state":"blocked"}"#,
                )],
            )
            .end(EndOfScript::ServeUntilDetach);
        let (socket, server) = serve_one(dir.path(), spec);
        let mut items = Vec::new();
        watch_events(&socket, None, |item| {
            items.push(item);
            items
                .iter()
                .filter(|item| matches!(item, WatchItem::AgentState(_)))
                .count()
                < 2
        })
        .await
        .expect("fleet stream");
        let seen = server.await.expect("scripted server task");

        let updates: Vec<&AgentStateUpdate> = items
            .iter()
            .filter_map(|item| match item {
                WatchItem::AgentState(update) => Some(update),
                WatchItem::Event(_) => None,
            })
            .collect();
        assert_eq!(updates.len(), 2, "the foreign key is filtered: {items:?}");
        assert!(
            updates
                .iter()
                .any(|update| update.terminal.as_ref() == Some(&first))
        );
        assert!(
            updates
                .iter()
                .any(|update| update.terminal.as_ref() == Some(&second))
        );
        for excluded in [child, satellite] {
            assert!(
                !seen.iter().any(|frame| matches!(
                    frame,
                    FrameKind::SubscribeMetadata { scope: Scope::Resource(id), .. }
                        if *id == excluded
                )),
                "only local Terminal resources are subscribed; sent {seen:?}"
            );
        }
    }

    #[tokio::test]
    async fn a_server_wide_watch_follows_a_new_terminal_without_a_wire_wildcard() {
        let first = ResourceId::local(7);
        let spawned = ResourceId::local(8);
        let dir = tempfile::tempdir().expect("temp dir");
        let update = agent_record(&spawned, r#"{"name":"new-agent","state":"working"}"#);
        let spawned_event = FrameKind::Event {
            terminal: Some(spawned.clone()),
            event: AgentEvent::ResourceSpawned {
                kind: ResourceKind::Terminal,
                parent: None,
            },
            stamp: None,
        };
        let (socket, server) = serve_one(
            dir.path(),
            ScriptSpec::new()
                .state(snapshot(vec![ResourceInfo::new(
                    first,
                    WindowId::new(1),
                    80,
                    24,
                )]))
                .push(spawned_event)
                .push_after_subscribe(
                    Scope::Resource(spawned.clone()),
                    RESOURCE_AGENT_KEY,
                    vec![update],
                )
                .end(EndOfScript::ServeUntilDetach),
        );
        let mut items = Vec::new();
        watch_events(&socket, None, |item| {
            let stop = matches!(
                &item,
                WatchItem::AgentState(update)
                    if update.terminal.as_ref() == Some(&ResourceId::local(8))
            );
            items.push(item);
            !stop
        })
        .await
        .expect("fleet stream");
        let seen = server.await.expect("scripted server task");

        assert!(
            seen.iter()
                .any(|f| matches!(f, FrameKind::SubscribeEvents { terminal: None, .. })),
            "sent {seen:?}"
        );
        assert!(
            seen.iter().any(|f| matches!(
                f,
                FrameKind::SubscribeMetadata { scope: Scope::Resource(id), key }
                    if *id == ResourceId::local(8) && key == RESOURCE_AGENT_KEY
            )),
            "the spawn event must extend the L3 subscription set; sent {seen:?}"
        );
        assert!(items.iter().any(|item| matches!(
            item,
            WatchItem::AgentState(update)
                if update.terminal.as_ref() == Some(&ResourceId::local(8))
        )));
    }

    // -- watch_resumable --------------------------------------------------

    fn stamped(pane: &ResourceId, event: AgentEvent, seq: u64) -> FrameKind {
        FrameKind::Event {
            terminal: Some(pane.clone()),
            event,
            stamp: Some(Box::new(EventStamp::new(seq, 1_000 + seq))),
        }
    }

    async fn drive_resumable(
        resume: &mut ResumeState,
        script: Vec<FrameKind>,
    ) -> (WatchOutcome, Vec<WatchItem>, Vec<FrameKind>) {
        use phux_protocol::caps::{ServerFeature, ServerFeatureSet};

        let dir = tempfile::tempdir().expect("temp dir");
        let spec = ScriptSpec::new()
            .server_features(ServerFeatureSet::with(&[ServerFeature::EventJournal]))
            .server_id(vec![0x01, 0x02])
            .extend(script)
            .end(EndOfScript::HangUp);
        let (socket, server) = serve_one(dir.path(), spec);
        let mut items = Vec::new();
        let outcome = watch_resumable(&socket, ResourceId::local(7), resume, None, |item| {
            items.push(item);
            true
        })
        .await
        .expect("scripted transport");
        let seen = server.await.expect("scripted server task");
        (outcome, items, seen)
    }

    /// The cursor goes in as `after_seq`, every stamped event carries its
    /// `seq` to the item, and the reached position comes back out.
    #[tokio::test]
    async fn a_resumable_watch_replays_from_its_cursor_and_advances_it() {
        use crate::resource::cursor::Cursor;

        let pane = ResourceId::local(7);
        let mut resume = ResumeState::new(Cursor::new(vec![0x01, 0x02], 5));
        let (outcome, items, seen) = drive_resumable(
            &mut resume,
            vec![
                stamped(&pane, AgentEvent::Bell, 6),
                stamped(&pane, AgentEvent::Dirty, 9),
            ],
        )
        .await;
        assert_eq!(outcome, WatchOutcome::Ended);
        assert!(seen.iter().any(|frame| matches!(
            frame,
            FrameKind::SubscribeEvents {
                after_seq: Some(5),
                ..
            }
        )));
        assert_eq!(items.len(), 2);
        let WatchItem::Event(first) = &items[0] else {
            panic!("expected an event, got {:?}", items[0]);
        };
        assert_eq!(first.stamp.as_ref().map(|stamp| stamp.seq), Some(6));
        assert!(!resume.cursor_void());
        assert_eq!(resume.cursor().expect("cursor").to_string(), "0102:9");
    }

    /// A cursor from another incarnation is not sent; the watch starts live
    /// and says so.
    #[tokio::test]
    async fn a_resumable_watch_voids_a_foreign_cursor() {
        use crate::resource::cursor::Cursor;

        let mut resume = ResumeState::new(Cursor::new(vec![0xff], 5));
        let (_outcome, _items, seen) = drive_resumable(&mut resume, Vec::new()).await;
        // Not sent; the watch starts with journal semantics and no replay, so
        // an `expired` lease reads as `expired` rather than `released`.
        assert!(seen.iter().any(|frame| matches!(
            frame,
            FrameKind::SubscribeEvents {
                after_seq: Some(crate::resource::cursor::NO_REPLAY),
                ..
            }
        )));
        assert!(resume.cursor_void());
        assert_eq!(resume.cursor().expect("fresh cursor").to_string(), "0102:0");
    }
}
