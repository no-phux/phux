//! Snapshot tests for stable protocol frames outside the protocol-0.7
//! bootstrap surface. Bootstrap and handshake codecs use focused semantic
//! round-trip/malformed tests because their negotiated fields are better
//! defended as values than as duplicated hex fixtures.
//!
//! One table-driven test encodes each stable fixture and compares its hex dump
//! with the named golden under `tests/snapshots/`. Fixture names are
//! load-bearing: renaming one or changing its bytes must surface in review.

#![allow(clippy::unwrap_used)]

use crate::common::dump_frame;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::FrameKind;
mod input_metadata;
mod lifecycle;
mod state_journal;

/// Stable frame fixtures in protocol order. Profile-bound output, attach
/// bootstrap, and handshake frames are covered by semantic wire tests.
fn frame_fixtures() -> Vec<(&'static str, FrameKind)> {
    let mut fixtures = input_metadata::fixtures();
    fixtures.extend(lifecycle::fixtures());
    fixtures.extend(state_journal::fixtures());
    fixtures
}

/// ADR-0145: the cell-size fields are appended after `rows`, so a frame
/// without them is byte-identical to the pre-field shape, and a frame that
/// carries only one axis decodes as "no cell size" rather than half of one.
#[test]
fn resize_terminal_cell_px_is_an_appended_optional_pair() {
    let encode = |cell_px| {
        crate::common::encode(&FrameKind::ResizeTerminal {
            terminal_id: ResourceId::local(0x2A),
            cols: 80,
            rows: 24,
            cell_px,
        })
    };
    let without = encode(None);
    let with = encode(Some((9, 18)));
    // Same field prefix (after the length word); the pair is a 10-byte tail.
    assert_eq!(with.len(), without.len() + 10);
    assert_eq!(with[4..without.len()], without[4..]);

    // Drop the trailing CELL_HEIGHT_PX field and fix the length word.
    let mut half = with[..with.len() - 5].to_vec();
    let body_len = u32::try_from(half.len() - 4).unwrap();
    half[..4].copy_from_slice(&body_len.to_be_bytes());
    let (decoded, rest) = FrameKind::decode(&half).unwrap();
    assert!(rest.is_empty());
    assert_eq!(
        decoded,
        FrameKind::ResizeTerminal {
            terminal_id: ResourceId::local(0x2A),
            cols: 80,
            rows: 24,
            cell_px: None,
        }
    );
}

/// Every fixture's hex dump matches its committed golden byte-for-byte.
#[test]
fn frame_wire_snapshots_match_goldens() {
    for (name, frame) in frame_fixtures() {
        insta::assert_snapshot!(name, dump_frame(&frame));
    }
}
