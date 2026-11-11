//! Parser and codec conformance tests.
#![allow(clippy::expect_used, reason = "tests")]
use super::*;
use phux_protocol::input::{
    focus::FocusEvent,
    mouse::{MouseAction, MouseButton, MouseEvent},
};

fn is_ground(p: &StdinParser) -> bool {
    p.state == State::Ground
}

/// Feed `bytes` to a fresh parser, which must yield exactly one event and
/// end back in the ground state.
fn one_event(bytes: &[u8]) -> InputEvent {
    let mut p = StdinParser::new();
    let mut evs = p.feed(bytes);
    assert_eq!(evs.len(), 1, "input {bytes:?} produced {evs:?}");
    assert!(
        is_ground(&p),
        "input {bytes:?} left the parser mid-sequence"
    );
    evs.remove(0)
}

fn one_key(bytes: &[u8]) -> KeyEvent {
    match one_event(bytes) {
        InputEvent::Key(key) => key,
        other => panic!("input {bytes:?}: expected a key, got {other:?}"),
    }
}

fn one_mouse(bytes: &[u8]) -> MouseEvent {
    match one_event(bytes) {
        InputEvent::Mouse(mouse) => mouse,
        other => panic!("input {bytes:?}: expected a mouse event, got {other:?}"),
    }
}

fn one_paste(bytes: &[u8]) -> PasteEvent {
    match one_event(bytes) {
        InputEvent::Paste(paste) => paste,
        other => panic!("input {bytes:?}: expected a paste, got {other:?}"),
    }
}

mod keys;
mod streams;
