//! Allocation and copy gate for the frames on the live terminal path.
//!
//! Every PTY chunk the server forwards is one `RESOURCE_OUTPUT` encode, and
//! every keystroke is one `INPUT_KEY` encode plus a `FRAME_ACK`-class control
//! frame on some profiles. The server's writer and the client both encode
//! into a reused batch buffer, so a steady-state encode must not touch the
//! allocator at all; decoding a `RESOURCE_OUTPUT` owns its payload with one
//! copy and nothing else. A thread-local counting allocator pins both, so the
//! figures are exact on any machine regardless of load.

#![allow(clippy::unwrap_used, clippy::print_stderr, reason = "measurement test")]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use bytes::{Bytes, BytesMut};
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::wire::frame::FrameKind;
use phux_protocol::ResourceId;

std::thread_local! {
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
    static ALLOC_BYTES: Cell<usize> = const { Cell::new(0) };
}

struct Counting;

// SAFETY: forwards every call to `System` unchanged; the only addition is
// bumping thread-local `Cell`s, which never touch the returned memory.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        note(layout.size());
        // SAFETY: the caller upholds `alloc`'s layout contract.
        unsafe { System.alloc(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        note(new_size);
        // SAFETY: the caller upholds `realloc`'s pointer/layout contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: the caller upholds `dealloc`'s pointer/layout pairing.
        unsafe { System.dealloc(ptr, layout) }
    }
}

fn note(size: usize) {
    if COUNTING.try_with(Cell::get).unwrap_or(false) {
        let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
        let _ = ALLOC_BYTES.try_with(|n| n.set(n.get() + size));
    }
}

#[global_allocator]
static A: Counting = Counting;

/// Allocations and bytes requested while `body` runs on this thread.
fn measure(body: impl FnOnce()) -> (usize, usize) {
    ALLOCS.set(0);
    ALLOC_BYTES.set(0);
    COUNTING.set(true);
    body();
    COUNTING.set(false);
    (ALLOCS.get(), ALLOC_BYTES.get())
}

const fn stream() -> StreamId {
    StreamId::new(3).unwrap()
}

const fn generation() -> BootstrapId {
    BootstrapId::new(9).unwrap()
}

fn output(payload: &Bytes) -> FrameKind {
    FrameKind::ResourceOutput {
        terminal_id: ResourceId::Local { id: 7 },
        stream_id: stream(),
        bootstrap_id: generation(),
        seq: 123_456,
        bytes: payload.clone(),
    }
}

fn hot_frames(payload: &Bytes) -> Vec<(&'static str, FrameKind)> {
    vec![
        ("RESOURCE_OUTPUT", output(payload)),
        (
            "FRAME_ACK",
            FrameKind::FrameAck {
                terminal_id: ResourceId::Local { id: 7 },
                stream_id: stream(),
                bootstrap_id: generation(),
                seq: 123_456,
            },
        ),
        (
            "INPUT_KEY",
            FrameKind::InputKey {
                terminal_id: ResourceId::Local { id: 7 },
                event: KeyEvent {
                    action: KeyAction::Press,
                    key: PhysicalKey::A,
                    mods: ModSet::empty(),
                    consumed_mods: ModSet::empty(),
                    composing: false,
                    text: Some("a".to_owned()),
                    unshifted_codepoint: Some(u32::from('a')),
                },
            },
        ),
        ("PING", FrameKind::Ping { nonce: 42 }),
    ]
}

/// Allocations one steady-state encode of `name` may make: none for the
/// scalar-only frames a PTY flood produces, one scratch buffer for the nested
/// key event (it used to be three).
fn encode_budget(name: &str) -> usize {
    usize::from(name == "INPUT_KEY")
}

/// A steady-state encode into a reused buffer stays inside its budget.
#[test]
fn hot_frames_encode_into_a_reused_buffer_without_allocating() {
    const FRAMES: usize = 64;
    let payload = Bytes::from(vec![b'x'; 4096]);
    let mut buf = BytesMut::with_capacity(64 * 1024);
    let mut over_budget = Vec::new();
    for (name, frame) in hot_frames(&payload) {
        let (allocs, bytes) = measure(|| {
            for _ in 0..FRAMES {
                buf.clear();
                frame.encode(&mut buf);
            }
        });
        eprintln!("encode {name}: {allocs} allocs / {bytes} bytes over {FRAMES} frames");
        if allocs > encode_budget(name) * FRAMES {
            over_budget.push((name, allocs));
        }
    }
    assert!(
        over_budget.is_empty(),
        "encoding allocated past its budget: {over_budget:?}"
    );
}

/// A borrowed slice still copies the payload once. A shared buffer does not:
/// `decode_shared` aliases the frozen frame.
#[test]
fn resource_output_decode_shared_aliases_the_socket_buffer() {
    let payload = Bytes::from(vec![b'x'; 4096]);
    let mut wire = BytesMut::new();
    output(&payload).encode(&mut wire);
    let frame = wire.freeze();
    let (decoded, tail) = FrameKind::decode_shared(&frame, None).unwrap();
    assert!(tail.is_empty());
    let FrameKind::ResourceOutput { bytes, .. } = decoded else {
        panic!("resource output");
    };
    assert_eq!(bytes.as_ref(), payload.as_ref());
    let frame_start = frame.as_ptr() as usize;
    let frame_end = frame_start + frame.len();
    let payload_start = bytes.as_ptr() as usize;
    assert!(
        payload_start >= frame_start && payload_start + bytes.len() <= frame_end,
        "payload must alias the frozen frame"
    );
    let (allocs, allocated) = measure(|| {
        let (again, _) = FrameKind::decode_shared(&frame, None).unwrap();
        std::hint::black_box(again);
    });
    assert_eq!(allocs, 0, "shared decode allocates {allocated} bytes");
}

/// Decoding `RESOURCE_OUTPUT` from a borrowed slice copies the payload out of
/// the transport buffer exactly once and allocates nothing else.
#[test]
fn resource_output_decode_owns_its_payload_with_one_copy() {
    let payload = Bytes::from(vec![b'x'; 4096]);
    let mut wire = BytesMut::new();
    output(&payload).encode(&mut wire);
    let (allocs, bytes) = measure(|| {
        let (frame, tail) = FrameKind::decode(&wire).unwrap();
        assert!(tail.is_empty());
        std::hint::black_box(frame);
    });
    eprintln!("decode RESOURCE_OUTPUT(4096): {allocs} allocs / {bytes} bytes");
    assert_eq!(allocs, 1, "one allocation: the owned payload");
    assert!(
        bytes <= payload.len() + 64,
        "{bytes} bytes allocated for a {}-byte payload",
        payload.len()
    );
}
