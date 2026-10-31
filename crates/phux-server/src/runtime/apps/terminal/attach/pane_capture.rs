//! Per-pane `ATTACH` capture: consumer registration and staging.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use phux_terminal::actor::TerminalResource as _;

/// The negotiated shape shared by every pane capture in one ATTACH.
pub(super) struct PaneCaptureContext<'a> {
    pub(in crate::runtime) state: &'a SharedState,
    pub(in crate::runtime) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    /// Cancelled by a pump whose generation became unrecoverable.
    pub(super) connection_token: &'a CancellationToken,
    /// Released once the aggregate publication is on the wire.
    pub(super) live_gate_rx: tokio::sync::watch::Receiver<bool>,
    pub(in crate::runtime) client_id: ClientId,
    pub(super) wire_client_id: phux_protocol::ids::ClientId,
    pub(in crate::runtime) client_caps: ClientCapabilities,
    pub(super) stream_id: StreamId,
    pub(super) bootstrap_id: BootstrapId,
    pub(in crate::runtime) profile: BootstrapStreamProfile,
    pub(in crate::runtime) limits: BootstrapLimits,
    /// The ATTACH's scrollback request, capped in lines.
    pub(super) scrollback: Option<u32>,
    pub(super) chunk_bytes: usize,
}

impl PaneCaptureContext<'_> {
    /// Did this client negotiate state-sync output?
    pub(super) const fn wants_state_sync(&self) -> bool {
        matches!(
            self.client_caps.output_mode,
            phux_protocol::caps::OutputMode::StateSync
        )
    }

    /// Register the per-consumer state-sync entry (ADR-0018) before the
    /// snapshot, so its cache is primed against the same state. When the
    /// actor tick-manages the consumer, its tick is the sole emitter and the
    /// broadcast pump must be suppressed (two `seq` streams on one mailbox
    /// violate SPEC §12.2). Any failure falls back to the broadcast path.
    pub(super) async fn register_consumer(
        &self,
        handle: &ResourceHandle,
        wire_terminal_id: &phux_protocol::ids::ResourceId,
        terminal_id: ResourceId,
        bootstrap_max_bytes: usize,
        bootstrap_max_frames: usize,
    ) -> ConsumerRegistration {
        let Some(wire_id) = wire_terminal_id.local_id() else {
            return ConsumerRegistration::default();
        };
        let Ok(terminal) = handle.terminal() else {
            return ConsumerRegistration::default();
        };
        let (attach_reply_tx, attach_reply_rx) = oneshot::channel();
        if terminal
            .consumer_attach
            .send(ConsumerAttachRequest {
                client_id: self.wire_client_id,
                outbound: self.out_tx.clone(),
                wire_terminal_id: wire_id,
                stream_id: self.stream_id,
                bootstrap_id: self.bootstrap_id,
                wants_state_sync: self.wants_state_sync(),
                state_sync_scrollback: self.scrollback,
                image_protocols: self.client_caps.image_protocols,
                bootstrap_max_bytes,
                bootstrap_max_frames,
                bootstrap_chunk_bytes: self.chunk_bytes,
                // A direct consumer's transport is reliable and ordered.
                live_gate: self.live_gate_rx.clone(),
                loss_tolerant: false,
                reply: attach_reply_tx,
            })
            .await
            .is_err()
        {
            warn!(
                ?terminal_id,
                "per-consumer state-sync register: actor mailbox closed",
            );
            return ConsumerRegistration {
                actor_gone: true,
                ..ConsumerRegistration::default()
            };
        }
        match attach_reply_rx.await {
            Ok(Ok(outcome)) => {
                trace!(
                    ?terminal_id,
                    tick_managed = outcome.tick_managed,
                    "per-consumer state-sync entry registered",
                );
                ConsumerRegistration {
                    registered: true,
                    tick_managed: outcome.tick_managed,
                    state_sync_bootstrap: outcome.state_sync_bootstrap,
                    actor_gone: false,
                }
            }
            Ok(Err(err)) => {
                warn!(
                    ?terminal_id,
                    error = %err,
                    "per-consumer state-sync register failed; broadcast path still serves this pane",
                );
                ConsumerRegistration::default()
            }
            Err(_) => {
                warn!(
                    ?terminal_id,
                    "per-consumer state-sync register: actor dropped reply",
                );
                ConsumerRegistration {
                    actor_gone: true,
                    ..ConsumerRegistration::default()
                }
            }
        }
    }

    /// Stage this pane's output pump behind its publication gate. It
    /// subscribes before the snapshot is requested, so nothing is missed, and
    /// forwards nothing until the bootstrap is queued. A fatal fault releases
    /// only this client's consumer state: the pane is shared.
    pub(super) fn spawn_pane_pump(
        &self,
        staging: &mut AttachStaging,
        terminal_id: ResourceId,
        wire_terminal_id: &phux_protocol::ids::ResourceId,
        handle: &ResourceHandle,
        terminal: &phux_terminal::actor::TerminalHandle,
    ) {
        let output_rx = handle.output.subscribe();
        let ctx = OutputPumpContext {
            out_tx: self.out_tx.clone(),
            resize: terminal.resize.clone(),
            wire_terminal_id: wire_terminal_id.clone(),
            stream_id: self.stream_id,
            initial_bootstrap_id: self.bootstrap_id,
            client_id: self.client_id,
            client_caps: self.client_caps,
            profile: self.profile,
            limits: self.limits,
            lag_label: "ResourceOutput pump",
            stale_skip: true,
            cancel: None,
            drain: Some(CancellationToken::new()),
            snapshot: Some(terminal.snapshot.clone()),
            last_seq: None,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            terminal: terminal.clone(),
        };
        let pump_state = self.state.clone();
        let pump_connection_token = self.connection_token.clone();
        let client_id = self.client_id;
        let (gate_tx, gate_rx) = oneshot::channel::<OutputPumpStart>();
        staging.gates.push(SnapshotGate {
            terminal_id,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            wire_terminal_id: wire_terminal_id.clone(),
            terminal: terminal.clone(),
            gate: gate_tx,
            cut: None,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            native_cursor: None,
        });
        pump::spawn_tracked(
            self.state,
            client_id,
            terminal_id,
            Some(&mut staging.pumps),
            ctx.drain.clone(),
            async move {
                let Some(fault) = run_output_pump(&ctx, gate_rx, output_rx).await else {
                    return;
                };
                release_after_pump_fault(fault, &pump_state, client_id, &pump_connection_token);
            },
        );
    }

    /// Adapt one pane's synthesized snapshot to the client's capabilities,
    /// frame it, and charge the retained result to the aggregate budget.
    ///
    /// `label` names the capture in the rollback reason the client is told.
    pub(super) fn stage_synthesized_frames(
        &self,
        staging: &mut AttachStaging,
        wire_terminal_id: phux_protocol::ids::ResourceId,
        snapshot: phux_terminal::grid::SnapshotBytes,
        base_seq: u64,
        label: &str,
    ) -> Result<(), String> {
        let cols = snapshot.cols;
        let rows = snapshot.rows;
        let Ok(adapted) =
            adapt_bootstrap_snapshot(snapshot, self.client_caps, staging.budget.remaining_bytes())
        else {
            return Err(format!("{label} adaptation exceeded source budget"));
        };
        debug_assert!(adapted.peak_bytes <= staging.budget.remaining_bytes());
        let Ok(mut frames) = synthesized_bootstrap_frames(
            wire_terminal_id,
            self.stream_id,
            self.bootstrap_id,
            self.profile,
            self.limits,
            cols,
            rows,
            base_seq,
            adapted.payloads,
        ) else {
            return Err(format!("{label} exceeded negotiated bounds"));
        };
        let AttachStaging {
            budget,
            frames: staged,
            ..
        } = staging;
        if budget
            .append_accounted(staged, &mut frames, adapted.retained_bytes)
            .is_err()
        {
            return Err("aggregate bootstrap staging budget exceeded".to_owned());
        }
        Ok(())
    }
}
