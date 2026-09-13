//! Server-pushed watch stream — the push half of the agent surface
//! (SPEC §7.5, ADR-0022 'events', `phux-y2t`) plus the derived agent-state
//! record (ADR-0040 / ADR-0046).
//!
//! Sends `SUBSCRIBE_EVENTS` and `SUBSCRIBE_METADATA` for the
//! `phux.agent/v1` key. A scoped watch installs one metadata subscription; a
//! server-wide watch enumerates local Terminals after subscribing to lifecycle
//! events and follows resource creation/closure to maintain the set. It then
//! streams the `EVENT` and `METADATA_CHANGED` frames the server pushes back on
//! that one connection, invoking a caller-supplied sink per item until the
//! transport closes (server gone, or the caller drops the future). The
//! subscription neither attaches nor resizes a pane.
//!
//! The metadata half is what makes ADR-0046 observable headlessly. The
//! server's detector re-reads each Terminal on a timer and publishes a
//! `phux.agent/v1` record on every `(kind, name, state)` transition; until
//! a consumer subscribed to that key, every one of those publications was
//! computed and dropped.
//!
//! This is an *additive accelerator* of the [`crate::wait`] poll floor:
//! a `watch` consumer learns of activity immediately rather than on the
//! next poll tick. A consumer that only polls still converges; the event
//! stream just cuts latency.

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::Path;
use std::time::Duration;

use phux_protocol::ids::{ResourceId, ResourceKind};
use phux_protocol::wire::frame::{AgentEvent, FrameKind, Scope};

use crate::agent_meta::{AgentRecord, RESOURCE_AGENT_KEY, parse_agent_record};
use crate::attach::AttachError;
use crate::attach::connection::Connection;
use crate::state::get_state_on_with_interleaved;

/// One streamed agent event plus the Terminal it concerns.
///
/// `terminal` is `None` for a server-scoped event with no single owning
/// Terminal (none of the v0.2 events are server-scoped today, but the
/// envelope allows it). The CLI `phux watch` renders one of these per
/// line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    /// The Terminal the event concerns, or `None` if server-scoped.
    pub terminal: Option<ResourceId>,
    /// The event payload.
    pub event: AgentEvent,
}

/// One observed change to a Terminal's `phux.agent/v1` record
/// (`docs/spec/L3.md` §3.7).
///
/// `record` is the new record, or `None` when the key was deleted — or when
/// the bytes did not parse as a record with a non-empty `name`, which
/// L3 §3.7 reads as "no declared agent" rather than as an error. Both cases
/// are reported rather than dropped: a consumer waiting on an agent has to
/// learn that it went away.
///
/// `previous` is the last record **this watch session** observed for the
/// same Terminal, so a consumer can render a transition. It is `None` on
/// the first record seen for a Terminal, and it is deliberately not
/// server state: the server publishes whole records with last-writer-wins
/// semantics and keeps no per-consumer transition history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentStateUpdate {
    /// The Terminal whose record changed. Always `Some` in practice —
    /// `phux.agent/v1` is Terminal-scoped — but carried as an `Option` so
    /// it renders through the same selector formatting as [`WatchEvent`].
    pub terminal: Option<ResourceId>,
    /// The new record, or `None` for a deletion / unreadable value.
    pub record: Option<AgentRecord>,
    /// The record this session last saw for the same Terminal, if any.
    pub previous: Option<AgentRecord>,
}

/// One item on the watch stream: an agent event, or an agent-state change.
///
/// Both ride the same connection, so a consumer sees them in the order the
/// server pushed them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WatchItem {
    /// An `EVENT` frame (SPEC §7.5).
    Event(WatchEvent),
    /// A `METADATA_CHANGED` frame for the `phux.agent/v1` key.
    AgentState(AgentStateUpdate),
}

