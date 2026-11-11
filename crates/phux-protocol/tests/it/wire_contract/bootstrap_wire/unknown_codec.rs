//! A codec tag this build does not know is a newer server's kind, not a
//! malformed frame: `BOOTSTRAP_BEGIN` decodes as `Unknown` and the frames
//! after it still parse.

use super::{
    BootstrapStreamProfile, Bytes, BytesMut, FrameKind, ResourceId, TYPE_BOOTSTRAP_BEGIN,
    assert_round_trip, bootstrap, framed, local_terminal, stream, tlv_field,
};

fn unknown_begin(codec: &[u8], cols: u16, rows: u16, output_mode: u8) -> Vec<u8> {
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &local_terminal(9));
    tlv_field(&mut fields, 2, &1u64.to_be_bytes());
    tlv_field(&mut fields, 3, &1u64.to_be_bytes());
    tlv_field(&mut fields, 4, codec);
    tlv_field(&mut fields, 5, &cols.to_be_bytes());
    tlv_field(&mut fields, 6, &rows.to_be_bytes());
    tlv_field(&mut fields, 7, &[output_mode]);
    tlv_field(&mut fields, 8, &4u64.to_be_bytes());
    tlv_field(&mut fields, 99, b"a newer field");
    framed(TYPE_BOOTSTRAP_BEGIN, &fields)
}

#[test]
fn an_unknown_codec_tag_decodes_as_unknown_and_the_next_frame_still_parses() {
    let chunk = FrameKind::BootstrapChunk {
        terminal_id: ResourceId::local(9),
        stream_id: stream(1),
        bootstrap_id: bootstrap(1),
        chunk_seq: 0,
        payload: Bytes::from_static(b"opaque"),
    };
    let mut wire = unknown_begin(&[0x77, 0xAA, 0xBB], 0, 0, 0);
    let mut encoded = BytesMut::new();
    chunk.encode(&mut encoded);
    wire.extend_from_slice(&encoded);

    let (begin, rest) = FrameKind::decode(&wire).unwrap();
    assert_eq!(
        begin,
        FrameKind::BootstrapBegin {
            terminal_id: ResourceId::local(9),
            stream_id: stream(1),
            bootstrap_id: bootstrap(1),
            profile: BootstrapStreamProfile::Unknown { codec_tag: 0x77 },
            cols: 0,
            rows: 0,
            base_seq: 4,
        }
    );
    assert_eq!(FrameKind::decode(rest).unwrap(), (chunk, &[][..]));
}

#[test]
fn an_unknown_codec_keeps_whatever_grid_and_output_mode_it_names() {
    for (cols, rows, output_mode) in [(80, 24, 0), (0, 0, 9), (1, 0, 1)] {
        let (frame, _) = FrameKind::decode(&unknown_begin(&[0x40], cols, rows, output_mode))
            .unwrap_or_else(|error| panic!("{cols}x{rows} mode {output_mode}: {error}"));
        assert!(matches!(
            frame,
            FrameKind::BootstrapBegin {
                profile: BootstrapStreamProfile::Unknown { codec_tag: 0x40 },
                cols: c,
                rows: r,
                ..
            } if (c, r) == (cols, rows)
        ));
    }
}

#[test]
fn an_unknown_profile_round_trips_its_tag() {
    assert_round_trip(&FrameKind::BootstrapBegin {
        terminal_id: ResourceId::local(3),
        stream_id: stream(2),
        bootstrap_id: bootstrap(5),
        profile: BootstrapStreamProfile::Unknown { codec_tag: 200 },
        cols: 0,
        rows: 0,
        base_seq: 0,
    });
}

#[test]
fn a_known_tag_with_an_unknown_engine_codec_is_still_malformed() {
    // Native names an engine codec from the client's own offer.
    assert!(FrameKind::decode(&unknown_begin(&[1, 0x7F], 80, 24, 0)).is_err());
}
