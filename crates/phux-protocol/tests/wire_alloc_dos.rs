//! Repro + regression: attacker-controlled count prefixes must not drive
//! pre-allocation disproportionate to the bytes actually present in the frame.
//!
//! A tiny frame (single-digit bytes) that declares a 4-billion-element list
//! pre-`fix` calls `Vec::with_capacity(4e9)`. Depending on the platform that
//! either aborts the process (allocator returns null -> `handle_alloc_error`)
//! or silently reserves tens of GiB of address space. Either way it is a
//! decode-path denial of service: the decoder must reject the frame having
//! reserved no more
//! than the remaining input could justify.
//!
//! The test installs a recording global allocator that captures the single
//! largest allocation request made on this thread while decoding, so the
//! assertion is deterministic across platforms (it does not depend on whether
//! the OS overcommits).

#![allow(clippy::cast_possible_truncation, clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use phux_protocol::caps::BootstrapLimits;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    FrameKind, MAX_HISTORY_CURSOR_BYTES, MAX_HISTORY_PAGE_ROWS,
    MAX_INPUT_TERMINAL_REPLY_BYTES, TYPE_BOOTSTRAP_CHUNK, TYPE_HISTORY_PAGE,
    TYPE_HISTORY_REJECTED, TYPE_HISTORY_TOMBSTONE, TYPE_INPUT_TERMINAL_REPLY,
};

std::thread_local! {
    static MAX_ALLOC: Cell<usize> = const { Cell::new(0) };
    static RECORDING: Cell<bool> = const { Cell::new(false) };
}

struct RecordingAlloc;

// SAFETY: forwards every call straight to `System`; the only added behaviour is
// updating thread-local `Cell`s, which never touch the returned pointer or
// violate the `GlobalAlloc` contract.
unsafe impl GlobalAlloc for RecordingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if RECORDING.try_with(Cell::get).unwrap_or(false) {
            let _ = MAX_ALLOC.try_with(|max| max.set(max.get().max(layout.size())));
        }
        // SAFETY: same layout precondition the caller already upholds.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: `ptr`/`layout` pairing is the caller's responsibility.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static A: RecordingAlloc = RecordingAlloc;

fn largest_alloc_during(decode_input: &[u8]) -> usize {
    MAX_ALLOC.set(0);
    RECORDING.set(true);
    let _ = FrameKind::decode(decode_input);
    RECORDING.set(false);
    MAX_ALLOC.get()
}

fn largest_alloc_during_with_limits(
    decode_input: &[u8],
    limits: BootstrapLimits,
) -> (Result<(), DecodeError>, usize) {
    MAX_ALLOC.set(0);
    RECORDING.set(true);
    let result = FrameKind::decode_with_limits(decode_input, limits).map(|_| ());
    RECORDING.set(false);
    (result, MAX_ALLOC.get())
}

/// Build a frame: 4-byte length header, then body bytes.
fn framed(body: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(4 + body.len());
    buf.extend_from_slice(&(body.len() as u32).to_be_bytes());
    buf.extend_from_slice(body);
    buf
}

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

/// Append one field-tagged TLV field: `field_id || wire_type(4) || len || value`.
fn tlv_field(out: &mut Vec<u8>, field_id: u32, value: &[u8]) {
    put_varint(out, u64::from(field_id));
    out.push(4); // wire_type BYTES
    put_varint(out, value.len() as u64);
    out.extend_from_slice(value);
}

#[test]
fn metadata_keys_huge_count_does_not_over_reserve() {
    // METADATA_KEYS (0xD2): the KEYS field (id 2) value is a positional u32
    // count + strings. Declare count = u32::MAX inside a tiny field.
    let mut keys_value = Vec::new();
    keys_value.extend_from_slice(&u32::MAX.to_be_bytes());
    let mut body = vec![0xD2];
    tlv_field(&mut body, 1, &0u32.to_be_bytes()); // request_id
    tlv_field(&mut body, 2, &keys_value); // keys: huge count, no elements
    let frame = framed(&body);
    let max = largest_alloc_during(&frame);
    assert!(FrameKind::decode(&frame).is_err());
    // A sane decoder reserves on the order of the input, never gigabytes.
    // 1 MiB is a generous ceiling.
    assert!(
        max < 1 << 20,
        "decoder reserved {max} bytes for a {}-byte frame",
        frame.len()
    );
}

