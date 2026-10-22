//! Spawn/move replies, corked batches, framing violations, and routing.

use super::*;

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
