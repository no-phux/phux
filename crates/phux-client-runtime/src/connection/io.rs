//! Transport establishment and framed I/O for one connection attempt.

use bytes::BytesMut;
use futures_util::StreamExt;
use phux_dial::TlsClientIdentity;
use phux_dial::ws::{WsActivity, WsReader, WsWriter, recv_activity_alive};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::wire::framing;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{ConnectOptions, ConnectionEnd, Target, Transport};
use crate::control::encode;
use crate::dial::{
    dial_message, load_token, plan_quic_with_token, plan_ws, quic_closed_message, timed_out,
};
use crate::reconnect::is_fatal_refusal;

const MAX_INBOUND_BATCH: usize = 256;

/// One established lane, read and written as SPEC section 5 frames.
pub(super) enum Io {
    Stream {
        reader: Box<dyn AsyncRead + Unpin + Send>,
        writer: Box<dyn AsyncWrite + Unpin + Send>,
        pending: BytesMut,
        /// Kept so the QUIC endpoint's driver outlives the connection and a
        /// clean close can be issued.
        quic: Option<(quinn::Endpoint, quinn::Connection)>,
    },
    Ws {
        reader: WsReader,
        writer: WsWriter,
        deferred_error: Option<String>,
    },
}

impl Io {
    /// The next bounded transport batch, or `Ok(None)` on a clean close.
    /// Cancel-safe: partial bytes stay buffered.
    pub(super) async fn read_frames(&mut self, name: &str) -> Result<Option<Vec<Vec<u8>>>, String> {
        match self {
            Self::Stream {
                reader,
                pending,
                quic,
                ..
            } => read_stream_frames(reader, pending, quic.as_ref(), name).await,
            Self::Ws {
                reader,
                writer,
                deferred_error,
            } => {
                if let Some(error) = deferred_error.take() {
                    return Err(error);
                }
                read_ws_frames(reader, writer, deferred_error, name).await
            }
        }
    }

    pub(super) async fn write_frame(&mut self, name: &str, frame: &[u8]) -> Result<(), String> {
        match self {
            Self::Stream { writer, .. } => writer
                .write_all(frame)
                .await
                .map_err(|error| format!("{name}: send failed: {error}")),
            Self::Ws { writer, .. } => writer
                .send(frame)
                .await
                .map_err(|error| dial_message(name, &error)),
        }
    }

    /// A liveness probe: a WebSocket ping, or a protocol `PING` the server
    /// answers with `PONG` on the byte-stream lanes.
    pub(super) async fn probe(&mut self, name: &str) -> Result<(), String> {
        match self {
            Self::Stream { .. } => {
                self.write_frame(name, &encode(&FrameKind::Ping { nonce: 1 }))
                    .await
            }
            Self::Ws { writer, .. } => writer
                .send_ping()
                .await
                .map_err(|error| dial_message(name, &error)),
        }
    }

    pub(super) fn close(self) {
        if let Self::Stream {
            quic: Some((endpoint, connection)),
            ..
        } = self
        {
            connection.close(quinn::VarInt::from_u32(0), b"session closed");
            endpoint.close(quinn::VarInt::from_u32(0), b"");
        }
    }
}

async fn read_stream_frames(
    reader: &mut Box<dyn AsyncRead + Unpin + Send>,
    pending: &mut BytesMut,
    quic: Option<&(quinn::Endpoint, quinn::Connection)>,
    name: &str,
) -> Result<Option<Vec<Vec<u8>>>, String> {
    loop {
        let frames = take_complete_frames(pending, name)?;
        if !frames.is_empty() {
            return Ok(Some(frames));
        }
        let read = reader
            .read_buf(pending)
            .await
            .map_err(|error| format!("{name}: the connection was lost: {error}"))?;
        if read != 0 {
            continue;
        }
        if let Some((_, connection)) = quic
            && let Some(reason) = connection.close_reason()
        {
            return Err(quic_closed_message(name, &reason));
        }
        return Ok(None);
    }
}

fn take_complete_frames(pending: &mut BytesMut, name: &str) -> Result<Vec<Vec<u8>>, String> {
    let mut frames = Vec::new();
    while frames.len() < MAX_INBOUND_BATCH {
        match framing::split_frame(pending) {
            Ok(Some(frame)) => frames.push(frame.to_vec()),
            Err(error) if frames.is_empty() => return Err(format!("{name}: {error}")),
            // Preserve already-decoded frames. On error, the malformed bytes
            // remain at the front of `pending` and fail the next pump step.
            Ok(None) | Err(_) => break,
        }
    }
    Ok(frames)
}

async fn read_ws_frames(
    reader: &mut WsReader,
    writer: &mut WsWriter,
    deferred_error: &mut Option<String>,
    name: &str,
) -> Result<Option<Vec<Vec<u8>>>, String> {
    match recv_activity_alive(reader, writer)
        .await
        .map_err(|error| dial_message(name, &error))?
    {
        WsActivity::Message(frame) => {
            validate_ws_frame(&frame, name)?;
            let mut frames = vec![frame];
            while frames.len() < MAX_INBOUND_BATCH {
                let frame = match reader.try_recv_message() {
                    Ok(Some(frame)) => frame,
                    Ok(None) => break,
                    Err(error) => {
                        *deferred_error = Some(dial_message(name, &error));
                        break;
                    }
                };
                if let Err(error) = validate_ws_frame(&frame, name) {
                    *deferred_error = Some(error);
                    break;
                }
                frames.push(frame);
            }
            Ok(Some(frames))
        }
        // A pong or a peer ping: no phux payload, but proof of life.
        WsActivity::Control => Ok(Some(vec![Vec::new()])),
        WsActivity::Closed => Ok(None),
    }
}

