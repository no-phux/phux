//! Opaque lifecycle records, payload limits, compression envelopes, and agent resume.

use super::{
    BootstrapId, BootstrapLimits, BootstrapStreamProfile, Bytes, BytesMut, DecodeError,
    EngineCodec, FrameKind, ResourceId, StreamId, TYPE_BOOTSTRAP_BEGIN, assert_round_trip,
    bootstrap, encode_without_field, encoded_field_ids, framed, stream, tlv_field,
};

#[test]
fn opaque_lifecycle_records_reencode_byte_identically() {
    let terminal_id = ResourceId::local(44);
    let stream_id = stream(45);
    let bootstrap_id = bootstrap(46);
    let future_record = Bytes::from_static(b"\xff\x80\0future-checkpoint-v255\xfe");
    let cursor = Bytes::from_static(b"\0\xffcursor\x80");
    let next_cursor = Bytes::from_static(b"\xfe\xfdnext\0");
    let frames = [
        FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile: BootstrapStreamProfile::NativeState {
                codec: EngineCodec::LibghosttyCheckpointV2,
            },
            cols: 132,
            rows: 43,
            base_seq: 901,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: future_record.clone(),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: Some(cursor.clone()),
        },
        FrameKind::HistoryRequest {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: cursor.clone(),
            max_bytes: 4096,
            max_rows: 1024,
        },
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq: 1,
            cursor,
            next_cursor: Some(next_cursor),
            payload: future_record,
            rows: 3,
        },
    ];

    for frame in frames {
        let mut encoded = BytesMut::new();
        frame.encode(&mut encoded);
        let (decoded, tail) = FrameKind::decode(&encoded).unwrap();
        assert!(tail.is_empty());
        assert_eq!(decoded, frame);

        let mut reencoded = BytesMut::new();
        decoded.encode(&mut reencoded);
        assert_eq!(reencoded, encoded);
    }
}

#[test]
fn negotiated_response_payload_limits_reject_but_request_budgets_reach_the_host() {
    let limits = BootstrapLimits::new(1024, 2048).unwrap();
    let chunk = FrameKind::BootstrapChunk {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        chunk_seq: 0,
        payload: Bytes::from(vec![0; 1025]),
    };
    let mut encoded = BytesMut::new();
    chunk.encode(&mut encoded);
    assert_eq!(
        FrameKind::decode_with_limits(&encoded, limits).unwrap_err(),
        DecodeError::BootstrapLimitExceeded
    );
    assert!(FrameKind::decode(&encoded).is_ok());

    let page = FrameKind::HistoryPage {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        page_seq: 1,
        cursor: Bytes::from_static(b"cursor"),
        next_cursor: None,
        payload: Bytes::from(vec![0; 2049]),
        rows: 1,
    };
    encoded.clear();
    page.encode(&mut encoded);
    assert_eq!(
        FrameKind::decode_with_limits(&encoded, limits).unwrap_err(),
        DecodeError::BootstrapLimitExceeded
    );
    assert!(FrameKind::decode(&encoded).is_ok());

    let request = FrameKind::HistoryRequest {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        cursor: Bytes::from_static(b"cursor"),
        max_bytes: 2049,
        max_rows: 1024,
    };
    encoded.clear();
    request.encode(&mut encoded);
    let (decoded, tail) = FrameKind::decode_with_limits(&encoded, limits).unwrap();
    assert!(tail.is_empty());
    assert_eq!(decoded, request);
}

#[test]
fn native_live_bytes_are_codec_opaque_and_round_trip_exactly() {
    let raw = Bytes::from_static(b"\x1b]8;;https://example.invalid\x07link\x1b]8;;\x07\xff");
    let frame = FrameKind::ResourceOutput {
        terminal_id: ResourceId::local(8),
        stream_id: stream(8),
        bootstrap_id: bootstrap(8),
        seq: 1,
        bytes: raw.clone(),
    };
    let mut encoded = BytesMut::new();
    frame.encode(&mut encoded);
    let (decoded, _) = FrameKind::decode(&encoded).unwrap();
    let FrameKind::ResourceOutput { bytes, .. } = decoded else {
        panic!("expected terminal output");
    };
    assert_eq!(bytes, raw);
}

