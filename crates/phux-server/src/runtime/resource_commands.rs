//! `AgentSession` verbs (ADR-0102, ADR-0103): spawn a session under a
//! Terminal parent, append to its stream, and bootstrap a consumer onto it.
//!
//! Stream-derived state ([`StreamEvidence`]) is forwarded to the parent
//! Terminal's arbiter, so existing `phux.agent/v1` consumers see it
//! unchanged.

use bytes::Bytes;
use phux_protocol::caps::{BootstrapLimits, BootstrapStreamProfile};
use phux_protocol::ids::ResourceKind;
use phux_protocol::wire::frame::{
    AgentEvent, CommandResult, CommandValue, ErrorCode, FrameKind, SpawnError, SpawnResource,
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
use crate::runtime::attach::refuse_spawn;
use crate::state::{ClientId, SharedState};

/// Payload budget for one `BOOTSTRAP_CHUNK` when the connection negotiated
/// none. Records are at most 16 KiB, so this fits several per chunk.
const DEFAULT_AGENT_CHUNK_BYTES: usize = 64 * 1024;

/// Handle a `SPAWN_RESOURCE` whose kind is `AgentSession` (ADR-0103 §1).
/// The parent must be a live local Terminal (an `AgentSession` parent is
/// `ParentKindMismatch`, ADR-0104 §5); a satellite-addressed spawn is relayed
/// with the parent reduced to the satellite's `Local` space (ADR-0104 §6).
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
        refuse_spawn(
            out_tx,
            request_id,
            SpawnError::SpawnFailed("an agent session spawn must name a provider".to_owned()),
        )
        .await;
        return;
    };
    let Some(parent) = resource.parent.clone() else {
        refuse_spawn(out_tx, request_id, SpawnError::ParentNotFound).await;
        return;
    };

    if let Some(host) = satellite {
        relay_agent_session_spawn(
            state, client_id, request_id, resource, host, &parent, out_tx,
        )
        .await;
        return;
    }

    // A `Satellite` parent without a `satellite` route is not ours.
    let Some((core_parent, wire_parent)) = state.with(|s| {
        parent
            .is_local()
            .then(|| s.terminal_from_wire(&parent))
            .flatten()
            .map(|core| (core, parent.clone()))
    }) else {
        debug!(?client_id, request_id, %parent, "SPAWN_RESOURCE: agent session parent not found");
        refuse_spawn(out_tx, request_id, SpawnError::ParentNotFound).await;
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

    // One borrow: an id observed in an event must already resolve.
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
        // Journaled before the exit watcher exists (ADR-0123), and fanned out
        // to the parent's subscribers too: nobody can be subscribed to a
        // resource they are hearing about for the first time.
        let announcement = crate::state::EventRecord::new(
            Some(wire.clone()),
            AgentEvent::ResourceSpawned {
                kind: ResourceKind::AgentSession,
                parent: Some(wire_parent.clone()),
            },
        )
        .with_parent(Some(wire_parent.clone()))
        .with_actor(Some(client_id))
        .with_operation_id(resource.idempotency_key);
        let _ = s.record_and_fanout(announcement);
        // ADR-0126: the key binds in the step that registers the session.
        super::idempotent_create::bind_spawned(s, resource.idempotency_key, &wire);
        // ADR-0109: provenance before the spawner's own subscription.
        s.record_spawn(core, client_id);
        s.subscribe_terminal(client_id, core, Some(out_tx.clone()));
        // ADR-0127: a session attached as `VIEWER` watches its spawns too.
        s.mark_if_viewer_session(client_id, &wire);
        Ok((core, wire))
    });
    let (core_session, wire_session) = match registered {
        Ok(pair) => pair,
        Err(error) => {
            refuse_spawn(out_tx, request_id, error).await;
            return;
        }
    };

    crate::runtime::client::spawn_terminal_exit_watcher(
        state.clone(),
        core_session,
        Some(bundle.exit_notify),
        root_token.clone(),
        None,
    );

    // ADR-0103 §6: the parent's `REPORT_AGENT_STATE` now lands on this stream.
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

    let instance = resource
        .bind_instance
        .then(|| state.with(|s| s.idspace.instance()));
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceSpawned {
            request_id,
            result: crate::runtime::attach::spawned_result(wire_session.clone(), instance),
        }))
        .await;
    // The subscription alone forwards nothing: run the (empty) bootstrap and
    // live pump the `ATTACH_RESOURCE` path runs.
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
        "SPAWN_RESOURCE: agent session bound to its parent"
    );
}

