/// Encode one outbound message onto the end of `batch`, recording its frame
/// boundary in `ends`. Returns the message of an [`Outbound::TerminalError`],
/// after which the caller must stop draining.
fn encode_into_batch(
    message: Outbound,
    compression: Compression,
    scratch: &mut BytesMut,
    batch: &mut BytesMut,
    ends: &mut Vec<usize>,
) -> Option<String> {
    let (frame, terminal_message) = match message {
        Outbound::Frame(frame) => (frame, None),
        Outbound::TerminalError {
            request_id,
            code,
            message,
        } => (
            FrameKind::Error {
                request_id,
                code,
                message: message.clone(),
            },
            Some(message),
        ),
    };
    let encode_started = std::time::Instant::now();
    frame.encode_compressed(compress_policy(&frame, compression), scratch, batch);
    crate::perf::WIRE_ENCODE.record_elapsed(encode_started);
    ends.push(batch.len());
    terminal_message
}

/// Which frames are worth wrapping (proto.md §6.4): bootstrap and history
/// payloads only. Those are large and gate first paint; live
/// `RESOURCE_OUTPUT` measured slower when compressed.
const fn compress_policy(frame: &FrameKind, negotiated: Compression) -> Compression {
    match frame {
        FrameKind::BootstrapChunk { .. } | FrameKind::HistoryPage { .. } => negotiated,
        _ => Compression::None,
    }
}

/// Absorb up to [`MAX_WRITE_COALESCE`] further messages that are *already*
/// queued into the same batch. Stops on an empty or closed mailbox, and on a
/// terminal-error sentinel (returned so the caller can finish the shutdown
/// handshake).
fn drain_ready_into_batch(
    rx: &mut tokio::sync::mpsc::Receiver<Outbound>,
    generation_fence: &mut OutboundGenerationFence,
    compression: Compression,
    scratch: &mut BytesMut,
    batch: &mut BytesMut,
    ends: &mut Vec<usize>,
) -> Option<String> {
    for _ in 1..MAX_WRITE_COALESCE {
        let Ok(next) = rx.try_recv() else {
            return None;
        };
        if !generation_fence.admits(&next) {
            continue;
        }
        if let Some(message) = encode_into_batch(next, compression, scratch, batch, ends) {
            return Some(message);
        }
    }
    None
}

/// How a writer learns that its connection's authority was withdrawn.
#[derive(Clone, Default)]
pub(crate) struct RevocationWatch {
    rx: Option<tokio::sync::watch::Receiver<Option<Goodbye>>>,
    /// Whether this writer carries the connection's control stream, the one
    /// the goodbye frames ride. A bound Terminal stream only closes.
    control: bool,
}

impl RevocationWatch {
    /// The watch for the connection's control-stream writer.
    const fn control(rx: tokio::sync::watch::Receiver<Option<Goodbye>>) -> Self {
        Self {
            rx: Some(rx),
            control: true,
        }
    }

    /// The watch for a bound Terminal stream's writer.
    const fn stream(rx: tokio::sync::watch::Receiver<Option<Goodbye>>) -> Self {
        Self {
            rx: Some(rx),
            control: false,
        }
    }

    /// The goodbye owed, once the connection is revoked.
    fn current(&self) -> Option<Goodbye> {
        self.rx.as_ref().and_then(|rx| *rx.borrow())
    }

    /// Resolve once the connection is revoked; never, without a signal.
    async fn revoked(&mut self) -> Goodbye {
        let Some(rx) = self.rx.as_mut() else {
            return std::future::pending().await;
        };
        loop {
            let current = *rx.borrow_and_update();
            if let Some(goodbye) = current {
                return goodbye;
            }
            if rx.changed().await.is_err() {
                return std::future::pending().await;
            }
        }
    }

    /// Run one transport operation unless the connection is revoked while
    /// it is pending. `None` means the operation was abandoned mid-way.
    async fn unless_revoked<T>(
        &mut self,
        operation: impl std::future::Future<Output = io::Result<T>>,
    ) -> Option<io::Result<T>> {
        tokio::select! {
            biased;
            result = operation => Some(result),
            _ = self.revoked() => None,
        }
    }
}

/// Why a writer stops taking messages.
enum WriterStop {
    /// The mailbox is done: close normally.
    Closed,
    /// The connection's authority was withdrawn.
    Revoked(Goodbye),
}

/// The next message the generation fence admits, unless the mailbox closes
/// or the connection is revoked first. Revocation wins a tie.
async fn next_admitted(
    rx: &mut tokio::sync::mpsc::Receiver<Outbound>,
    close: &mut tokio::sync::watch::Receiver<bool>,
    close_control_open: &mut bool,
    generation_fence: &mut OutboundGenerationFence,
    revocation: &mut RevocationWatch,
) -> Result<Outbound, WriterStop> {
    loop {
        let next = tokio::select! {
            biased;
            goodbye = revocation.revoked() => return Err(WriterStop::Revoked(goodbye)),
            next = next_outbound(rx, close, close_control_open) => next,
        };
        let message = next.ok_or(WriterStop::Closed)?;
        if generation_fence.admits(&message) {
            return Ok(message);
        }
    }
}

