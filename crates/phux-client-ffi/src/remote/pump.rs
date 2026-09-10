//! The tunnel's own thread: dial the resolved host, then relay SPEC §5 frames
//! between the embedder's Unix-domain socket and the remote transport.
//!
//! Framing is untouched in both directions. QUIC carries the same
//! length-prefixed byte stream as a Unix socket, so that lane is a byte copy.
//! WebSocket carries exactly one frame per binary message, so that lane cuts
//! the embedder's byte stream at frame boundaries on the way out and checks
//! each message is exactly one frame on the way in. Nothing is decoded, and
//! the phux session kernel on the embedder's side never learns which lane it
//! is on.
//!
//! Both lanes run their two directions concurrently. An embedder's socket
//! worker typically reads only between its own writes (Cockpit's does), so a
//! lane that stopped reading the embedder while it delivered to it would
//! deadlock a large paste against heavy output as soon as both socket
//! buffers filled.
//!
//! Trust and credential rules are the CLI's (`phux attach`'s
//! `plan_quic_dial` / `plan_ws_dial`): a routable host needs a certificate
//! pin, and a routable WebSocket also needs `wss://` and a bearer token. The
//! token file is read here, on this thread, just before the dial, and the
//! owned copy is dropped as soon as the dial returns.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use bytes::BytesMut;
use futures_util::StreamExt;
use futures_util::stream::SplitStream;
use phux_dial::quic::{QuicDial, parse_token_hex};
use phux_dial::ws::{WS_LIVENESS_TIMEOUT, Ws, WsDial, WsKeepalive, WsLiveness, WsTarget, WsWriter};
use phux_dial::{CertTrust, DialError};
use phux_protocol::wire::framing;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::unix::{ReadHalf, WriteHalf};
use tokio::sync::{Notify, mpsc};
use tokio::time::Instant;
use tokio_tungstenite::tungstenite::Message;

use super::Shared;
use super::target::{Resolved, Transport, read_token};

/// Name resolution plus transport establishment. The CLI has no overall
/// bound here; an embedder does, because a status line that says
/// "connecting" for macOS's 75-second TCP connect timeout reads as a hang.
const DIAL_TIMEOUT: Duration = Duration::from_secs(15);

/// Largest frame body the embedder may hand the WebSocket lane: the SPEC §5
/// hard bound, the same ceiling Cockpit's socket worker enforces inbound
/// (`transport.max_frame_bytes` less its 4-byte prefix).
const MAX_FRAME_BODY: usize = 16 * 1024 * 1024;

/// Run one tunnel to completion on the calling (dedicated) thread.
///
/// The terminal state is published BEFORE the embedder's socket is dropped,
/// so an embedder that reads EOF and then asks why always finds the answer.
pub(super) fn run(
    shared: &Arc<Shared>,
    cancel: &Arc<Notify>,
    resolved: &Resolved,
    stream: std::os::unix::net::UnixStream,
) {
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(err) => {
            shared.fail(format!("could not start the tunnel runtime: {err}"));
            drop(stream);
            return;
        }
    };
    runtime.block_on(async {
        let Some(mut socket) = adopt(shared, stream) else {
            return;
        };
        let outcome = tokio::select! {
            () = cancel.notified() => Ok(()),
            outcome = serve(shared, resolved, &mut socket) => outcome,
        };
        match outcome {
            Ok(()) => shared.close(),
            Err(reason) => shared.fail(reason),
        }
        drop(socket);
    });
    // A name lookup still parked on the blocking pool must not hold the
    // embedder's `free` hostage; the process owns no result it could deliver.
    runtime.shutdown_background();
}

/// Hand the embedder's socket to tokio. Every failure is published while a
/// descriptor still holds the socket open, so the embedder never reads EOF
/// before it can read why.
fn adopt(
    shared: &Shared,
    stream: std::os::unix::net::UnixStream,
) -> Option<tokio::net::UnixStream> {
    let unusable =
        |err: std::io::Error| format!("the embedder's transport socket is unusable: {err}");
    if let Err(err) = stream.set_nonblocking(true) {
        shared.fail(unusable(err));
        return None;
    }
    // `from_std` consumes the stream even when it fails; this duplicate is
    // what keeps the socket open until the reason is published.
    let hold = match stream.try_clone() {
        Ok(hold) => hold,
        Err(err) => {
            shared.fail(unusable(err));
            return None;
        }
    };
    match tokio::net::UnixStream::from_std(stream) {
        Ok(socket) => Some(socket),
        Err(err) => {
            shared.fail(unusable(err));
            drop(hold);
            None
        }
    }
}