#[test]
fn spawn_terminal_huge_command_list_does_not_over_reserve() {
    // SPAWN_TERMINAL (0x22): the COMMAND field (id 3) value is a positional u32
    // count + strings. Declare count = u32::MAX inside a tiny field.
    let mut cmd_value = Vec::new();
    cmd_value.extend_from_slice(&u32::MAX.to_be_bytes());
    let mut body = vec![0x22];
    tlv_field(&mut body, 1, &0u32.to_be_bytes()); // request_id
    tlv_field(&mut body, 2, &1u32.to_be_bytes()); // group
    tlv_field(&mut body, 3, &cmd_value); // command: huge count
    let frame = framed(&body);
    let max = largest_alloc_during(&frame);
    assert!(FrameKind::decode(&frame).is_err());
    assert!(max < 1 << 20, "command-list reserved {max} bytes");
}

#[test]
fn spawn_terminal_huge_env_list_does_not_over_reserve() {
    // SPAWN_TERMINAL env: the ENV field (id 5) value is a positional u32 count
    // + pairs. Declare count = u32::MAX inside a tiny field.
    let mut env_value = Vec::new();
    env_value.extend_from_slice(&u32::MAX.to_be_bytes());
    let mut body = vec![0x22];
    tlv_field(&mut body, 1, &0u32.to_be_bytes()); // request_id
    tlv_field(&mut body, 2, &1u32.to_be_bytes()); // group
    tlv_field(&mut body, 5, &env_value); // env: huge count
    let frame = framed(&body);
    let max = largest_alloc_during(&frame);
    assert!(FrameKind::decode(&frame).is_err());
    assert!(max < 1 << 20, "env-list reserved {max} bytes");
}

#[test]
fn attached_snapshot_huge_sessions_list_does_not_over_reserve() {
    // ATTACHED (0x81): the SNAPSHOT field (id 1) value is a positional
    // SessionSnapshot that starts with a u32 sessions count. Declare
    // count = u32::MAX inside a tiny field.
    let mut snap_value = Vec::new();
    snap_value.extend_from_slice(&u32::MAX.to_be_bytes()); // sessions count
    let mut body = vec![0x81];
    tlv_field(&mut body, 1, &snap_value); // snapshot
    let frame = framed(&body);
    let max = largest_alloc_during(&frame);
    assert!(FrameKind::decode(&frame).is_err());
    assert!(max < 1 << 20, "snapshot sessions reserved {max} bytes");
}

#[test]
fn negotiated_oversize_bootstrap_chunk_rejects_before_payload_allocation() {
    let limits = BootstrapLimits::new(1024, 2048).unwrap();
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    let payload = vec![0xA5; 256 * 1024];
    let mut body = vec![TYPE_BOOTSTRAP_CHUNK];
    tlv_field(&mut body, 1, &terminal);
    tlv_field(&mut body, 2, &1_u64.to_be_bytes());
    tlv_field(&mut body, 3, &1_u64.to_be_bytes());
    tlv_field(&mut body, 4, &0_u32.to_be_bytes());
    tlv_field(&mut body, 5, &payload);
    let frame = framed(&body);

    let (result, max) = largest_alloc_during_with_limits(&frame, limits);
    assert_eq!(result.unwrap_err(), DecodeError::BootstrapLimitExceeded);
    assert_eq!(
        max, 0,
        "oversized borrowed payload must fail before allocation"
    );
}

