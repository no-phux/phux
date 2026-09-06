//! `phux-w7z2.55` — HELLO's negotiated layers and the transport-authenticated
//! peer identity survive a mid-connection `DETACH`.
//!
//! `DETACH` (proto.md §7.2) ends an *attachment*. The reference server answers
//! `DETACHED` and keeps reading, because the same connection may serve a later
//! `ATTACH` — so the peer on the far side is still the peer the transport
//! authenticated, and it is still speaking the layer set it advertised.
//! Neither can be renegotiated: a second HELLO is a protocol error
//! (proto.md §6.1). Discarding either at `DETACH` therefore silently changed
//! what a live connection was entitled to do, in both directions:
//!
//! 1. **Fail-open.** `ServerState::client_layers` defaults to
//!    `LayerSet::all()` for a client the server has no HELLO record for (the
//!    permissive default that keeps test scaffolding simple). Forgetting the
//!    real record therefore did not restrict the connection, it *promoted*
//!    it: an L1-only consumer that attached and detached began passing the
//!    §11.5 out-of-tier gate and receiving L3 replies it had never negotiated.
//! 2. **Fail-closed.** `SHUTDOWN` is accepted on the local Unix socket only,
//!    and that check reads `PeerIdentity::transport`. With the identity gone
//!    the transport was `None`, so a local operator who detached could no
//!    longer stop their own server on the connection they still held.
//!
//! Both tests drive the production wire shape — real HELLO, real `ATTACH`,
//! real `DETACH` — and both are ordered by server replies rather than by the
//! clock: every step waits for the frame the previous step must produce.
//!
//! The negative assertion in the first test needs care, since "no
//! `METADATA_VALUE` arrives" is not directly observable. It is anchored on a
//! *following* `PING`: the per-client read loop processes frames in order and
//! the outbound mailbox is FIFO, so a `PONG` in hand proves the `GET_METADATA`
//! ahead of it was already fully dispatched. Anything the gate would have
//! emitted must therefore have arrived first. No sleep, no timeout-as-proof.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{ClientCapabilities, ColorSupport, LayerSet};
use phux_protocol::ids::GroupId;
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, FrameKind, Scope, TYPE_ATTACH_READY, TYPE_COMMAND_RESULT,
    TYPE_DETACHED, TYPE_HELLO_OK, TYPE_METADATA_VALUE, TYPE_PONG,
};
use tempfile::TempDir;
use tokio::net::UnixStream;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, attach_by_name, recv_typed, run_local, send_frame, spawn_server,
    wait_for_raw_socket,
};

const SESSION: &str = "work";

/// Matches `phux_server::state::DEFAULT_GROUP_ID`.
const fn collection_scope() -> Scope {
    Scope::Group(GroupId::new(1))
}

/// Connect and complete HELLO advertising exactly `layers`.
///
/// Deliberately not `wait_for_socket`, which hard-codes `LayerSet::all()` —
/// the whole point of the first test is a consumer that never negotiated L3.
async fn connect_with_layers(path: &std::path::Path, layers: LayerSet) -> UnixStream {
    let mut stream = wait_for_raw_socket(path, SOCKET_CONNECT_DEADLINE).await;
    send_frame(
        &mut stream,
        &FrameKind::Hello {
            client_name: "phux-w7z2.55-detach-test".to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: ClientCapabilities::new()
                .with_color_support(ColorSupport::TrueColor)
                .with_layers(layers),
        },
    )
    .await;
    let (type_byte, _) = recv_typed(&mut stream).await;
    assert_eq!(type_byte, TYPE_HELLO_OK, "HELLO must be accepted");
    stream
}

/// `ATTACH` to the seeded session and read through to `ATTACH_READY`, the
/// last frame of the attach handshake.
async fn attach_and_settle(stream: &mut UnixStream) {
    send_frame(stream, &attach_by_name(SESSION)).await;
    loop {
        let (type_byte, _) = recv_typed(stream).await;
        if type_byte == TYPE_ATTACH_READY {
            return;
        }
    }
}

