//! Dispatch for the kind-aware verbs: spawning an `AgentSession`, feeding
//! its stream, and bootstrapping a consumer onto it.
//!
//! Everything a Terminal already handles stays in `runtime::commands` and
//! `runtime::attach`; what lives here is the work that only exists because
//! the server serves more than one [`ResourceKind`] (ADR-0102). The three
//! entry points are the three places a client can address a session:
//! `spawn_agent_session` creates one under a Terminal parent,
//! `handle_append_resource_output` is the producer's write path
//! (ADR-0103 §3), and `attach_agent_session` replays the retained ring and
//! goes live.
//!
//! The state derived from the stream does not stay here. An accepted append
//! yields at most one [`StreamEvidence`], and this module forwards it to the
//! *parent* Terminal's control mailbox, where the same arbiter the detector
//! and the hooks feed ranks it and writes `phux.agent/v1`. That is what
//! makes `phux watch`, the `agent-state-changed` hook, and the TUI badge
//! work over a stream without any of them learning a new surface.

use bytes::Bytes;
use phux_protocol::caps::{BootstrapLimits, BootstrapStreamProfile};
use phux_protocol::ids::ResourceKind;
use phux_protocol::wire::frame::{
    AgentEvent, CommandResult, CommandValue, ErrorCode, FrameKind, SpawnError, SpawnResource,
    SpawnResult,
};
use tokio::sync::oneshot;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::mailbox::Outbound;
use crate::resource::PaneOutput;
use crate::resource::agent_session::{
    AgentSessionActor, AgentSessionBootstrap, AppendRejection, AppendRequest, BootstrapRequest,
    StreamEvidence,
};
use crate::state::{ClientId, SharedState};

/// Payload budget for one `BOOTSTRAP_CHUNK` when the connection negotiated
/// none. Records are at most 16 KiB, so this fits several per chunk.
const DEFAULT_AGENT_CHUNK_BYTES: usize = 64 * 1024;

// ---- spawn ------------------------------------------------------------------

