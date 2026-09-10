//! C-ABI tests for the remote tunnel: every call goes through the exported
//! `extern "C"` functions with `#[repr(C)]` structs, as an embedder's would.

use std::io::{Read, Write};
use std::os::fd::IntoRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

use super::*;
use crate::types::ABI_VERSION;

fn span(text: &str) -> PhuxBytes {
    bytes_out(text.as_bytes())
}

fn target_struct(target: &str, config: &str) -> PhuxRemoteTarget {
    PhuxRemoteTarget {
        size: mem::size_of::<PhuxRemoteTarget>(),
        version: ABI_VERSION,
        target: span(target),
        config_path: span(config),
    }
}

fn empty_info() -> PhuxRemoteTunnelInfo {
    PhuxRemoteTunnelInfo {
        size: mem::size_of::<PhuxRemoteTunnelInfo>(),
        version: ABI_VERSION,
        state: u32::MAX,
        transport: u32::MAX,
        name: PhuxBytes::default(),
        endpoint: PhuxBytes::default(),
        session: PhuxBytes::default(),
        message: PhuxBytes::default(),
    }
}

fn text(bytes: PhuxBytes) -> String {
    // SAFETY: spans returned by info are borrowed from a live tunnel.
    let slice = unsafe { bytes_in(bytes.data, bytes.len) }.expect("span");
    String::from_utf8(slice.to_vec()).expect("utf8")
}

struct Tunnel(*mut PhuxRemoteTunnel);

impl Tunnel {
    fn resolve(target: &str, config: &Path) -> Self {
        let config = config.to_str().expect("utf8 path");
        let request = target_struct(target, config);
        let mut out = ptr::null_mut();
        // SAFETY: valid struct and out pointer for the call.
        let result = unsafe { phux_remote_tunnel_resolve(&raw const request, &raw mut out) };
        assert_eq!(result, PhuxClientResult::Ok);
        assert!(!out.is_null());
        Self(out)
    }

    fn info(&self) -> (u32, u32, String, String, String, String) {
        let mut info = empty_info();
        // SAFETY: live tunnel, writable struct.
        let result = unsafe { phux_remote_tunnel_info(self.0, &raw mut info) };
        assert_eq!(result, PhuxClientResult::Ok);
        (
            info.state,
            info.transport,
            text(info.name),
            text(info.endpoint),
            text(info.session),
            text(info.message),
        )
    }

    fn state(&self) -> u32 {
        self.info().0
    }

    fn start(&self, fd: c_int) -> PhuxClientResult {
        // SAFETY: live tunnel owned by this thread; fd ownership transfers.
        unsafe { phux_remote_tunnel_start(self.0, fd) }
    }

