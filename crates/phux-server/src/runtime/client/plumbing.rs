//! A connection's ending, its terminal-stream bindings, and the outbound plumbing it closes through.

use super::{
    Arc, AtomicU8, CancellationToken, ClientId, Compression, ConnectionEnding,
    DEFAULT_CLIENT_MAILBOX, DetachReason, ErrorCode, FrameKind, FrameWriter, Goodbye, HashMap,
    JoinSet, Ordering, Outbound, QuicWriter, RevocationWatch, SharedState, StreamId,
    WRITER_DRAIN_TIMEOUT, WireResourceId, abort_output_pumps, close_client_writer,
    detach_and_release_consumer_state, mpsc, writer_task,
};

/// Why one connection must end, and what the peer is owed on the way out
/// (SPEC §9: `ERROR`, then `DETACHED`, then close).
pub(super) struct ConnectionClose {
    /// `Some(reason)` when the connection may be attached, so its pumps and
    /// consumer state are torn down first; `None` in the handshake phase.
    pub(super) attached_reason: Option<&'static str>,
    /// The `DETACHED` reason: `PROTOCOL_ERROR` for a violation,
    /// `AUTHENTICATION_FAILED` when HELLO's authentication outcome refused
    /// the peer (workload-auth §7).
    pub(super) detach_reason: DetachReason,
    pub(super) code: ErrorCode,
    pub(super) message: String,
}

/// One bound Terminal stream's outbound half (proto.md §4.2): a mailbox
/// drained by the same writer task every transport uses. Retirement closes
/// admission, then drains with a bound.
pub(super) struct StreamBinding {
    pub(super) tx: mpsc::Sender<Outbound>,
    pub(super) stream_id: StreamId,
    pub(super) writer: JoinSet<()>,
    pub(super) writer_close: tokio::sync::watch::Sender<bool>,
    pub(super) ingress_active: Arc<std::sync::atomic::AtomicBool>,
    pub(super) ingress_cancel: CancellationToken,
    pub(super) diagnostics: Option<crate::stream_diagnostics::StreamTracker>,
}

impl StreamBinding {
    pub(super) fn begin_retirement(&self) {
        if let Some(tracker) = &self.diagnostics {
            tracker.set_active(false);
        }
        self.ingress_active.store(false, Ordering::Release);
        self.ingress_cancel.cancel();
        let _ = self.writer_close.send(true);
    }

    pub(super) async fn retire(&mut self) {
        self.begin_retirement();
        if tokio::time::timeout(WRITER_DRAIN_TIMEOUT, async {
            while self.writer.join_next().await.is_some() {}
        })
        .await
        .is_err()
        {
            self.writer.shutdown().await;
        }
    }
}

impl Drop for StreamBinding {
    fn drop(&mut self) {
        self.ingress_active.store(false, Ordering::Release);
        self.ingress_cancel.cancel();
    }
}

/// One connection's outbound plumbing. The output pumps are separate from
/// the writer so DETACH can abort them while the writer still emits
/// DETACHED and serves a later ATTACH.
pub(super) struct ClientPlumbing {
    pub(super) out_tx: mpsc::Sender<Outbound>,
    pub(super) writer_close: tokio::sync::watch::Sender<bool>,
    /// Sibling tasks (today: just the writer).
    pub(super) sibling_tasks: JoinSet<()>,
    /// Per-attach raw-output pumps.
    pub(super) output_pumps: JoinSet<()>,
    /// Bound Terminal streams (QUIC multi-stream only): terminal → binding.
    /// Empty on every other transport, where all frames share `out_tx`.
    pub(super) stream_bindings: HashMap<WireResourceId, StreamBinding>,
    pub(super) retired_streams: JoinSet<()>,
    /// The frame compression the writer applies, published by HELLO after
    /// the writer is already running.
    pub(super) compression: Arc<AtomicU8>,
    /// The connection's revocation signal, handed to every writer it spawns.
    pub(super) revocation: tokio::sync::watch::Receiver<Option<Goodbye>>,
}

