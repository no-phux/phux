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

#![allow(clippy::unwrap_used)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use phux_protocol::wire::frame::FrameKind;

mod common;
use common::{framed_tlv, tlv_field};

static MAX_ALLOC: AtomicUsize = AtomicUsize::new(0);
static RECORDING: AtomicUsize = AtomicUsize::new(0);

struct RecordingAlloc;

// SAFETY: forwards every call straight to `System`; the only added behaviour is
// reading `layout.size()` and updating two atomics, neither of which touches
// the returned pointer or violates the `GlobalAlloc` contract.
unsafe impl GlobalAlloc for RecordingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if RECORDING.load(Ordering::Relaxed) == 1 {
            MAX_ALLOC.fetch_max(layout.size(), Ordering::Relaxed);
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
    MAX_ALLOC.store(0, Ordering::Relaxed);
    RECORDING.store(1, Ordering::Relaxed);
    let _ = FrameKind::decode(decode_input);
    RECORDING.store(0, Ordering::Relaxed);
    MAX_ALLOC.load(Ordering::Relaxed)
}

#[test]
fn metadata_keys_huge_count_does_not_over_reserve() {
    // METADATA_KEYS (0xD2): the KEYS field (id 2) value is a positional u32
    // count + strings. Declare count = u32::MAX inside a tiny field.
    let mut keys_value = Vec::new();
    keys_value.extend_from_slice(&u32::MAX.to_be_bytes());
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u32.to_be_bytes()); // request_id
    tlv_field(&mut fields, 2, &keys_value); // keys: huge count, no elements
    let frame = framed_tlv(0xD2, &fields);
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
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u32.to_be_bytes()); // request_id
    tlv_field(&mut fields, 2, &1u32.to_be_bytes()); // group
    tlv_field(&mut fields, 3, &cmd_value); // command: huge count
    let frame = framed_tlv(0x22, &fields);
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
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &0u32.to_be_bytes()); // request_id
    tlv_field(&mut fields, 2, &1u32.to_be_bytes()); // group
    tlv_field(&mut fields, 5, &env_value); // env: huge count
    let frame = framed_tlv(0x22, &fields);
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
    let mut fields = Vec::new();
    tlv_field(&mut fields, 1, &snap_value); // snapshot
    let frame = framed_tlv(0x81, &fields);
    let max = largest_alloc_during(&frame);
    assert!(FrameKind::decode(&frame).is_err());
    assert!(max < 1 << 20, "snapshot sessions reserved {max} bytes");
}