#[test]
fn malformed_bootstrap_payload_length_rejects_without_reserving() {
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    let mut body = vec![TYPE_BOOTSTRAP_CHUNK];
    tlv_field(&mut body, 1, &terminal);
    tlv_field(&mut body, 2, &1_u64.to_be_bytes());
    tlv_field(&mut body, 3, &1_u64.to_be_bytes());
    tlv_field(&mut body, 4, &0_u32.to_be_bytes());
    put_varint(&mut body, 5);
    body.push(4);
    put_varint(&mut body, u64::MAX);
    let frame = framed(&body);

    let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
    assert!(matches!(
        result,
        Err(DecodeError::LengthOverflow | DecodeError::UnexpectedEof)
    ));
    assert_eq!(
        max, 0,
        "malformed payload length must fail before allocation"
    );
}

#[test]
fn oversized_terminal_reply_rejects_before_owned_bytes_allocation() {
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    let payload = vec![0xA5; MAX_INPUT_TERMINAL_REPLY_BYTES + 1];
    let mut body = vec![TYPE_INPUT_TERMINAL_REPLY];
    tlv_field(&mut body, 1, &terminal);
    tlv_field(&mut body, 2, &payload);
    let frame = framed(&body);

    let (result, max) =
        largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
    assert_eq!(
        result.unwrap_err(),
        DecodeError::InputTerminalReplyLimitExceeded
    );
    assert_eq!(
        max, 0,
        "oversized terminal reply must fail before Bytes allocation"
    );
}

#[test]
fn oversized_history_status_cursors_reject_before_owned_copy() {
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    let cursor = vec![0xA5; MAX_HISTORY_CURSOR_BYTES + 1];
    for type_byte in [TYPE_HISTORY_TOMBSTONE, TYPE_HISTORY_REJECTED] {
        let mut body = vec![type_byte];
        tlv_field(&mut body, 1, &terminal);
        tlv_field(&mut body, 2, &1_u64.to_be_bytes());
        tlv_field(&mut body, 3, &1_u64.to_be_bytes());
        tlv_field(&mut body, 4, &cursor);
        tlv_field(&mut body, 5, &[0]);
        if type_byte == TYPE_HISTORY_REJECTED {
            tlv_field(&mut body, 6, &4096_u32.to_be_bytes());
            tlv_field(&mut body, 7, &256_u32.to_be_bytes());
        }
        let frame = framed(&body);

        let (result, max) =
            largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
        assert_eq!(result.unwrap_err(), DecodeError::BootstrapLimitExceeded);
        assert_eq!(max, 0, "oversized cursor must fail before Bytes allocation");
    }
}

#[test]
fn malformed_history_page_scalars_reject_before_payload_allocation_in_any_field_order() {
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    let payload = vec![0xA5; 256 * 1024];

    for (page_seq, rows, expected) in [
        (None::<u64>, Some(1_u32), DecodeError::UnexpectedEof),
        (
            Some(0_u64),
            Some(1_u32),
            DecodeError::InvalidHistoryPageSequence,
        ),
        (
            Some(1_u64),
            Some(MAX_HISTORY_PAGE_ROWS + 1),
            DecodeError::HistoryRowLimitExceeded,
        ),
    ] {
        let mut body = vec![TYPE_HISTORY_PAGE];
        tlv_field(&mut body, 6, &payload);
        tlv_field(&mut body, 1, &terminal);
        tlv_field(&mut body, 2, &1_u64.to_be_bytes());
        tlv_field(&mut body, 3, &1_u64.to_be_bytes());
        tlv_field(&mut body, 4, b"cursor");
        if let Some(page_seq) = page_seq {
            tlv_field(&mut body, 7, &page_seq.to_be_bytes());
        }
        if let Some(rows) = rows {
            tlv_field(&mut body, 8, &rows.to_be_bytes());
        }
        let frame = framed(&body);

        let (result, max) =
            largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
        assert_eq!(result.unwrap_err(), expected);
        assert_eq!(
            max, 0,
            "malformed page scalar must fail before opaque payload allocation"
        );
    }
}
