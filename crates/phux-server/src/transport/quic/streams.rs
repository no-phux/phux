//! Terminal streams on a QUIC connection: accept and bind them, then pump their frames under provenance and budget checks.

use super::{
    ADMISSION_DEADLINE, AdmittedFrame, Arc, AtomicBool, FRAME_BODY_DEADLINE, LENGTH_PREFIX,
    MAX_PENDING_STREAM_BINDS, QuicStreamEvent, QuicStreamFailure, SendWindow, debug, framing, io,
    read_length_prefixed, refuse_terminal_stream,
};

/// Accept client-opened Terminal streams for one upgraded connection. Each
/// must open with a well-formed `STREAM_BIND` within [`ADMISSION_DEADLINE`]
/// or is reset unread; well-formed binds go to the client task as `Bound`.
pub(super) async fn accept_terminal_streams(
    conn: quinn::Connection,
    window: SendWindow,
    frames_tx: tokio::sync::mpsc::Sender<AdmittedFrame>,
    events_tx: tokio::sync::mpsc::Sender<QuicStreamEvent>,
    frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    terminal_frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
) {
    let pending = std::sync::Arc::new(tokio::sync::Semaphore::new(MAX_PENDING_STREAM_BINDS));
    let mut bind_tasks = tokio::task::JoinSet::new();
    loop {
        let accepted = tokio::select! {
            accepted = conn.accept_bi() => accepted,
            Some(_) = bind_tasks.join_next(), if !bind_tasks.is_empty() => continue,
        };
        let Ok((send, mut recv)) = accepted else {
            return;
        };
        let Ok(permit) = pending.clone().try_acquire_owned() else {
            debug!("quic terminal stream refused: pending bind capacity exhausted");
            refuse_terminal_stream(send, recv);
            continue;
        };
        let window = window.clone();
        let frames = frames_tx.clone();
        let events = events_tx.clone();
        let frame_bytes = frame_bytes.clone();
        let terminal_frame_bytes = terminal_frame_bytes.clone();
        bind_tasks.spawn(async move {
            let _permit = permit;
            let bind = tokio::time::timeout(ADMISSION_DEADLINE, read_stream_bind(&mut recv)).await;
            let Some(bind) = bind.ok().flatten() else {
                debug!("quic terminal stream refused: no well-formed STREAM_BIND within deadline");
                refuse_terminal_stream(send, recv);
                return;
            };
            let event = QuicStreamEvent::Bound {
                terminal_id: bind.terminal_id,
                stream_id: bind.stream_id,
                send,
                recv,
                window,
                frames,
                frame_bytes,
                terminal_frame_bytes,
            };
            let _ = events.send(event).await;
        });
    }
}

/// Pump one bound Terminal stream's frames into the merged channel until the
/// stream ends, then emit `Ended` (or `Failed`).
#[allow(
    clippy::too_many_arguments,
    clippy::significant_drop_tightening,
    reason = "the admitted stream owns distinct transport, provenance, cancellation, and two-level budget handles; frame permits intentionally live through channel admission"
)]
pub(crate) async fn pump_terminal_stream(
    mut recv: quinn::RecvStream,
    terminal_id: phux_protocol::ids::ResourceId,
    stream_id: phux_protocol::ids::StreamId,
    frames_tx: tokio::sync::mpsc::Sender<AdmittedFrame>,
    frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    terminal_frame_bytes: std::sync::Arc<tokio::sync::Semaphore>,
    active: Arc<AtomicBool>,
    cancelled: tokio_util::sync::CancellationToken,
    diagnostics: Option<crate::stream_diagnostics::StreamTracker>,
) {
    loop {
        let read = tokio::select! {
            () = cancelled.cancelled() => return,
            read = read_framed_bounded(
                &mut recv,
                &frame_bytes,
                Some(&terminal_frame_bytes),
                Some(active.clone()),
                diagnostics.as_ref(),
            ) => read,
        };
        let failure = match read {
            Ok(Some(frame)) if terminal_frame_matches(&frame.bytes, &terminal_id, stream_id) => {
                let closed = tokio::select! {
                    () = cancelled.cancelled() => true,
                    result = frames_tx.send(frame) => result.is_err(),
                };
                if closed {
                    return;
                }
                continue;
            }
            Ok(Some(_)) => {
                debug!(
                    ?terminal_id,
                    ?stream_id,
                    "frame refused on mismatched Terminal stream"
                );
                Some(QuicStreamFailure::Transport(
                    "frame provenance did not match STREAM_BIND".to_owned(),
                ))
            }
            Ok(None) => None,
            Err(err) => Some(stream_failure(&err)),
        };
        let event = match failure {
            None => QuicStreamEvent::Ended {
                terminal_id,
                stream_id,
            },
            Some(failure) => QuicStreamEvent::Failed {
                terminal_id,
                stream_id,
                failure,
            },
        };
        tokio::select! {
            () = cancelled.cancelled() => {}
            _ = frames_tx.send(AdmittedFrame::event(event)) => {}
        }
        return;
    }
}

pub(super) fn stream_failure(err: &io::Error) -> QuicStreamFailure {
    if let Some(framing) = err
        .get_ref()
        .and_then(|source| source.downcast_ref::<framing::FramingError>())
    {
        return QuicStreamFailure::Framing(*framing);
    }
    if err.kind() == io::ErrorKind::TimedOut || err.kind() == io::ErrorKind::UnexpectedEof {
        return QuicStreamFailure::IncompleteFrame;
    }
    QuicStreamFailure::Transport(err.to_string())
}

