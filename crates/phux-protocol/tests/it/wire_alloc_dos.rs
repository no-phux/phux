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
//! The binary's global allocator (`crate::alloc_probe`) captures the single
//! largest allocation request made on this thread while decoding, so the
//! assertion is deterministic across platforms (it does not depend on whether
//! the OS overcommits).

#![allow(clippy::unwrap_used)]

use std::cell::Cell;

use phux_protocol::caps::BootstrapLimits;
use phux_protocol::wire::DecodeError;
use phux_protocol::wire::frame::{
    FrameKind, MAX_APPEND_BYTES, MAX_HISTORY_CURSOR_BYTES, MAX_HISTORY_PAGE_ROWS,
    MAX_INPUT_TERMINAL_REPLY_BYTES, TYPE_BOOTSTRAP_CHUNK, TYPE_COMMAND, TYPE_HISTORY_PAGE,
    TYPE_HISTORY_REJECTED, TYPE_HISTORY_TOMBSTONE, TYPE_INPUT_TERMINAL_REPLY,
};

use crate::common::{framed_tlv, put_varint, tlv_field};

fn framed(body: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&u32::try_from(body.len()).unwrap().to_be_bytes());
    frame.extend_from_slice(body);
    frame
}

std::thread_local! {
    static MAX_ALLOC: Cell<usize> = const { Cell::new(0) };
    static RECORDING: Cell<bool> = const { Cell::new(false) };
}

/// Record an allocation request of `size` bytes while a decode is being
/// measured. The binary's global allocator (`crate::alloc_probe`) calls this.
#[expect(
    clippy::redundant_pub_crate,
    reason = "`pub` alone trips `unreachable_pub`; the allocator in `main.rs` calls this"
)]
pub(crate) fn record(size: usize) {
    if RECORDING.try_with(Cell::get).unwrap_or(false) {
        let _ = MAX_ALLOC.try_with(|max| max.set(max.get().max(size)));
    }
}

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

/// A decoder that trusts a declared count reserves gigabytes for a frame that
/// carries none of the promised elements. 1 MiB is a generous ceiling for
/// anything these fixtures legitimately need.
const ALLOC_CEILING: usize = 1 << 20;

/// Assert a hostile frame both fails to decode and never reserves on the order
/// of its declared count rather than its actual size.
///
/// The cases differ only in which field carries the bogus `u32::MAX`.
fn assert_bounded_alloc(frame: &[u8], what: &str) {
    let max = largest_alloc_during(frame);
    assert!(
        FrameKind::decode(frame).is_err(),
        "{what}: a frame declaring u32::MAX elements must not decode"
    );
    assert!(
        max < ALLOC_CEILING,
        "{what}: decoder reserved {max} bytes for a {}-byte frame",
        frame.len()
    );
}

/// A hostile frame of `type_byte` whose `fields` precede `count_field`, a
/// positional list value that declares `u32::MAX` elements, then `filler`
/// bytes of `0xFF`, so the first element fails to decode.
fn huge_count_frame(
    type_byte: u8,
    fields: &[(u32, &[u8])],
    count_field: u32,
    filler: usize,
) -> Vec<u8> {
    let mut value = u32::MAX.to_be_bytes().to_vec();
    value.resize(4 + filler, 0xFF);
    let mut body = Vec::new();
    for (id, bytes) in fields {
        tlv_field(&mut body, *id, bytes);
    }
    tlv_field(&mut body, count_field, &value);
    framed_tlv(type_byte, &body)
}

const REQUEST_ID: [u8; 4] = 0u32.to_be_bytes();
const GROUP_1: [u8; 4] = 1u32.to_be_bytes();

