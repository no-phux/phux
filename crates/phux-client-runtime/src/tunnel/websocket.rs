//! WebSocket tunnel establishment, framing and concurrent forwarding halves.

use futures_util::StreamExt;
use futures_util::stream::SplitStream;
use phux_dial::ws::{Ws, WsKeepalive, WsLiveness, WsWriter};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{ReadHalf, WriteHalf};
use tokio::sync::mpsc;
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use super::TunnelShared;
use crate::dial::{
    DIAL_TIMEOUT, check_inbound, dial_message, load_token, plan_ws, stalled, take_frame, timed_out,
};
use crate::target::Resolved;

pub(super) async fn serve_ws(
    shared: &TunnelShared,
    resolved: &Resolved,
    url: &str,
    socket: &mut tokio::net::UnixStream,
) -> Result<(), String> {
    let name = resolved.name.as_str();
    let started = std::time::Instant::now();
    let ws = tokio::time::timeout(DIAL_TIMEOUT, async {
        let dial = plan_ws(resolved, url, load_token(resolved)?)?;
        tracing::info!(host = name, transport = "ws", url, "remote tunnel dialing");
        // The registry's enrolled identity, never `PHUX_WORKLOAD_*` inherited
        // from a launcher shell.
        let connected = phux_dial::ws::dial_with_identity(&dial, &resolved.client_identity)
            .await
            .map_err(|err| dial_message(name, &err));
        // The Authorization header has been sent; drop the owned token now.
        drop(dial);
        connected
    })
    .await
    .map_err(|_| timed_out(name))??;
    tracing::info!(
        host = name,
        transport = "ws",
        elapsed_ms = started.elapsed().as_millis(),
        "remote tunnel connected; relaying frames"
    );
    shared.connected();
    relay_ws(name, ws, socket).await
}

/// The WebSocket lane as two concurrent halves, like the QUIC lane's two
/// copies. Keepalive pings are asked for by the inbound half, which owns the
/// liveness clock, and sent by the outbound half, which owns the writer.
async fn relay_ws(name: &str, ws: Ws, socket: &mut tokio::net::UnixStream) -> Result<(), String> {
    let (tx, rx) = ws.split();
    let (pings_tx, pings_rx) = mpsc::channel(1);
    let (mut from_embedder, mut to_embedder) = socket.split();
    tokio::select! {
        outbound = embedder_to_host(name, &mut from_embedder, WsWriter { tx }, pings_rx) => outbound,
        inbound = host_to_embedder(name, rx, &mut to_embedder, pings_tx) => inbound,
    }
}

/// `Ok` only when the embedder closed its end.
async fn embedder_to_host(
    name: &str,
    from_embedder: &mut ReadHalf<'_>,
    mut writer: WsWriter,
    mut pings: mpsc::Receiver<()>,
) -> Result<(), String> {
    let mut pending = bytes::BytesMut::with_capacity(64 * 1024);
    loop {
        // Both arms are cancel-safe: `read_buf` keeps partial bytes in
        // `pending`, and a ping request is a unit with no payload to lose.
        tokio::select! {
            read = from_embedder.read_buf(&mut pending) => match read {
                Ok(0) => {
                    tracing::info!(host = name, "embedder closed its end");
                    return Ok(());
                }
                Ok(_) => forward_frames(name, &mut pending, &mut writer).await?,
                Err(err) => return Err(format!("{name}: reading the embedder's frames failed: {err}")),
            },
            Some(()) = pings.recv() => writer.send_ping().await.map_err(|err| dial_message(name, &err))?,
        }
    }
}

/// Never `Ok`: the host ending the stream is always a reason.
async fn host_to_embedder(
    name: &str,
    mut rx: SplitStream<Ws>,
    to_embedder: &mut WriteHalf<'_>,
    pings: mpsc::Sender<()>,
) -> Result<(), String> {
    // The same liveness policy `recv_message_alive` applies, kept here
    // because the writer that sends the ping lives in the other half.
    let mut keepalive = WsKeepalive::new(Instant::now());
    loop {
        let nap = match keepalive.poll(Instant::now()) {
            WsLiveness::Dead => return Err(stalled(name)),
            WsLiveness::Ping => {
                keepalive.note_ping(Instant::now());
                // Capacity one: a ping already queued answers this request.
                let _ = pings.try_send(());
                continue;
            }
            WsLiveness::Idle(nap) => nap,
        };
        let Ok(next) = tokio::time::timeout(nap, rx.next()).await else {
            continue;
        };
        match next {
            None | Some(Ok(Message::Close(_))) => {
                return Err(format!("{name} closed the connection"));
            }
            Some(Err(err)) => return Err(format!("{name}: the connection was lost: {err}")),
            Some(Ok(Message::Binary(frame))) => {
                keepalive.note_inbound(Instant::now());
                check_inbound(name, &frame)?;
                to_embedder
                    .write_all(&frame)
                    .await
                    .map_err(|err| format!("{name}: delivering a frame failed: {err}"))?;
                // Time spent waiting on the embedder is not the host's
                // silence.
                keepalive.note_inbound(Instant::now());
            }
            // Text, ping, pong: not a phux frame, but proof of life.
            Some(Ok(_)) => keepalive.note_inbound(Instant::now()),
        }
    }
}

/// Send every complete frame buffered in `pending` as one binary message,
/// leaving a partial tail for the next read.
async fn forward_frames(
    name: &str,
    pending: &mut bytes::BytesMut,
    writer: &mut WsWriter,
) -> Result<(), String> {
    while let Some(frame) = take_frame(pending).map_err(|err| format!("{name}: {err}"))? {
        writer
            .send(&frame)
            .await
            .map_err(|err| dial_message(name, &err))?;
    }
    Ok(())
}