/// Verify the stream's provenance before a frame can enter shared dispatch:
/// only Terminal-scoped input/ack frames for the bound Terminal (and, where
/// the frame names one, the bound generation) are allowed.
pub(super) fn terminal_frame_matches(
    framed: &[u8],
    terminal_id: &phux_protocol::ids::ResourceId,
    stream_id: phux_protocol::ids::StreamId,
) -> bool {
    use phux_protocol::wire::frame::FrameKind;
    let Ok((frame, tail)) = FrameKind::decode(framed) else {
        return false;
    };
    if !tail.is_empty() {
        return false;
    }
    match frame {
        FrameKind::InputKey {
            terminal_id: id, ..
        }
        | FrameKind::InputMouse {
            terminal_id: id, ..
        }
        | FrameKind::InputFocus {
            terminal_id: id, ..
        }
        | FrameKind::InputPaste {
            terminal_id: id, ..
        }
        | FrameKind::InputTerminalReply {
            terminal_id: id, ..
        }
        | FrameKind::ResizeTerminal {
            terminal_id: id, ..
        } => id == *terminal_id,
        FrameKind::FrameAck {
            terminal_id: id,
            stream_id: generation,
            ..
        }
        | FrameKind::HistoryRequest {
            terminal_id: id,
            stream_id: generation,
            ..
        } => id == *terminal_id && generation == stream_id,
        _ => false,
    }
}

/// Read one `STREAM_BIND` header off a fresh Terminal stream.
pub(super) async fn read_stream_bind(
    recv: &mut quinn::RecvStream,
) -> Option<phux_protocol::wire::stream_bind::StreamBind> {
    let (len_buf, body) = read_length_prefixed(
        recv,
        phux_protocol::wire::stream_bind::MAX_STREAM_BIND_BYTES,
    )
    .await?;
    let mut framed = Vec::with_capacity(LENGTH_PREFIX + body.len());
    framed.extend_from_slice(&len_buf);
    framed.extend_from_slice(&body);
    phux_protocol::wire::stream_bind::decode(&framed)
        .ok()
        .map(|(bind, _)| bind)
}

/// Fill `buf` from the stream: `Ok(true)` when filled, `Ok(false)` on a clean
/// finish before any byte (EOF at a boundary), `UnexpectedEof` on a partial
/// read, `Err` on a transport error.
pub(super) async fn read_exact_quic(
    recv: &mut quinn::RecvStream,
    buf: &mut [u8],
) -> io::Result<bool> {
    match recv.read_exact(buf).await {
        Ok(()) => Ok(true),
        Err(quinn::ReadExactError::FinishedEarly(0)) => Ok(false),
        Err(quinn::ReadExactError::FinishedEarly(_)) => Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "QUIC stream finished during a framed value",
        )),
        Err(err) => Err(io::Error::other(err)),
    }
}

pub(super) fn budget_closed(what: &str) -> io::Error {
    io::Error::new(
        io::ErrorKind::NotConnected,
        format!("{what} byte budget closed"),
    )
}

/// Read one frame while holding byte permits for it. Once the first header
/// byte arrives the rest must arrive within [`FRAME_BODY_DEADLINE`].
pub(super) async fn read_framed_bounded(
    recv: &mut quinn::RecvStream,
    budget: &std::sync::Arc<tokio::sync::Semaphore>,
    terminal_budget: Option<&std::sync::Arc<tokio::sync::Semaphore>>,
    origin: Option<Arc<AtomicBool>>,
    diagnostics: Option<&crate::stream_diagnostics::StreamTracker>,
) -> io::Result<Option<AdmittedFrame>> {
    let mut header = [0u8; LENGTH_PREFIX];
    if !read_exact_quic(recv, &mut header[..1]).await? {
        return Ok(None);
    }
    let admitted = tokio::time::timeout(FRAME_BODY_DEADLINE, async {
        if !read_exact_quic(recv, &mut header[1..]).await? {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "QUIC stream finished mid-header",
            ));
        }
        let body_len = framing::decode_length(header)?;
        let permit_count = u32::try_from(LENGTH_PREFIX + body_len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "QUIC frame size does not fit permit count",
            )
        })?;
        // Terminal sub-budget first: taking connection permits first would
        // let a second maximum Terminal frame eat the reserved control room.
        let terminal_bytes = match terminal_budget {
            Some(terminal_budget) => Some(
                terminal_budget
                    .clone()
                    .acquire_many_owned(permit_count)
                    .await
                    .map_err(|_| budget_closed("QUIC Terminal"))?,
            ),
            None => None,
        };
        let connection_bytes = budget
            .clone()
            .acquire_many_owned(permit_count)
            .await
            .map_err(|_| budget_closed("QUIC mux"))?;
        let mut framed = framing::frame_buffer(header)?;
        let ticket = diagnostics.map(|tracker| tracker.enqueue(u64::from(permit_count)));
        if !read_exact_quic(recv, &mut framed[LENGTH_PREFIX..]).await? {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "stream finished mid-frame",
            ));
        }
        Ok::<_, io::Error>(AdmittedFrame {
            bytes: framed,
            _connection_bytes: Some(connection_bytes),
            _terminal_bytes: terminal_bytes,
            origin,
            event: None,
            _queue_ticket: ticket,
        })
    })
    .await
    .map_err(|_| {
        io::Error::new(
            io::ErrorKind::TimedOut,
            "QUIC incomplete-frame deadline elapsed",
        )
    })??;
    Ok(Some(admitted))
}