/// Every positional list a hostile peer can inflate: (label, type byte,
/// fields before the list, list field id).
type HugeCountCase = (&'static str, u8, &'static [(u32, &'static [u8])], u32);

const HUGE_COUNT_CASES: &[HugeCountCase] = &[
    // METADATA_KEYS (0xD2): KEYS (id 2) is a u32 count + strings.
    ("metadata keys", 0xD2, &[(1, &REQUEST_ID)], 2),
    // DIRECTORY_LISTING (0xD3): ENTRIES (id 4) is a u32 count + (name, flags).
    (
        "directory listing entries",
        0xD3,
        &[(1, &REQUEST_ID), (2, b"/")],
        4,
    ),
    // SPAWN_RESOURCE (0x22): COMMAND (id 3) is a u32 count + strings.
    (
        "spawn command list",
        0x22,
        &[(1, &REQUEST_ID), (2, &GROUP_1)],
        3,
    ),
    // SPAWN_RESOURCE: ENV (id 5) is a u32 count + pairs.
    (
        "spawn env list",
        0x22,
        &[(1, &REQUEST_ID), (2, &GROUP_1)],
        5,
    ),
    // ATTACHED (0x81): SNAPSHOT (id 1) starts with a u32 sessions count.
    ("attached sessions list", 0x81, &[], 1),
];

#[test]
fn huge_declared_counts_do_not_over_reserve() {
    for &(what, type_byte, fields, count_field) in HUGE_COUNT_CASES {
        assert_bounded_alloc(&huge_count_frame(type_byte, fields, count_field, 0), what);
    }
}

/// `ResourceId::local(1)` as its positional wire bytes.
fn terminal_local1() -> [u8; 5] {
    let mut terminal = [0_u8; 5];
    terminal[1..].copy_from_slice(&1_u32.to_be_bytes());
    terminal
}

#[test]
fn negotiated_oversize_bootstrap_chunk_rejects_before_payload_allocation() {
    let limits = BootstrapLimits::new(1024, 2048).unwrap();
    let terminal = terminal_local1();
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
    let terminal = terminal_local1();
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
    let terminal = terminal_local1();
    let payload = vec![0xA5; MAX_INPUT_TERMINAL_REPLY_BYTES + 1];
    let mut body = vec![TYPE_INPUT_TERMINAL_REPLY];
    tlv_field(&mut body, 1, &terminal);
    tlv_field(&mut body, 2, &payload);
    let frame = framed(&body);

    let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
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
fn oversized_append_resource_output_rejects_before_owned_bytes_allocation() {
    // COMMAND (0x31) carrying APPEND_RESOURCE_OUTPUT (tag 0x1a): the payload
    // is a u32-prefixed leaf `bytes` inside the positional command value. One
    // byte over the cap must be refused before the Vec copy is made.
    let mut command = vec![0x1au8];
    command.push(0); // ResourceId::Local tag
    command.extend_from_slice(&1_u32.to_be_bytes());
    let payload_len = MAX_APPEND_BYTES + 1;
    command.extend_from_slice(&u32::try_from(payload_len).unwrap().to_be_bytes());
    command.extend(std::iter::repeat_n(0xA5u8, payload_len));
    let mut body = vec![TYPE_COMMAND];
    tlv_field(&mut body, 1, &5_u32.to_be_bytes());
    tlv_field(&mut body, 2, &command);
    let frame = framed(&body);

    let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
    assert_eq!(
        result.unwrap_err(),
        DecodeError::AppendResourceOutputLimitExceeded
    );
    assert_eq!(
        max, 0,
        "oversized append must fail before the owned bytes allocation"
    );
}

#[test]
fn append_resource_output_declared_length_past_frame_rejects_without_reserving() {
    // A u32 length prefix far larger than the bytes present must error on the
    // leaf read (LengthOverflow / EOF), never reserve.
    let mut command = vec![0x1au8];
    command.push(0);
    command.extend_from_slice(&1_u32.to_be_bytes());
    command.extend_from_slice(&u32::MAX.to_be_bytes());
    command.extend_from_slice(b"abc");
    let mut body = vec![TYPE_COMMAND];
    tlv_field(&mut body, 1, &5_u32.to_be_bytes());
    tlv_field(&mut body, 2, &command);
    let frame = framed(&body);

    let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
    assert!(matches!(
        result,
        Err(DecodeError::LengthOverflow | DecodeError::UnexpectedEof)
    ));
    assert!(
        max < ALLOC_CEILING,
        "declared-length append reserved {max} bytes"
    );
}

#[test]
fn snapshot_huge_resource_facet_count_does_not_over_reserve() {
    // ATTACHED (0x81): a well-formed empty snapshot followed by a trailing
    // resource-facet list declaring u32::MAX rows and carrying none.
    let mut snap = Vec::new();
    snap.extend_from_slice(&0_u32.to_be_bytes()); // sessions
    snap.extend_from_slice(&0_u32.to_be_bytes()); // windows
    snap.extend_from_slice(&0_u32.to_be_bytes()); // panes
    snap.extend_from_slice(&1_u32.to_be_bytes()); // focused_session
    snap.extend_from_slice(&1_u32.to_be_bytes()); // focused_window
    snap.push(0); // focused_resource tag local
    snap.extend_from_slice(&1_u32.to_be_bytes());
    snap.extend_from_slice(&u32::MAX.to_be_bytes()); // facet rows: huge
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap);
    tlv_field(&mut fields, 2, &7_u32.to_be_bytes());
    tlv_field(&mut fields, 3, &1_u32.to_be_bytes());
    assert_bounded_alloc(&framed_tlv(0x81, &fields), "snapshot facet rows");
}

#[test]
fn oversized_history_status_cursors_reject_before_owned_copy() {
    let terminal = terminal_local1();
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

        let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
        assert_eq!(result.unwrap_err(), DecodeError::BootstrapLimitExceeded);
        assert_eq!(max, 0, "oversized cursor must fail before Bytes allocation");
    }
}

#[test]
fn malformed_history_page_scalars_reject_before_payload_allocation_in_any_field_order() {
    let terminal = terminal_local1();
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

        let (result, max) = largest_alloc_during_with_limits(&frame, BootstrapLimits::default());
        assert_eq!(result.unwrap_err(), expected);
        assert_eq!(
            max, 0,
            "malformed page scalar must fail before opaque payload allocation"
        );
    }
}

/// Filler that makes a hostile frame large enough that "at most one element
/// per remaining byte" is still an amplification: the reservation must be
/// bounded by the input's bytes, not its byte count times the element size.
/// `0xFF` makes the first element fail to decode, so only the up-front
/// reservation is measured.
const FILLER: usize = 256 * 1024;

/// Assert a hostile frame never reserves more than the bytes it carries.
fn assert_alloc_within_input(frame: &[u8], what: &str) {
    let max = largest_alloc_during(frame);
    assert!(
        max <= frame.len(),
        "{what}: decoder reserved {max} bytes for a {}-byte frame",
        frame.len()
    );
}

#[test]
fn padded_huge_counts_reserve_no_more_than_the_input() {
    // The attached sessions list and both spawn lists; env pairs are the
    // widest string element.
    for &(what, type_byte, fields, count_field) in &HUGE_COUNT_CASES[2..] {
        assert_alloc_within_input(
            &huge_count_frame(type_byte, fields, count_field, FILLER),
            what,
        );
    }
}

/// The organization bounds (`docs/organization.md`) are enforced at decode:
/// a name over 256 bytes, an arrangement over 64 KiB, a bulk move or undo
/// over 256 items, an attribute list over 64, and a cursor over 4 KiB are
/// each refused, and a hostile arrangement group count reserves no more
/// than the bytes present.
#[allow(clippy::missing_const_for_fn, reason = "test helpers")]
mod organization_bounds;