/// Handle a `SPAWN_TERMINAL` whose kind is `AgentSession` (ADR-0103 §1).
///
/// The parent is validated before anything is created: it must resolve on
/// this server, still be live, and be a Terminal. A session may not parent
/// another (ADR-0104 §5), and that falls out of the kind check — an
/// `AgentSession` parent is `ParentKindMismatch`, not a second level.
///
/// A satellite-addressed spawn never reaches the local registry: the parent
/// is reduced to the satellite's `Local` space and the whole request is
/// relayed, so the satellite binds a child to its own Terminal exactly as it
/// would for a local consumer (ADR-0104 §6).
#[allow(clippy::too_many_arguments, clippy::too_many_lines)]
pub(crate) async fn spawn_agent_session(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    resource: &SpawnResource,
    satellite: Option<&phux_protocol::ids::SatelliteHost>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    bootstrap_limits: BootstrapLimits,
    connection_token: &CancellationToken,
) {
    let Some(provider) = resource.provider.as_deref().filter(|p| !p.is_empty()) else {
        refuse(
            out_tx,
            request_id,
            SpawnError::SpawnFailed("an agent session spawn must name a provider".to_owned()),
        )
        .await;
        return;
    };
    let Some(parent) = resource.parent.clone() else {
        refuse(out_tx, request_id, SpawnError::ParentNotFound).await;
        return;
    };

    if let Some(host) = satellite {
        relay_agent_session_spawn(state, request_id, resource, host, &parent, out_tx).await;
        return;
    }

    // A `Satellite`-tagged parent with no `satellite` field addresses a
    // resource this server does not own, so it resolves to nothing here.
    // `ParentNotFound` is the accurate answer: the hub has no such local
    // resource, and routing is the consumer's to state.
    let Some((core_parent, wire_parent)) = state.with(|s| {
        parent
            .is_local()
            .then(|| s.terminal_from_wire(&parent))
            .flatten()
            .map(|core| (core, parent.clone()))
    }) else {
        debug!(?client_id, request_id, %parent, "SPAWN_TERMINAL: agent session parent not found");
        refuse(out_tx, request_id, SpawnError::ParentNotFound).await;
        return;
    };

    let log_bytes = state.with(crate::state::ServerState::agent_log_bytes);
    let token = root_token.child_token();
    let bundle = AgentSessionActor::build(
        core_parent,
        provider,
        resource.native_id.as_deref(),
        token.clone(),
        log_bytes,
    );

    // Registry insert, engine registration, and wire-id interning happen in
    // one borrow: a consumer that observes the id in an event must be able
    // to resolve it in the same instant.
    let registered = state.with_mut(|s| {
        let facet = phux_core::resource::AgentFacet {
            provider: provider.to_owned(),
            native_id: resource.native_id.clone(),
            state: None,
        };
        let core = match s.registry_mut().new_agent_session(core_parent, facet) {
            Ok(core) => core,
            Err(phux_core::registry::RegistryError::ParentKindMismatch { .. }) => {
                return Err(SpawnError::ParentKindMismatch);
            }
            Err(_) => return Err(SpawnError::ParentNotFound),
        };
        let _ = s.spawn_resource_actor(core, bundle.handle.clone(), token, bundle.actor.run());
        let wire = s.intern_terminal_wire(core);
        s.subscribe_terminal(client_id, core, Some(out_tx.clone()));
        Ok((core, wire))
    });
    let (core_session, wire_session) = match registered {
        Ok(pair) => pair,
        Err(error) => {
            refuse(out_tx, request_id, error).await;
            return;
        }
    };

    crate::runtime::client::spawn_terminal_exit_watcher(
        state.clone(),
        core_session,
        Some(bundle.exit_notify),
        root_token.clone(),
    );

    // ADR-0103 §6: with a child bound, the parent's `REPORT_AGENT_STATE`
    // becomes a synthesized `state` record on this stream instead of a
    // second opinion beside it.
    if let Some(parent_handle) = state.with(|s| s.resource_handle(core_parent).cloned())
        && let Ok(facet) = bundle.handle.agent_session()
    {
        let _ = parent_handle
            .control
            .send(crate::resource::ControlRequest::BindAgentSession {
                append: facet.append.clone(),
            })
            .await;
    }

    let _ = out_tx
        .send(Outbound::Frame(FrameKind::TerminalSpawned {
            request_id,
            result: SpawnResult::Ok(wire_session.clone()),
        }))
        .await;
    // The same announcement a Terminal spawn makes, carrying what the
    // envelope cannot say: this resource is a session, and it lives inside
    // that pane.
    crate::runtime::client::broadcast_event(
        state,
        Some(&wire_session),
        &AgentEvent::PaneSpawned {
            kind: ResourceKind::AgentSession,
            parent: Some(wire_parent),
        },
    );
    // A Terminal spawn's auto-subscribed owner also gets a live output
    // pump wired in the same stroke (`attach::spawn_terminal_output_pump`),
    // so `s.subscribe_terminal` above put this client on the subscriber
    // *list* but nothing yet forwards `PaneOutput::Live` into its mailbox.
    // Running the same bootstrap-then-pump the ATTACH_TERMINAL path runs —
    // trivially empty, since nothing has been appended yet — closes that
    // gap without a second delivery mechanism to keep in step with the
    // first.
    let _ = attach_agent_session(
        state,
        client_id,
        &wire_session,
        core_session,
        &bundle.handle,
        out_tx,
        bootstrap_limits,
        connection_token,
    )
    .await;
    debug!(
        ?client_id,
        request_id,
        session = %wire_session,
        provider,
        "SPAWN_TERMINAL: agent session bound to its parent"
    );
}

/// Forward a satellite-addressed session spawn over the owning hub link,
/// with the parent reduced to that satellite's `Local` space.
async fn relay_agent_session_spawn(
    state: &SharedState,
    request_id: u32,
    resource: &SpawnResource,
    host: &phux_protocol::ids::SatelliteHost,
    parent: &phux_protocol::ids::TerminalId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let phux_protocol::ids::TerminalId::Satellite {
        host: parent_host,
        id,
    } = parent
    else {
        refuse(
            out_tx,
            request_id,
            SpawnError::SpawnFailed(
                "a satellite-addressed session must name a parent on that satellite".to_owned(),
            ),
        )
        .await;
        return;
    };
    if parent_host != host {
        refuse(
            out_tx,
            request_id,
            SpawnError::SpawnFailed(
                "the parent belongs to a different satellite than the spawn".to_owned(),
            ),
        )
        .await;
        return;
    }
    let mut forwarded = resource.clone();
    forwarded.parent = Some(phux_protocol::ids::TerminalId::local(*id));
    let spawn = crate::hub::relay::SatelliteSpawn {
        group: crate::state::DEFAULT_GROUP_ID,
        command: None,
        cwd: None,
        env: None,
        term: None,
        owner_terminal: None,
        initial_size: None,
        resource: Some(Box::new(forwarded)),
    };
    crate::runtime::attach::dispatch_satellite_spawn(state, out_tx, request_id, host, Ok(spawn))
        .await;
}

