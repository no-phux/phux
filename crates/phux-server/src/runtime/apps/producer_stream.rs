//! The stream mechanics a producer-fed kind shares: one ordered attach
//! transaction (subscribe, cut, retire the prior generation, publish
//! BEGIN/chunks/READY, run the live pump), the live pump itself, and the
//! spawn registration every producer child goes through.
//!
//! This module names no engine. Each kind brings its cut (how it chunks a
//! payload and which profile it announces), its lag recovery, and its
//! client-visible refusals; everything here is what those kinds would
//! otherwise each write out the same way.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use phux_protocol::caps::{BootstrapLimits, BootstrapStreamProfile, ResourceKindSet};
use phux_protocol::ids::{
    BootstrapId, IdempotencyKey, ResourceId as WireResourceId, ResourceKind as WireKind, StreamId,
};
use phux_protocol::wire::frame::{
    AgentEvent, CommandResult, ErrorCode, FrameKind, SpawnError, TombstoneReason,
};
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::mailbox::Outbound;
use crate::resource::{ResourceHandle, ResourceOutput};
use crate::runtime::commands::AttachResourceFailure;
use crate::runtime::spawn_reply::spawned_result;
use crate::state::{ClientId, ServerState, SharedState};

/// The identity every frame of one bootstrap generation carries.
#[derive(Clone, Debug)]
#[allow(
    clippy::struct_field_names,
    reason = "the fields are the wire frames' own names"
)]
pub(super) struct StreamIds {
    pub(super) terminal_id: WireResourceId,
    pub(super) stream_id: StreamId,
    pub(super) bootstrap_id: BootstrapId,
}

impl StreamIds {
    /// One `BOOTSTRAP_CHUNK` of this generation.
    pub(super) fn chunk(&self, chunk_seq: u32, payload: Bytes) -> FrameKind {
        FrameKind::BootstrapChunk {
            terminal_id: self.terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            chunk_seq,
            payload,
        }
    }
}

/// One cut of a producer stream: where live output resumes, what profile
/// its BEGIN announces, and how the kind splits its payload into chunks.
pub(super) trait StreamCut {
    /// The sequence the live pump resumes after.
    fn base_seq(&self) -> u64;
    /// The profile `BOOTSTRAP_BEGIN` announces for this cut.
    fn profile(&self) -> BootstrapStreamProfile;
    /// The payload as `BOOTSTRAP_CHUNK`s within the negotiated budget.
    fn chunks(&self, ids: StreamIds, limits: BootstrapLimits) -> impl Iterator<Item = FrameKind>;
}

/// The bootstrap frame run for one cut: BEGIN, the kind's chunks, then READY.
pub(super) fn bootstrap_frames(
    cut: &impl StreamCut,
    ids: StreamIds,
    limits: BootstrapLimits,
) -> impl Iterator<Item = FrameKind> {
    let begin = FrameKind::BootstrapBegin {
        terminal_id: ids.terminal_id.clone(),
        stream_id: ids.stream_id,
        bootstrap_id: ids.bootstrap_id,
        profile: cut.profile(),
        cols: 0,
        rows: 0,
        base_seq: cut.base_seq(),
    };
    let ready = FrameKind::BootstrapReady {
        terminal_id: ids.terminal_id.clone(),
        stream_id: ids.stream_id,
        bootstrap_id: ids.bootstrap_id,
        history_cursor: None,
    };
    std::iter::once(begin)
        .chain(cut.chunks(ids, limits))
        .chain(std::iter::once(ready))
}

/// How a kind brings a viewer the broadcast lapped back up to date.
pub(super) trait LagRecovery {
    /// Recover a viewer that lagged at `cursor`, `skipped` outputs behind:
    /// the new cursor, or `None` when the pump must end. The pump has
    /// already checked it was not cancelled.
    async fn recover(&self, pump: &mut PumpCore, cursor: u64, skipped: u64) -> Option<u64>;
}

/// Why a [`PumpCore::republish`] ended without a delivered replacement.
pub(super) enum RepublishEnd {
    /// The generation could not be advanced or its tombstone not delivered.
    Fenced,
    /// The replacement, already named `next` in the state table, was cut
    /// short: the viewer went away or the pump was cancelled mid-bootstrap.
    CutShort(BootstrapId),
}

