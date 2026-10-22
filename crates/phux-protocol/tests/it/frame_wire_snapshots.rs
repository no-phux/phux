//! Snapshot tests for stable protocol frames outside the protocol-0.7
//! bootstrap surface. Bootstrap and handshake codecs use focused semantic
//! round-trip/malformed tests because their negotiated fields are better
//! defended as values than as duplicated hex fixtures.
//!
//! One table-driven test encodes each stable fixture and compares its hex dump
//! with the named golden under `tests/snapshots/`. Fixture names are
//! load-bearing: renaming one or changing its bytes must surface in review.

#![allow(clippy::unwrap_used)]

use bytes::BytesMut;
use phux_protocol::ids::ResourceId;
use phux_protocol::wire::frame::FrameKind;
mod input_metadata;
mod lifecycle;
mod state_journal;

/// Render `bytes` as an `xxd`-style hex dump: 16 cols per row,
/// `OFFSET | HEX HEX HEX ... | ASCII`.
fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    if bytes.is_empty() {
        out.push_str("(empty)\n");
        return out;
    }
    for (chunk_idx, chunk) in bytes.chunks(16).enumerate() {
        let offset = chunk_idx * 16;
        let _ = write!(out, "{offset:08x} |");
        for (i, b) in chunk.iter().enumerate() {
            if i == 8 {
                out.push(' ');
            }
            let _ = write!(out, " {b:02x}");
        }
        let pad_cells = 16 - chunk.len();
        for i in 0..pad_cells {
            if chunk.len() + i == 8 {
                out.push(' ');
            }
            out.push_str("   ");
        }
        out.push_str(" |");
        for b in chunk {
            let c = if (0x20..=0x7e).contains(b) {
                *b as char
            } else {
                '.'
            };
            out.push(c);
        }
        out.push('\n');
    }
    out
}

fn dump_frame(frame: &FrameKind) -> String {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    hex_dump(&buf)
}

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
        let mut buf = BytesMut::new();
        FrameKind::ResizeTerminal {
            terminal_id: ResourceId::local(0x2A),
            cols: 80,
            rows: 24,
            cell_px,
        }
        .encode(&mut buf);
        buf.to_vec()
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
