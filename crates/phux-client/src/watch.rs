//! Server-pushed watch stream — the push half of the agent surface
//! (SPEC §7.5, ADR-0022 'events', `phux-y2t`) plus the derived agent-state
//! record (ADR-0040 / ADR-0046).
//!
//! Sends `SUBSCRIBE_EVENTS` and, when the watch is scoped to a single
//! Terminal, `SUBSCRIBE_METADATA` for the `phux.agent/v1` key on that
//! Terminal. It then streams the `EVENT` and `METADATA_CHANGED` frames the
//! server pushes back on that one connection, invoking a caller-supplied
//! sink per item until the transport closes (server gone, or the caller
//! drops the future). The subscription neither attaches nor resizes the
//! pane — an agent can `watch` a Terminal without disturbing the live
//! session.
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

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use phux_protocol::ids::TerminalId;
use phux_protocol::wire::frame::{AgentEvent, FrameKind, Scope};

use crate::agent_meta::{AgentRecord, TERMINAL_AGENT_KEY, parse_agent_record};
use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// One streamed agent event plus the Terminal it concerns.
///
/// `terminal` is `None` for a server-scoped event with no single owning
/// Terminal (none of the v0.2 events are server-scoped today, but the
/// envelope allows it). The CLI `phux watch` renders one of these per
/// line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WatchEvent {
    /// The Terminal the event concerns, or `None` if server-scoped.
    pub terminal: Option<TerminalId>,
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
    pub terminal: Option<TerminalId>,
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
/// **A server-wide watch (`terminal: None`) carries no agent-state
/// items.** `SUBSCRIBE_METADATA` names one `(scope, key)` pair and L3 has
/// no wildcard-Terminal scope, so there is no way to ask for "every
/// Terminal's agent record" without either enumerating the Terminals that
/// exist right now (which silently misses every pane spawned afterwards)
/// or changing the wire. The CLI's `phux watch` always resolves a target,
/// so it always gets the metadata half.
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
    terminal: Option<TerminalId>,
    sink: F,
) -> Result<(), AttachError>
where
    F: FnMut(WatchItem) -> bool,
{
    let mut conn = subscribe(socket, terminal).await?;
    stream_items(&mut conn, sink).await
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
    terminal: Option<TerminalId>,
) -> Result<Connection, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    conn.send(&FrameKind::SubscribeEvents {
        terminal: terminal.clone(),
    })
    .await?;
    if let Some(id) = &terminal {
        conn.send(&FrameKind::SubscribeMetadata {
            scope: Scope::Terminal(id.clone()),
            key: TERMINAL_AGENT_KEY.to_owned(),
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
    let mut last_seen: HashMap<TerminalId, AgentRecord> = HashMap::new();
    loop {
        match conn.recv().await {
            Ok(FrameKind::Event { terminal, event }) => {
                if !sink(WatchItem::Event(WatchEvent { terminal, event })) {
                    return Ok(());
                }
            }
            Ok(FrameKind::MetadataChanged { scope, key, value }) => {
                if key != TERMINAL_AGENT_KEY {
                    continue;
                }
                // `phux.agent/v1` is Terminal-scoped (L3 §3.7); a record
                // published at any other scope is not one of ours.
                let Scope::Terminal(id) = scope else {
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
    terminal: Option<TerminalId>,
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
                let _ = tokio::time::timeout(d, fut).await;
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

    use super::*;

    fn agent_record(scope_terminal: &TerminalId, json: &str) -> FrameKind {
        FrameKind::MetadataChanged {
            scope: Scope::Terminal(scope_terminal.clone()),
            key: TERMINAL_AGENT_KEY.to_owned(),
            value: Some(json.as_bytes().to_vec()),
        }
    }

    /// Drive `watch_events` against the shared scripted server, collecting
    /// every streamed item and returning it alongside the frames the client
    /// actually sent.
    async fn drive(
        terminal: Option<TerminalId>,
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

    /// The bug this module's metadata half exists to fix: a terminal-scoped
    /// watch must ask for the `phux.agent/v1` key, or the ADR-0046
    /// detector's publications never reach a headless consumer.
    #[tokio::test]
    async fn terminal_scoped_watch_subscribes_to_events_and_the_agent_key() {
        let pane = TerminalId::local(7);
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
                FrameKind::SubscribeMetadata { scope: Scope::Terminal(id), key }
                    if *id == pane && key == TERMINAL_AGENT_KEY
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
        let pane = TerminalId::local(7);
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
        let pane = TerminalId::local(3);
        let (items, _seen) = drive(
            Some(pane.clone()),
            vec![
                agent_record(&pane, r#"{"name":"reviewer","state":"blocked"}"#),
                FrameKind::MetadataChanged {
                    scope: Scope::Terminal(pane.clone()),
                    key: TERMINAL_AGENT_KEY.to_owned(),
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
        let pane = TerminalId::local(3);
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
        let pane = TerminalId::local(7);
        let (items, _seen) = drive(
            Some(pane.clone()),
            vec![
                FrameKind::MetadataChanged {
                    scope: Scope::Terminal(pane.clone()),
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
        let pane = TerminalId::local(7);
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

    /// A server-wide watch has no `(scope, key)` to name, so it subscribes
    /// to events only. Asserted so the limitation is visible in the suite
    /// rather than discovered by a consumer.
    #[tokio::test]
    async fn a_server_wide_watch_subscribes_to_events_only() {
        let (_items, seen) = drive(
            None,
            vec![FrameKind::Event {
                terminal: None,
                event: AgentEvent::Bell,
            }],
        )
        .await;

        assert!(
            seen.iter()
                .any(|f| matches!(f, FrameKind::SubscribeEvents { terminal: None })),
            "sent {seen:?}"
        );
        assert!(
            !seen
                .iter()
                .any(|f| matches!(f, FrameKind::SubscribeMetadata { .. })),
            "L3 has no wildcard-Terminal scope to subscribe to; sent {seen:?}"
        );
    }
}