/// One viewer's live subscription to a producer stream.
pub(super) struct PumpCore {
    pub(super) state: SharedState,
    pub(super) client_id: ClientId,
    pub(super) core: phux_core::ids::ResourceId,
    pub(super) out_tx: mpsc::Sender<Outbound>,
    pub(super) terminal_id: WireResourceId,
    pub(super) stream_id: StreamId,
    pub(super) bootstrap_id: BootstrapId,
    pub(super) limits: BootstrapLimits,
    pub(super) cancel: CancellationToken,
    /// The last output delivered, shared with a replacing attach's tombstone.
    pub(super) last_seq: Arc<AtomicU64>,
}

impl PumpCore {
    /// Forward outputs after `base_seq` until the viewer detaches, goes
    /// away, or the resource closes; a lag goes to `recovery`.
    pub(super) async fn run(
        mut self,
        recovery: impl LagRecovery,
        mut live: broadcast::Receiver<ResourceOutput>,
        base_seq: u64,
    ) {
        let mut cursor = base_seq;
        loop {
            let output = tokio::select! {
                () = self.cancel.cancelled() => return,
                received = live.recv() => received,
            };
            let next = match output {
                // A batch is cut atomically: at or behind the cursor, every
                // output in it was already delivered.
                Ok(ResourceOutput::Live { seq, .. }) if seq <= cursor => Some(cursor),
                Ok(ResourceOutput::Live { seq, bytes, .. }) => {
                    self.forward(seq, bytes).await.then_some(seq)
                }
                // A producer stream has no grid to resync.
                Ok(ResourceOutput::Resync { .. } | ResourceOutput::Control { .. }) => Some(cursor),
                Err(RecvError::Lagged(skipped)) => self.lagged(&recovery, cursor, skipped).await,
                Err(RecvError::Closed) => None,
            };
            let Some(next) = next else { return };
            cursor = next;
        }
    }

    async fn lagged(
        &mut self,
        recovery: &impl LagRecovery,
        cursor: u64,
        skipped: u64,
    ) -> Option<u64> {
        if self.cancel.is_cancelled() {
            return None;
        }
        recovery.recover(self, cursor, skipped).await
    }

    /// Put one live frame on the viewer's mailbox; `false` once it is gone.
    pub(super) async fn forward(&self, seq: u64, bytes: Bytes) -> bool {
        let frame = FrameKind::ResourceOutput {
            terminal_id: self.terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            seq,
            bytes,
        };
        if !self.send(frame).await {
            return false;
        }
        self.last_seq.store(seq, Ordering::Release);
        true
    }

    async fn send(&self, frame: FrameKind) -> bool {
        self.out_tx.send(Outbound::Frame(frame)).await.is_ok()
    }

    /// Fence the generation at `cursor` (`OUTBOUND_GAP`) and bootstrap the
    /// viewer again from `cut`; its `base_seq` once delivered.
    pub(super) async fn republish(
        &mut self,
        cursor: u64,
        cut: &impl StreamCut,
    ) -> Result<u64, RepublishEnd> {
        let next = self
            .state
            .with_mut(|s| {
                s.resources_mut()
                    .advance_pump_generation(self.client_id, self.core, &self.last_seq)
            })
            .ok_or(RepublishEnd::Fenced)?;
        let tombstone = FrameKind::BootstrapTombstone {
            terminal_id: self.terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
            reason: TombstoneReason::OutboundGap,
            last_valid_seq: cursor,
        };
        if !self.send(tombstone).await {
            return Err(RepublishEnd::Fenced);
        }
        self.bootstrap_id = next;
        for frame in bootstrap_frames(cut, self.ids(), self.limits) {
            if self.cancel.is_cancelled() || !self.send(frame).await {
                return Err(RepublishEnd::CutShort(next));
            }
        }
        self.last_seq.store(cut.base_seq(), Ordering::Release);
        Ok(cut.base_seq())
    }

    fn ids(&self) -> StreamIds {
        StreamIds {
            terminal_id: self.terminal_id.clone(),
            stream_id: self.stream_id,
            bootstrap_id: self.bootstrap_id,
        }
    }
}

