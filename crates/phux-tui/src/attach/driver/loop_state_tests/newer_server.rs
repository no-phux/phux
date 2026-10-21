// A newer 1.x server's additions reach the attach loop through
// `Connection::recv`: an unknown frame type or enum value is skipped rather
// than ending the attach, a state-bearing frame carrying a value this build
// does not know still decodes, and a spawn reply with an unknown refusal
// applies.

/// A frame of `type_byte` with the given `(field id, value)` TLV fields.
fn newer_server_raw(type_byte: u8, fields: &[(u8, &[u8])]) -> Vec<u8> {
    let mut body = vec![type_byte];
    for (id, value) in fields {
        // Every id and length here is under 128: one varint byte each.
        body.extend([*id, 2, u8::try_from(value.len()).unwrap()]);
        body.extend_from_slice(value);
    }
    let mut frame = u32::try_from(body.len()).unwrap().to_be_bytes().to_vec();
    frame.extend(body);
    frame
}

/// A bootstrapped attach loop whose server end is a raw socket.
async fn newer_server_loop() -> (SessionLoop, Connection, tokio::net::UnixStream, Vec<u8>) {
    let (a, b) = tokio::net::UnixStream::pair().unwrap();
    let mut client = Connection::from_stream(a);
    let negotiated = NegotiatedBootstrap {
        profile: BootstrapProfile::SynthesizedVtRaw,
        limits: BootstrapLimits::default(),
        server_features: ServerFeatureSet::new(),
        server_features_ext: phux_protocol::ServerFeatureExtSet::new(),
    };
    let mut state = SessionLoop::new(
        negotiated,
        PredictiveConfig::disabled(),
        false,
        None,
        EntryPick::default(),
        None,
    )
    .unwrap();
    let mut out = Vec::new();
    state
        .bootstrap(&mut client, &mut out, initial_attached(), None)
        .await
        .unwrap();
    (state, client, b, out)
}

#[tokio::test(flavor = "current_thread")]
async fn a_newer_servers_unknown_frames_and_values_do_not_end_the_attach() {
    use phux_protocol::wire::frame::{
        HistoryTombstoneReason, SpawnError, SpawnResult, TYPE_HISTORY_TOMBSTONE,
        TYPE_METADATA_CHANGED, TYPE_RESOURCE_SPAWNED,
    };
    use tokio::io::AsyncWriteExt as _;

    let (mut state, mut client, mut server, mut out) = Box::pin(newer_server_loop()).await;
    let local = [0x00, 0, 0, 0, 1];
    let mut burst = newer_server_raw(0x6E, &[]);
    burst.extend(newer_server_raw(
        TYPE_METADATA_CHANGED,
        &[(1, &[0xFE]), (2, b"key")],
    ));
    // A history tombstone whose reason a newer server allocated.
    burst.extend(newer_server_raw(
        TYPE_HISTORY_TOMBSTONE,
        &[
            (1, &local),
            (2, &1_u64.to_be_bytes()),
            (3, &1_u64.to_be_bytes()),
            (4, b"cursor"),
            (5, &[0x7E]),
        ],
    ));
    // A spawn refusal whose tag a newer server allocated.
    burst.extend(newer_server_raw(
        TYPE_RESOURCE_SPAWNED,
        &[(1, &77_u32.to_be_bytes()), (2, &[0x01, 0xFE])],
    ));
    server.write_all(&burst).await.unwrap();

    let tombstone = client.recv().await.expect("unknown frames are skipped");
    assert!(matches!(
        tombstone,
        FrameKind::HistoryTombstone {
            reason: HistoryTombstoneReason::Unknown,
            ..
        }
    ));
    let spawned = client.recv().await.expect("the spawn refusal decodes");
    assert_eq!(
        spawned,
        FrameKind::ResourceSpawned {
            request_id: 77,
            result: SpawnResult::Err(SpawnError::Unknown(0xFE)),
        }
    );
    // The tombstone's reason maps onto the kernel's in `kernel_input`; this
    // pane has no bootstrapped generation for it to retire here.
    let step = state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            spawned,
            false,
            &mut RepaintAccumulator::default(),
        )
        .await
        .expect("a newer server's value is not a protocol fault");
    assert!(!matches!(step, FrameStep::Exit(_)));
}

/// A newer server refuses a spawn it cannot read with a correlated `ERROR
/// UNSUPPORTED`: the parked spawn settles as refused and the attach stays.
#[tokio::test(flavor = "current_thread")]
async fn a_spawn_answered_unsupported_settles_and_keeps_the_attach() {
    use phux_protocol::wire::frame::ErrorCode;

    let (mut state, mut client, _server, mut out) = Box::pin(newer_server_loop()).await;
    state
        .mirror
        .pending_floating
        .insert(9, "Agent Board".to_owned());
    let step = state
        .apply_server_frame(
            &mut client,
            &mut out,
            None,
            FrameKind::Error {
                request_id: Some(9),
                code: ErrorCode::Unsupported,
                message: "unknown spawn kind".to_owned(),
            },
            false,
            &mut RepaintAccumulator::default(),
        )
        .await
        .expect("a correlated UNSUPPORTED is not a protocol fault");
    assert!(!matches!(step, FrameStep::Exit(_)));
    assert!(state.mirror.pending_floating.is_empty(), "the spawn settled");
}