    fn wait_for(&self, wanted: u32) {
        let started = Instant::now();
        while self.state() != wanted {
            assert!(
                started.elapsed() < Duration::from_secs(10),
                "state stuck at {}",
                self.state()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Tunnel {
    fn drop(&mut self) {
        // SAFETY: the unique pointer from resolve, freed once.
        unsafe { phux_remote_tunnel_free(self.0) };
    }
}

fn registry(dir: &Path, name: &str, endpoint: &str) -> std::path::PathBuf {
    let config = dir.join("config.toml");
    std::fs::write(
        &config,
        format!("[[remote]]\nname = \"{name}\"\nendpoint = \"{endpoint}\"\nsession = \"work\"\n"),
    )
    .expect("config");
    config
}

#[test]
fn malformed_arguments_are_refused_without_a_tunnel() {
    let mut out = ptr::dangling_mut::<PhuxRemoteTunnel>();
    // SAFETY: null target is the case under test; out is writable.
    let result = unsafe { phux_remote_tunnel_resolve(ptr::null(), &raw mut out) };
    assert_eq!(result, PhuxClientResult::InvalidArgument);
    assert!(out.is_null(), "out must be cleared before any failure");

    let mut stale = target_struct("mini", "");
    stale.version = ABI_VERSION + 1;
    // SAFETY: valid pointers; the version is what is wrong.
    let result = unsafe { phux_remote_tunnel_resolve(&raw const stale, &raw mut out) };
    assert_eq!(result, PhuxClientResult::InvalidArgument);

    let relative = target_struct("mini", "relative/config.toml");
    // SAFETY: valid pointers; the relative path is what is wrong.
    let result = unsafe { phux_remote_tunnel_resolve(&raw const relative, &raw mut out) };
    assert_eq!(result, PhuxClientResult::InvalidArgument);

    let nul = target_struct("mi\0ni", "");
    // SAFETY: valid pointers; the NUL is what is wrong.
    let result = unsafe { phux_remote_tunnel_resolve(&raw const nul, &raw mut out) };
    assert_eq!(result, PhuxClientResult::InvalidArgument);

    // A negative descriptor is refused; a null tunnel still closes the fd.
    let (ours, theirs) = UnixStream::pair().expect("pair");
    // SAFETY: null tunnel is the case under test; the fd is transferred.
    let result = unsafe { phux_remote_tunnel_start(ptr::null_mut(), theirs.into_raw_fd()) };
    assert_eq!(result, PhuxClientResult::InvalidArgument);
    let mut byte = [0_u8; 1];
    assert_eq!(
        (&ours).read(&mut byte).expect("eof"),
        0,
        "the transferred fd was closed"
    );
    // SAFETY: freeing null is a documented no-op.
    unsafe { phux_remote_tunnel_free(ptr::null_mut()) };
}

#[test]
fn unregistered_host_is_a_failed_tunnel_that_names_the_pairing_command() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = registry(dir.path(), "mini", "ws://127.0.0.1:1");
    let tunnel = Tunnel::resolve("me@studio", &config);
    let (state, transport, name, endpoint, _, message) = tunnel.info();
    assert_eq!(state, REMOTE_TUNNEL_FAILED);
    assert_eq!(transport, REMOTE_TRANSPORT_NONE);
    assert_eq!(name, "me@studio");
    assert!(endpoint.is_empty());
    assert!(message.contains("phux --remote me@studio"), "{message}");
    let (_ours, theirs) = UnixStream::pair().expect("pair");
    assert_eq!(
        tunnel.start(theirs.into_raw_fd()),
        PhuxClientResult::InvalidState
    );
}

#[test]
fn registered_host_resolves_its_display_fields_without_dialing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = registry(dir.path(), "mini", "quic://127.0.0.1:8788");
    let tunnel = Tunnel::resolve("mini:9999", &config);
    let (state, transport, name, endpoint, session, message) = tunnel.info();
    assert_eq!(state, REMOTE_TUNNEL_RESOLVED);
    assert_eq!(transport, REMOTE_TRANSPORT_QUIC);
    assert_eq!(name, "mini");
    // The CLI's `with_port_override` rewrites to the TYPED host plus the
    // explicit port; the tunnel must mean the same dial.
    assert_eq!(
        endpoint, "quic://mini:9999",
        "explicit port overrides the dial only"
    );
    assert_eq!(session, "work");
    assert!(message.is_empty());
    let registry_text = std::fs::read_to_string(&config).expect("config");
    assert!(
        registry_text.contains("8788"),
        "the registry is never rewritten"
    );
}

fn frame(body: &[u8]) -> Vec<u8> {
    let mut out = u32::try_from(body.len())
        .expect("small")
        .to_be_bytes()
        .to_vec();
    out.extend_from_slice(body);
    out
}

/// A loopback WebSocket peer that answers every binary message with the same
/// bytes and records what it received. Runs until the client disconnects.
fn spawn_echo_server() -> (u16, std::sync::mpsc::Receiver<Vec<u8>>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (seen_tx, seen_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            let (tcp, _) = listener.accept().await.expect("accept");
            let mut ws = tokio_tungstenite::accept_async(tcp).await.expect("upgrade");
            while let Some(Ok(message)) = ws.next().await {
                if let Message::Binary(bytes) = message {
                    let _ = seen_tx.send(bytes.to_vec());
                    if ws.send(Message::Binary(bytes)).await.is_err() {
                        break;
                    }
                }
            }
        });
    });
    (port, seen_rx)
}

fn read_exactly(stream: &mut UnixStream, len: usize) -> Vec<u8> {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("timeout");
    let mut out = vec![0; len];
    stream.read_exact(&mut out).expect("read");
    out
}

#[test]
fn websocket_tunnel_relays_whole_frames_both_ways_and_closes_cleanly() {
    let (port, seen) = spawn_echo_server();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = registry(dir.path(), "loop", &format!("ws://127.0.0.1:{port}"));
    let tunnel = Tunnel::resolve("loop", &config);
    let (mut ours, theirs) = UnixStream::pair().expect("pair");
    assert_eq!(tunnel.start(theirs.into_raw_fd()), PhuxClientResult::Ok);

    // Two frames in one write, the second split across two writes: the relay
    // must cut at declared lengths, one WebSocket message per frame.
    let first = frame(b"hello");
    let second = frame(b"phux frame");
    let mut burst = first.clone();
    burst.extend_from_slice(&second[..3]);
    ours.write_all(&burst).expect("write");
    ours.write_all(&second[3..]).expect("write");

    assert_eq!(
        seen.recv_timeout(Duration::from_secs(5)).expect("first"),
        first
    );
    assert_eq!(
        seen.recv_timeout(Duration::from_secs(5)).expect("second"),
        second
    );
    assert_eq!(read_exactly(&mut ours, first.len()), first);
    assert_eq!(read_exactly(&mut ours, second.len()), second);
    assert_eq!(tunnel.state(), REMOTE_TUNNEL_CONNECTED);

    drop(ours);
    tunnel.wait_for(REMOTE_TUNNEL_CLOSED);
    assert!(
        tunnel.info().5.is_empty(),
        "a clean close carries no failure message"
    );
}