/// The frames a revoked connection is owed (`workload-auth.md` §7 step 3).
fn goodbye_frames(revocation: Revocation) -> [FrameKind; 2] {
    [
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::PermissionDenied,
            message: revocation.message().to_owned(),
        },
        FrameKind::Detached {
            reason: Some(revocation.detach_reason()),
            message: revocation.message().to_owned(),
        },
    ]
}

/// Say what a revoked connection is owed, then close (`workload-auth.md` §7
/// steps 3 and 4). Nothing queued behind the revocation is written: the
/// mailbox drops with this task. Each step is bounded by
/// [`WRITER_DRAIN_TIMEOUT`], so a peer that stopped reading cannot hold the
/// connection open.
async fn say_goodbye<W: FrameWriter>(
    writer: &mut W,
    buf: &mut BytesMut,
    goodbye: Goodbye,
    control: bool,
    client_id: ClientId,
) {
    if let (true, Goodbye::Announce(revocation)) = (control, goodbye) {
        buf.clear();
        let mut ends = Vec::with_capacity(2);
        for frame in goodbye_frames(revocation) {
            frame.encode(buf);
            ends.push(buf.len());
        }
        let farewell = async {
            writer.write_frames(buf, &ends).await?;
            writer.flush().await
        };
        if !matches!(
            tokio::time::timeout(WRITER_DRAIN_TIMEOUT, farewell).await,
            Ok(Ok(()))
        ) {
            debug!(
                ?client_id,
                "goodbye to a revoked connection was not delivered"
            );
        }
    }
    let _ = tokio::time::timeout(WRITER_DRAIN_TIMEOUT, writer.close()).await;
    debug!(?client_id, ?goodbye, "writer closed: authority withdrawn");
}

/// Settle one transport write or flush. `false` when the writer must stop:
/// the step failed, or a revocation interrupted it (`outcome` is `None`);
/// either way the transport is closed.
async fn step_succeeded<W: FrameWriter>(
    writer: &mut W,
    outcome: Option<io::Result<()>>,
    client_id: ClientId,
    step: &'static str,
) -> bool {
    match outcome {
        Some(Ok(())) => true,
        Some(Err(err)) => {
            debug!(?client_id, error = %err, step, "writer step failed; client task ending");
            let _ = writer.close().await;
            false
        }
        None => {
            abandon_after_revocation(writer, client_id).await;
            false
        }
    }
}

/// A write the revocation interrupted may have stopped mid-frame, so no
/// goodbye can follow it: close within the bound.
async fn abandon_after_revocation<W: FrameWriter>(writer: &mut W, client_id: ClientId) {
    debug!(?client_id, "revoked mid-write; closing without a goodbye");
    let _ = tokio::time::timeout(WRITER_DRAIN_TIMEOUT, writer.close()).await;
}

/// Take the next mailbox item, honouring the close control.
///
/// Clears `close_control_open` once the control has fired or gone away, so
/// later turns wait on the mailbox alone. `None` means the mailbox is done.
async fn next_outbound(
    rx: &mut tokio::sync::mpsc::Receiver<Outbound>,
    close: &mut tokio::sync::watch::Receiver<bool>,
    close_control_open: &mut bool,
) -> Option<Outbound> {
    loop {
        if !*close_control_open {
            return rx.recv().await;
        }
        tokio::select! {
            biased;
            changed = close.changed() => {
                if changed.is_ok() && *close.borrow_and_update() {
                    rx.close();
                }
                *close_control_open = false;
            }
            message = rx.recv() => return message,
        }
    }
}

/// Write the `DETACHED` that follows an ordered terminal `ERROR`, then flush
/// (a buffering transport could otherwise lose both) and close.
async fn finish_with_terminal_error<W: FrameWriter>(
    writer: &mut W,
    buf: &mut BytesMut,
    message: String,
    client_id: ClientId,
) {
    buf.clear();
    FrameKind::Detached {
        reason: Some(DetachReason::ProtocolError),
        message,
    }
    .encode(buf);
    if let Err(err) = writer.write_frame(buf).await {
        debug!(?client_id, error = %err, "writer error on terminal DETACHED");
    }
    let _ = writer.flush().await;
    let _ = writer.close().await;
}