/// The negotiated context of one stream attach.
pub(super) struct StreamAttach<'a> {
    pub(super) state: &'a SharedState,
    pub(super) client_id: ClientId,
    pub(super) terminal_id: &'a WireResourceId,
    pub(super) core: phux_core::ids::ResourceId,
    pub(super) handle: &'a ResourceHandle,
    pub(super) out_tx: &'a mpsc::Sender<Outbound>,
    pub(super) limits: BootstrapLimits,
    pub(super) connection_token: &'a CancellationToken,
}

/// Bootstrap a viewer at one cut, then live: allocate the generation,
/// subscribe before `cut` runs so nothing published between them is lost,
/// end the prior pump before BEGIN, tombstone a replaced generation
/// (`EXPLICIT_REATTACH`), publish the cut, and hand the viewer to the pump.
/// `cut` is the kind's cut, with its own refusal when it cannot be made.
pub(super) async fn attach_stream<C: StreamCut>(
    attach: StreamAttach<'_>,
    cut: impl Future<Output = Result<C, CommandResult>>,
    recovery: impl LagRecovery + 'static,
) -> CommandResult {
    let StreamAttach {
        state,
        client_id,
        terminal_id,
        core,
        handle,
        out_tx,
        limits,
        connection_token,
    } = attach;
    let Some(bootstrap_id) = state.with_mut(|s| s.resources_mut().next_bootstrap_id(client_id))
    else {
        return CommandResult::error(
            ErrorCode::ResourceExhausted,
            "ATTACH_RESOURCE bootstrap id space exhausted",
        );
    };
    let stream_id = crate::runtime::stream_id_from(client_id.0);
    let live = handle.output.subscribe();
    // Await the old task's exit before BEGIN: no prior generation may leak
    // output between this replacement's bootstrap frames.
    crate::runtime::pump::stop_output(state, client_id, core).await;
    let cut = match cut.await {
        Ok(cut) => cut,
        Err(result) => return result,
    };

    let (cancel, done, last_seq, prior) = state.with_mut(|s| {
        s.resources_mut()
            .replace_pump(client_id, core, bootstrap_id)
    });
    let pump_done = done.clone().drop_guard();
    if let Some((prior_id, prior_done, prior_seq)) = prior {
        prior_done.cancelled().await;
        let tombstone = FrameKind::BootstrapTombstone {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id: prior_id,
            reason: TombstoneReason::ExplicitReattach,
            last_valid_seq: prior_seq.load(Ordering::Acquire),
        };
        if out_tx.send(Outbound::Frame(tombstone)).await.is_err() {
            return CommandResult::error(
                ErrorCode::ResourceExhausted,
                "the client went away before its replacement bootstrap",
            );
        }
    }
    let ids = StreamIds {
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
    };
    for frame in bootstrap_frames(&cut, ids, limits) {
        if out_tx.send(Outbound::Frame(frame)).await.is_err() {
            return CommandResult::error(
                ErrorCode::ResourceExhausted,
                "the client went away mid-bootstrap",
            );
        }
    }
    let base_seq = cut.base_seq();
    last_seq.store(base_seq, Ordering::Release);

    let pump = PumpCore {
        state: state.clone(),
        client_id,
        core,
        out_tx: out_tx.clone(),
        terminal_id: terminal_id.clone(),
        stream_id,
        bootstrap_id,
        limits,
        cancel,
        last_seq,
    };
    let connection_cancel = connection_token.clone();
    let task = tokio::task::spawn_local(async move {
        let _done = pump_done;
        tokio::select! {
            () = connection_cancel.cancelled() => {}
            () = pump.run(recovery, live, base_seq) => {}
        }
    });
    state.with_mut(|s| {
        s.resources_mut()
            .track_output_pump(client_id, core, task.abort_handle(), done, None);
    });
    CommandResult::Ok
}

/// A stream attach's result as `ResourceApp::bootstrap` reports it.
pub(super) fn result_to_attach_failure(
    label: &str,
    result: CommandResult,
) -> Result<(), AttachResourceFailure> {
    match result {
        CommandResult::Ok | CommandResult::OkWith(_) => Ok(()),
        CommandResult::Error { code, message } => Err(AttachResourceFailure { code, message }),
        other => Err(AttachResourceFailure::internal(&format!(
            "{label} attach returned an unexpected result: {other:?}"
        ))),
    }
}

