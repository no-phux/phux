//! Repro + regression: deeply-nested `LayoutNode::Split` recursion.

use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::FrameKind;

mod common;
use common::attached_with_layout;

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