/// Subscribe to the watch stream for `terminal` (or server-wide when
/// `None`) and invoke `sink` for every item until the transport closes.
///
/// Opens a fresh connection and sends `SUBSCRIBE_EVENTS`, plus
/// `SUBSCRIBE_METADATA` for `phux.agent/v1` when the watch names a
/// Terminal; no `HELLO` and no `ATTACH` (the subscription stands alone,
/// matching the `GET_SCREEN` control path). Returns `Ok(())` on a clean
/// server-side EOF (the [`AttachError::Disconnected`] the framed reader
/// yields), so a caller that loops until the server exits sees a tidy
/// success rather than an error. Any other transport/protocol failure
/// surfaces as [`AttachError`].
///
/// A server-wide watch (`terminal: None`) uses [`subscribe_fleet`] to carry
/// agent-state items for every local Terminal. L3 still has no wildcard
/// scope: the client enumerates existing Terminals only after registering a
/// server-wide lifecycle subscription, then follows resource spawn/close
/// events to keep the per-Terminal metadata set current.
///
/// `sink` returning `false` stops the stream early (the caller asked to
/// stop, e.g. on a Ctrl-C handler racing the recv); returning `true`
/// keeps streaming.
///
/// # Errors
///
/// Returns [`AttachError`] on connect/transport/protocol failure. A clean
/// EOF is NOT an error (returns `Ok(())`).
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

/// A server-wide event subscription plus one agent-record subscription for
/// every local Terminal currently known to the server.
///
/// The event subscription is registered before the server is enumerated.
/// Lifecycle events interleaved with that enumeration are retained and
/// replayed, closing the usual enumerate/follow race without a wildcard L3
/// scope or any wire change.
#[derive(Debug)]
pub struct FleetSubscription {
    pub(crate) conn: Connection,
    pub(crate) terminals: HashSet<ResourceId>,
    pending: VecDeque<FrameKind>,
}

impl FleetSubscription {
    /// Local Terminals currently covered by this subscription.
    #[must_use]
    pub const fn terminals(&self) -> &HashSet<ResourceId> {
        &self.terminals
    }

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

/// Establish a race-free fleet-wide agent-state subscription.
///
/// This is enumerate-and-follow on one ordered connection:
/// `SUBSCRIBE_EVENTS(Server)` first, then `GET_STATE(Server)`, then one
/// existing `SUBSCRIBE_METADATA(Terminal, phux.agent/v1)` per local Terminal.
/// Events captured ahead of the state reply are applied before subscriptions
/// are installed and retained for the stream consumer.
///
/// # Errors
///
/// Returns [`AttachError`] on connect, enumeration, or subscription failure.
pub async fn subscribe_fleet(socket: &Path) -> Result<FleetSubscription, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    conn.send(&FrameKind::SubscribeEvents { terminal: None })
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
/// pruning closed Terminals.
///
/// # Errors
///
/// Returns [`AttachError`] on transport/protocol failure. A clean EOF is not
/// an error, matching [`stream_items`].
pub async fn stream_fleet_items<F>(
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
                let FrameKind::Event { terminal, event } = frame else {
                    unreachable!();
                };
                if !sink(WatchItem::Event(WatchEvent { terminal, event })) {
                    return Ok(());
                }
            }
            Ok(FrameKind::MetadataChanged { scope, key, value }) => {
                if key != RESOURCE_AGENT_KEY {
                    continue;
                }
                let Scope::Resource(id) = scope else {
                    continue;
                };
                if !subscription.terminals.contains(&id) {
                    continue;
                }
                let record = value.as_deref().and_then(parse_agent_record);
                let previous = match &record {
                    Some(new) => last_seen.insert(id.clone(), new.clone()),
                    None => last_seen.remove(&id),
                };
                if !sink(WatchItem::AgentState(AgentStateUpdate {
                    terminal: Some(id),
                    record,
                    previous,
                })) {
                    return Ok(());
                }
            }
            Ok(_) => {}
            Err(AttachError::Disconnected) => return Ok(()),
            Err(err) => return Err(err),
        }
    }
}

/// Open a connection and register the watch subscriptions on it, returning
/// the connection before a single frame is read back.
///
/// Split out of [`watch_events`] so a consumer that must not lose a
/// transition between "subscribe" and "read the current level" can order the
/// two itself, **on this one connection**. The server handles one
/// connection's frames in the order they arrive, so a `GET_METADATA` sent
/// after this returns is answered strictly after the subscription is
/// registered, and any `METADATA_CHANGED` published in between arrives ahead
/// of that answer as an interleaved frame rather than being dropped. Two
/// connections would only give a race. `phux agent wait` depends on exactly
/// that ordering (ADR-0076 point 5).
///
/// # Errors
///
/// Returns [`AttachError`] on connect or send failure.
pub async fn subscribe(
    socket: &Path,
    terminal: Option<ResourceId>,
) -> Result<Connection, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    conn.send(&FrameKind::SubscribeEvents {
        terminal: terminal.clone(),
    })
    .await?;
    if let Some(id) = &terminal {
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Resource(id.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
        })
        .await?;
    }
    Ok(conn)
}

