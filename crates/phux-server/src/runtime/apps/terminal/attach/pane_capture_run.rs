//! Per-pane `ATTACH` capture: native bootstrap and snapshot requests.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;
use phux_terminal::actor::TerminalResource as _;

impl PaneCaptureContext<'_> {
    /// Capture one pane's native checkpoint against the remaining aggregate
    /// budget and record the cut its pump must resume from.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    async fn stage_native_bootstrap(
        &self,
        staging: &mut AttachStaging,
        terminal_id: ResourceId,
        wire_terminal_id: &phux_protocol::ids::ResourceId,
        terminal: &phux_terminal::actor::TerminalHandle,
    ) -> Result<(), CaptureFailure> {
        let captured = request_native_checkpoint(terminal, |reply| {
            phux_terminal::actor::NativeBootstrapRequest {
                owner: self.client_id.0,
                terminal_id: wire_terminal_id.clone(),
                stream_id: self.stream_id,
                bootstrap_id: self.bootstrap_id,
                limits: self.limits,
                max_bytes: staging.budget.remaining_bytes(),
                max_frames: staging.budget.remaining_frames(),
                reply,
            }
        })
        .await;
        let mut reply = match captured {
            Ok(reply) => reply,
            Err(NativeRequestFailure::Refused(error)) => {
                warn!(?terminal_id, %error, "native checkpoint failed before attach publication");
                return Err(CaptureFailure::Failed(
                    "native checkpoint capture failed".to_owned(),
                ));
            }
            Err(NativeRequestFailure::Unsent) => {
                warn!(?terminal_id, "pane actor dropped before native bootstrap");
                return Err(CaptureFailure::Closed(
                    "pane actor dropped native bootstrap request".to_owned(),
                ));
            }
            Err(NativeRequestFailure::Dropped) => {
                warn!(?terminal_id, "pane actor dropped native checkpoint reply");
                return Err(CaptureFailure::Closed(
                    "pane actor dropped native checkpoint reply".to_owned(),
                ));
            }
        };
        let cut = reply.base_seq;
        let publication_cursor = reply.publication_cursor;
        let AttachStaging {
            budget,
            frames,
            gates,
            ..
        } = staging;
        if budget
            .append_accounted(frames, &mut reply.frames, reply.retained_bytes)
            .is_err()
        {
            return Err(CaptureFailure::Failed(
                "aggregate bootstrap staging budget exceeded".to_owned(),
            ));
        }
        if let Some(gate) = gates
            .iter_mut()
            .find(|gate| gate.terminal_id == terminal_id)
        {
            gate.cut = Some(cut);
            gate.native_cursor = Some(publication_cursor);
        }
        Ok(())
    }

    /// Ask one pane's actor for a bounded synthesized snapshot and the actor
    /// cut it was taken at.
    async fn request_pane_snapshot(
        &self,
        terminal: &phux_terminal::actor::TerminalHandle,
        terminal_id: ResourceId,
        max_bytes: usize,
        max_frames: usize,
    ) -> Result<(phux_terminal::grid::SnapshotBytes, u64), CaptureFailure> {
        let (reply_tx, reply_rx) = oneshot::channel();
        if terminal
            .snapshot
            .send(SnapshotRequest {
                scrollback: self.scrollback,
                image_protocols: self.client_caps.image_protocols,
                max_bytes,
                max_frames,
                chunk_bytes: self.chunk_bytes,
                reply: reply_tx,
            })
            .await
            .is_err()
        {
            warn!(
                ?terminal_id,
                "pane actor dropped before synthesized bootstrap"
            );
            return Err(CaptureFailure::Closed(
                "pane actor dropped synthesized bootstrap request".to_owned(),
            ));
        }
        match reply_rx.await {
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(error)) => {
                warn!(?terminal_id, %error, "bounded snapshot synthesis failed");
                Err(CaptureFailure::Failed(
                    "synthesized bootstrap source limit exceeded".to_owned(),
                ))
            }
            Err(_) => {
                warn!(
                    ?terminal_id,
                    "pane actor dropped synthesized snapshot reply"
                );
                Err(CaptureFailure::Closed(
                    "pane actor dropped synthesized snapshot reply".to_owned(),
                ))
            }
        }
    }

    /// Capture one pane's bounded bootstrap, staging its pump and charging its
    /// retained result to the aggregate budget before the next actor is asked
    /// for its own source.
    async fn capture_pane(
        &self,
        staging: &mut AttachStaging,
        pane: AttachSnapshotPane,
    ) -> Result<(), CaptureFailure> {
        let synthesized_source_max =
            bootstrap_source_ceiling(staging.budget.remaining_bytes(), self.client_caps);
        let terminal_id = pane.terminal_id;
        let handle = pane.handle;
        let terminal = handle
            .terminal()
            .map_err(|error| error.to_string())?
            .clone();
        staging.handles.push(handle.clone());
        let wire_terminal_id = pane.wire_terminal_id;
        let registration = self
            .register_consumer(
                &handle,
                &wire_terminal_id,
                terminal_id,
                synthesized_source_max,
                staging.budget.remaining_frames(),
            )
            .await;
        if self.wants_state_sync() && !registration.registered {
            warn!(
                ?terminal_id,
                "state-sync registration failed before aggregate attach publication"
            );
            let reason = "state-sync consumer registration failed".to_owned();
            return Err(if registration.actor_gone {
                CaptureFailure::Closed(reason)
            } else {
                CaptureFailure::Failed(reason)
            });
        }
        // A tick-managed consumer's deltas come from the actor, in its own
        // sequence space: a broadcast pump beside it would double-emit.
        if !registration.tick_managed {
            self.spawn_pane_pump(staging, terminal_id, &wire_terminal_id, &handle, &terminal);
        }
        if let Some(state_sync) = registration.state_sync_bootstrap {
            return Ok(self.stage_synthesized_frames(
                staging,
                wire_terminal_id,
                state_sync.snapshot,
                state_sync.base_seq,
                "state-sync bootstrap",
            )?);
        }
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if publishes_native_checkpoints(self.profile) {
            return self
                .stage_native_bootstrap(staging, terminal_id, &wire_terminal_id, &terminal)
                .await;
        }
        let (snapshot, cut) = self
            .request_pane_snapshot(
                &terminal,
                terminal_id,
                synthesized_source_max,
                staging.budget.remaining_frames(),
            )
            .await?;
        self.stage_synthesized_frames(
            staging,
            wire_terminal_id,
            snapshot,
            cut,
            "synthesized bootstrap",
        )?;
        if let Some(gate) = staging
            .gates
            .iter_mut()
            .find(|gate| gate.terminal_id == terminal_id)
        {
            gate.cut = Some(cut);
        }
        Ok(())
    }

    /// Capture every bootstrap-capable pane in turn, then stage authoritative
    /// closures for snapshot participants that had no actor handle. The first
    /// failure is the rollback reason.
    ///
    /// A pane whose actor ends while the attach is capturing (its process
    /// exited, or a kill or abandoned spawn reaped it after the snapshot was
    /// taken) is not a failure: `ATTACH_READY` follows once every pane is
    /// READY or closed (L1 §8), so it is staged as closed like a participant
    /// that had no actor at all. Only a pane whose actor is still running can
    /// fail the attach.
    pub(super) async fn capture_panes(
        &self,
        staging: &mut AttachStaging,
        panes: Vec<AttachSnapshotPane>,
        mut closed_before_ready: Vec<phux_protocol::ids::ResourceId>,
    ) -> Result<(), String> {
        for pane in panes {
            let terminal_id = pane.terminal_id;
            let wire_terminal_id = pane.wire_terminal_id.clone();
            match self.capture_pane(staging, pane).await {
                Ok(()) => continue,
                Err(CaptureFailure::Failed(reason)) => return Err(reason),
                Err(CaptureFailure::Closed(reason)) => {
                    debug!(?terminal_id, %reason, "pane closed during ATTACH capture");
                }
            }
            staging.forget_pane(terminal_id);
            closed_before_ready.push(wire_terminal_id);
        }
        staging
            .append_closures(closed_before_ready)
            .map_err(|()| "aggregate bootstrap staging budget exceeded".to_owned())
    }
}