#[test]
fn failure_is_published_before_the_embedder_sees_eof() {
    // Bind then drop: nothing listens on this loopback port afterwards.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port();
    let dir = tempfile::tempdir().expect("tempdir");
    let config = registry(dir.path(), "gone", &format!("ws://127.0.0.1:{port}"));
    let tunnel = Tunnel::resolve("gone", &config);
    let (mut ours, theirs) = UnixStream::pair().expect("pair");
    assert_eq!(tunnel.start(theirs.into_raw_fd()), PhuxClientResult::Ok);
    ours.set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout");
    let mut byte = [0_u8; 1];
    assert_eq!(ours.read(&mut byte).expect("eof"), 0);
    let (state, _, name, _, _, message) = tunnel.info();
    assert_eq!(
        state, REMOTE_TUNNEL_FAILED,
        "EOF must never precede the reason"
    );
    assert_eq!(name, "gone");
    assert!(message.contains("did not answer"), "{message}");
}

/// A host whose two directions are independent, as a phux server's are: it
/// streams `frames` binary frames out while it reads everything sent to it,
/// and reports the byte count it received.
fn spawn_streaming_server(
    frames: usize,
    body: usize,
    expect_in: usize,
) -> (u16, std::sync::mpsc::Receiver<usize>) {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let (received_tx, received_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        runtime.block_on(async move {
            listener.set_nonblocking(true).expect("nonblocking");
            let listener = tokio::net::TcpListener::from_std(listener).expect("listener");
            let (tcp, _) = listener.accept().await.expect("accept");
            let ws = tokio_tungstenite::accept_async(tcp).await.expect("upgrade");
            let (mut sink, mut stream) = ws.split();
            let sender = tokio::spawn(async move {
                for index in 0..frames {
                    let body = vec![u8::try_from(index % 251).expect("byte"); body];
                    if sink
                        .send(Message::Binary(frame(&body).into()))
                        .await
                        .is_err()
                    {
                        break;
                    }
                }
                sink
            });
            let mut total = 0;
            while total < expect_in {
                match stream.next().await {
                    Some(Ok(Message::Binary(bytes))) => total += bytes.len(),
                    Some(Ok(_)) => {}
                    _ => break,
                }
            }
            let _ = received_tx.send(total);
            // Hold the connection until the client leaves.
            let _sink = sender.await;
            while stream.next().await.is_some() {}
        });
    });
    (port, received_rx)
}

#[test]
fn websocket_relay_moves_a_large_burst_each_way_while_the_embedder_is_not_reading() {
    // 96 frames of 1 KiB each way: well past both 8 KiB socket-pair buffers.
    // Like Cockpit's worker inside a paste, the embedder writes its whole
    // burst before it reads anything. A relay that stopped reading the
    // embedder while delivering the host's output would deadlock here.
    const FRAMES: usize = 96;
    const BODY: usize = 1024;
    const TOTAL: usize = FRAMES * (BODY + 4);
    const { assert!(TOTAL > 64 * 1024) };
    let (port, received) = spawn_streaming_server(FRAMES, BODY, TOTAL);
    let dir = tempfile::tempdir().expect("tempdir");
    let config = registry(dir.path(), "burst", &format!("ws://127.0.0.1:{port}"));
    let tunnel = Tunnel::resolve("burst", &config);
    let (ours, theirs) = UnixStream::pair().expect("pair");
    assert_eq!(tunnel.start(theirs.into_raw_fd()), PhuxClientResult::Ok);

    let mut writer_end = ours.try_clone().expect("clone");
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        for _ in 0..FRAMES {
            if writer_end.write_all(&frame(&[0xa5; BODY])).is_err() {
                return;
            }
        }
        let _ = done_tx.send(());
    });
    done_rx
        .recv_timeout(Duration::from_secs(15))
        .expect("the embedder's burst must drain while its inbound side is full");
    assert_eq!(
        received
            .recv_timeout(Duration::from_secs(15))
            .expect("host count"),
        TOTAL
    );

    let mut reader = ours;
    let inbound = read_exactly(&mut reader, TOTAL);
    for (index, chunk) in inbound.chunks(BODY + 4).enumerate() {
        assert_eq!(
            &chunk[..4],
            &u32::try_from(BODY).expect("small").to_be_bytes()
        );
        assert!(
            chunk[4..]
                .iter()
                .all(|&byte| usize::from(byte) == index % 251)
        );
    }
    assert_eq!(tunnel.state(), REMOTE_TUNNEL_CONNECTED);
}

#[test]
fn free_cancels_an_unanswered_dial_promptly() {
    // A TEST-NET-1 address never answers; without cancellation this dial
    // would sit in QUIC's handshake until the dial timeout.
    let dir = tempfile::tempdir().expect("tempdir");
    let config = dir.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[[remote]]\nname = \"void\"\nendpoint = \"quic://192.0.2.1:8788\"\n\
             cert-fingerprint = \"{}\"\n",
            "ab".repeat(32)
        ),
    )
    .expect("config");
    let tunnel = Tunnel::resolve("void", &config);
    let (_ours, theirs) = UnixStream::pair().expect("pair");
    assert_eq!(tunnel.start(theirs.into_raw_fd()), PhuxClientResult::Ok);
    assert_eq!(tunnel.state(), REMOTE_TUNNEL_CONNECTING);
    let started = Instant::now();
    drop(tunnel);
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "free took {:?}",
        started.elapsed()
    );
}