/// Writer task: drain the outbound mailbox into the transport, batching what
/// is already queued, until the mailbox closes. Once `revocation` fires,
/// nothing still queued is written; the goodbye is, and the transport closes
/// (`docs/spec/workload-auth.md` §7).
async fn writer_task<W: FrameWriter>(
    mut writer: W,
    mut rx: tokio::sync::mpsc::Receiver<Outbound>,
    mut close: tokio::sync::watch::Receiver<bool>,
    compression: Arc<AtomicU8>,
    client_id: ClientId,
    mut revocation: RevocationWatch,
) {
    let mut buf = BytesMut::with_capacity(1024);
    // Uncompressed image of a frame being wrapped.
    let mut scratch = BytesMut::new();
    // End offset of each frame in `buf`, for message-oriented transports.
    let mut ends: Vec<usize> = Vec::new();
    let mut close_control_open = true;
    let mut generation_fence = OutboundGenerationFence {
        diagnostics: writer.stream_tracker(),
        ..OutboundGenerationFence::default()
    };
    'writer: loop {
        let message = match next_admitted(
            &mut rx,
            &mut close,
            &mut close_control_open,
            &mut generation_fence,
            &mut revocation,
        )
        .await
        {
            Ok(message) => message,
            Err(WriterStop::Closed) => break 'writer,
            Err(WriterStop::Revoked(goodbye)) => {
                say_goodbye(
                    &mut writer,
                    &mut buf,
                    goodbye,
                    revocation.control,
                    client_id,
                )
                .await;
                return;
            }
        };
        // One write per batch of already-queued messages; the drain never
        // waits, so a lone keystroke echo adds no latency.
        buf.clear();
        ends.clear();
        // An uncompressed batch is always legal, so `Relaxed` suffices.
        let compression = Compression::from_u8(compression.load(Ordering::Relaxed));
        let mut terminal_message =
            encode_into_batch(message, compression, &mut scratch, &mut buf, &mut ends);
        if terminal_message.is_none() {
            terminal_message = drain_ready_into_batch(
                &mut rx,
                &mut generation_fence,
                compression,
                &mut scratch,
                &mut buf,
                &mut ends,
            );
        }
        crate::perf::WIRE_BATCH_FRAMES.record_len(ends.len());
        // Revoked while gathering: none of the batch is owed.
        if let Some(goodbye) = revocation.current() {
            say_goodbye(
                &mut writer,
                &mut buf,
                goodbye,
                revocation.control,
                client_id,
            )
            .await;
            return;
        }
        let write_started = std::time::Instant::now();
        let batch_len = buf.len();
        let written = revocation
            .unless_revoked(writer.write_frames(&mut buf, &ends))
            .await;
        if !step_succeeded(&mut writer, written, client_id, "write").await {
            return;
        }
        if let Some(message) = terminal_message {
            finish_with_terminal_error(&mut writer, &mut buf, message, client_id).await;
            return;
        }
        // `write_frames` may only buffer (WebSocket does); flush once per batch.
        let flushed = revocation.unless_revoked(writer.flush()).await;
        if !step_succeeded(&mut writer, flushed, client_id, "flush").await {
            return;
        }
        phux_perf::echo_hop::record("frame_send");
        crate::perf::WIRE_WRITE.record_elapsed(write_started);
        crate::perf::WIRE_WRITE_BYTES.record_len(batch_len);
        crate::perf::WIRE_BYTES_OUT.add_len(batch_len);
    }
    if let Err(err) = writer.close().await {
        debug!(?client_id, error = %err, "writer close failed");
    }
    debug!(?client_id, "writer task exiting (channel closed)");
}

#[cfg(test)]
mod compress_policy_tests {
    use phux_protocol::caps::Compression;
    use phux_protocol::ids::{ResourceId, SessionId, WindowId};
    use phux_protocol::wire::frame::{
        CommandResult, ErrorCode, FrameKind, MoveError, MoveResult, SpawnError, SpawnResult,
    };

    use super::compress_policy;

    /// A frame that settles a request is never compressed, whatever the
    /// connection negotiated: a client that cannot inflate it (or fails to)
    /// would otherwise leave the request waiting. Only bootstrap and history
    /// payloads are.
    #[test]
    fn frames_that_settle_a_request_are_never_compressed() {
        let settling = [
            FrameKind::CommandResult {
                request_id: 1,
                result: CommandResult::Ok,
            },
            FrameKind::ResourceSpawned {
                request_id: 1,
                result: SpawnResult::Err(SpawnError::GroupNotFound),
            },
            FrameKind::ResourceMoved {
                request_id: 1,
                result: MoveResult::Err(MoveError::UnsupportedSatelliteRoute),
            },
            FrameKind::MetadataValue {
                request_id: 1,
                value: Some(vec![0; 64]),
            },
            FrameKind::MetadataKeys {
                request_id: 1,
                keys: vec!["k".to_owned()],
            },
            FrameKind::DirectoryListing {
                request_id: 1,
                result: Err(crate::hub::relay::listing_refusal("/", "x")),
            },
            FrameKind::PathResults {
                request_id: 1,
                result: Err(crate::hub::relay::path_refusal("/", "x")),
            },
            FrameKind::Error {
                request_id: Some(1),
                code: ErrorCode::Unsupported,
                message: "x".to_owned(),
            },
            FrameKind::Attached {
                attach_id: 1,
                snapshot: phux_protocol::wire::info::SessionSnapshot::new(
                    SessionId::new(1),
                    WindowId::new(1),
                    ResourceId::local(1),
                ),
                initial_client_id: phux_protocol::ids::ClientId::new(1),
            },
            FrameKind::AttachReady { attach_id: 1 },
        ];
        for frame in &settling {
            assert_eq!(
                compress_policy(frame, Compression::Deflate),
                Compression::None,
                "{frame:?}"
            );
        }
    }
}
