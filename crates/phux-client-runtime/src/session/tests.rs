//! Session behavior against a scripted peer on a socket pair.

#![allow(clippy::expect_used, reason = "test assertions")]

use std::io;
use std::time::Duration;

use bytes::{Bytes, BytesMut};
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::caps::{
    BootstrapCapabilities, BootstrapStreamProfile, select_bootstrap_profile,
};
use phux_protocol::ids::{BootstrapId, GroupId, ResourceId, SatelliteHost, StreamId};
use phux_protocol::wire::frame::{
    Command, CommandResult, ErrorCode, MAX_FRAME_LEN, Scope, SpawnResult, StateScope,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::*;

/// A hanging wait is the defect under test, so every round trip is capped.
const WEDGE_TIMEOUT: Duration = Duration::from_secs(20);

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    tokio::task::LocalSet::new().block_on(&rt, fut)
}

/// A frame carrying `seq`, for ordering assertions.
fn seq_ack(seq: u64) -> FrameKind {
    FrameKind::FrameAck {
        terminal_id: ResourceId::local(1),
        stream_id: StreamId::new(1).expect("stream"),
        bootstrap_id: BootstrapId::new(1).expect("bootstrap"),
        seq,
    }
}

fn pair() -> (Session, UnixStream) {
    let (near, far) = UnixStream::pair().expect("socketpair");
    (Session::from_stream(near), far)
}

/// Every frame `peer` holds once `expected` bytes have arrived.
async fn read_frames(peer: &mut UnixStream, expected: usize) -> Vec<FrameKind> {
    let mut buf = BytesMut::new();
    while buf.len() < expected {
        assert!(
            peer.read_buf(&mut buf).await.expect("read") > 0,
            "peer closed"
        );
    }
    let mut frames = Vec::new();
    while let Some(frame) = framing::split_frame(&mut buf).expect("well-formed") {
        frames.push(FrameKind::decode(&frame).expect("decodes").0);
    }
    frames
}

/// Spawn a server half that plays `script` once the request arrives.
fn scripted(script: Vec<FrameKind>) -> Session {
    let (client, server) = UnixStream::pair().expect("pair");
    let mut server = Session::from_stream(server);
    tokio::task::spawn_local(async move {
        server.recv().await.expect("request frame");
        for frame in &script {
            server.send(frame).await.expect("scripted frame");
        }
    });
    Session::from_stream(client)
}

fn ack(request_id: u32) -> FrameKind {
    FrameKind::CommandResult {
        request_id,
        result: CommandResult::Ok,
    }
}

fn bootstrap() -> Vec<FrameKind> {
    let terminal_id = ResourceId::local(1);
    let stream_id = StreamId::new(1).expect("stream");
    let bootstrap_id = BootstrapId::new(1).expect("bootstrap");
    vec![
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 120,
            rows: 40,
            base_seq: 0,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: Bytes::from_static(b"opening screen"),
        },
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
    ]
}

/// One `COMMAND` round trip against `script`.
fn request_against(script: Vec<FrameKind>) -> Reply {
    block_on(async {
        let mut client = scripted(script);
        let command = Command::GetState {
            scope: StateScope::Server,
        };
        tokio::time::timeout(WEDGE_TIMEOUT, client.request(7, command))
            .await
            .expect("the request must resolve")
            .expect("reply")
    })
}

// ----- handshake ----------------------------------------------------------

fn hello_ok(
    offered: &ClientCapabilities,
    server_caps: phux_protocol::caps::ServerCapabilities,
) -> FrameKind {
    let (selected_profile, bootstrap_limits) =
        select_bootstrap_profile(offered, &BootstrapCapabilities::new()).expect("intersect");
    FrameKind::HelloOk {
        protocol_major: PROTOCOL_VERSION.major,
        protocol_minor: PROTOCOL_VERSION.minor,
        protocol_patch: PROTOCOL_VERSION.patch,
        server_caps,
        server_id: b"incarnation-1".to_vec(),
        selected_profile,
        bootstrap_limits,
    }
}

