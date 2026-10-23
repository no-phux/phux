//! Attach stream: extracted from the shared module.
use super::{
    AttachResourceSession, BootstrapLimits, BootstrapProfile, CancellationToken,
    ClientCapabilities, ClientId, Command, CommandContext, CommandResult, ControlAction,
    ControlRequest, ErrorCode, Outbound, ResourceHandle, RoleEffects, RolePolicy, SharedState,
    debug, oneshot, pump, terminal_not_found, wrong_resource_kind,
};

/// The attribution a kill stamps on the `pane_closed` of everything it
/// closes: the sending connection and, when keyed, its `operation_id`.
pub(super) const fn kill_attribution(
    client_id: ClientId,
    operation_id: Option<phux_protocol::ids::IdempotencyKey>,
) -> crate::state::CloseAttribution {
    crate::state::CloseAttribution {
        actor: Some(client_id),
        operation_id,
    }
}

/// `KILL_RESOURCE`: close the pane through the same teardown a natural exit
/// takes, so `RESOURCE_CLOSED` still fires.
pub(super) fn handle_kill_terminal(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    attribution: crate::state::CloseAttribution,
) -> CommandResult {
    state
        .with(|s| s.terminal_from_wire(terminal_id))
        .map_or_else(
            || terminal_not_found(terminal_id),
            |core_id| {
                // ADR-0104 §2: the target and its children close in one lock,
                // and the commit is the teardown (L1 §5.2).
                let committed = state.with_mut(|s| {
                    super::super::committed_close::commit_close(
                        s,
                        &[core_id],
                        phux_protocol::wire::frame::CloseReason::Killed,
                        attribution,
                    )
                });
                super::super::committed_close::announce(state, committed);
                CommandResult::Ok
            },
        )
}

/// `KILL_RESOURCE_IF` (ADR-0109, L1 §5.2.1): check and kill under one lock so
/// no attach lands between them. A refusal kills nothing.
pub(super) fn handle_kill_resource_if(
    state: &SharedState,
    terminal_id: &phux_protocol::ids::ResourceId,
    precondition: &phux_protocol::wire::frame::KillPrecondition,
    attribution: crate::state::CloseAttribution,
) -> CommandResult {
    let admitted = state.with_mut(|s| {
        let core = s.admit_conditional_kill(terminal_id, precondition)?;
        Ok(super::super::committed_close::commit_close(
            s,
            &[core],
            phux_protocol::wire::frame::CloseReason::Killed,
            attribution,
        ))
    });
    match admitted {
        Ok(committed) => {
            super::super::committed_close::announce(state, committed);
            CommandResult::Ok
        }
        Err(refusal) => kill_if_refusal(terminal_id, refusal),
    }
}

/// The typed refusal a `KILL_RESOURCE_IF` answers when it killed nothing.
pub(super) fn kill_if_refusal(
    terminal_id: &phux_protocol::ids::ResourceId,
    refusal: crate::state::KillIfRefusal,
) -> CommandResult {
    match refusal {
        crate::state::KillIfRefusal::NotFound => terminal_not_found(terminal_id),
        crate::state::KillIfRefusal::Precondition(why) => CommandResult::Error {
            code: ErrorCode::PreconditionFailed,
            message: format!("{terminal_id} not killed: {why}"),
        },
    }
}