/// Forward a satellite-addressed session spawn over the owning hub link,
/// with the parent reduced to that satellite's `Local` space.
async fn relay_agent_session_spawn(
    state: &SharedState,
    client_id: ClientId,
    request_id: u32,
    resource: &SpawnResource,
    host: &phux_protocol::ids::SatelliteHost,
    parent: &phux_protocol::ids::ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
) {
    let phux_protocol::ids::ResourceId::Satellite {
        host: parent_host,
        id,
    } = parent
    else {
        refuse_spawn(
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
        refuse_spawn(
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
    forwarded.parent = Some(phux_protocol::ids::ResourceId::local(*id));
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
    crate::runtime::attach::dispatch_satellite_spawn(
        state,
        client_id,
        out_tx,
        request_id,
        host,
        Ok(spawn),
    )
    .await;
}

/// Handle `APPEND_RESOURCE_OUTPUT` (ADR-0103 §3), replying with the stamped
/// `{"seq":…,"ts_ms":…}` header. Only a local owner connection may produce:
/// a remote peer must not forge a local agent's lifecycle.
pub(crate) async fn handle_append_resource_output(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
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

/// Feed a streamed question into the parent's ask ledger (ADR-0036), and
/// retract the stream's ask when the turn ends.
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
        let wire = s.intern_terminal_wire(parent);
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
        crate::hub::metadata_mirror::publish_asked_flag(s, &wire, s.agent_is_asked(parent));
        (payload, Some(wire))
    });
    if let (Some(payload), Some(wire_parent)) = (payload, wire_parent) {
        crate::runtime::client::broadcast_event(state, Some(&wire_parent), &payload.into_event());
    }
}

/// Record the session's derived state on its own facet and feed it to the
/// parent Terminal's arbiter at the `Stream` rank (ADR-0103 §5).
async fn publish_stream_evidence(
    state: &SharedState,
    session: phux_core::ids::ResourceId,
    parent: Option<phux_core::ids::ResourceId>,
    evidence: StreamEvidence,
) {
    use phux_protocol::wire::frame::ReportedAgentState;
    let (word, reported) = match evidence {
        StreamEvidence::Working => (Some("working"), Some(ReportedAgentState::Working)),
        StreamEvidence::Blocked => (Some("blocked"), Some(ReportedAgentState::Blocked)),
        StreamEvidence::Done => (Some("done"), Some(ReportedAgentState::Done)),
        StreamEvidence::Retract => (None, None),
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
        state: reported,
        reply,
    };
    if parent_handle.control.send(request).await.is_ok() {
        // `Err` means the pane has no detector, not a producer error.
        if let Ok(Err(reason)) = rx.await {
            debug!(%reason, "APPEND_RESOURCE_OUTPUT: parent declined the stream evidence");
        }
    }
}

/// Bootstrap a consumer onto an `AgentSession` stream (ADR-0103 §4):
/// `BEGIN` (profile `AgentEventsJsonlV1`, grid `0 x 0`), the retained ring in
/// chunks, and `READY` with no history cursor, then live records. The live
/// subscription precedes the cut so nothing appended between them is lost.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn attach_agent_session(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
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
            message: "ATTACH_RESOURCE bootstrap id space exhausted".to_owned(),
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
            "ATTACH_RESOURCE: the replay starts after evicted records"
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
    terminal_id: &phux_protocol::ids::ResourceId,
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
        // Chunks hold whole records only.
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
    terminal_id: phux_protocol::ids::ResourceId,
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
            Ok(PaneOutput::Live { seq, bytes, .. }) => {
                if seq <= base_seq {
                    continue;
                }
                if out_tx
                    .send(Outbound::Frame(FrameKind::ResourceOutput {
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
            // A session has no grid to resync and no generation to tombstone.
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