/// The envelope is decode-invisible: a wrapped frame decodes to exactly the
/// frame that was wrapped, with a byte-identical opaque payload — the property
/// §6.2 requires of native records across server, transport, and relay.
#[test]
fn compressed_bootstrap_chunk_round_trips_byte_identically() {
    use phux_protocol::caps::Compression;

    // A native page's shape: long structural runs, which is what makes the
    // real bootstrap prefix compressible in the first place.
    let payload: Vec<u8> = (0..8_u32 * 1024)
        .flat_map(|cell| [1, 0, 0, 0, 0, 0, 0, 0, b'a' + (cell % 26) as u8, 0, 0, 0])
        .collect();
    let frame = FrameKind::BootstrapChunk {
        terminal_id: ResourceId::local(7),
        stream_id: StreamId::new(3).expect("stream id"),
        bootstrap_id: BootstrapId::new(11).expect("bootstrap id"),
        chunk_seq: 2,
        payload: bytes::Bytes::from(payload.clone()),
    };

    let mut plain = bytes::BytesMut::new();
    frame.encode(&mut plain);
    let mut scratch = bytes::BytesMut::new();
    let mut wrapped = bytes::BytesMut::new();
    frame.encode_compressed(Compression::Deflate, &mut scratch, &mut wrapped);

    assert!(
        wrapped.len() * 4 < plain.len(),
        "a structural payload must actually shrink: {} vs {}",
        wrapped.len(),
        plain.len()
    );
    assert_eq!(
        wrapped[4],
        phux_protocol::wire::frame::TYPE_FRAME_COMPRESSED,
        "the outer frame is the envelope"
    );

    let (decoded, rest) = FrameKind::decode(&wrapped).expect("decodes");
    assert!(rest.is_empty(), "one frame in, one frame out");
    match decoded {
        FrameKind::BootstrapChunk {
            chunk_seq,
            payload: got,
            ..
        } => {
            assert_eq!(chunk_seq, 2);
            assert_eq!(got.as_ref(), payload.as_slice(), "opaque bytes are exact");
        }
        other => panic!("expected a BootstrapChunk, got {other:?}"),
    }
}

/// A body that cannot pay for the transform is written plain, so the two
/// encoders agree byte-for-byte on everything interactive.
#[test]
fn small_frames_are_never_wrapped() {
    use phux_protocol::caps::Compression;

    let frame = FrameKind::BootstrapChunk {
        terminal_id: ResourceId::local(1),
        stream_id: StreamId::new(1).expect("stream id"),
        bootstrap_id: BootstrapId::new(1).expect("bootstrap id"),
        chunk_seq: 0,
        payload: bytes::Bytes::from_static(b"tiny"),
    };
    let mut plain = bytes::BytesMut::new();
    frame.encode(&mut plain);
    let mut scratch = bytes::BytesMut::new();
    let mut wrapped = bytes::BytesMut::new();
    frame.encode_compressed(Compression::Deflate, &mut scratch, &mut wrapped);
    assert_eq!(plain, wrapped, "a small frame takes the plain path");
}

/// An envelope may not wrap another envelope: unbounded nesting is an
/// amplification vector (each layer multiplies the work a single received
/// frame costs), and there is no legitimate reason to compress twice.
///
/// The encoder never produces one, so the envelope is hand-built here around
/// an inner body whose type byte is `FRAME_COMPRESSED`.
#[test]
fn nested_envelopes_are_rejected() {
    use phux_protocol::wire::encode::Encoder;
    use phux_protocol::wire::field;
    use phux_protocol::wire::frame::TYPE_FRAME_COMPRESSED;

    let mut inner_body = vec![TYPE_FRAME_COMPRESSED];
    inner_body.extend(std::iter::repeat_n(0_u8, 64 * 1024));
    let deflated =
        phux_protocol::wire::compress::deflate(&inner_body).expect("a run of zeros compresses");

    let mut out = BytesMut::new();
    out.extend_from_slice(&[0_u8; 4]);
    let body_start = out.len();
    {
        let mut enc = Encoder::new(&mut out);
        enc.write_u8(TYPE_FRAME_COMPRESSED);
        enc.write_field_with(field::frame_compressed::ALGORITHM, |e| e.write_u8(1));
        enc.write_field_with(field::frame_compressed::UNCOMPRESSED_LEN, |e| {
            e.write_u32_be(u32::try_from(inner_body.len()).unwrap());
        });
        enc.write_field(field::frame_compressed::PAYLOAD, &deflated);
    }
    let body_len = u32::try_from(out.len() - body_start).unwrap();
    out[0..4].copy_from_slice(&body_len.to_be_bytes());

    assert_eq!(
        FrameKind::decode(&out).expect_err("nesting refused"),
        DecodeError::CompressedFrameInvalid,
    );
}