/// Answer the HELLO that arrives with `answer(offer)`.
fn negotiate_against(
    answer: impl FnOnce(&ClientCapabilities) -> FrameKind + 'static,
) -> (Session, Result<(), SessionError>) {
    block_on(async {
        let (mut client, server) = pair();
        let mut server = Session::from_stream(server);
        let peer = tokio::task::spawn_local(async move {
            let FrameKind::Hello { client_caps, .. } = server.recv().await.expect("HELLO") else {
                panic!("the first frame is HELLO");
            };
            server.send(&answer(&client_caps)).await.expect("answer");
            server
        });
        let negotiated = client.negotiate("probe".to_owned(), control_caps()).await;
        drop(peer.await.expect("peer"));
        (client, negotiated)
    })
}

#[test]
fn negotiation_records_the_server_and_refuses_a_second_round() {
    let caps = phux_protocol::caps::ServerCapabilities::new().with_features(
        phux_protocol::caps::ServerFeatureSet::with(&[ServerFeature::AttachRoles]),
    );
    let (mut client, negotiated) = negotiate_against(move |offer| {
        assert!(
            !offer.quic_streams,
            "a Unix socket never offers QUIC streams"
        );
        hello_ok(offer, caps)
    });
    negotiated.expect("negotiated");
    assert_eq!(client.server_id(), Some(&b"incarnation-1"[..]));
    assert!(client.negotiated_bootstrap().is_some());
    assert_eq!(client.observer_role_policy(), Some(RolePolicy::VIEWER));
    assert!(!client.multistream_enabled(), "UDS is single-stream");
    let again = block_on(client.negotiate("probe".to_owned(), control_caps()));
    assert!(matches!(again, Err(SessionError::Protocol(_))), "{again:?}");
}

#[test]
fn a_hello_error_is_a_refusal_and_any_other_reply_is_skew() {
    let (_, refused) = negotiate_against(|_| FrameKind::Error {
        request_id: None,
        code: ErrorCode::PermissionDenied,
        message: "not on this socket".to_owned(),
    });
    assert!(
        matches!(&refused, Err(SessionError::Refused(message)) if message == "not on this socket"),
        "{refused:?}"
    );
    let (_, skewed) = negotiate_against(|_| FrameKind::Pong { nonce: 1 });
    assert!(
        matches!(&skewed, Err(SessionError::Protocol(message)) if message.contains("unexpected HELLO reply")),
        "{skewed:?}"
    );
}

#[test]
fn a_hello_ok_outside_the_offer_is_a_protocol_error() {
    let (client, refused) = negotiate_against(|offer| {
        let FrameKind::HelloOk {
            server_caps,
            server_id,
            selected_profile,
            bootstrap_limits,
            ..
        } = hello_ok(offer, phux_protocol::caps::ServerCapabilities::new())
        else {
            unreachable!("hello_ok builds HELLO_OK");
        };
        FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major.wrapping_add(1),
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            server_caps,
            server_id,
            selected_profile,
            bootstrap_limits,
        }
    });
    assert!(
        matches!(refused, Err(SessionError::Protocol(_))),
        "{refused:?}"
    );
    assert!(client.server().is_none(), "nothing negotiated on a refusal");
}

#[test]
fn attach_ids_are_non_zero_and_wrap_past_zero() {
    let (mut client, _peer) = block_on(async { pair() });
    assert_eq!(client.next_attach_id(), 1);
    assert_eq!(client.next_attach_id(), 2);
    client.next_attach_id = u32::MAX;
    assert_eq!(client.next_attach_id(), u32::MAX);
    assert_eq!(client.next_attach_id(), 1, "zero is never handed out");
}

// ----- correlated requests -----------------------------------------------

#[test]
fn pre_ack_bootstrap_is_returned_instead_of_discarded() {
    let mut script = bootstrap();
    script.push(ack(7));
    let reply = request_against(script);
    assert!(matches!(reply.result(), CommandResult::Ok));
    assert!(matches!(
        reply.interleaved(),
        [
            FrameKind::BootstrapBegin { .. },
            FrameKind::BootstrapChunk { .. },
            FrameKind::BootstrapReady { .. }
        ]
    ));
}

