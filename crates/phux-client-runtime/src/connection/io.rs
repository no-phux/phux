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
    },
}

impl Io {
    /// The next inbound frame, or `Ok(None)` on a clean close. Cancel-safe:
    /// partial bytes stay buffered.
    pub(super) async fn read_frame(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        match self {
            Self::Stream {
                reader,
                pending,
                quic,
                ..
            } => read_stream_frame(reader, pending, quic.as_ref(), name).await,
            Self::Ws { reader, writer } => read_ws_frame(reader, writer, name).await,
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

async fn read_stream_frame(
    reader: &mut Box<dyn AsyncRead + Unpin + Send>,
    pending: &mut BytesMut,
    quic: Option<&(quinn::Endpoint, quinn::Connection)>,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    loop {
        if let Some(frame) =
            framing::split_frame(pending).map_err(|error| format!("{name}: {error}"))?
        {
            return Ok(Some(frame.to_vec()));
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

async fn read_ws_frame(
    reader: &mut WsReader,
    writer: &mut WsWriter,
    name: &str,
) -> Result<Option<Vec<u8>>, String> {
    match recv_activity_alive(reader, writer)
        .await
        .map_err(|error| dial_message(name, &error))?
    {
        WsActivity::Message(frame) => {
            framing::check_frame(&frame)
                .map_err(|error| format!("{name} sent a malformed frame: {error}"))?;
            Ok(Some(frame))
        }
        // A pong or a peer ping: no phux payload, but proof of life.
        WsActivity::Control => Ok(Some(Vec::new())),
        WsActivity::Closed => Ok(None),
    }
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