/// Queue a typed `TERMINAL_SPAWNED` refusal.
async fn refuse(out_tx: &tokio::sync::mpsc::Sender<Outbound>, request_id: u32, error: SpawnError) {
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::TerminalSpawned {
            request_id,
            result: SpawnResult::Err(error),
        }))
        .await;
}

// ---- append -----------------------------------------------------------------

/// Handle `APPEND_RESOURCE_OUTPUT` (ADR-0103 §3): the producer's write onto
/// a session's stream.
///
/// The reply is the header the server stamped —
/// `{"seq":…,"ts_ms":…}` — so a producer can print exactly what it wrote
/// without reading the stream back.
///
/// The producer must hold the `Input` verb on the parent, which under the
/// current policy every owner-socket client does and no remote client does:
/// a stream that anyone reachable over the network could write would let a
/// remote peer forge a local agent's lifecycle.
pub(crate) async fn handle_append_resource_output(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    bytes: Bytes,
) -> CommandResult {
    let Some((core, handle)) = state.with(|s| {
        s.terminal_from_wire(terminal_id)
            .and_then(|core| s.resource_handle(core).cloned().map(|h| (core, h)))
    }) else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: format!("no such resource: {terminal_id:?}"),
        };
    };
    let session = match handle.agent_session() {
        Ok(session) => session,
        Err(error) => return crate::runtime::commands::wrong_resource_kind(error),
    };
    if !state.with(|s| s.client_may_produce(client_id)) {
        return CommandResult::Error {
            code: ErrorCode::NotProducer,
            message: "appending to a resource stream requires a local owner connection".to_owned(),
        };
    }

    let (reply, rx) = oneshot::channel();
    if session
        .append
        .try_send(AppendRequest { bytes, reply })
        .is_err()
    {
        return CommandResult::Error {
            code: ErrorCode::Overflow,
            message: "the session's append queue is full".to_owned(),
        };
    }
    let accepted = match rx.await {
        Ok(Ok(accepted)) => accepted,
        Ok(Err(AppendRejection::Invalid(error))) => {
            return CommandResult::Error {
                code: ErrorCode::RecordInvalid,
                message: error.to_string(),
            };
        }
        Ok(Err(AppendRejection::Overflow(message))) => {
            return CommandResult::Error {
                code: ErrorCode::Overflow,
                message,
            };
        }
        Ok(Err(AppendRejection::Closed)) | Err(_) => {
            return CommandResult::Error {
                code: ErrorCode::TerminalNotFound,
                message: "the session closed before the append landed".to_owned(),
            };
        }
    };

    if let Some(evidence) = accepted.evidence {
        publish_stream_evidence(state, core, handle.parent, evidence).await;
    }
    publish_stream_ask(state, handle.parent, accepted.ask, accepted.evidence);
    CommandResult::OkWith(CommandValue::Json(format!(
        "{{\"seq\":{},\"ts_ms\":{}}}",
        accepted.first_seq, accepted.ts_ms
    )))
}

/// Feed a question the stream carried into the parent's ask ledger at the
/// `Stream` rung, and retract that rung when the turn ends.
///
/// A separate ledger from the state one on purpose (ADR-0036): a question
/// outlives the `blocked` that announced it and is answered, not
/// superseded. So a `stop` or a `session_end` retracts the stream's ask even
/// though it is the state ladder that carries those words — the agent is no
/// longer waiting on anyone.
fn publish_stream_ask(
    state: &SharedState,
    parent: Option<phux_core::ids::ResourceId>,
    ask: Option<crate::resource::agent_session::StreamAsk>,
    evidence: Option<StreamEvidence>,
) {
    let Some(parent) = parent else {
        return;
    };
    let (payload, wire_parent) = state.with_mut(|s| {
        let wire = Some(s.intern_terminal_wire(parent));
        let payload = if let Some(ask) = ask {
            s.report_stream_ask(
                parent,
                crate::agent_asked::AskedPayload {
                    id: ask.id,
                    question: ask.question,
                    suggestions: ask.suggestions,
                    elapsed_seconds: None,
                },
            )
            .emit_payload()
        } else {
            if matches!(
                evidence,
                Some(StreamEvidence::Done | StreamEvidence::Retract)
            ) {
                s.retract_agent_asked(parent, crate::agent_asked::AskedSource::Stream);
            }
            None
        };
        (payload, wire)
    });
    if let (Some(payload), Some(wire_parent)) = (payload, wire_parent) {
        crate::runtime::client::broadcast_event(state, Some(&wire_parent), &payload.into_event());
    }
}