/// A hub pushes one uncorrelated ERROR per unreachable satellite before the
/// `GET_STATE` ack; swallowing it hides a partial fleet view.
#[test]
fn satellite_degradation_error_is_not_swallowed_by_get_state() {
    let reply = request_against(vec![
        FrameKind::Error {
            request_id: None,
            code: ErrorCode::UnsupportedSatelliteRoute,
            message: "no satellite route to build-box".to_owned(),
        },
        ack(7),
    ]);
    assert!(matches!(reply.result(), CommandResult::Ok));
    match reply.interleaved() {
        [FrameKind::Error { message, .. }] => {
            assert_eq!(message, "no satellite route to build-box");
        }
        other => panic!("degradation notice must survive, got {other:?}"),
    }
}

/// proto.md §9: an ERROR correlated to the request is its answer.
#[test]
fn correlated_error_answers_the_request_instead_of_hanging_forever() {
    let reply = request_against(vec![FrameKind::Error {
        request_id: Some(7),
        code: ErrorCode::InvalidCommand,
        message: "command not supported by this server".to_owned(),
    }]);
    match reply.result() {
        CommandResult::Error { code, message } => {
            assert_eq!(*code, ErrorCode::InvalidCommand);
            assert_eq!(message, "command not supported by this server");
        }
        other => panic!("a correlated ERROR is this command's answer, got {other:?}"),
    }
    assert!(reply.interleaved().is_empty());
}

/// Pipelined requests share a session: another request's ack is kept.
#[test]
fn another_requests_ack_is_kept_not_consumed() {
    let reply = request_against(vec![ack(99), ack(7)]);
    assert!(
        matches!(
            reply.interleaved(),
            [FrameKind::CommandResult { request_id: 99, .. }]
        ),
        "got {:?}",
        reply.interleaved()
    );
}

#[test]
fn a_server_that_closes_before_answering_is_a_disconnect() {
    block_on(async {
        let mut client = scripted(Vec::new());
        let reply = tokio::time::timeout(
            WEDGE_TIMEOUT,
            client.request(
                7,
                Command::GetState {
                    scope: StateScope::Server,
                },
            ),
        )
        .await
        .expect("resolves");
        assert!(
            matches!(reply, Err(SessionError::Disconnected)),
            "{reply:?}"
        );
    });
}

fn metadata_value(request_id: u32, value: Option<&[u8]>) -> FrameKind {
    FrameKind::MetadataValue {
        request_id,
        value: value.map(<[u8]>::to_vec),
    }
}

fn refusal(request_id: u32) -> FrameKind {
    FrameKind::Error {
        request_id: Some(request_id),
        code: ErrorCode::PermissionDenied,
        message: "policy refused that scope".to_owned(),
    }
}

fn metadata_against(script: Vec<FrameKind>) -> Reply<Answer<Option<Vec<u8>>>> {
    block_on(async {
        let mut client = scripted(script);
        tokio::time::timeout(
            WEDGE_TIMEOUT,
            client.request_metadata(7, Scope::Resource(ResourceId::local(1)), "k".to_owned()),
        )
        .await
        .expect("the read must resolve")
        .expect("metadata reply")
    })
}

/// A correlated refusal answers a metadata read, and stays distinct from an
/// unset key; an uncorrelated notice is kept.
#[test]
fn metadata_refusal_differs_from_unset_and_notices_are_kept() {
    let reply = metadata_against(vec![refusal(7)]);
    let refused = reply.result().as_ref().expect_err("refused");
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
    let unset = metadata_against(vec![metadata_value(7, None)]);
    assert_eq!(unset.result().as_ref().ok(), Some(&None));
    let notice = FrameKind::Error {
        request_id: None,
        code: ErrorCode::SatelliteUnreachable,
        message: "satellite build-box is unreachable".to_owned(),
    };
    let noted = metadata_against(vec![notice, metadata_value(7, Some(b"v"))]);
    assert_eq!(noted.result().as_ref().ok(), Some(&Some(b"v".to_vec())));
    assert!(matches!(
        noted.interleaved(),
        [FrameKind::Error {
            request_id: None,
            ..
        }]
    ));
}

