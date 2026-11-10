//! The headless reader keeps the kernel's document-lane rails.

use bytes::Bytes;
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::wire::frame::TombstoneReason;

use super::*;

const MAX_CHUNKS: u32 = 4_096;

fn surface() -> ResourceId {
    ResourceId::local(6)
}

fn ids(bootstrap: u64) -> (StreamId, BootstrapId) {
    (
        StreamId::new(1).unwrap(),
        BootstrapId::new(bootstrap).unwrap(),
    )
}

fn begin(bootstrap: u64, profile: BootstrapStreamProfile) -> FrameKind {
    let (stream_id, bootstrap_id) = ids(bootstrap);
    FrameKind::BootstrapBegin {
        terminal_id: surface(),
        stream_id,
        bootstrap_id,
        profile,
        cols: 0,
        rows: 0,
        base_seq: 3,
    }
}

fn chunk(bootstrap: u64, chunk_seq: u32, payload: &'static [u8]) -> FrameKind {
    let (stream_id, bootstrap_id) = ids(bootstrap);
    FrameKind::BootstrapChunk {
        terminal_id: surface(),
        stream_id,
        bootstrap_id,
        chunk_seq,
        payload: Bytes::from_static(payload),
    }
}

fn ready(bootstrap: u64) -> FrameKind {
    let (stream_id, bootstrap_id) = ids(bootstrap);
    FrameKind::BootstrapReady {
        terminal_id: surface(),
        stream_id,
        bootstrap_id,
        history_cursor: None,
    }
}

fn output(bootstrap: u64, seq: u64) -> FrameKind {
    let (stream_id, bootstrap_id) = ids(bootstrap);
    FrameKind::ResourceOutput {
        terminal_id: surface(),
        stream_id,
        bootstrap_id,
        seq,
        bytes: Bytes::from_static(b"frame"),
    }
}

fn begun() -> Reader {
    let mut reader = Reader::new(surface());
    let begin = begin(1, BootstrapStreamProfile::PixelSurfacePngV1);
    assert!(matches!(reader.absorb(begin).unwrap(), Absorbed::Nothing));
    reader
}

fn published() -> Reader {
    let mut reader = begun();
    assert!(matches!(
        reader.absorb(ready(1)).unwrap(),
        Absorbed::Document(_)
    ));
    reader
}

#[test]
fn chunks_out_of_order_or_of_another_generation_end_the_read() {
    assert!(begun().absorb(chunk(1, 1, b"x")).is_err(), "a gap");
    assert!(
        begun().absorb(chunk(2, 0, b"x")).is_err(),
        "another bootstrap"
    );
    let mut reader = begun();
    reader.absorb(chunk(1, 0, b"x")).unwrap();
    assert!(reader.absorb(chunk(1, 0, b"x")).is_err(), "a duplicate");
}

#[test]
fn staging_past_the_chunk_ceiling_ends_the_read() {
    let mut reader = begun();
    for chunk_seq in 0..MAX_CHUNKS {
        reader.absorb(chunk(1, chunk_seq, b"")).unwrap();
    }
    assert!(reader.absorb(chunk(1, MAX_CHUNKS, b"")).is_err());
}

#[test]
fn stale_or_foreign_live_documents_end_the_read() {
    assert!(
        published().absorb(output(1, 3)).is_err(),
        "not newer than the cut"
    );
    assert!(
        published().absorb(output(2, 4)).is_err(),
        "another bootstrap"
    );
    let mut reader = published();
    let Absorbed::Document(live) = reader.absorb(output(1, 9)).unwrap() else {
        panic!("a newer document");
    };
    assert_eq!(live.seq, 9);
}

#[test]
fn a_tombstoned_generation_is_dropped_until_the_recut_lands() {
    let mut reader = published();
    let (stream_id, bootstrap_id) = ids(1);
    let tombstone = FrameKind::BootstrapTombstone {
        terminal_id: surface(),
        stream_id,
        bootstrap_id,
        reason: TombstoneReason::OutboundGap,
        last_valid_seq: 3,
    };
    assert!(matches!(
        reader.absorb(tombstone).unwrap(),
        Absorbed::Nothing
    ));
    assert!(matches!(
        reader.absorb(output(1, 4)).unwrap(),
        Absorbed::Nothing
    ));
    let recut = begin(2, BootstrapStreamProfile::PixelSurfacePngV1);
    reader.absorb(recut).unwrap();
    reader.absorb(chunk(2, 0, b"png")).unwrap();
    let Absorbed::Document(cut) = reader.absorb(ready(2)).unwrap() else {
        panic!("the recut lands");
    };
    assert_eq!(cut.bytes, b"png");
}

#[test]
fn a_typed_bootstrap_without_a_content_type_or_a_terminal_profile_is_refused() {
    let mut reader = Reader::new(surface());
    reader
        .absorb(begin(1, BootstrapStreamProfile::DocumentTypedV1))
        .unwrap();
    assert!(reader.absorb(ready(1)).is_err());
    let mut reader = Reader::new(surface());
    assert!(
        reader
            .absorb(begin(1, BootstrapStreamProfile::SynthesizedVtRaw))
            .is_err()
    );
}

#[test]
fn a_later_typed_cut_without_its_own_content_type_is_refused() {
    let mut reader = Reader::new(surface());
    reader
        .absorb(begin(1, BootstrapStreamProfile::DocumentTypedV1))
        .unwrap();
    let (stream_id, bootstrap_id) = ids(1);
    let named = phux_protocol::wire::frame::typed_document::encode_bootstrap("text/plain", b"a");
    reader
        .absorb(FrameKind::BootstrapChunk {
            terminal_id: surface(),
            stream_id,
            bootstrap_id,
            chunk_seq: 0,
            payload: named.into(),
        })
        .unwrap();
    assert!(matches!(
        reader.absorb(ready(1)).unwrap(),
        Absorbed::Document(_)
    ));
    reader
        .absorb(begin(2, BootstrapStreamProfile::DocumentTypedV1))
        .unwrap();
    assert!(reader.absorb(ready(2)).is_err(), "no stale content type");
}