/// Handle `ATTACH_RESOURCE` (SPEC §5.1): subscribe the caller to one
/// Terminal's content stream, snapshot first, without a session `ATTACH`.
/// Idempotent: a re-attach re-sends a snapshot without a second pump. It
/// never resizes; interactive callers follow with `RESIZE_TERMINAL`.
#[allow(
    clippy::too_many_arguments,
    reason = "negotiated connection context threaded from dispatch"
)]
pub(super) async fn handle_attach_terminal(
    ctx: &CommandContext,
    terminal_id: &phux_protocol::ids::ResourceId,
    role: RolePolicy,
    agent_after_seq: Option<u64>,
) -> CommandResult {
    let CommandContext {
        state,
        client_id,
        out_tx,
        ..
    } = ctx;
    let client_id = *client_id;
    let subscription =
        match subscribe_attach_terminal(state, client_id, terminal_id, out_tx, Some(role)) {
            Ok(subscription) => subscription,
            Err(refusal) => return refusal,
        };
    announce_role_effects(&subscription, client_id).await;
    let AttachSubscription {
        core,
        handle,
        effects,
        was_viewer,
        ..
    } = subscription;

    let stream_id = crate::runtime::attach::stream_id_from(client_id.0);
    match bootstrap_attach_terminal(
        state,
        client_id,
        terminal_id,
        core,
        &handle,
        out_tx,
        stream_id,
        ctx.client_caps,
        ctx.profile,
        ctx.limits,
        &ctx.token,
        agent_after_seq,
    )
    .await
    {
        Ok(()) => CommandResult::Ok,
        Err(failure) => {
            // The COMMAND path drops its subscription on failure and undoes
            // any lease seizure or role widening it made (ADR-0127).
            let restore = was_viewer && !role.is_viewer();
            let (released, holder) = state.with_mut(|s| {
                s.unsubscribe_terminal(client_id, core);
                if restore {
                    s.set_viewer_mark(client_id, terminal_id, true);
                }
                let released = s.undo_attach_takeover(client_id, core, effects);
                (released, s.input_lease_holder(core))
            });
            if restore && effects.role_changed {
                let _ = handle
                    .control
                    .send(ControlRequest::LeaseChanged {
                        input_holder: holder.map(super::super::wire_client),
                        action: ControlAction::RoleChanged,
                        actor: Some(super::super::wire_client(client_id)),
                    })
                    .await;
            }
            if released {
                let _ = handle
                    .control
                    .send(ControlRequest::LeaseChanged {
                        input_holder: None,
                        action: ControlAction::Released,
                        actor: Some(super::super::wire_client(client_id)),
                    })
                    .await;
            }
            CommandResult::Error {
                code: failure.code,
                message: failure.message,
            }
        }
    }
}

/// Bootstrap one Terminal's content stream into `content_tx` without
/// touching the caller's subscription. The QUIC `STREAM_BIND` path
/// (proto.md §4.2) subscribes on the control mailbox and bootstraps on the
/// stream's; the COMMAND path uses one mailbox for both.
///
/// Failures cancel the generation but never unsubscribe; the returned
/// [`AttachResourceFailure`] shapes the caller's refusal.
#[allow(
    clippy::too_many_arguments,
    reason = "negotiated connection context plus the split mailboxes"
)]
pub(crate) async fn bootstrap_attach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    core: phux_core::ids::ResourceId,
    handle: &ResourceHandle,
    content_tx: &tokio::sync::mpsc::Sender<Outbound>,
    stream_id: phux_protocol::ids::StreamId,
    client_caps: ClientCapabilities,
    bootstrap_profile: BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
    connection_token: &CancellationToken,
    agent_after_seq: Option<u64>,
) -> Result<(), AttachResourceFailure> {
    // A non-Terminal stream has its own bootstrap shape (ADR-0103 §4).
    if handle.kind == crate::resource::ResourceKind::AgentSession {
        return match crate::runtime::resource_commands::attach_agent_session(
            state,
            client_id,
            terminal_id,
            core,
            handle,
            content_tx,
            bootstrap_limits,
            connection_token,
            agent_after_seq,
        )
        .await
        {
            CommandResult::Ok | CommandResult::OkWith(_) => Ok(()),
            CommandResult::Error { code, message } => Err(AttachResourceFailure { code, message }),
            other => Err(AttachResourceFailure::internal(&format!(
                "agent session attach returned an unexpected result: {other:?}"
            ))),
        };
    }
    let Some(stream_profile) = crate::runtime::attach::bootstrap_stream_profile(bootstrap_profile)
    else {
        return Err(AttachResourceFailure {
            code: ErrorCode::CodecUnavailable,
            message: "ATTACH_RESOURCE selected an unsupported bootstrap profile".to_owned(),
        });
    };

    let Some(bootstrap_id) = state.with_mut(|s| s.next_attach_terminal_bootstrap_id(client_id))
    else {
        return Err(AttachResourceFailure {
            code: ErrorCode::ResourceExhausted,
            message: "ATTACH_RESOURCE bootstrap id space exhausted".to_owned(),
        });
    };

    let terminal = match handle.terminal() {
        Ok(terminal) => terminal.clone(),
        Err(error) => {
            state.with_mut(|s| s.cancel_attach_terminal_pump(client_id, core));
            return Err(AttachResourceFailure {
                code: ErrorCode::WrongResourceKind,
                message: error.to_string(),
            });
        }
    };
    let session = AttachResourceSession {
        state,
        out_tx: content_tx,
        connection_token,
        terminal_id,
        core,
        handle: handle.clone(),
        terminal,
        client_id,
        stream_id,
        client_caps,
        stream_profile,
        bootstrap_limits,
    };

    let mut generation = match session.establish_generation(bootstrap_id).await {
        Ok(generation) => generation,
        Err(failure) => return Err(session.failed(failure)),
    };

    let finished = if let Some(state_sync) = generation.state_sync_bootstrap.take() {
        session.finish_state_sync(&generation, state_sync).await
    } else {
        session.finish_bootstrap(&mut generation).await
    };
    finished.map_err(|failure| session.failed(failure))
}