#[test]
fn metadata_keys_round_trip() {
    let reply = block_on(async {
        let mut client = scripted(vec![FrameKind::MetadataKeys {
            request_id: 4,
            keys: vec!["a".to_owned(), "b".to_owned()],
        }]);
        client
            .request_metadata_keys(4, Scope::Resource(ResourceId::local(1)))
            .await
            .expect("keys")
    });
    assert_eq!(
        reply.into_result_ignoring_interleaved().expect("answered"),
        vec!["a".to_owned(), "b".to_owned()]
    );
}

/// A swarm's metadata reads cost one round trip per [`METADATA_WINDOW`].
/// The server answers nothing until a whole window has arrived, backwards,
/// refusing every seventh read and pushing a notice per window.
#[test]
fn metadata_reads_pipeline_one_round_trip_per_window() {
    const READS: usize = 300;
    let (answers, interleaved, round_trips) = block_on(async {
        let (client, server) = UnixStream::pair().expect("pair");
        let mut server = Session::from_stream(server);
        let server = tokio::task::spawn_local(async move {
            let mut round_trips = 0;
            let mut received = 0;
            while received < READS {
                let window = METADATA_WINDOW.min(READS - received);
                let mut ids = Vec::with_capacity(window);
                while ids.len() < window {
                    match server.recv().await.expect("read") {
                        FrameKind::GetMetadata { request_id, .. } => ids.push(request_id),
                        other => panic!("expected GET_METADATA, got {other:?}"),
                    }
                }
                received += window;
                round_trips += 1;
                server
                    .send(&FrameKind::Error {
                        request_id: None,
                        code: ErrorCode::UnsupportedSatelliteRoute,
                        message: "edge is unreachable".to_owned(),
                    })
                    .await
                    .expect("notice");
                for request_id in ids.into_iter().rev() {
                    let reply = if request_id % 7 == 0 {
                        refusal(request_id)
                    } else {
                        metadata_value(request_id, Some(&request_id.to_le_bytes()))
                    };
                    server.send(&reply).await.expect("answer");
                }
            }
            round_trips
        });
        let mut client = Session::from_stream(client);
        let reads = (0..READS)
            .map(|n| {
                let pane = ResourceId::local(u32::try_from(n).expect("small"));
                (Scope::Resource(pane), "phux.agent/v1".to_owned())
            })
            .collect();
        let (answers, interleaved) =
            tokio::time::timeout(WEDGE_TIMEOUT, client.request_metadata_many(100, reads))
                .await
                .expect("a timeout here is a read per round trip")
                .expect("metadata replies")
                .into_parts();
        drop(client);
        (answers, interleaved, server.await.expect("server"))
    });
    assert_eq!(round_trips, READS.div_ceil(METADATA_WINDOW));
    assert_eq!(answers.len(), READS);
    for (index, answer) in answers.iter().enumerate() {
        let request_id = 100 + u32::try_from(index).expect("small");
        if request_id % 7 == 0 {
            assert!(answer.is_err(), "read {index} was refused");
        } else {
            assert_eq!(
                answer.as_ref().expect("answered").as_deref(),
                Some(&request_id.to_le_bytes()[..])
            );
        }
    }
    assert_eq!(interleaved.len(), round_trips, "one notice per window");
}

fn spawn_frame(request_id: u32) -> FrameKind {
    FrameKind::SpawnResource {
        request_id,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: Some(SatelliteHost::new("build-box")),
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: None,
    }
}

