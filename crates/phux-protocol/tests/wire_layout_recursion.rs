//! Repro + regression: deeply-nested `LayoutNode::Split` recursion.

#![allow(clippy::cast_possible_truncation)]

use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::FrameKind;

/// Append an unsigned LEB128 varint.
fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let byte = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(byte);
            break;
        }
        out.push(byte | 0x80);
    }
}

/// Append one TLV field: `field_id || wire_type(4 = BYTES) || len || value`.
fn tlv_field(out: &mut Vec<u8>, field_id: u32, value: &[u8]) {
    put_varint(out, u64::from(field_id));
    out.push(4);
    put_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

fn attached_with_layout(layout: &[u8]) -> Vec<u8> {
    let mut win = Vec::new();
    win.extend_from_slice(&1u32.to_be_bytes()); // window id
    win.extend_from_slice(&1u32.to_be_bytes()); // session id
    win.extend_from_slice(&0u16.to_be_bytes()); // index
    win.extend_from_slice(&0u32.to_be_bytes()); // name len 0
    win.push(0); // active_pane None
    win.push(1); // layout Some
    win.extend_from_slice(layout);

    // Positional SessionSnapshot (the value of the ATTACHED SNAPSHOT field
    // under field-tagged TLV — the snapshot itself stays positional).
    let mut snap = Vec::new();
    snap.extend_from_slice(&0u32.to_be_bytes()); // sessions 0
    snap.extend_from_slice(&1u32.to_be_bytes()); // windows 1
    snap.extend_from_slice(&win);
    snap.extend_from_slice(&0u32.to_be_bytes()); // panes 0
    snap.extend_from_slice(&1u32.to_be_bytes()); // focused_session
    snap.extend_from_slice(&1u32.to_be_bytes()); // focused_window
    snap.push(0); // focused_pane tag local
    snap.extend_from_slice(&1u32.to_be_bytes());

    // Field-tagged ATTACHED body: SNAPSHOT (id 1), INITIAL_CLIENT_ID (id 2),
    // and protocol-0.7 attach correlation id (id 3).
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap);
    tlv_field(&mut fields, 2, &7u32.to_be_bytes());
    tlv_field(&mut fields, 3, &1u32.to_be_bytes());

    let mut body = vec![0x81u8];
    body.extend_from_slice(&fields);

    let mut frame = Vec::new();
    frame.extend_from_slice(&(body.len() as u32).to_be_bytes());
    frame.extend_from_slice(&body);
    frame
}

/// Build a left-leaning split chain `depth` deep, with leaves for every child.
fn split_chain(depth: usize) -> Vec<u8> {
    let half = 0.5f32.to_be_bytes();
    let mut layout = Vec::new();
    for _ in 0..depth {
        layout.push(1u8); // LAYOUT_TAG_SPLIT
        layout.push(0u8); // SPLIT_DIR_HORIZONTAL
        layout.extend_from_slice(&half);
    }
    // innermost left leaf
    layout.push(0u8);
    layout.push(0u8);
    layout.extend_from_slice(&1u32.to_be_bytes());
    // a right leaf per split
    for _ in 0..depth {
        layout.push(0u8);
        layout.push(0u8);
        layout.extend_from_slice(&1u32.to_be_bytes());
    }
    layout
}

#[test]
fn deeply_nested_layout_errors_not_overflows() {
    // Far beyond MAX_LAYOUT_DEPTH (64). Pre-fix: SIGABRT via stack overflow.
    // Post-fix: clean LayoutTooDeep error.
    let frame = attached_with_layout(&split_chain(100_000));
    let err = FrameKind::decode(&frame).expect_err("must reject deep layout");
    assert_eq!(err, DecodeError::LayoutTooDeep);
}

#[test]
fn shallow_layout_still_round_trips() {
    // A 4-deep split chain (within MAX_LAYOUT_DEPTH) must still decode OK.
    let frame = attached_with_layout(&split_chain(4));
    assert!(FrameKind::decode(&frame).is_ok());
}
