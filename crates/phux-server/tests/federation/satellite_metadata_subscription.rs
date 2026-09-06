//! phux-w7z2.57 — a `SUBSCRIBE_METADATA` naming a satellite pane is refused
//! **on the wire**, not silently accepted.
//!
//! L3 metadata does not federate: the hub relay carries L1 commands and
//! `SUBSCRIBE_EVENTS` across a satellite link and nothing else, so a
//! subscription to `Scope::Terminal(Satellite { .. })` can never produce a
//! `METADATA_CHANGED`. Before this ticket the server recorded it anyway, and
//! the consumer waited forever for a frame no code path emits — which is how
//! `phux agent wait host/@N` came to report `no_agent_record` about a live
//! remote agent.
//!
//! `SUBSCRIBE_METADATA` has no reply frame, so "the server accepted it" and
//! "the server dropped it" are indistinguishable by silence alone. The
//! refusal therefore rides an uncorrelated `ERROR
//! { UNSUPPORTED_SATELLITE_ROUTE }` push — the same shape
//! `SUBSCRIBE_EVENTS` already uses for a missing route, and an `ErrorCode`
//! that has shipped since 0.7.0, so no peer can fail to decode it.
//!
//! # Synchronization
//!
//! No sleeps and no "the client sent the frame" barrier: a frame that gets no
//! reply cannot anchor anything. Instead the test pipelines a `GET_METADATA`
//! behind the subscribe. The server handles one connection's frames in
//! order, so the refusal — emitted synchronously inside the `SUBSCRIBE`
//! handler — must precede the `METADATA_VALUE`. Reading the first frame and
//! asserting it is the `ERROR` is therefore deterministic in both directions:
//! with the refusal missing, the first frame is the `METADATA_VALUE` and the
//! assertion fires rather than hanging.

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::unwrap_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]

use phux_protocol::ids::{SatelliteHost, TerminalId};
use phux_protocol::wire::frame::{ErrorCode, FrameKind, Scope, TYPE_ERROR, TYPE_METADATA_VALUE};
use tempfile::TempDir;

use phux_server_testkit::{
    SOCKET_CONNECT_DEADLINE, recv_typed, run_local, send_frame, spawn_server, wait_for_socket,
};

const AGENT_KEY: &str = "phux.agent/v1";
/// Correlation id for the barrier read. Arbitrary; only its echo matters.
const BARRIER_REQUEST_ID: u32 = 0x0057_57F0;

fn satellite_scope() -> Scope {
    Scope::Terminal(TerminalId::Satellite {
        host: SatelliteHost::new("gpubox"),
        id: 7,
    })
}

#[test]
fn subscribe_metadata_on_a_satellite_scope_is_refused_on_the_wire() {
    run_local(async {
        let tmp = TempDir::new().unwrap();
        let socket_path = tmp.path().join("phux.sock");
        let (shutdown_tx, server_handle) = spawn_server(socket_path.clone(), None);

        // `wait_for_socket` completes the protocol-0.7 HELLO advertising
        // `LayerSet::all()`, so the SPEC §11.5 L3 tier gate is passed by a
        // real negotiation — the refusal under test is the satellite one, not
        // the out-of-tier drop that shares this handler.
        let mut stream = wait_for_socket(&socket_path, SOCKET_CONNECT_DEADLINE).await;

        send_frame(
            &mut stream,
            &FrameKind::SubscribeMetadata {
                scope: satellite_scope(),
                key: AGENT_KEY.to_owned(),
            },
        )
        .await;
        // The barrier: a command that DOES reply, pipelined behind the one
        // that does not. Its answer cannot overtake the refusal.
        send_frame(
            &mut stream,
            &FrameKind::GetMetadata {
                request_id: BARRIER_REQUEST_ID,
                scope: Scope::Global,
                key: "phux.test.barrier/v1".to_owned(),
            },
        )
        .await;

        let (type_byte, frame) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_ERROR,
            "the satellite subscribe must be refused before the barrier answers \
             (got type 0x{type_byte:02x}: {frame:?})",
        );
        match frame {
            FrameKind::Error {
                request_id,
                code,
                message,
            } => {
                assert_eq!(
                    request_id, None,
                    "SUBSCRIBE_METADATA carries no request_id to correlate to",
                );
                assert_eq!(
                    code,
                    ErrorCode::UnsupportedSatelliteRoute,
                    "the refusal must be typed, not a generic internal error",
                );
                // The code alone says "no route"; the message has to say
                // which route and why, or a consumer cannot tell this from a
                // misconfigured hub registry.
                assert!(
                    message.contains("does not federate"),
                    "diagnostic must name the limitation: {message}",
                );
                assert!(
                    message.contains("gpubox"),
                    "diagnostic must name the satellite: {message}",
                );
                assert!(
                    message.contains(AGENT_KEY),
                    "diagnostic must name the key: {message}",
                );
            }
            other => panic!("expected ERROR, got {other:?}"),
        }

        // The barrier still answers: a refused subscription must not poison
        // the connection. `SUBSCRIBE_METADATA` is best-effort by design and
        // tearing the transport down over one is exactly what the L3 dispatch
        // declines to do.
        let (type_byte, frame) = recv_typed(&mut stream).await;
        assert_eq!(
            type_byte, TYPE_METADATA_VALUE,
            "the pipelined GET must still be served (got type 0x{type_byte:02x})",
        );
        match frame {
            FrameKind::MetadataValue { request_id, value } => {
                assert_eq!(request_id, BARRIER_REQUEST_ID);
                assert_eq!(value, None, "the barrier key was never set");
            }
            other => panic!("expected METADATA_VALUE, got {other:?}"),
        }

        drop(stream);
        let _ = shutdown_tx.send(());
        server_handle.await.unwrap().unwrap();
    });
}