#[test]
fn spawn_keeps_pushed_frames_and_takes_a_correlated_refusal() {
    let spawned = FrameKind::ResourceSpawned {
        request_id: 7,
        result: SpawnResult::Ok(ResourceId::local(3)),
    };
    let mut script = bootstrap();
    script.push(spawned);
    let reply = block_on(async { scripted(script).request_spawn(&spawn_frame(7)).await })
        .expect("spawn reply");
    assert!(matches!(reply.result(), Ok(SpawnResult::Ok(_))));
    assert_eq!(reply.interleaved().len(), 3);
    let refused = block_on(async {
        scripted(vec![refusal(7)])
            .request_spawn(&spawn_frame(7))
            .await
    })
    .expect("spawn reply");
    assert!(refused.result().is_err());
}

/// The correlation id comes from the frame, so a frame of another kind has
/// no id to wait on and is refused before anything is sent.
#[test]
fn spawn_and_move_reject_a_frame_of_another_kind() {
    block_on(async {
        let (mut client, _peer) = pair();
        assert!(matches!(
            client.request_spawn(&ack(1)).await,
            Err(SessionError::Protocol(_))
        ));
        assert!(matches!(
            client.request_move(&ack(1)).await,
            Err(SessionError::Protocol(_))
        ));
    });
}

// ----- framed send and receive -------------------------------------------

/// Nothing reaches the peer while corked, and everything arrives in send
/// order on the uncork, in one batch.
#[test]
fn a_corked_batch_is_withheld_then_shipped_in_order() {
    block_on(async {
        let (mut client, mut peer) = pair();
        client.cork();
        for seq in 1..=3 {
            client.send(&seq_ack(seq)).await.expect("corked send");
        }
        let mut early = [0u8; 64];
        assert!(
            matches!(peer.try_read(&mut early), Err(ref err) if err.kind() == io::ErrorKind::WouldBlock),
            "corked frames must not reach the peer before the uncork",
        );
        client.uncork().await.expect("uncork");
        let one = encode(&seq_ack(1)).len();
        let frames = read_frames(&mut peer, 3 * one).await;
        assert_eq!(frames, vec![seq_ack(1), seq_ack(2), seq_ack(3)]);
        // The uncork un-corks: the next send goes straight through.
        client.send(&seq_ack(9)).await.expect("send");
        assert_eq!(read_frames(&mut peer, one).await, vec![seq_ack(9)]);
    });
}

#[test]
fn try_recv_drains_a_burst_without_waiting() {
    block_on(async {
        let (mut client, mut peer) = pair();
        assert!(client.try_recv().expect("nothing yet").is_none());
        let mut burst = Vec::new();
        for seq in 1..=3 {
            burst.extend_from_slice(&encode(&seq_ack(seq)));
        }
        peer.write_all(&burst).await.expect("burst");
        assert_eq!(client.recv().await.expect("first"), seq_ack(1));
        assert_eq!(client.try_recv().expect("second"), Some(seq_ack(2)));
        assert_eq!(client.try_recv().expect("third"), Some(seq_ack(3)));
        assert!(client.try_recv().expect("drained").is_none());
    });
}

/// A write into a server that already hung up is recorded as such, and the
/// read side then reports the EOF a reconnect keys on.
#[test]
fn a_write_into_a_closed_peer_is_recorded_and_recv_reports_the_eof() {
    block_on(async {
        let (mut client, peer) = pair();
        drop(peer);
        assert!(!client.write_hit_closed_peer());
        let resize = FrameKind::ResizeTerminal {
            terminal_id: ResourceId::local(1),
            cols: 80,
            rows: 24,
            cell_px: None,
        };
        let err = client.send(&resize).await.expect_err("peer is gone");
        assert!(
            matches!(&err, SessionError::Io(io) if io.kind() == io::ErrorKind::BrokenPipe),
            "{err:?}"
        );
        assert!(client.write_hit_closed_peer());
        assert!(matches!(
            client.recv().await,
            Err(SessionError::Disconnected)
        ));
    });
}

// ----- SPEC §5 ERROR { FRAME_TOO_LARGE } ------------------------------------