/// A producer child built and checked by its kind, ready to register.
pub(super) struct SpawnedChild<'a, F> {
    pub(super) core: phux_core::ids::ResourceId,
    pub(super) handle: ResourceHandle,
    pub(super) token: CancellationToken,
    pub(super) actor: F,
    pub(super) kind: WireKind,
    pub(super) parent: &'a WireResourceId,
    pub(super) client_id: ClientId,
    pub(super) idempotency_key: Option<IdempotencyKey>,
    pub(super) out_tx: &'a mpsc::Sender<Outbound>,
}

/// Register a spawned producer child in the borrow that created it, so an
/// id observed in an event already resolves: start its actor (reaping the
/// child when that fails), announce it, bind its idempotency key, and
/// subscribe the spawner to it.
pub(super) fn register_spawned_child<F>(
    s: &mut ServerState,
    child: SpawnedChild<'_, F>,
) -> Result<WireResourceId, SpawnError>
where
    F: Future<Output = ()> + 'static,
{
    let SpawnedChild {
        core,
        handle,
        token,
        actor,
        kind,
        parent,
        client_id,
        idempotency_key,
        out_tx,
    } = child;
    let wire = s
        .spawn_resource_actor(core, handle, token, actor)
        .map_err(|error| {
            s.reap_resource(core);
            SpawnError::SpawnFailed(error.to_string())
        })?;
    // Journaled before the exit watcher exists (ADR-0123), and fanned out
    // to the parent's subscribers too: nobody can be subscribed to a
    // resource they are hearing about for the first time.
    let announcement = crate::state::EventRecord::new(
        Some(wire.clone()),
        AgentEvent::ResourceSpawned {
            kind,
            parent: Some(parent.clone()),
        },
    )
    .with_parent(Some(parent.clone()))
    .with_actor(Some(client_id))
    .with_operation_id(idempotency_key);
    let _ = s.record_and_fanout(announcement);
    // ADR-0126: the key binds in the step that registers the child.
    crate::runtime::idempotent_create::bind_spawned(s, idempotency_key, &wire);
    // ADR-0109: provenance before the spawner's own subscription.
    s.resources_mut().record_spawn(core, client_id);
    s.subscribe_resource(client_id, core, Some(out_tx.clone()));
    // ADR-0127: a session attached as `VIEWER` watches its spawns too.
    s.mark_if_viewer_session(client_id, &wire);
    Ok(wire)
}

/// What answering a registered producer spawn needs.
pub(super) struct SpawnFinish<'a> {
    pub(super) state: &'a SharedState,
    pub(super) request_id: u32,
    pub(super) out_tx: &'a mpsc::Sender<Outbound>,
    pub(super) root_token: &'a CancellationToken,
    pub(super) bind_instance: bool,
    pub(super) decodes: ResourceKindSet,
    pub(super) kind: WireKind,
}

/// Watch the registered child's exit, run the kind's `bind` step, answer
/// the spawn, then stream the child to its spawner through `attach` when
/// its `HELLO` lists the kind. One that does not stays its producer,
/// streamed nothing. Both steps are closures so neither future is held
/// while the other runs.
pub(super) async fn finish_spawn(
    finish: SpawnFinish<'_>,
    core: phux_core::ids::ResourceId,
    wire: &WireResourceId,
    exit_notify: oneshot::Receiver<phux_core::process::ExitOutcome>,
    bind: impl AsyncFnOnce(),
    attach: impl AsyncFnOnce() -> CommandResult,
) {
    let SpawnFinish {
        state,
        request_id,
        out_tx,
        root_token,
        bind_instance,
        decodes,
        kind,
    } = finish;
    crate::runtime::connections::spawn_resource_exit_watcher(
        state.clone(),
        core,
        Some(exit_notify),
        root_token.clone(),
        None,
    );
    bind().await;
    let instance = bind_instance.then(|| state.with(|s| s.idspace.instance()));
    let _ = out_tx
        .send(Outbound::Frame(FrameKind::ResourceSpawned {
            request_id,
            result: spawned_result(wire.clone(), instance),
        }))
        .await;
    // The subscription alone forwards nothing: run the (empty) bootstrap
    // and live pump the `ATTACH_RESOURCE` path runs.
    if decodes.contains(kind) {
        let _ = attach().await;
    }
}