/// `Ok` means the embedder closed its end; every other ending is a reason.
async fn serve(
    shared: &Shared,
    resolved: &Resolved,
    socket: &mut tokio::net::UnixStream,
) -> Result<(), String> {
    match &resolved.transport {
        Transport::Quic(authority) => serve_quic(shared, resolved, authority, socket).await,
        Transport::Ws(url) => serve_ws(shared, resolved, url, socket).await,
    }
}

async fn serve_quic(
    shared: &Shared,
    resolved: &Resolved,
    authority: &str,
    socket: &mut tokio::net::UnixStream,
) -> Result<(), String> {
    let name = resolved.name.as_str();
    let established = tokio::time::timeout(DIAL_TIMEOUT, async {
        let dial = plan_quic(resolved, authority).await?;
        let connected = phux_dial::quic::dial(&dial)
            .await
            .map_err(|err| dial_message(name, &err));
        // The bearer preamble has been written; the owned token goes now,
        // not at the end of the session.
        drop(dial);
        connected
    })
    .await
    .map_err(|_| timed_out(name))??;
    let (endpoint, connection, mut to_host, mut from_host) = established;
    shared.connected();
    let (mut from_embedder, mut to_embedder) = socket.split();
    let result = tokio::select! {
        outbound = tokio::io::copy(&mut from_embedder, &mut to_host) => outbound
            .map(|_| ())
            .map_err(|err| format!("{name}: sending failed: {err}")),
        inbound = tokio::io::copy(&mut from_host, &mut to_embedder) => Err(match inbound {
            Ok(_) => format!("{name} closed the connection"),
            Err(err) => format!("{name}: the connection was lost: {err}"),
        }),
    };
    connection.close(quinn::VarInt::from_u32(0), b"tunnel closed");
    endpoint.close(quinn::VarInt::from_u32(0), b"");
    result
}

async fn plan_quic(resolved: &Resolved, authority: &str) -> Result<QuicDial, String> {
    let name = resolved.name.as_str();
    let (bare, addr) = resolve_quic_addr(name, authority).await?;
    let trust = quic_trust(
        name,
        resolved.cert_fingerprint.as_deref(),
        addr.ip().is_loopback(),
    )?;
    let token = match load_token(resolved)? {
        Some(token) => Some(parse_token_hex(&token).map_err(|_| bad_token(resolved))?),
        None => None,
    };
    Ok(QuicDial {
        addr,
        server_name: quic_server_name(bare),
        token,
        trust,
    })
}

/// Split `HOST:PORT` and resolve it to its first address, as the CLI's
/// `resolve_quic_target` does. Returns the unbracketed host beside it.
async fn resolve_quic_addr<'a>(
    name: &str,
    authority: &'a str,
) -> Result<(&'a str, SocketAddr), String> {
    let (host, port) = authority
        .rsplit_once(':')
        .and_then(|(host, port)| Some((host, port.parse::<u16>().ok()?)))
        .ok_or_else(|| format!("{name}: endpoint quic://{authority} needs HOST:PORT"))?;
    let bare = host.trim_matches(['[', ']']);
    let addr = tokio::net::lookup_host((bare, port))
        .await
        .map_err(|err| {
            format!("{name}: could not resolve {bare}: {err}; is its network connected?")
        })?
        .next()
        .ok_or_else(|| format!("{name}: {bare} resolved to no addresses"))?;
    Ok((bare, addr))
}

/// Pin when the registry carries a fingerprint, trust loopback's dev
/// certificate, and refuse an unpinned routable dial (the CLI's `quic_trust`).
fn quic_trust(name: &str, pin: Option<&str>, loopback: bool) -> Result<CertTrust, String> {
    match pin {
        Some(fingerprint) => Ok(CertTrust::Pinned(fingerprint.to_owned())),
        None if loopback => Ok(CertTrust::SkipVerify),
        None => Err(unpinned(name)),
    }
}

/// Conventional SNI for a DNS name; the historical `localhost` for an IP
/// literal, matching the server's self-signed SANs (as the CLI does).
fn quic_server_name(bare: &str) -> String {
    if bare.parse::<IpAddr>().is_ok() {
        "localhost".to_owned()
    } else {
        bare.to_owned()
    }
}