fn assert_frame_too_large(frame: &FrameKind, header: [u8; 4]) {
    let declared = u32::from_be_bytes(header);
    assert!(
        matches!(
            frame,
            FrameKind::Error {
                request_id: None,
                code: ErrorCode::FrameTooLarge,
                message,
            } if message.contains(&declared.to_string())
        ),
        "expected ERROR{{FRAME_TOO_LARGE}} naming {declared}, got {frame:?}"
    );
}

async fn goodbye(peer: &mut UnixStream) -> FrameKind {
    let mut buf = BytesMut::new();
    loop {
        if let Some(frame) = framing::split_frame(&mut buf).expect("well-formed") {
            return FrameKind::decode(&frame).expect("decodes").0;
        }
        assert!(
            peer.read_buf(&mut buf).await.expect("read") > 0,
            "peer closed first"
        );
    }
}

/// SPEC §5: a peer receiving a length outside `1..=MAX_FRAME_LEN` MUST send
/// `ERROR { FRAME_TOO_LARGE }` and close, on either receive path.
#[test]
fn a_framing_violation_is_answered_with_frame_too_large() {
    for header in [
        0u32.to_be_bytes(),
        (MAX_FRAME_LEN + 1).to_be_bytes(),
        u32::MAX.to_be_bytes(),
    ] {
        block_on(async {
            let (mut client, mut peer) = pair();
            peer.write_all(&header).await.expect("header");
            let err = client.recv().await.expect_err("violation");
            assert!(matches!(err, SessionError::Framing(_)), "{err:?}");
            assert_frame_too_large(&goodbye(&mut peer).await, header);
        });
    }
    block_on(async {
        let header = 0u32.to_be_bytes();
        let (mut client, mut peer) = pair();
        let mut bytes = encode(&seq_ack(1));
        bytes.extend_from_slice(&header);
        peer.write_all(&bytes).await.expect("burst");
        assert_eq!(client.recv().await.expect("legal frame"), seq_ack(1));
        let err = client.try_recv().expect_err("violation");
        assert!(matches!(err, SessionError::Framing(_)), "{err:?}");
        assert_frame_too_large(&goodbye(&mut peer).await, header);
    });
}

/// The goodbye is not stuck behind a corked batch: the batch is dropped.
#[test]
fn a_framing_violation_drops_a_corked_batch() {
    block_on(async {
        let (mut client, mut peer) = pair();
        client.cork();
        client.send(&seq_ack(1)).await.expect("corked");
        peer.write_all(&0u32.to_be_bytes()).await.expect("header");
        assert!(client.recv().await.is_err());
        assert_frame_too_large(&goodbye(&mut peer).await, 0u32.to_be_bytes());
        client.uncork().await.expect("nothing left to ship");
    });
}

// ----- multistream off a QUIC link ----------------------------------------

#[test]
fn a_unix_session_routes_every_terminal_frame_on_control() {
    block_on(async {
        let (mut client, mut peer) = pair();
        let terminal = ResourceId::local(5);
        assert!(!client.multistream_enabled());
        assert!(client.can_route_terminal(&terminal));
        client
            .bind_terminal(&terminal)
            .await
            .expect("a no-op off QUIC");
        client.send(&seq_ack(4)).await.expect("on control");
        assert_eq!(
            read_frames(&mut peer, encode(&seq_ack(4)).len()).await,
            vec![seq_ack(4)]
        );
        client.unbind_terminal(&terminal);
        client.unbind_all_terminals();
        client.shutdown().await;
    });
}

#[test]
fn the_pong_check_reads_only_a_matching_pong() {
    let pong = Bytes::from(encode(&FrameKind::Pong { nonce: 9 }));
    assert!(is_pong(&pong, 9));
    assert!(!is_pong(&pong, 8));
    assert!(!is_pong(
        &Bytes::from(encode(&FrameKind::Ping { nonce: 9 })),
        9
    ));
    let held = vec![Bytes::from_static(b"x"); MUX_FRAME_CHANNEL + 1];
    assert!(check_barrier_bound(held.iter().take(MUX_FRAME_CHANNEL)).is_ok());
    assert!(check_barrier_bound(held.iter()).is_err());
}