fn validate_ws_frame(frame: &[u8], name: &str) -> Result<(), String> {
    framing::check_frame(frame)
        .map(|_| ())
        .map_err(|error| format!("{name} sent a malformed frame: {error}"))
}

pub(super) async fn dial(target: &Target, options: ConnectOptions) -> Result<Io, ConnectionEnd> {
    match &target.transport {
        Transport::Uds(path) => dial_uds(target.name.as_str(), path, options).await,
        Transport::Ws(url) => dial_ws(target, url, options).await,
        Transport::Quic(authority) => dial_quic(target, authority, options).await,
    }
}

async fn dial_uds(
    name: &str,
    path: &std::path::Path,
    options: ConnectOptions,
) -> Result<Io, ConnectionEnd> {
    let stream = tokio::time::timeout(options.dial_timeout, tokio::net::UnixStream::connect(path))
        .await
        .map_err(|_| ConnectionEnd::Dropped(Some(timed_out(name))))?
        .map_err(|error| {
            ConnectionEnd::Dropped(Some(format!(
                "{name}: could not connect to {}: {error}",
                path.display()
            )))
        })?;
    let (reader, writer) = stream.into_split();
    Ok(Io::Stream {
        reader: Box::new(reader),
        writer: Box::new(writer),
        pending: BytesMut::with_capacity(64 * 1024),
        quic: None,
    })
}

async fn dial_ws(target: &Target, url: &str, options: ConnectOptions) -> Result<Io, ConnectionEnd> {
    let name = target.name.as_str();
    let resolved = target
        .resolved()
        .ok_or_else(|| ConnectionEnd::Refused(format!("{name}: a WebSocket target needs a URL")))?;
    let ws = tokio::time::timeout(options.dial_timeout, async {
        // A planning failure is configuration, which no retry changes.
        let token = match target.token.clone() {
            Some(token) => Some(token),
            None => load_token(&resolved).map_err(ConnectionEnd::Refused)?,
        };
        let plan = plan_ws(&resolved, url, token).map_err(ConnectionEnd::Refused)?;
        let connected = phux_dial::ws::dial_with_identity(&plan, &TlsClientIdentity::None).await;
        // The Authorization header has been sent; drop the owned token now.
        drop(plan);
        connected.map_err(|error| classify(name, &error))
    })
    .await
    .map_err(|_| ConnectionEnd::Dropped(Some(timed_out(name))))??;
    let (tx, rx) = ws.split();
    Ok(Io::Ws {
        reader: WsReader::new(rx),
        writer: WsWriter { tx },
        deferred_error: None,
    })
}

async fn dial_quic(
    target: &Target,
    authority: &str,
    options: ConnectOptions,
) -> Result<Io, ConnectionEnd> {
    let name = target.name.as_str();
    let resolved = target.resolved().ok_or_else(|| {
        ConnectionEnd::Refused(format!("{name}: a QUIC target needs an authority"))
    })?;
    let established = tokio::time::timeout(options.dial_timeout, async {
        let plan = plan_quic_with_token(&resolved, authority, target.token.clone())
            .await
            .map_err(ConnectionEnd::Refused)?;
        let connected = phux_dial::quic::dial_with_identity(&plan, &TlsClientIdentity::None).await;
        // The bearer preamble has been written; the owned token goes now.
        drop(plan);
        connected.map_err(|error| classify(name, &error))
    })
    .await
    .map_err(|_| ConnectionEnd::Dropped(Some(timed_out(name))))??;
    let (endpoint, connection, send, recv) = established;
    if let Some(reason) = connection.close_reason() {
        return Err(ConnectionEnd::Dropped(Some(quic_closed_message(
            name, &reason,
        ))));
    }
    Ok(Io::Stream {
        reader: Box::new(recv),
        writer: Box::new(send),
        pending: BytesMut::with_capacity(64 * 1024),
        quic: Some((endpoint, connection)),
    })
}

/// A refusal no retry can satisfy ends the session; everything else walks
/// the ladder.
fn classify(name: &str, error: &phux_dial::DialError) -> ConnectionEnd {
    if is_fatal_refusal(error) {
        ConnectionEnd::Refused(dial_message(name, error))
    } else {
        ConnectionEnd::Dropped(Some(dial_message(name, error)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn complete_frames_are_delivered_before_a_later_malformed_frame() {
        let mut pending = BytesMut::new();
        FrameKind::Ping { nonce: 7 }.encode(&mut pending);
        pending.extend_from_slice(&[0, 0, 0, 0]);

        let valid = take_complete_frames(&mut pending, "fixture").expect("valid prefix");

        assert_eq!(valid.len(), 1);
        assert!(take_complete_frames(&mut pending, "fixture").is_err());
    }

    #[test]
    fn a_stream_read_drains_complete_frames_in_bounded_batches() {
        let mut pending = BytesMut::new();
        for nonce in 0..300 {
            FrameKind::Ping { nonce }.encode(&mut pending);
        }

        let first = take_complete_frames(&mut pending, "fixture").expect("first batch");
        let second = take_complete_frames(&mut pending, "fixture").expect("second batch");

        assert_eq!(first.len(), MAX_INBOUND_BATCH);
        assert_eq!(second.len(), 300 - MAX_INBOUND_BATCH);
        assert!(pending.is_empty());
        let (first_frame, tail) = FrameKind::decode(&first[0]).expect("frame decodes");
        assert!(tail.is_empty());
        assert!(matches!(first_frame, FrameKind::Ping { nonce: 0 }));
        let (last_frame, tail) =
            FrameKind::decode(second.last().expect("last frame")).expect("frame decodes");
        assert!(tail.is_empty());
        assert!(matches!(last_frame, FrameKind::Ping { nonce: 299 }));
    }
}