/// Resolve the wire terminal id and register the caller as an output
/// subscriber in one critical section.
///
/// The mailbox is remembered with the subscription because a session-less
/// subscriber has no other route for lifecycle fanout such as
/// `RESOURCE_CLOSED` (L1 §3.1). `role` (ADR-0127) applies in the same
/// critical section; `None` leaves the role untouched (a `STREAM_BIND`).
pub(crate) fn subscribe_attach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    role: Option<RolePolicy>,
) -> Result<AttachSubscription, CommandResult> {
    let not_found = || terminal_not_found(terminal_id);
    state.with_mut(|s| {
        let core = s.terminal_from_wire(terminal_id).ok_or_else(not_found)?;
        let handle = s.resource_handle(core).cloned().ok_or_else(not_found)?;
        // The lease is a Terminal-facet concept (L1 §1.1), so a takeover of
        // any other kind is refused the way `ACQUIRE_INPUT` refuses it.
        if role.is_some_and(RolePolicy::takes_over)
            && let Err(error) = handle.terminal()
        {
            return Err(wrong_resource_kind(error));
        }
        let was_subscribed = s.subscribers_for_terminal(core).contains(&client_id);
        let was_viewer = s.is_viewer(client_id, terminal_id);
        s.subscribe_terminal(client_id, core, Some(out_tx.clone()));
        let effects = role.map_or_else(RoleEffects::default, |policy| {
            s.apply_attach_role(client_id, terminal_id, Some(core), policy, was_subscribed)
        });
        Ok(AttachSubscription {
            core,
            handle,
            effects,
            was_viewer,
            holder: s.input_lease_holder(core),
        })
    })
}

/// What [`subscribe_attach_terminal`] registered, and what its declared role
/// changed (ADR-0127).
pub(crate) struct AttachSubscription {
    /// The subscribed Terminal.
    pub(crate) core: phux_core::ids::ResourceId,
    /// Its engine handle.
    pub(crate) handle: ResourceHandle,
    /// What the role changed; empty when no role was applied.
    pub(crate) effects: RoleEffects,
    /// Whether the connection had declared `VIEWER` on this Terminal
    /// before this attach, so a failed widening can put the tombstone back.
    pub(crate) was_viewer: bool,
    /// The lease holder once the role applied, which every broadcast names.
    pub(crate) holder: Option<ClientId>,
}

/// Why an `ATTACH_RESOURCE` stage failed; [`AttachResourceSession::failed`]
/// rolls the partial generation back exactly once.
#[derive(Debug)]
pub(crate) struct AttachResourceFailure {
    /// Wire error code the caller receives.
    pub(crate) code: ErrorCode,
    /// Human-readable explanation attached to that code.
    pub(crate) message: String,
}

impl AttachResourceFailure {
    /// An internal fault with a fixed explanation — the dominant shape.
    pub(super) fn internal(message: &str) -> Self {
        Self {
            code: ErrorCode::InternalError,
            message: message.to_owned(),
        }
    }
}

