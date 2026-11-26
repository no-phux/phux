//! Publication of a freshly spawned pane to its spawning client.
#![allow(
    clippy::wildcard_imports,
    reason = "the split attach modules share the parent attach namespace"
)]

use super::*;

/// The freshly spawned pane and everything its publication needs.
pub(in crate::runtime) struct SpawnPublication<'a> {
    pub(in crate::runtime) state: &'a SharedState,
    pub(in crate::runtime) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(in crate::runtime) request_id: u32,
    pub(in crate::runtime) client_id: ClientId,
    pub(in crate::runtime) core_terminal_id: ResourceId,
    pub(in crate::runtime) wire_terminal_id: phux_protocol::ids::ResourceId,
    pub(in crate::runtime) handle: ResourceHandle,
    pub(in crate::runtime) terminal: phux_terminal_api::actor::TerminalHandle,
    pub(in crate::runtime) client_caps: ClientCapabilities,
    pub(in crate::runtime) stream_id: StreamId,
    pub(in crate::runtime) profile: BootstrapStreamProfile,
    pub(in crate::runtime) limits: BootstrapLimits,
    /// Answer with the instance token (ADR-0109).
    pub(in crate::runtime) bind_instance: bool,
    /// Unbound again if the pane is reaped (ADR-0126).
    pub(in crate::runtime) idempotency_key: Option<phux_protocol::ids::IdempotencyKey>,
}

impl SpawnPublication<'_> {
    /// Drop the pane nobody can reach: the client never received a usable
    /// generation for it.
    fn reap(&self, lease: &crate::runtime::apps::StreamLease) {
        lease.reap_spawn(&self.wire_terminal_id, self.idempotency_key);
    }

    /// Queue the successful `RESOURCE_SPAWNED` reply. `false` once the
    /// client's outbound mailbox has closed.
    async fn queue_spawned_ok(&self, lease: &crate::runtime::apps::StreamLease) -> bool {
        let instance = self
            .bind_instance
            .then(|| self.state.with(|s| s.idspace.instance()));
        lease
            .queue(
                self.out_tx,
                FrameKind::ResourceSpawned {
                    request_id: self.request_id,
                    result: spawned_result(self.wire_terminal_id.clone(), instance),
                },
            )
            .await
    }

    /// Capture the pane's first native checkpoint. `None` once the actor is
    /// gone or refuses the capture.
    pub(in crate::runtime) async fn capture_native_checkpoint(
        &self,
    ) -> Option<phux_terminal_api::actor::NativeBootstrapReply> {
        let captured = past_invalidation(|| {
            request_native_checkpoint(&self.terminal, |reply| {
                phux_terminal_api::actor::NativeBootstrapRequest::full_budget(
                    self.client_id.0,
                    self.wire_terminal_id.clone(),
                    self.stream_id,
                    initial_bootstrap_id(),
                    self.limits,
                    reply,
                )
            })
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
    pub(in crate::runtime) async fn publish(
        self,
        output_pumps: &mut JoinSet<()>,
        connection_token: &CancellationToken,
        defer_subscription: bool,
    ) {
        let host = crate::runtime::apps::StreamHost::new(self.state, self.client_id);
        let Some(reservation) = host.reserve(
            self.core_terminal_id,
            initial_bootstrap_id(),
            connection_token,
        ) else {
            return;
        };
        let lease = reservation.lease.clone();
        let _ = lease
            .run(self.publish_reserved(output_pumps, reservation, defer_subscription))
            .await;
    }

    async fn publish_reserved(
        self,
        output_pumps: &mut JoinSet<()>,
        reservation: crate::runtime::apps::StreamReservation,
        defer_subscription: bool,
    ) {
        let lease = reservation.lease.clone();
        let mut withdrawal = PendingSpawn {
            publication: &self,
            lease: &lease,
            published: false,
        };
        if !reservation.ready().await {
            return;
        }
        if defer_subscription {
            if self.queue_spawned_ok(&lease).await && reservation.published(|| true) {
                withdrawal.published = true;
            }
            return;
        }
        let output_rx = self.handle.output.subscribe();
        let Some(gate_tx) = spawn_terminal_output_pump(
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
                cancel: Some(lease.cancellation.clone()),
                drain: Some(CancellationToken::new()),
                snapshot: Some(self.terminal.snapshot.clone()),
                last_seq: Some(std::sync::Arc::clone(&lease.last_seq)),
                terminal: self.terminal.clone(),
            },
            output_rx,
            &lease,
            output_pumps,
        ) else {
            return;
        };
        withdrawal.published = self.publish_first_generation(gate_tx, reservation).await;
    }

    /// Capture the pane's first synthesized snapshot and the actor cut it was
    /// taken at. `None` once the actor is gone or refuses.
    async fn capture_snapshot(&self) -> Option<(phux_terminal_api::grid::SnapshotBytes, u64)> {
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
        if publishes_native_checkpoints(self.profile) {
            let reply = self
                .capture_native_checkpoint()
                .await
                .ok_or("native checkpoint preflight failed")?;
            return Ok(FirstGeneration {
                cut: reply.base_seq,
                capture: Some(reply.capture),
                frames: reply.frames,
            });
        }
        let (snapshot, cut) = self
            .capture_snapshot()
            .await
            .ok_or("snapshot preflight failed")?;
        let replay = downsample_for_caps(
            &self.terminal.presentation,
            &bytes::Bytes::from(snapshot.bytes),
            self.client_caps,
        );
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
            capture: None,
        })
    }

    /// Reply, queue the first generation, activate a native publication, then
    /// release the parked output pump. Any failure reaps the pane.
    async fn publish_first_generation(
        &self,
        gate_tx: oneshot::Sender<OutputPumpStart>,
        reservation: crate::runtime::apps::StreamReservation,
    ) -> bool {
        let lease = reservation.lease.clone();
        let generation = match self.stage_first_generation().await {
            Ok(generation) => generation,
            Err(reason) => {
                lease
                    .queue(
                        self.out_tx,
                        FrameKind::ResourceSpawned {
                            request_id: self.request_id,
                            result: phux_protocol::wire::frame::SpawnResult::Err(
                                SpawnError::SpawnFailed(reason.to_owned()),
                            ),
                        },
                    )
                    .await;
                return false;
            }
        };
        if !self.queue_spawned_ok(&lease).await {
            return false;
        }
        crate::runtime::session_attach::publication_phase("spawn_bootstrap_queue", "started");
        for frame in generation.frames {
            if !lease.queue(self.out_tx, frame).await {
                crate::runtime::session_attach::publication_phase(
                    "spawn_bootstrap_queue",
                    "outbound_closed",
                );
                return false;
            }
        }
        crate::runtime::session_attach::publication_phase("spawn_bootstrap_queue", "queued");
        let mut start = OutputPumpStart {
            superseded: false,
            published_cut: generation.cut,
            replay: Vec::new(),
            live: None,
            interest: None,
        };
        if let Some(capture) = generation.capture {
            let Ok(publication) = activate_native_publication(&self.terminal, capture).await else {
                return false;
            };
            start.adopt_native_publication(publication);
        }
        reservation.published(|| {
            lease
                .last_seq
                .store(start.published_cut, std::sync::atomic::Ordering::Release);
            gate_tx.send(start).is_ok()
        })
    }
}

