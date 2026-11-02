//! Publication of a freshly spawned pane to its spawning client.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

/// The freshly spawned pane and everything its publication needs.
pub(super) struct SpawnPublication<'a> {
    pub(in crate::runtime) state: &'a SharedState,
    pub(in crate::runtime) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(super) request_id: u32,
    pub(in crate::runtime) client_id: ClientId,
    pub(super) core_terminal_id: ResourceId,
    pub(super) wire_terminal_id: phux_protocol::ids::ResourceId,
    pub(super) handle: ResourceHandle,
    pub(super) terminal: phux_terminal::actor::TerminalHandle,
    pub(in crate::runtime) client_caps: ClientCapabilities,
    pub(super) stream_id: StreamId,
    pub(in crate::runtime) profile: BootstrapStreamProfile,
    pub(in crate::runtime) limits: BootstrapLimits,
    /// Answer with the instance token (ADR-0109).
    pub(super) bind_instance: bool,
    /// Unbound again if the pane is reaped (ADR-0126).
    pub(super) idempotency_key: Option<phux_protocol::ids::IdempotencyKey>,
}

impl SpawnPublication<'_> {
    /// Drop the pane nobody can reach: the client never received a usable
    /// generation for it.
    fn reap(&self) {
        self.state.with_mut(|s| {
            let _ = crate::runtime::client::reap_pane_journaling_close(s, self.core_terminal_id);
            crate::runtime::idempotent_create::unbind_spawned(
                s,
                self.idempotency_key,
                &self.wire_terminal_id,
            );
        });
    }

    /// Queue the successful `RESOURCE_SPAWNED` reply. `false` once the
    /// client's outbound mailbox has closed.
    async fn queue_spawned_ok(&self) -> bool {
        let instance = self
            .bind_instance
            .then(|| self.state.with(|s| s.idspace.instance()));
        self.out_tx
            .send(Outbound::Frame(FrameKind::ResourceSpawned {
                request_id: self.request_id,
                result: spawned_result(self.wire_terminal_id.clone(), instance),
            }))
            .await
            .is_ok()
    }

    /// Capture the pane's first native checkpoint. `None` once the actor is
    /// gone or refuses the capture.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) async fn capture_native_checkpoint(
        &self,
    ) -> Option<phux_terminal::actor::NativeBootstrapReply> {
        let captured = request_native_checkpoint(&self.terminal, |reply| {
            phux_terminal::actor::NativeBootstrapRequest::full_budget(
                self.client_id.0,
                self.wire_terminal_id.clone(),
                self.stream_id,
                initial_bootstrap_id(),
                self.limits,
                reply,
            )
        })
        .await;
        if let Err(NativeRequestFailure::Refused(error)) = &captured {
            let core_terminal_id = self.core_terminal_id;
            warn!(?core_terminal_id, %error, "native spawn preflight failed");
        }
        captured.ok()
    }

    /// Subscribe the output pump (before the reply, so early PTY output is
    /// buffered), then publish the first generation. Under QUIC multi-stream
    /// (`defer_subscription`) only the reply goes out; the pump and bootstrap
    /// start at the client's `STREAM_BIND`.
    pub(super) async fn publish(
        self,
        output_pumps: &mut JoinSet<()>,
        connection_token: &CancellationToken,
        defer_subscription: bool,
    ) {
        if defer_subscription {
            if !self.queue_spawned_ok().await {
                self.reap();
            }
            return;
        }
        let output_rx = self.handle.output.subscribe();
        let gate_tx = spawn_terminal_output_pump(
            OutputPumpContext {
                out_tx: self.out_tx.clone(),
                resize: self.terminal.resize.clone(),
                wire_terminal_id: self.wire_terminal_id.clone(),
                stream_id: self.stream_id,
                initial_bootstrap_id: initial_bootstrap_id(),
                client_id: self.client_id,
                client_caps: self.client_caps,
                profile: self.profile,
                limits: self.limits,
                lag_label: "SPAWN_RESOURCE output pump",
                stale_skip: true,
                cancel: None,
                drain: Some(CancellationToken::new()),
                snapshot: Some(self.terminal.snapshot.clone()),
                last_seq: None,
                #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
                terminal: self.terminal.clone(),
            },
            output_rx,
            self.state,
            connection_token,
            self.core_terminal_id,
            output_pumps,
        );
        self.publish_first_generation(gate_tx).await;
    }

    /// Capture the pane's first synthesized snapshot and the actor cut it was
    /// taken at. `None` once the actor is gone or refuses.
    async fn capture_snapshot(&self) -> Option<(phux_terminal::grid::SnapshotBytes, u64)> {
        let (snapshot_tx, snapshot_rx) = oneshot::channel();
        self.terminal
            .snapshot
            .send(SnapshotRequest {
                scrollback: None,
                image_protocols: self.client_caps.image_protocols,
                max_bytes: usize::MAX,
                max_frames: usize::MAX,
                chunk_bytes: 1,
                reply: snapshot_tx,
            })
            .await
            .ok()?;
        snapshot_rx.await.ok()?.ok()
    }

    /// Capture and frame the first generation for the negotiated profile.
    async fn stage_first_generation(&self) -> Result<FirstGeneration, &'static str> {
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if publishes_native_checkpoints(self.profile) {
            let reply = self
                .capture_native_checkpoint()
                .await
                .ok_or("native checkpoint preflight failed")?;
            return Ok(FirstGeneration {
                cut: reply.base_seq,
                cursor: Some(reply.publication_cursor),
                frames: reply.frames,
            });
        }
        let (snapshot, cut) = self
            .capture_snapshot()
            .await
            .ok_or("snapshot preflight failed")?;
        let replay = downsample_for_caps(&bytes::Bytes::from(snapshot.bytes), self.client_caps);
        let frames = synthesized_bootstrap_frames(
            self.wire_terminal_id.clone(),
            self.stream_id,
            initial_bootstrap_id(),
            self.profile,
            self.limits,
            snapshot.cols,
            snapshot.rows,
            cut,
            [replay],
        )
        .map_err(|()| "bootstrap limits rejected snapshot")?;
        Ok(FirstGeneration {
            frames,
            cut,
            #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
            cursor: None,
        })
    }

    /// Reply, queue the first generation, activate a native publication, then
    /// release the parked output pump. Any failure reaps the pane.
    async fn publish_first_generation(&self, gate_tx: oneshot::Sender<OutputPumpStart>) {
        let generation = match self.stage_first_generation().await {
            Ok(generation) => generation,
            Err(reason) => {
                self.reap();
                refuse_spawn(
                    self.out_tx,
                    self.request_id,
                    SpawnError::SpawnFailed(reason.to_owned()),
                )
                .await;
                return;
            }
        };
        if !self.queue_spawned_ok().await {
            self.reap();
            return;
        }
        for frame in generation.frames {
            if self.out_tx.send(Outbound::Frame(frame)).await.is_err() {
                self.reap();
                return;
            }
        }
        #[cfg_attr(
            not(all(feature = "native-engine", not(target_arch = "wasm32"))),
            allow(unused_mut)
        )]
        let mut start = OutputPumpStart {
            published_cut: generation.cut,
            replay: Vec::new(),
            live: None,
        };
        #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
        if let Some(cursor) = generation.cursor {
            let Ok(publication) = activate_native_publication(
                &self.terminal,
                self.client_id.0,
                self.wire_terminal_id.clone(),
                self.stream_id,
                initial_bootstrap_id(),
                cursor,
            )
            .await
            else {
                self.reap();
                return;
            };
            start.replay = publication.replay;
            start.live = Some(publication.live);
        }
        let _ = gate_tx.send(start);
    }
}

/// A spawn's first generation, staged before anything is published.
struct FirstGeneration {
    pub(super) frames: Vec<FrameKind>,
    pub(super) cut: u64,
    /// The native publication to activate once the frames are queued.
    #[cfg(all(feature = "native-engine", not(target_arch = "wasm32")))]
    pub(super) cursor: Option<phux_terminal::native_state::OpaqueHistoryCursor>,
}