/// The pump generation an attach established, plus the handshake artifacts
/// its bootstrap publication still has to consume.
pub(super) struct AttachResourceGeneration {
    /// Replica generation stamped on every frame this attach publishes.
    pub(super) bootstrap_id: phux_protocol::ids::BootstrapId,
    /// Shared cursor the pump keeps current so a replacement generation can
    /// resume from where this one stopped.
    pub(super) generation_last_seq: std::sync::Arc<std::sync::atomic::AtomicU64>,
    /// Releases the actor's live state-sync emission once the bootstrap is on
    /// the wire.
    pub(super) live_gate_tx: tokio::sync::watch::Sender<bool>,
    /// Releases the raw pump's first delta at the published cut. `None` when
    /// the actor's tick owns emission and no raw pump was spawned.
    pub(super) snapshot_gate: Option<oneshot::Sender<u64>>,
    /// Atomic synthesized bootstrap captured with state-sync registration.
    pub(super) state_sync_bootstrap: Option<crate::terminal_actor::StateSyncBootstrap>,
    /// Hands the pump its post-replay live receiver at the native publication
    /// fence.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) native_publication_gate:
        Option<oneshot::Sender<crate::terminal_actor::NativePublicationReply>>,
}

/// The one-shot gates one `ATTACH_RESOURCE` pump generation waits on before
/// it reaches steady state.
pub(super) struct AttachResourcePumpChannels {
    /// Cancels this generation when a replacement attach supersedes it.
    pub(super) token: CancellationToken,
    /// The pane's broadcast output, subscribed before the prior pump stopped.
    pub(super) output_rx: tokio::sync::broadcast::Receiver<crate::terminal_actor::PaneOutput>,
    /// Delivers the published cut, releasing the first forwarded delta.
    pub(super) snapshot_gate: oneshot::Receiver<u64>,
    /// Delivers the post-replay live receiver at the native publication fence.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) native_publication_gate:
        oneshot::Receiver<crate::terminal_actor::NativePublicationReply>,
}

impl AttachResourcePumpChannels {
    /// Wait for this generation's publication: the published cut, then (for
    /// a native profile) the post-replay live receiver. `None` when the
    /// generation was cancelled or abandoned first.
    pub(super) async fn published(
        self,
        #[cfg_attr(
            not(all(feature = "native-engine", not(target_arch = "wasm32"))),
            allow(unused_variables)
        )]
        profile: phux_protocol::caps::BootstrapStreamProfile,
    ) -> Option<(
        crate::runtime::attach::OutputPumpStart,
        tokio::sync::broadcast::Receiver<crate::terminal_actor::PaneOutput>,
    )> {
        let published_cut = tokio::select! {
            () = self.token.cancelled() => return None,
            result = self.snapshot_gate => result.ok()?,
        };
        let mut start = crate::runtime::attach::OutputPumpStart {
            published_cut,
            replay: Vec::new(),
            live: None,
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if native_checkpoint_profile(profile) {
            let publication = self.native_publication_gate.await.ok()?;
            start.live = Some(publication.live);
            start.replay = publication.replay;
        }
        Some((start, self.output_rx))
    }
}

/// Handle `DETACH_RESOURCE` (SPEC §5.1): drop every output task and the
/// event subscription the caller holds on the terminal. Idempotent, so a
/// detach never races a natural close into an error.
pub(crate) async fn handle_detach_terminal(
    state: &SharedState,
    client_id: ClientId,
    terminal_id: &phux_protocol::ids::ResourceId,
) -> CommandResult {
    use crate::terminal_actor::ConsumerDetachRequest;

    let handle = state.with_mut(|s| {
        s.unsubscribe_terminal_events(client_id, terminal_id);
        // A declared `VIEWER` outlives the subscription (ADR-0127).
        let core = s
            .terminal_from_wire(terminal_id)
            .or_else(|| s.draining_terminal_from_wire(terminal_id))?;
        s.unsubscribe_terminal(client_id, core);
        Some((core, s.resource_handle(core).cloned()))
    });
    let Some((core, handle)) = handle else {
        return CommandResult::Ok;
    };
    pump::stop_output(state, client_id, core).await;
    state.with_mut(|s| s.cancel_attach_terminal_pump(client_id, core));
    if let Some(handle) = handle {
        // Fence actor-emitted StateSync output before COMMAND_RESULT is queued.
        let (reply_tx, reply_rx) = oneshot::channel();
        let _ = handle
            .consumer_detach
            .send(ConsumerDetachRequest {
                client_id: super::super::wire_client(client_id),
                reply: reply_tx,
            })
            .await;
        let _ = reply_rx.await;
    }
    debug!(?client_id, ?terminal_id, "DETACH_RESOURCE unsubscribed");
    CommandResult::Ok
}

pub mod subscription;
pub(crate) use subscription::*;
