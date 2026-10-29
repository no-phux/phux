//! Inbound framing: stream and WebSocket reads, frame extraction, and read errors.

use futures_util::FutureExt;
use phux_protocol::wire::framing;
use tokio::io::AsyncReadExt;

use super::{
    AsyncRead, Bytes, BytesMut, DialError, LinkError, MAX_INBOUND_BATCH, QuicLink, WsActivity,
    WsReader, WsWriter, recv_activity_alive,
};

pub(super) async fn read_stream_frames(
    reader: &mut Box<dyn AsyncRead + Unpin + Send + Sync>,
    pending: &mut BytesMut,
    quic: Option<&QuicLink>,
) -> Result<Option<Vec<Bytes>>, LinkError> {
    loop {
        let frames = take_complete_frames(pending)?;
        if !frames.is_empty() {
            return Ok(Some(frames));
        }
        let read = reader
            .read_buf(pending)
            .await
            .map_err(|error| read_error(error, quic))?;
        if read != 0 {
            continue;
        }
        if let Some(reason) = quic.and_then(|link| link.connection.close_reason()) {
            return Err(LinkError::Closed(phux_dial::quic::close_error(&reason)));
        }
        return Ok(None);
    }
}

/// Complete frames already buffered, else whatever one read polled once
/// (against a no-op waker) adds. Sound because these reads are cancel-safe
/// and the next `read_frames().await` re-polls with a real waker.
pub(super) fn try_read_stream_frames(
    reader: &mut Box<dyn AsyncRead + Unpin + Send + Sync>,
    pending: &mut BytesMut,
    quic: Option<&QuicLink>,
) -> Result<Vec<Bytes>, LinkError> {
    let frames = take_complete_frames(pending)?;
    if !frames.is_empty() {
        return Ok(frames);
    }
    match reader.read_buf(pending).now_or_never() {
        None | Some(Ok(0)) => Ok(Vec::new()),
        Some(Ok(_)) => take_complete_frames(pending),
        Some(Err(error)) => Err(read_error(error, quic)),
    }
}

fn read_error(error: std::io::Error, quic: Option<&QuicLink>) -> LinkError {
    refused_or(LinkError::Read(error), quic)
}

/// A failed read or write. A QUIC stream sees a connection close as a bare
/// "connection lost" in either direction; when that close was the server
/// refusing the bearer token, report the refusal instead, which ends the
/// session (the driver does not redial it) and words it as "re-pair".
pub(super) fn refused_or(error: LinkError, quic: Option<&QuicLink>) -> LinkError {
    if !matches!(error, LinkError::Read(_) | LinkError::Write(_)) {
        return error;
    }
    match quic
        .and_then(|link| link.connection.close_reason())
        .map(|reason| phux_dial::quic::close_error(&reason))
    {
        Some(refused @ DialError::AuthRefused(_)) => LinkError::Closed(refused),
        _ => error,
    }
}

/// Split a read's complete frames off `pending` without copying them: each
/// frame is a frozen view of the read buffer. A queued frame therefore keeps
/// its read slab alive until the consumer decodes it, bounded by the inbound
/// queue's ceilings (a slab is `pending`'s 64 KiB, or one oversized frame).
pub(super) fn take_complete_frames(pending: &mut BytesMut) -> Result<Vec<Bytes>, LinkError> {
    let mut frames = Vec::new();
    while frames.len() < MAX_INBOUND_BATCH {
        match framing::split_frame(pending) {
            Ok(Some(frame)) => frames.push(frame.freeze()),
            Err(error) if frames.is_empty() => return Err(LinkError::Framing(error)),
            // Preserve already-decoded frames. On error, the malformed bytes
            // remain at the front of `pending` and fail the next pump step.
            Ok(None) | Err(_) => break,
        }
    }
    Ok(frames)
}

pub(super) async fn read_ws_frames(
    reader: &mut WsReader,
    writer: &mut WsWriter,
    deferred_error: &mut Option<LinkError>,
) -> Result<Option<Vec<Bytes>>, LinkError> {
    match recv_activity_alive(reader, writer)
        .await
        .map_err(LinkError::Ws)?
    {
        WsActivity::Message(frame) => {
            let mut frames = vec![ws_frame(frame)?];
            while frames.len() < MAX_INBOUND_BATCH {
                match try_read_ws_frame(reader) {
                    Ok(mut more) if !more.is_empty() => frames.append(&mut more),
                    Ok(_) => break,
                    Err(error) => {
                        *deferred_error = Some(error);
                        break;
                    }
                }
            }
            Ok(Some(frames))
        }
        // A pong or a peer ping: no phux payload, but proof of life.
        WsActivity::Control => Ok(Some(vec![Bytes::new()])),
        WsActivity::Closed => Ok(None),
    }
}

/// A message tungstenite already decoded, if any. A close seen here is
/// none; the next read reports it.
pub(super) fn try_read_ws_frame(reader: &mut WsReader) -> Result<Vec<Bytes>, LinkError> {
    match reader.try_recv_message().map_err(LinkError::Ws)? {
        Some(frame) => Ok(vec![ws_frame(frame)?]),
        None => Ok(Vec::new()),
    }
}

/// One binary message as one frame: the check the server applies to its
/// own inbound messages, since a message that disagrees with its declared
/// length would desynchronize the framed stream.
fn ws_frame(message: Vec<u8>) -> Result<Bytes, LinkError> {
    framing::check_frame(&message).map_err(LinkError::Framing)?;
    Ok(Bytes::from(message))
}