/// Stream [`WatchItem`]s off an already-[`subscribe`]d connection, invoking
/// `sink` per item until it returns `false` or the transport closes.
///
/// The streaming half of [`watch_events`]; the item shape is identical, so a
/// consumer that subscribed by hand renders exactly what `phux watch` does.
///
/// # Errors
///
/// Returns [`AttachError`] on transport/protocol failure. A clean EOF is NOT
/// an error (returns `Ok(())`).
pub async fn stream_items<F>(conn: &mut Connection, mut sink: F) -> Result<(), AttachError>
where
    F: FnMut(WatchItem) -> bool,
{
    // Per-Terminal memory of the last record seen, so an update can carry
    // the state it came from. Session-local by construction — see
    // [`AgentStateUpdate::previous`].
    let mut last_seen: HashMap<ResourceId, AgentRecord> = HashMap::new();
    loop {
        match conn.recv().await {
            Ok(FrameKind::Event { terminal, event }) => {
                if !sink(WatchItem::Event(WatchEvent { terminal, event })) {
                    return Ok(());
                }
            }
            Ok(FrameKind::MetadataChanged { scope, key, value }) => {
                if key != RESOURCE_AGENT_KEY {
                    continue;
                }
                // `phux.agent/v1` is Terminal-scoped (L3 §3.7); a record
                // published at any other scope is not one of ours.
                let Scope::Resource(id) = scope else {
                    continue;
                };
                let record = value.as_deref().and_then(parse_agent_record);
                let previous = match &record {
                    Some(new) => last_seen.insert(id.clone(), new.clone()),
                    None => last_seen.remove(&id),
                };
                let update = AgentStateUpdate {
                    terminal: Some(id),
                    record,
                    previous,
                };
                if !sink(WatchItem::AgentState(update)) {
                    return Ok(());
                }
            }
            // Other frames the server might interleave are ignored — the
            // watch connection only ever subscribed to events and the one
            // metadata key, so in practice only those two arrive, but be
            // liberal.
            Ok(_other) => {}
            // A clean EOF means the server closed the connection (it
            // exited, or the pane's session ended). That is the normal
            // terminal state for `watch`, not a failure.
            Err(AttachError::Disconnected) => return Ok(()),
            Err(err) => return Err(err),
        }
    }
}

/// How a [`watch_bounded`] run ended.
///
/// The three endings are genuinely different answers and a caller that
/// collapses them reports a lie: [`Self::Stopped`] is "the thing you were
/// waiting for happened", [`Self::TimedOut`] is "it had not happened yet",
/// and [`Self::Ended`] is "it can no longer happen on this stream". Plain
/// [`watch_events`] cannot tell the first from the third — both are
/// `Ok(())` — which is exactly why a gate needs this wrapper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WatchOutcome {
    /// The sink returned `false`, i.e. its condition was met. The item that
    /// met it was handed to the sink before it asked to stop, so a printing
    /// sink has already rendered it.
    Stopped,
    /// `timeout` elapsed with the sink still asking for more. The connection
    /// is dropped where it stands; a partially-consumed burst is not
    /// recovered.
    TimedOut,
    /// The server closed the stream first (it exited, or the pane's session
    /// ended) without the sink ever asking to stop.
    Ended,
}