/// A tiny payload may not declare a huge inflated length: the receiver sizes
/// its allocation from the handshake, not from a number the sender picked.
///
/// The bound is the larger negotiated payload limit plus an envelope
/// allowance, so a declaration above that is refused before anything is
/// allocated or inflated. Bounding by the 16 MiB frame cap instead would let a
/// few hundred bytes on the wire cost the receiver 16 MiB of memory per frame.
#[test]
fn an_envelope_may_not_declare_more_than_the_negotiated_bounds() {
    use phux_protocol::wire::encode::Encoder;
    use phux_protocol::wire::field;
    use phux_protocol::wire::frame::TYPE_FRAME_COMPRESSED;

    // A real inner frame body (everything past the length prefix), so the
    // payload is above suspicion in both shape and size and only the declared
    // length lies. A body of raw zeros would fail as an unknown frame type
    // before the bound was ever reached, and would prove nothing.
    let mut framed = BytesMut::new();
    FrameKind::BootstrapChunk {
        terminal_id: ResourceId::local(1),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        chunk_seq: 0,
        payload: Bytes::from(vec![0_u8; 64 * 1024]),
    }
    .encode(&mut framed);
    let body = framed[4..].to_vec();
    let deflated = phux_protocol::wire::compress::deflate(&body).expect("compresses");

    let envelope = |declared: u32| {
        let mut out = BytesMut::new();
        out.extend_from_slice(&[0_u8; 4]);
        let body_start = out.len();
        {
            let mut enc = Encoder::new(&mut out);
            enc.write_u8(TYPE_FRAME_COMPRESSED);
            enc.write_field_with(field::frame_compressed::ALGORITHM, |e| e.write_u8(1));
            enc.write_field_with(field::frame_compressed::UNCOMPRESSED_LEN, |e| {
                e.write_u32_be(declared);
            });
            enc.write_field(field::frame_compressed::PAYLOAD, &deflated);
        }
        let len = u32::try_from(out.len() - body_start).unwrap();
        out[0..4].copy_from_slice(&len.to_be_bytes());
        out
    };

    // Just under the 16 MiB frame cap: legal under the old bound, refused now.
    let greedy = envelope(15 * 1024 * 1024);
    assert_eq!(
        FrameKind::decode(&greedy).expect_err("a greedy declaration is refused"),
        DecodeError::LengthOverflow,
        "an envelope must not be able to name an allocation the connection \
         never negotiated"
    );

    // The honest declaration still decodes, so the bound rejects only liars.
    let honest = envelope(u32::try_from(body.len()).unwrap());
    assert!(
        FrameKind::decode(&honest).is_ok(),
        "a truthful envelope within the negotiated bounds must still decode"
    );
}

#[test]
fn agent_resume_profile_round_trips_field_nine_and_full_profiles_omit_it() {
    let frame = |profile| FrameKind::BootstrapBegin {
        terminal_id: ResourceId::local(7),
        stream_id: stream(1),
        bootstrap_id: bootstrap(2),
        profile,
        cols: 0,
        rows: 0,
        base_seq: 5,
    };
    let full = frame(BootstrapStreamProfile::AgentEventsJsonlV1);
    assert_eq!(encoded_field_ids(&full), vec![1, 2, 3, 4, 5, 6, 7, 8]);
    for after_seq in [0, 3, 5] {
        let resume = frame(BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq });
        assert_round_trip(&resume);
        assert_eq!(encoded_field_ids(&resume), vec![1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(
            FrameKind::decode(&encode_without_field(&resume, 9))
                .unwrap()
                .0,
            full
        );
    }
}

#[test]
fn resume_field_rejects_terminal_profiles_invalid_modes_lengths_and_future_cuts() {
    let with_cursor = |profile, cols, rows, cursor: &[u8], override_mode: Option<u8>| {
        let frame = FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(7),
            stream_id: stream(1),
            bootstrap_id: bootstrap(2),
            profile,
            cols,
            rows,
            base_seq: 5,
        };
        let mut encoded = BytesMut::new();
        frame.encode(&mut encoded);
        let mut fields = encoded[5..].to_vec();
        tlv_field(&mut fields, 9, cursor);
        if let Some(mode) = override_mode {
            tlv_field(&mut fields, 7, &[mode]);
        }
        framed(TYPE_BOOTSTRAP_BEGIN, &fields)
    };
    for profile in [
        BootstrapStreamProfile::SynthesizedVtRaw,
        BootstrapStreamProfile::SynthesizedVtStateSync,
        BootstrapStreamProfile::NativeState {
            codec: EngineCodec::LibghosttyCheckpointV2,
        },
    ] {
        assert_eq!(
            FrameKind::decode(&with_cursor(profile, 80, 24, &3u64.to_be_bytes(), None))
                .unwrap_err(),
            DecodeError::InvalidBootstrapProfile
        );
    }
    for length in [0, 1, 7, 9] {
        assert!(
            FrameKind::decode(&with_cursor(
                BootstrapStreamProfile::AgentEventsJsonlV1,
                0,
                0,
                &vec![0; length],
                None,
            ))
            .is_err()
        );
    }
    for (cols, rows, after_seq, mode) in [(0, 0, 6u64, None), (80, 24, 3, None), (0, 0, 3, Some(1))]
    {
        assert_eq!(
            FrameKind::decode(&with_cursor(
                BootstrapStreamProfile::AgentEventsJsonlV1,
                cols,
                rows,
                &after_seq.to_be_bytes(),
                mode,
            ))
            .unwrap_err(),
            DecodeError::InvalidBootstrapProfile
        );
    }
}