/// Send `DETACH` and read through to the `DETACHED` acknowledgement. The
/// connection stays open afterwards; that is the state under test.
async fn detach_and_settle(stream: &mut UnixStream) {
    send_frame(stream, &FrameKind::Detach).await;
    loop {
        let (type_byte, _) = recv_typed(stream).await;
        if type_byte == TYPE_DETACHED {
            return;
        }
    }
}

/// An L1-only consumer stays L1-only after `DETACH`.
///
/// Before the fix, `ServerState::detach` dropped the cached `HELLO.layers`
/// and the permissive `LayerSet::all()` default took over, so the
/// `GET_METADATA` below was answered with `METADATA_VALUE` instead of the
/// silence SPEC §11.5 requires.
#[test]
fn l1_only_consumer_still_fails_the_l3_gate_after_detach() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some(SESSION));

        // L1 only: `LayerSet::new()` is the L1-only set (L1 is always on).
        let mut stream = connect_with_layers(&socket_path, LayerSet::new()).await;
        attach_and_settle(&mut stream).await;
        detach_and_settle(&mut stream).await;

        // An L3 request from a non-L3 consumer is dropped silently. The key
        // need not exist: an L3 consumer gets `METADATA_VALUE { value: None }`
        // for a missing key, so its *absence* is what distinguishes the tiers.
        send_frame(
            &mut stream,
            &FrameKind::GetMetadata {
                request_id: 7,
                scope: collection_scope(),
                key: "phux.tui.layout/v1".to_owned(),
            },
        )
        .await;
        // In-order barrier: PONG proves GET_METADATA was already dispatched.
        send_frame(&mut stream, &FrameKind::Ping { nonce: 0x7255 }).await;

        loop {
            let (type_byte, frame) = recv_typed(&mut stream).await;
            assert_ne!(
                type_byte, TYPE_METADATA_VALUE,
                "L1-only consumer received an L3 reply after DETACH \
                 (phux-w7z2.55: negotiated layers must survive detach); got {frame:?}",
            );
            if type_byte == TYPE_PONG {
                break;
            }
        }

        drop(stream);
        shutdown_tx.send(()).ok();
        server_handle.await.unwrap().unwrap();
    });
}

/// The transport-authenticated peer identity survives `DETACH`, so the
/// `SHUTDOWN` local-socket gate still recognises a local peer.
///
/// Before the fix, `detach_and_release_consumer_state` removed the identity
/// and the gate saw `transport: None`, refusing with `PERMISSION_DENIED`.
#[test]
fn local_peer_can_still_shut_down_after_detach() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (_shutdown_tx, server_handle) = spawn_server(socket_path.clone(), Some(SESSION));

        let mut stream = connect_with_layers(&socket_path, LayerSet::all()).await;
        attach_and_settle(&mut stream).await;
        detach_and_settle(&mut stream).await;

        send_frame(
            &mut stream,
            &FrameKind::Command {
                request_id: 11,
                command: Command::Shutdown,
            },
        )
        .await;

        let result = loop {
            let (type_byte, frame) = recv_typed(&mut stream).await;
            if type_byte == TYPE_COMMAND_RESULT
                && let FrameKind::CommandResult { request_id, result } = frame
                && request_id == 11
            {
                break result;
            }
        };
        match result {
            CommandResult::Ok => {}
            CommandResult::Error { code, message } => {
                assert_ne!(
                    code,
                    ErrorCode::PermissionDenied,
                    "SHUTDOWN refused after DETACH (phux-w7z2.55: peer identity \
                     must survive detach): {message}",
                );
                panic!("SHUTDOWN failed after DETACH: {code:?} {message}");
            }
            other => panic!("expected Ok from SHUTDOWN, got {other:?}"),
        }

        drop(stream);
        // SHUTDOWN cancels the root token; the runtime exits on its own.
        server_handle.await.unwrap().unwrap();
    });
}
