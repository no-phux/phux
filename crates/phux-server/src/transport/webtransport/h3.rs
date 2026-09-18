//! Minimal HTTP/3 + WebTransport accept owned by phux (phux-50wm).
//!
//! Enough of the H3 control plane to complete a CONNECT session against a
//! wtransport or browser client, while exposing the raw CONNECT HEADERS
//! payload *before* any header map is built.

use std::io;
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::{AsyncRead as TokioAsyncRead, AsyncWrite as TokioAsyncWrite, ReadBuf};
use wtransport_proto::bytes::{AsyncRead, AsyncWrite};
use wtransport_proto::frame::{Frame, FrameKind};
use wtransport_proto::ids::{SessionId, StreamId};
use wtransport_proto::session::SessionResponse;
use wtransport_proto::settings::Settings;
use wtransport_proto::stream_header::StreamHeader;
use wtransport_proto::varint::VarInt;

/// Streams that must stay open for the WebTransport session to remain live.
#[allow(
    dead_code,
    reason = "held so dropping the phux stream does not reset CONNECT or SETTINGS"
)]
pub(super) struct SessionStreams {
    pub(super) connection: quinn::Connection,
    pub(super) connect_send: quinn::SendStream,
    pub(super) connect_recv: quinn::RecvStream,
    pub(super) settings_send: quinn::SendStream,
}

pub(super) struct H3Recv<'a>(pub(super) &'a mut quinn::RecvStream);

impl AsyncRead for H3Recv<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut [u8],
    ) -> Poll<io::Result<usize>> {
        let mut read_buf = ReadBuf::new(buf);
        match Pin::new(&mut self.0).poll_read(cx, &mut read_buf) {
            Poll::Ready(Ok(())) => Poll::Ready(Ok(read_buf.filled().len())),
            Poll::Ready(Err(err)) => Poll::Ready(Err(err)),
            Poll::Pending => Poll::Pending,
        }
    }
}

pub(super) struct H3Send<'a>(pub(super) &'a mut quinn::SendStream);

impl AsyncWrite for H3Send<'_> {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }
}

/// Open the local control stream and advertise WebTransport SETTINGS.
pub(super) async fn send_local_settings(conn: &quinn::Connection) -> io::Result<quinn::SendStream> {
    let mut send = conn.open_uni().await.map_err(io::Error::other)?;
    StreamHeader::new_control()
        .write_async(&mut H3Send(&mut send))
        .await
        .map_err(io::Error::other)?;
    let settings = Settings::builder()
        .qpack_max_table_capacity(VarInt::from_u32(0))
        .qpack_blocked_streams(VarInt::from_u32(0))
        .enable_connect_protocol()
        .enable_webtransport()
        .enable_h3_datagrams()
        .webtransport_max_sessions(VarInt::from_u32(1))
        .build();
    settings
        .generate_frame()
        .write_async(&mut H3Send(&mut send))
        .await
        .map_err(io::Error::other)?;
    Ok(send)
}

/// Drain peer control / QPACK uni streams so they cannot stall CONNECT.
pub(super) fn drain_uni_streams(conn: quinn::Connection) {
    tokio::spawn(async move {
        loop {
            let Ok(mut recv) = conn.accept_uni().await else {
                break;
            };
            let mut buf = [0_u8; 256];
            while recv.read(&mut buf).await.ok().flatten().is_some() {}
        }
    });
}

/// Accept the CONNECT request and return its raw QPACK HEADERS payload.
pub(super) async fn accept_connect(
    conn: &quinn::Connection,
) -> io::Result<(quinn::SendStream, quinn::RecvStream, Vec<u8>, SessionId)> {
    let (send, mut recv) = conn.accept_bi().await.map_err(io::Error::other)?;
    let session_id = session_id_from_stream(send.id())?;
    let payload = read_headers_payload(&mut recv).await?;
    Ok((send, recv, payload, session_id))
}

/// Reply to CONNECT with `200` or `403` and keep the stream open on success.
pub(super) async fn send_connect_status(send: &mut quinn::SendStream, ok: bool) -> io::Result<()> {
    let response = if ok {
        SessionResponse::ok()
    } else {
        SessionResponse::forbidden()
    };
    response
        .headers()
        .generate_frame()
        .write_async(&mut H3Send(send))
        .await
        .map_err(io::Error::other)?;
    if !ok {
        let _ = send.finish();
    }
    Ok(())
}

/// Accept the consumer's WebTransport bidi stream for phux frames.
pub(super) async fn accept_wt_bidi(
    conn: &quinn::Connection,
    session_id: SessionId,
) -> io::Result<(quinn::SendStream, quinn::RecvStream)> {
    loop {
        let (send, mut recv) = conn.accept_bi().await.map_err(io::Error::other)?;
        if frame_session_id(&mut recv).await? == Some(session_id) {
            return Ok((send, recv));
        }
    }
}

async fn read_headers_payload(recv: &mut quinn::RecvStream) -> io::Result<Vec<u8>> {
    loop {
        let frame = Frame::read_async(&mut H3Recv(recv))
            .await
            .map_err(io::Error::other)?;
        match frame.kind() {
            FrameKind::Exercise(_) => {}
            FrameKind::Headers => return Ok(frame.payload().to_vec()),
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "webtransport CONNECT first frame was not HEADERS",
                ));
            }
        }
    }
}

async fn frame_session_id(recv: &mut quinn::RecvStream) -> io::Result<Option<SessionId>> {
    loop {
        let frame = Frame::read_async(&mut H3Recv(recv))
            .await
            .map_err(io::Error::other)?;
        match frame.kind() {
            FrameKind::Exercise(_) => {}
            FrameKind::WebTransport => return Ok(frame.session_id()),
            _ => return Ok(None),
        }
    }
}

fn session_id_from_stream(id: quinn::StreamId) -> io::Result<SessionId> {
    let varint = VarInt::try_from_u64(u64::from(id)).map_err(io::Error::other)?;
    SessionId::try_from_session_stream(StreamId::new(varint)).map_err(io::Error::other)
}