/// Record the session's derived state and feed it to the parent Terminal's
/// arbiter at the `Stream` rank (ADR-0103 §5).
///
/// Two writes, deliberately: the session's own facet carries the state so
/// the inventory reports it without a round trip, and the parent's
/// `phux.agent/v1` record carries it so every consumer that already reads
/// that record — `phux watch`, the `agent-state-changed` hook, the TUI
/// badge — sees stream evidence without learning a new surface. The
/// invariants that record is held to (I1 and I2 in [`crate::agent_state`])
/// are unchanged, because the write still goes through the one path the
/// detector's own reports take.
async fn publish_stream_evidence(
    state: &SharedState,
    session: phux_core::ids::ResourceId,
    parent: Option<phux_core::ids::ResourceId>,
    evidence: StreamEvidence,
) {
    let word = match evidence {
        StreamEvidence::Working => Some("working"),
        StreamEvidence::Blocked => Some("blocked"),
        StreamEvidence::Done => Some("done"),
        StreamEvidence::Retract => None,
    };
    let parent_handle = state.with_mut(|s| {
        if let Some(facet) = s
            .registry_mut()
            .resource_mut(session)
            .and_then(|r| r.agent.as_mut())
        {
            facet.state = word.map(str::to_owned);
        }
        parent.and_then(|parent| s.resource_handle(parent).cloned())
    });
    let Some(parent_handle) = parent_handle else {
        return;
    };
    let (reply, rx) = oneshot::channel();
    let request = crate::resource::ControlRequest::ReportStreamState {
        state: word.map(|word| match word {
            "working" => phux_protocol::wire::frame::ReportedAgentState::Working,
            "blocked" => phux_protocol::wire::frame::ReportedAgentState::Blocked,
            _ => phux_protocol::wire::frame::ReportedAgentState::Done,
        }),
        reply,
    };
    if parent_handle.control.send(request).await.is_ok() {
        // The engine answers `Err` for a pane with no detector, which is
        // not a producer error: the record simply has no author here.
        if let Ok(Err(reason)) = rx.await {
            debug!(%reason, "APPEND_RESOURCE_OUTPUT: parent declined the stream evidence");
        }
    }
}

// ---- attach -----------------------------------------------------------------