async fn serve_ws(
    shared: &Shared,
    resolved: &Resolved,
    url: &str,
    socket: &mut tokio::net::UnixStream,
) -> Result<(), String> {
    let name = resolved.name.as_str();
    let ws = tokio::time::timeout(DIAL_TIMEOUT, async {
        let dial = plan_ws(resolved, url, load_token(resolved)?)?;
        let connected = phux_dial::ws::dial(&dial)
            .await
            .map_err(|err| dial_message(name, &err));
        // The Authorization header has been sent; drop the owned token now.
        drop(dial);
        connected
    })
    .await
    .map_err(|_| timed_out(name))??;
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
    let mut pending = BytesMut::with_capacity(64 * 1024);
    loop {
        // Both arms are cancel-safe: `read_buf` keeps partial bytes in
        // `pending`, and a ping request is a unit with no payload to lose.
        tokio::select! {
            read = from_embedder.read_buf(&mut pending) => match read {
                Ok(0) => return Ok(()),
                Ok(_) => forward_frames(name, &mut pending, &mut writer).await?,
                Err(err) => return Err(format!("{name}: reading the embedder's frames failed: {err}")),
            },
            Some(()) = pings.recv() => writer
                .send_ping()
                .await
                .map_err(|err| dial_message(name, &err))?,
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

/// One binary message must be exactly one SPEC §5 frame, the check the
/// server applies to its own inbound messages. A message that disagrees
/// with its declared length would desynchronize the embedder's framed
/// stream, so it fails the tunnel with a reason instead.
fn check_inbound(name: &str, frame: &[u8]) -> Result<(), String> {
    framing::check_frame(frame)
        .map(|_| ())
        .map_err(|err| format!("{name} sent a malformed frame: {err}"))
}

fn plan_ws(resolved: &Resolved, url: &str, token: Option<String>) -> Result<WsDial, String> {
    let name = resolved.name.as_str();
    let target = WsTarget::parse(url).map_err(|err| format!("{name}: {err}"))?;
    if !target.is_loopback() {
        if !target.secure {
            return Err(format!(
                "{name}: {url} is plaintext; a routable WebSocket needs wss://"
            ));
        }
        if resolved.cert_fingerprint.is_none() {
            return Err(unpinned(name));
        }
        if token.is_none() {
            return Err(format!(
                "{name} has no token file in the registry; re-pair it with `phux host enroll {name}`"
            ));
        }
    }
    let token = match token {
        Some(token) => {
            parse_token_hex(&token).map_err(|_| bad_token(resolved))?;
            Some(token.trim().to_owned())
        }
        None => None,
    };
    Ok(WsDial {
        url: url.to_owned(),
        token,
        trust: resolved
            .cert_fingerprint
            .clone()
            .map_or(CertTrust::SkipVerify, CertTrust::Pinned),
        tls_server_name: None,
    })
}

/// Read the bearer token for this dial. Failures name the file, never bytes.
fn load_token(resolved: &Resolved) -> Result<Option<String>, String> {
    read_token(resolved.token_file.as_deref()).map_err(|err| format!("{}: {err}", resolved.name))
}

/// Fixed wording, as the CLI's: a decoder's message can quote a character
/// of the token, and the token file's path is all an operator needs.
fn bad_token(resolved: &Resolved) -> String {
    resolved.token_file.as_ref().map_or_else(
        || format!("{}: the token is not valid hex", resolved.name),
        |path| {
            format!(
                "{}: the token file {} is not valid hex",
                resolved.name,
                path.display()
            )
        },
    )
}

/// Send every complete frame buffered in `pending` as one binary message,
/// leaving a partial tail for the next read.
async fn forward_frames(
    name: &str,
    pending: &mut BytesMut,
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

/// Split one complete `len:u32 BE` + body frame off the front of `pending`.
fn take_frame(pending: &mut BytesMut) -> Result<Option<BytesMut>, &'static str> {
    let Some(header) = pending.get(..4) else {
        return Ok(None);
    };
    let declared = u32::from_be_bytes([header[0], header[1], header[2], header[3]]) as usize;
    if declared == 0 || declared > MAX_FRAME_BODY {
        return Err("the embedder sent a frame with an invalid length");
    }
    if pending.len() < 4 + declared {
        return Ok(None);
    }
    Ok(Some(pending.split_to(4 + declared)))
}

fn unpinned(name: &str) -> String {
    format!(
        "{name} has no certificate pin in the registry; re-pair it with `phux host enroll {name}`"
    )
}

fn timed_out(name: &str) -> String {
    format!(
        "{name} did not answer within {}s; check that it is up and its network is connected",
        DIAL_TIMEOUT.as_secs()
    )
}

fn stalled(name: &str) -> String {
    format!(
        "{name} stopped answering (no WebSocket traffic for {}s and no pong for our keepalive ping)",
        WS_LIVENESS_TIMEOUT.as_secs()
    )
}

/// Word a dial failure the way an operator can act on it: reachability
/// versus credentials, the split `DialError` already draws.
fn dial_message(name: &str, err: &DialError) -> String {
    match err {
        DialError::Unreachable(detail) => format!(
            "{name} did not answer ({detail}); check that it is up and its network is connected"
        ),
        DialError::Connect(detail) => format!("{name}: {detail}"),
        DialError::Io(detail) => format!("{name}: {detail}"),
        DialError::Stalled(detail) => format!("{name} stopped answering ({detail})"),
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn frames_are_cut_at_their_declared_length() {
        let mut pending = BytesMut::from(&b"\x00\x00\x00\x02hi\x00\x00\x00\x03ab"[..]);
        let first = take_frame(&mut pending).expect("valid").expect("complete");
        assert_eq!(&first[..], b"\x00\x00\x00\x02hi");
        // The second frame is short one byte: nothing is taken, nothing lost.
        assert!(take_frame(&mut pending).expect("valid").is_none());
        assert_eq!(&pending[..], b"\x00\x00\x00\x03ab");
        pending.extend_from_slice(b"c");
        assert_eq!(
            &take_frame(&mut pending).expect("valid").expect("complete")[..],
            b"\x00\x00\x00\x03abc"
        );
        assert!(pending.is_empty());
    }

    #[test]
    fn zero_and_oversized_lengths_are_refused() {
        assert!(take_frame(&mut BytesMut::from(&b"\x00\x00\x00\x00"[..])).is_err());
        let oversized = u32::try_from(MAX_FRAME_BODY + 1)
            .expect("fits")
            .to_be_bytes();
        assert!(take_frame(&mut BytesMut::from(&oversized[..])).is_err());
    }

    #[test]
    fn an_inbound_message_must_be_exactly_one_frame() {
        assert!(check_inbound("mini", b"\x00\x00\x00\x02hi").is_ok());
        for malformed in [
            &b"\x00\x00\x00"[..],
            &b"\x00\x00\x00\x03hi"[..],
            &b"\x00\x00\x00\x01hi"[..],
            &b"\x00\x00\x00\x01h\x00\x00\x00\x01i"[..],
        ] {
            let err = check_inbound("mini", malformed).expect_err("malformed");
            assert!(err.starts_with("mini sent a malformed frame"), "{err}");
        }
    }

    fn resolved(url: &str, pin: Option<&str>) -> Resolved {
        Resolved {
            name: "mini".to_owned(),
            endpoint: url.to_owned(),
            session: None,
            transport: Transport::Ws(url.to_owned()),
            token_file: Some(PathBuf::from("/secret/mini.token")),
            cert_fingerprint: pin.map(str::to_owned),
        }
    }

    #[test]
    fn routable_websocket_needs_tls_pin_and_token_like_the_cli() {
        let pin = "ab".repeat(32);
        let routable = "wss://mini.ts.net:8787";
        assert!(
            plan_ws(
                &resolved("ws://mini.ts.net:8787", Some(&pin)),
                "ws://mini.ts.net:8787",
                Some("ab".to_owned())
            )
            .is_err()
        );
        assert!(plan_ws(&resolved(routable, None), routable, Some("ab".to_owned())).is_err());
        assert!(plan_ws(&resolved(routable, Some(&pin)), routable, None).is_err());
        let ok = plan_ws(
            &resolved(routable, Some(&pin)),
            routable,
            Some(" ab ".to_owned()),
        )
        .expect("pinned");
        assert_eq!(ok.token.as_deref(), Some("ab"));
        assert_eq!(ok.trust, CertTrust::Pinned(pin));
        // Loopback is the development path: no pin, no token, plaintext.
        let local = plan_ws(
            &resolved("ws://127.0.0.1:8787", None),
            "ws://127.0.0.1:8787",
            None,
        )
        .expect("loopback");
        assert_eq!(local.trust, CertTrust::SkipVerify);
    }

    #[test]
    fn a_malformed_token_is_reported_by_file_never_by_its_bytes() {
        let pin = "ab".repeat(32);
        let routable = "wss://mini.ts.net:8787";
        let err = plan_ws(
            &resolved(routable, Some(&pin)),
            routable,
            Some("zq-secret".to_owned()),
        )
        .expect_err("not hex");
        assert_eq!(
            err,
            "mini: the token file /secret/mini.token is not valid hex"
        );
        for leaked in ['z', 'q', 's'] {
            assert!(!err[..err.find(" the token file").expect("prefix")].contains(leaked));
        }
    }

    #[test]
    fn routable_quic_without_a_pin_is_refused_after_resolution() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let mut unpinned = resolved("quic://192.0.2.1:8788", None);
        unpinned.transport = Transport::Quic("192.0.2.1:8788".to_owned());
        unpinned.token_file = None;
        let err = runtime
            .block_on(plan_quic(&unpinned, "192.0.2.1:8788"))
            .expect_err("unpinned");
        assert!(err.contains("certificate pin"), "{err}");
        let loopback = runtime
            .block_on(plan_quic(&unpinned, "127.0.0.1:8788"))
            .expect("loopback");
        assert_eq!(loopback.trust, CertTrust::SkipVerify);
        assert_eq!(loopback.server_name, "localhost");
    }
}