/// Exact authority survives future cancellation without reaping a successor.
struct PendingSpawn<'a, 'b> {
    publication: &'a SpawnPublication<'b>,
    lease: &'a crate::runtime::apps::StreamLease,
    published: bool,
}

impl Drop for PendingSpawn<'_, '_> {
    fn drop(&mut self) {
        if !self.published {
            self.publication.reap(self.lease);
        }
    }
}

/// How often a spawn's first capture is asked for before the spawn is
/// refused.
const CAPTURE_ASKS: usize = 3;

/// `ask` for a capture, again while the actor answers that it invalidated
/// the cut mid-capture (`Resize`): a reflow, or the PTY's EOF landing the
/// loan. That is no refusal, and a pane has no pump yet to wait for the
/// resync, so a command that exits at once would be reaped as a failed
/// spawn, and an attach caught by its own resize refused. The actor outlives
/// EOF and captures the final screen when asked.
pub(in crate::runtime) async fn past_invalidation<T, F>(
    mut ask: impl FnMut() -> F,
) -> Result<T, NativeRequestFailure>
where
    F: Future<Output = Result<T, NativeRequestFailure>>,
{
    use phux_terminal_api::native_state::NativeStateError::Resize;
    let mut captured = ask().await;
    for _ in 1..CAPTURE_ASKS {
        if !matches!(captured, Err(NativeRequestFailure::Refused(Resize))) {
            break;
        }
        captured = ask().await;
    }
    captured
}

/// A spawn's first generation, staged before anything is published.
struct FirstGeneration {
    pub(super) frames: Vec<FrameKind>,
    pub(super) cut: u64,
    /// The native publication to activate once the frames are queued.
    pub(super) capture: Option<phux_terminal_api::actor::NativeCapture>,
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use phux_terminal_api::native_state::NativeStateError;

    use super::{CAPTURE_ASKS, NativeRequestFailure, past_invalidation};

    /// Asks answered with `refusals` first, then `Ok`; how many were made.
    async fn asks(refusals: usize, refusal: fn() -> NativeRequestFailure) -> (bool, usize) {
        let made = Cell::new(0);
        let captured = past_invalidation(|| {
            made.set(made.get() + 1);
            let answer = if made.get() > refusals {
                Ok(())
            } else {
                Err(refusal())
            };
            std::future::ready(answer)
        })
        .await;
        (captured.is_ok(), made.get())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn an_invalidated_first_capture_is_asked_for_again_not_failed() {
        let invalidated = || NativeRequestFailure::Refused(NativeStateError::Resize);
        assert_eq!(asks(0, invalidated).await, (true, 1));
        // A command that exited mid-capture: the second ask has its screen.
        assert_eq!(asks(1, invalidated).await, (true, 2));
        // Never forever.
        assert_eq!(asks(usize::MAX, invalidated).await, (false, CAPTURE_ASKS));
        // A real refusal, or a pane that is gone, is not asked about twice.
        let refused = || NativeRequestFailure::Refused(NativeStateError::LimitExceeded);
        assert_eq!(asks(1, refused).await, (false, 1));
        assert_eq!(asks(1, || NativeRequestFailure::Unsent).await, (false, 1));
    }
}