impl ClientPlumbing {
    /// Allocate the per-client outbound mailbox and spawn its writer task.
    pub(super) fn spawn<W: FrameWriter + 'static>(
        writer: W,
        client_id: ClientId,
        revocation: tokio::sync::watch::Receiver<Option<Goodbye>>,
    ) -> Self {
        let (out_tx, out_rx) = tokio::sync::mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
        let (writer_close, writer_close_rx) = tokio::sync::watch::channel(false);
        let mut sibling_tasks: JoinSet<()> = JoinSet::new();
        let compression = Arc::new(AtomicU8::new(Compression::None.as_u8()));
        sibling_tasks.spawn_local(writer_task(
            writer,
            out_rx,
            writer_close_rx,
            Arc::clone(&compression),
            client_id,
            RevocationWatch::control(revocation.clone()),
        ));
        Self {
            out_tx,
            writer_close,
            sibling_tasks,
            output_pumps: JoinSet::new(),
            stream_bindings: HashMap::new(),
            retired_streams: JoinSet::new(),
            compression,
            revocation,
        }
    }

    /// Publish the compression HELLO selected to the writer task. Called
    /// after `HELLO_OK` is queued, so that frame is never compressed.
    pub(super) fn set_compression(&self, compression: Compression) {
        self.compression
            .store(compression.as_u8(), Ordering::Relaxed);
    }

    /// The mailbox for Terminal-addressed outbound: the bound stream's sender
    /// when this connection bound the Terminal, else the control mailbox.
    pub(super) fn sender_for(&self, terminal_id: &WireResourceId) -> mpsc::Sender<Outbound> {
        self.stream_bindings
            .get(terminal_id)
            .map_or_else(|| self.out_tx.clone(), |binding| binding.tx.clone())
    }

    /// Bind a Terminal stream: create its mailbox, spawn its writer task
    /// draining into the stream's send half, and register both. Replaces any
    /// live binding for the Terminal, reaping its superseded writer.
    pub(super) async fn bind_stream(
        &mut self,
        client_id: ClientId,
        terminal_id: WireResourceId,
        stream_id: StreamId,
        writer: QuicWriter,
    ) -> (
        mpsc::Sender<Outbound>,
        Arc<std::sync::atomic::AtomicBool>,
        CancellationToken,
    ) {
        let (tx, rx) = tokio::sync::mpsc::channel::<Outbound>(DEFAULT_CLIENT_MAILBOX);
        let (writer_close, writer_close_rx) = tokio::sync::watch::channel(false);
        let mut writer_tasks: JoinSet<()> = JoinSet::new();
        let diagnostics = Some(writer.diagnostic_tracker());
        writer_tasks.spawn_local(writer_task(
            writer,
            rx,
            writer_close_rx,
            Arc::clone(&self.compression),
            client_id,
            RevocationWatch::stream(self.revocation.clone()),
        ));
        let sender = tx.clone();
        let ingress_active = Arc::new(std::sync::atomic::AtomicBool::new(true));
        let ingress_cancel = CancellationToken::new();
        if let Some(replaced) = self.stream_bindings.insert(
            terminal_id,
            StreamBinding {
                tx,
                stream_id,
                writer: writer_tasks,
                writer_close,
                ingress_active: Arc::clone(&ingress_active),
                ingress_cancel: ingress_cancel.clone(),
                diagnostics,
            },
        ) {
            self.retire_stream(replaced).await;
        }
        (sender, ingress_active, ingress_cancel)
    }

    /// Whether `stream_id` is the Terminal's live binding, not a stale one.
    pub(super) fn is_current_stream(
        &self,
        terminal_id: &WireResourceId,
        stream_id: StreamId,
    ) -> bool {
        self.stream_bindings
            .get(terminal_id)
            .is_some_and(|binding| binding.stream_id == stream_id)
    }

    /// Drop one stream binding; its retirement runs without blocking others.
    pub(super) async fn drop_stream_binding(&mut self, terminal_id: &WireResourceId) {
        if let Some(binding) = self.stream_bindings.remove(terminal_id) {
            self.retire_stream(binding).await;
        }
    }

    pub(super) async fn retire_stream(&mut self, mut binding: StreamBinding) {
        // Bound the draining writers churn can retain.
        binding.begin_retirement();
        while self.retired_streams.try_join_next().is_some() {}
        if self.retired_streams.len() >= 127 {
            self.retired_streams.shutdown().await;
        }
        self.retired_streams
            .spawn_local(async move { binding.retire().await });
    }

    /// Drop every stream binding (session DETACH, disconnect). Pumps are
    /// aborted by the caller first; draining writers remain connection-owned.
    pub(super) async fn drop_all_stream_bindings(&mut self) {
        for (_, binding) in std::mem::take(&mut self.stream_bindings) {
            self.retire_stream(binding).await;
        }
    }

    /// Close the connection as its frame loop `ending` requires.
    pub(super) async fn end(
        self,
        ending: ConnectionEnding,
        state: &SharedState,
        client_id: ClientId,
        root_token: &CancellationToken,
    ) {
        match ending {
            ConnectionEnding::TransportGone => {}
            ConnectionEnding::Violation(close) => self.close(close, state, client_id).await,
            ConnectionEnding::Cancelled => {
                self.close_for_cancellation(state, client_id, root_token)
                    .await;
            }
        }
    }

    /// End one connection for a protocol violation, in the order §9
    /// requires: tear down what may be attached, then `ERROR`, `DETACHED`,
    /// and close.
    pub(super) async fn close(
        mut self,
        close: ConnectionClose,
        state: &SharedState,
        client_id: ClientId,
    ) {
        self.drop_all_stream_bindings().await;
        while self.retired_streams.join_next().await.is_some() {}
        if let Some(reason) = close.attached_reason {
            abort_output_pumps(&mut self.output_pumps, client_id, reason).await;
            detach_and_release_consumer_state(state, client_id);
        }
        let ConnectionClose {
            detach_reason,
            code,
            message,
            ..
        } = close;
        let goodbye = [
            FrameKind::Error {
                request_id: None,
                code,
                message: message.clone(),
            },
            FrameKind::Detached {
                reason: Some(detach_reason),
                message,
            },
        ];
        for frame in goodbye {
            let _ = self.out_tx.send(Outbound::Frame(frame)).await;
        }
        close_client_writer(self.out_tx, &self.writer_close, &mut self.sibling_tasks).await;
    }

    /// End one connection because it was cancelled rather than because the
    /// peer misbehaved: no `ERROR` is owed, and a server-wide shutdown says so
    /// in the `DETACHED` reason before the writer drains.
    pub(super) async fn close_for_cancellation(
        mut self,
        state: &SharedState,
        client_id: ClientId,
        root_token: &CancellationToken,
    ) {
        abort_output_pumps(&mut self.output_pumps, client_id, "connection cancellation").await;
        self.drop_all_stream_bindings().await;
        while self.retired_streams.join_next().await.is_some() {}
        if root_token.is_cancelled() {
            let _ = self
                .out_tx
                .send(Outbound::Frame(FrameKind::Detached {
                    reason: Some(DetachReason::ServerShutdown),
                    message: "server is shutting down".to_owned(),
                }))
                .await;
        }
        detach_and_release_consumer_state(state, client_id);
        close_client_writer(self.out_tx, &self.writer_close, &mut self.sibling_tasks).await;
    }
}