/// Bootstrap a consumer onto an `AgentSession` stream (ADR-0103 §4).
///
/// The ADR-0070 shape with a different payload: `BOOTSTRAP_BEGIN` naming
/// the `AgentEventsJsonlV1` profile and the cut, chunks carrying the
/// retained records, then `READY`. The grid fields are `0 x 0` — a session
/// has no grid, and the sentinel is what `TerminalInfo` reports for it too.
/// `READY` carries no history cursor: the ring *is* the history, replayed
/// whole, so there is no older page to page back through and
/// `HISTORY_REQUEST` on this stream is refused.
///
/// The live subscription is taken *before* the cut is requested, so a
/// record appended between the two is delivered rather than lost; the pump
/// drops anything at or below `base_seq`, which the replay already carried.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn attach_agent_session(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::TerminalId,
    core: phux_core::ids::ResourceId,
    handle: &crate::resource::ResourceHandle,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    limits: BootstrapLimits,
    connection_token: &CancellationToken,
) -> CommandResult {
    let Ok(session) = handle.agent_session() else {
        return CommandResult::Error {
            code: ErrorCode::WrongResourceKind,
            message: "not an agent session".to_owned(),
        };
    };
    let Some(bootstrap_id) = state.with_mut(|s| s.next_attach_terminal_bootstrap_id(client_id))
    else {
        return CommandResult::Error {
            code: ErrorCode::ResourceExhausted,
            message: "ATTACH_TERMINAL bootstrap id space exhausted".to_owned(),
        };
    };
    let stream_id = crate::runtime::attach::stream_id_from(client_id.0);
    let live = handle.output.subscribe();

    let (reply, rx) = oneshot::channel();
    if session
        .bootstrap
        .send(BootstrapRequest { reply })
        .await
        .is_err()
    {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "the session closed before its bootstrap was cut".to_owned(),
        };
    }
    let Ok(cut) = rx.await else {
        return CommandResult::Error {
            code: ErrorCode::TerminalNotFound,
            message: "the session closed before its bootstrap was cut".to_owned(),
        };
    };
    if cut.dropped > 0 {
        debug!(
            session = %terminal_id,
            dropped = cut.dropped,
            "ATTACH_TERMINAL: the replay starts after evicted records"
        );
    }

    for frame in bootstrap_frames(terminal_id, stream_id, bootstrap_id, &cut, limits) {
        if out_tx.send(Outbound::Frame(frame)).await.is_err() {
            return CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                message: "the client went away mid-bootstrap".to_owned(),
            };
        }
    }

    let pump_out = out_tx.clone();
    let pump_terminal = terminal_id.clone();
    let cancel = connection_token.child_token();
    let done = CancellationToken::new();
    let pump_done = done.clone();
    let handle = tokio::task::spawn_local(async move {
        live_pump(
            live,
            pump_out,
            pump_terminal,
            stream_id,
            bootstrap_id,
            cut.base_seq,
            cancel,
        )
        .await;
        pump_done.cancel();
    });
    state.with_mut(|s| s.track_terminal_output_pump(client_id, core, handle.abort_handle(), done));
    CommandResult::Ok
}

/// The bootstrap frame run for one cut: BEGIN, the retained records in
/// chunks, then READY.
fn bootstrap_frames(
    terminal_id: &phux_protocol::ids::TerminalId,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    cut: &AgentSessionBootstrap,
    limits: BootstrapLimits,
) -> Vec<FrameKind> {
    let budget = usize::try_from(limits.max_chunk_bytes()).unwrap_or(DEFAULT_AGENT_CHUNK_BYTES);
    let mut frames = vec![FrameKind::BootstrapBegin {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        profile: BootstrapStreamProfile::AgentEventsJsonlV1,
        cols: 0,
        rows: 0,
        base_seq: cut.base_seq,
    }];
    let mut chunk_seq = 0u32;
    let mut payload: Vec<u8> = Vec::new();
    for record in &cut.records {
        // A record is never split across chunks: every chunk this stream
        // emits is itself a run of complete records, so a consumer that
        // decodes chunk by chunk never holds a half line.
        if !payload.is_empty() && payload.len().saturating_add(record.len()) > budget {
            frames.push(FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload: Bytes::from(std::mem::take(&mut payload)),
            });
            chunk_seq = chunk_seq.saturating_add(1);
        }
        payload.extend_from_slice(record);
    }
    if !payload.is_empty() {
        frames.push(FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload: Bytes::from(payload),
        });
    }
    frames.push(FrameKind::BootstrapReady {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    });
    frames
}

/// Forward live records to one attached consumer until it detaches or the
/// session closes.
async fn live_pump(
    mut live: tokio::sync::broadcast::Receiver<PaneOutput>,
    out_tx: tokio::sync::mpsc::Sender<Outbound>,
    terminal_id: phux_protocol::ids::TerminalId,
    stream_id: phux_protocol::ids::StreamId,
    bootstrap_id: phux_protocol::ids::BootstrapId,
    base_seq: u64,
    cancel: CancellationToken,
) {
    loop {
        let output = tokio::select! {
            () = cancel.cancelled() => return,
            received = live.recv() => received,
        };
        match output {
            Ok(PaneOutput::Live { seq, bytes }) => {
                if seq <= base_seq {
                    continue;
                }
                if out_tx
                    .send(Outbound::Frame(FrameKind::TerminalOutput {
                        terminal_id: terminal_id.clone(),
                        stream_id,
                        bootstrap_id,
                        seq,
                        bytes,
                    }))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // A session emits no resync and no ordered control: it has no
            // grid to reflow and no native generation to tombstone.
            Ok(PaneOutput::Resync { .. } | PaneOutput::Control { .. }) => {}
            Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                warn!(
                    session = %terminal_id,
                    skipped,
                    "agent session consumer fell behind; records were dropped from its view"
                );
            }
            Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
        }
    }
}