/// [`watch_events`] under an optional deadline, reporting *which* of the
/// three endings occurred.
///
/// The primitive behind `phux watch --timeout SECS [--until EVENT]`: a
/// caller that wants "block until this pane emits `asked`, or 120 seconds"
/// needs a deadline the stream owns, not an external `sleep`-and-`kill`
/// around the whole process, and needs to tell a satisfied gate from a
/// server that went away. `timeout` of `None` streams until the sink stops
/// it or the server closes, which is exactly [`watch_events`].
///
/// The deadline covers the connect too, deliberately: "give me an answer
/// within N seconds" is not honoured by a call that blocks indefinitely on a
/// socket that never accepts.
///
/// # Errors
///
/// Returns [`AttachError`] on connect/transport/protocol failure. A clean
/// EOF is not an error — it is [`WatchOutcome::Ended`].
pub async fn watch_bounded<F>(
    socket: &Path,
    terminal: Option<ResourceId>,
    timeout: Option<Duration>,
    mut sink: F,
) -> Result<WatchOutcome, AttachError>
where
    F: FnMut(WatchItem) -> bool,
{
    // Whether the sink asked to stop, recorded through the wrapper because
    // `watch_events` reports a sink-stop and a server EOF identically.
    let mut stopped = false;
    let expired = {
        let stream = watch_events(socket, terminal, |item| {
            let keep_going = sink(item);
            stopped |= !keep_going;
            keep_going
        });
        if let Some(deadline) = timeout {
            match tokio::time::timeout(deadline, stream).await {
                Ok(result) => {
                    result?;
                    false
                }
                // Dropping the future drops the connection: the subscription
                // is torn down by going away, which is all an un-attached
                // subscriber has to do.
                Err(_elapsed) => true,
            }
        } else {
            stream.await?;
            false
        }
    };

    // No stop/expiry race to arbitrate: `tokio::time::timeout` polls the
    // inner future before the timer, so a sink that stopped on the item that
    // arrived as the deadline fired still resolves as `Ok`.
    if expired {
        return Ok(WatchOutcome::TimedOut);
    }
    Ok(if stopped {
        WatchOutcome::Stopped
    } else {
        WatchOutcome::Ended
    })
}

/// Bounded one-shot over [`watch_events`]: collect events until `max_events`
/// are seen, `timeout` elapses, or the server closes — then return them.
///
/// This is the request/response shape a non-streaming caller (the MCP
/// `phux_watch` tool) needs: streaming is great for a live CLI, but a tool
/// call must return a finite result. `timeout` elapsing is success, not an
/// error — the collected prefix is returned. With both bounds `None`, it
/// streams until the server exits.
///
/// Agent-state items are filtered out: this collector's contract is the
/// `EVENT` taxonomy, and its one caller renders that vocabulary. Exposing
/// the agent record here is a separate surface decision, not a side effect
/// of the streaming layer growing a second item kind.
///
/// # Errors
///
/// Returns [`AttachError`] on connect/transport failure before any timeout.
pub async fn collect_events(
    socket: &Path,
    terminal: Option<ResourceId>,
    max_events: Option<usize>,
    timeout: Option<Duration>,
) -> Result<Vec<WatchEvent>, AttachError> {
    let mut collected: Vec<WatchEvent> = Vec::new();
    {
        let sink = |item: WatchItem| {
            if let WatchItem::Event(ev) = item {
                collected.push(ev);
            }
            // Keep going until we reach the cap (if any).
            max_events.is_none_or(|m| collected.len() < m)
        };
        let fut = watch_events(socket, terminal, sink);
        match timeout {
            // Timeout is a clean stop: drop the future, keep the prefix.
            Some(d) => {
                if let Ok(result) = tokio::time::timeout(d, fut).await {
                    result?;
                }
            }
            None => fut.await?,
        }
    }
    Ok(collected)
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::expect_used,
        clippy::unwrap_used,
        clippy::panic,
        reason = "tests"
    )]

    use tokio::net::UnixListener;

    use crate::agent_meta::AgentMetaState;
    use crate::testkit::{EndOfScript, ScriptSpec, ScriptedServer};
    use phux_protocol::ids::{SessionId, WindowId};
    use phux_protocol::wire::info::{ResourceInfo, SessionSnapshot};

    use super::*;

    fn agent_record(scope_terminal: &ResourceId, json: &str) -> FrameKind {
        FrameKind::MetadataChanged {
            scope: Scope::Resource(scope_terminal.clone()),
            key: RESOURCE_AGENT_KEY.to_owned(),
            value: Some(json.as_bytes().to_vec()),
        }
    }

    #[tokio::test]
    async fn collect_events_propagates_watch_error_before_timeout() {
        let dir = tempfile::tempdir().expect("temp dir");
        let missing_socket = dir.path().join("missing.sock");

        let result =
            collect_events(&missing_socket, None, None, Some(Duration::from_secs(1))).await;

        assert!(
            matches!(result, Err(AttachError::Io(_))),
            "an AttachError completed before the deadline must propagate, got {result:?}"
        );
    }

    #[tokio::test]
    async fn collect_events_returns_prefix_when_timeout_elapses() {
        let pane = ResourceId::local(7);
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let expected = WatchEvent {
            terminal: Some(pane.clone()),
            event: AgentEvent::Dirty,
        };
        let server = tokio::spawn(async move {
            ScriptedServer::accept(
                &listener,
                ScriptSpec::new()
                    .push(FrameKind::Event {
                        terminal: Some(pane),
                        event: AgentEvent::Dirty,
                    })
                    .end(EndOfScript::ServeUntilDetach),
            )
            .await
        });

        let collected = collect_events(
            &socket,
            Some(ResourceId::local(7)),
            None,
            Some(Duration::from_millis(150)),
        )
        .await
        .expect("elapsed timeout is a clean stop");

        assert_eq!(collected, vec![expected]);
        server.await.expect("scripted server task");
    }

    /// Drive `watch_events` against the shared scripted server, collecting
    /// every streamed item and returning it alongside the frames the client
    /// actually sent.
    async fn drive(
        terminal: Option<ResourceId>,
        script: Vec<FrameKind>,
    ) -> (Vec<WatchItem>, Vec<FrameKind>) {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let server = tokio::spawn(async move {
            ScriptedServer::accept(
                &listener,
                ScriptSpec::new().extend(script).end(EndOfScript::HangUp),
            )
            .await
        });
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

    /// phux-k0cw: `push_after_subscribe` releases its frames only to a client
    /// that subscribed to that exact `(scope, key)`.
    ///
    /// This is what makes the cross-session subscription assertions
    /// meaningful. The unkeyed `push` releases on the FIRST
    /// `SUBSCRIBE_METADATA` of any shape, so a test written against it passes
    /// even against a client that subscribed to the wrong key — exactly the
    /// bug worth catching once a client watches several keys at once.
    #[tokio::test]
    async fn a_keyed_push_waits_for_its_own_subscription() {
        async fn drive_keyed(scope: Scope, key: &str) -> Vec<WatchItem> {
            let pane = ResourceId::local(7);
            let dir = tempfile::tempdir().expect("temp dir");
            let socket = dir.path().join("phux.sock");
            let listener = UnixListener::bind(&socket).expect("bind scripted server");
            let key = key.to_owned();
            let record = agent_record(&pane, r#"{"name":"reviewer"}"#);
            let server = tokio::spawn(async move {
                ScriptedServer::accept(
                    &listener,
                    ScriptSpec::new()
                        .push_after_subscribe(scope, key, vec![record])
                        .end(EndOfScript::HangUp),
                )
                .await
            });
            let mut items = Vec::new();
            watch_events(&socket, Some(pane), |item| {
                items.push(item);
                true
            })
            .await
            .expect("a scripted hang-up is a clean EOF");
            let _ = server.await;
            items
        }

        // The watch subscribes to `Terminal(7) / phux.agent/v1`, so a push
        // keyed to exactly that is released.
        let matched = drive_keyed(Scope::Resource(ResourceId::local(7)), RESOURCE_AGENT_KEY).await;
        assert!(
            matched
                .iter()
                .any(|i| matches!(i, WatchItem::AgentState { .. })),
            "a push keyed to the subscribed pair must be released; got {matched:?}"
        );

        // Same key, different scope: never released, because a real server
        // fans a METADATA_CHANGED only to that scope's subscribers.
        let wrong_scope =
            drive_keyed(Scope::Resource(ResourceId::local(8)), RESOURCE_AGENT_KEY).await;
        assert!(
            wrong_scope.is_empty(),
            "a push keyed to another pane must not reach this watch; got {wrong_scope:?}"
        );

        // Same scope, different key: likewise.
        let wrong_key = drive_keyed(Scope::Resource(ResourceId::local(7)), "phux.other/v1").await;
        assert!(
            wrong_key.is_empty(),
            "a push keyed to another key must not reach this watch; got {wrong_key:?}"
        );
    }

    /// The bug this module's metadata half exists to fix: a terminal-scoped
    /// watch must ask for the `phux.agent/v1` key, or the ADR-0046
    /// detector's publications never reach a headless consumer.
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
                FrameKind::SubscribeEvents { terminal: Some(id) } if *id == pane
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
        // Still no ATTACH: watching must not disturb the live session, which
        // is exactly why the server had to learn to fan metadata out to an
        // un-attached subscriber.
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

    /// A tombstone is reported, not swallowed: a consumer waiting on an
    /// agent has to learn the record went away.
    #[tokio::test]
    async fn a_tombstone_streams_with_no_record_and_the_last_state_as_previous() {
        let pane = ResourceId::local(3);
        let (items, _seen) = drive(
            Some(pane.clone()),
            vec![
                agent_record(&pane, r#"{"name":"reviewer","state":"blocked"}"#),
                FrameKind::MetadataChanged {
                    scope: Scope::Resource(pane.clone()),
                    key: RESOURCE_AGENT_KEY.to_owned(),
                    value: None,
                },
            ],
        )
        .await;

        assert_eq!(items.len(), 2);
        let WatchItem::AgentState(tombstone) = &items[1] else {
            panic!("expected an agent-state item, got {:?}", items[1]);
        };
        assert!(tombstone.record.is_none());
        assert_eq!(
            tombstone.previous.as_ref().unwrap().state,
            AgentMetaState::Blocked
        );
    }

    /// A value that is not a readable record is "no declared agent"
    /// (L3 §3.7), not a stream-wedging parse error.
    #[tokio::test]
    async fn an_unreadable_value_streams_as_a_cleared_record() {
        let pane = ResourceId::local(3);
        let (items, _seen) = drive(
            Some(pane.clone()),
            vec![agent_record(&pane, "not a record")],
        )
        .await;

        assert_eq!(items.len(), 1);
        let WatchItem::AgentState(update) = &items[0] else {
            panic!("expected an agent-state item, got {:?}", items[0]);
        };
        assert!(update.record.is_none());
    }

    /// Metadata for some other key rides the same connection once a TUI-
    /// shaped consumer is in the mix; the watch stream must ignore it
    /// rather than render a line for it.
    #[tokio::test]
    async fn other_metadata_keys_are_ignored() {
        let pane = ResourceId::local(7);
        let (items, _seen) = drive(
            Some(pane.clone()),
            vec![
                FrameKind::MetadataChanged {
                    scope: Scope::Resource(pane.clone()),
                    key: "phux.tui.layout/v1".to_owned(),
                    value: Some(b"{}".to_vec()),
                },
                agent_record(&pane, r#"{"name":"reviewer","state":"idle"}"#),
            ],
        )
        .await;

        assert_eq!(items.len(), 1, "only the agent key renders: {items:?}");
    }

    /// Events and agent-state changes interleave on one connection, in the
    /// order the server pushed them.
    #[tokio::test]
    async fn events_and_agent_state_share_one_ordered_stream() {
        let pane = ResourceId::local(7);
        let (items, _seen) = drive(
            Some(pane.clone()),
            vec![
                FrameKind::Event {
                    terminal: Some(pane.clone()),
                    event: AgentEvent::Bell,
                },
                agent_record(&pane, r#"{"name":"reviewer","state":"blocked"}"#),
            ],
        )
        .await;

        assert!(matches!(items[0], WatchItem::Event(_)));
        assert!(matches!(items[1], WatchItem::AgentState(_)));
    }

    // -- watch_bounded ----------------------------------------------------

    /// Drive [`watch_bounded`] against the scripted server, stopping on the
    /// first item the predicate accepts. `end` decides whether the server
    /// hangs up after the script (an EOF) or stays connected (so a deadline
    /// is the only way out).
    async fn drive_bounded<P>(
        terminal: Option<ResourceId>,
        script: Vec<FrameKind>,
        end: EndOfScript,
        timeout: Option<Duration>,
        mut accept: P,
    ) -> (WatchOutcome, Vec<WatchItem>)
    where
        P: FnMut(&WatchItem) -> bool,
    {
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let server = tokio::spawn(async move {
            ScriptedServer::accept(&listener, ScriptSpec::new().extend(script).end(end)).await
        });
        let mut items = Vec::new();
        let outcome = watch_bounded(&socket, terminal, timeout, |item| {
            let stop = accept(&item);
            items.push(item);
            !stop
        })
        .await
        .expect("scripted transport");
        // The client's socket is gone either way by now (stopped, expired, or
        // EOF), so a `ServeUntilDetach` server resolves rather than hanging.
        server.await.expect("scripted server task");
        (outcome, items)
    }

    /// The gate `phux watch --until` is built on: the matching item is
    /// delivered to the sink *before* the stream stops, so the caller can
    /// render the event that satisfied it.
    #[tokio::test]
    async fn watch_bounded_stops_on_the_first_matching_item_and_still_delivers_it() {
        let pane = ResourceId::local(7);
        let (outcome, items) = drive_bounded(
            Some(pane.clone()),
            vec![
                FrameKind::Event {
                    terminal: Some(pane.clone()),
                    event: AgentEvent::Dirty,
                },
                FrameKind::Event {
                    terminal: Some(pane.clone()),
                    event: AgentEvent::Bell,
                },
                FrameKind::Event {
                    terminal: Some(pane.clone()),
                    event: AgentEvent::Idle,
                },
            ],
            EndOfScript::HangUp,
            None,
            |item| matches!(item, WatchItem::Event(ev) if ev.event == AgentEvent::Bell),
        )
        .await;

        assert_eq!(outcome, WatchOutcome::Stopped);
        assert_eq!(
            items.len(),
            2,
            "the matching item is rendered, then the stream stops: {items:?}"
        );
        assert!(matches!(&items[1], WatchItem::Event(ev) if ev.event == AgentEvent::Bell));
    }

    /// An agent-state change satisfies the gate too — it is one item kind on
    /// the same stream, and `agent_state` is a name `--until` can spell.
    #[tokio::test]
    async fn watch_bounded_can_stop_on_an_agent_state_item() {
        let pane = ResourceId::local(7);
        let (outcome, items) = drive_bounded(
            Some(pane.clone()),
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

    /// The deadline is the whole point: a server that stays connected and
    /// says nothing must produce `TimedOut`, not a hang. This is the
    /// `sleep`-and-`kill` shell workaround's replacement.
    #[tokio::test]
    async fn watch_bounded_reports_the_deadline_when_no_item_matches() {
        let pane = ResourceId::local(7);
        let (outcome, items) = drive_bounded(
            Some(pane.clone()),
            vec![FrameKind::Event {
                terminal: Some(pane.clone()),
                event: AgentEvent::Dirty,
            }],
            // Stay connected: only the deadline can end this watch.
            EndOfScript::ServeUntilDetach,
            Some(Duration::from_millis(150)),
            |item| matches!(item, WatchItem::Event(ev) if ev.event == AgentEvent::Bell),
        )
        .await;

        assert_eq!(outcome, WatchOutcome::TimedOut);
        assert_eq!(
            items.len(),
            1,
            "the non-matching prefix is still delivered: {items:?}"
        );
    }

    /// A server that closes before the awaited event arrives is `Ended`, not
    /// `Stopped` and not `TimedOut`. A gate that reported success here would
    /// tell a caller its event happened when the stream carrying it went
    /// away — the same class of false positive a level read of `idle` makes.
    #[tokio::test]
    async fn watch_bounded_distinguishes_a_server_eof_from_a_satisfied_gate() {
        let pane = ResourceId::local(7);
        let (outcome, _items) = drive_bounded(
            Some(pane.clone()),
            vec![FrameKind::Event {
                terminal: Some(pane.clone()),
                event: AgentEvent::Dirty,
            }],
            EndOfScript::HangUp,
            Some(Duration::from_secs(30)),
            |item| matches!(item, WatchItem::Event(ev) if ev.event == AgentEvent::Bell),
        )
        .await;

        assert_eq!(outcome, WatchOutcome::Ended);
    }

    fn snapshot(resources: Vec<ResourceInfo>) -> SessionSnapshot {
        SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
            .with_resources(resources)
    }

    /// The former limitation: a server-wide watch now enumerates every local
    /// Terminal and installs the exact L3 subscription for each, while
    /// excluding both satellite Terminals and non-Terminal resources.
    #[tokio::test]
    async fn a_server_wide_watch_streams_multiple_local_agents_and_filters_resources() {
        let first = ResourceId::local(7);
        let second = ResourceId::local(8);
        let child = ResourceId::local(9);
        let satellite = ResourceId::satellite("edge", 10);
        let dir = tempfile::tempdir().expect("temp dir");
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
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
        let server = tokio::spawn(async move { ScriptedServer::accept(&listener, spec).await });
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
        let socket = dir.path().join("phux.sock");
        let listener = UnixListener::bind(&socket).expect("bind scripted server");
        let update = agent_record(&spawned, r#"{"name":"new-agent","state":"working"}"#);
        let spawned_event = FrameKind::Event {
            terminal: Some(spawned.clone()),
            event: AgentEvent::ResourceSpawned {
                kind: ResourceKind::Terminal,
                parent: None,
            },
        };
        let server = tokio::spawn(async move {
            ScriptedServer::accept(
                &listener,
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
            )
            .await
        });
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
                .any(|f| matches!(f, FrameKind::SubscribeEvents { terminal: None })),
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
}
