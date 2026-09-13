//! Count allocations on the actual frame encoder with preallocated output.
//!
//! Run with `cargo run --locked -p phux-protocol --example encode_allocations`.
//! Counts are cumulative component allocations, not retained memory or latency.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use bytes::{Bytes, BytesMut};
use phux_protocol::ids::{BootstrapId, ResourceId, StreamId};
use phux_protocol::wire::frame::FrameKind;

struct Counting;
static COUNT: AtomicUsize = AtomicUsize::new(0);

// SAFETY: Allocation and deallocation are delegated unchanged to System.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: The caller supplies the allocator contract's valid layout.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: The pointer and layout are the matching System allocation.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: Counting = Counting;

#[allow(clippy::print_stdout, reason = "standalone measurement report")]
fn main() -> Result<(), &'static str> {
    let terminal_id = ResourceId::local(1);
    let stream_id = StreamId::new(1).ok_or("nonzero stream")?;
    let bootstrap_id = BootstrapId::new(1).ok_or("nonzero bootstrap")?;
    let frames = [
        ("ping", FrameKind::Ping { nonce: 1 }),
        (
            "ack",
            FrameKind::FrameAck {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                seq: 1,
            },
        ),
        (
            "output1k",
            FrameKind::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq: 1,
                bytes: Bytes::from(vec![b'x'; 1024]),
            },
        ),
    ];
    for (name, frame) in frames {
        let mut output = BytesMut::with_capacity(16 * 1024);
        let start = COUNT.load(Ordering::Relaxed);
        for _ in 0..10_000 {
            output.clear();
            frame.encode(&mut output);
            std::hint::black_box(&output);
        }
        let count = COUNT.load(Ordering::Relaxed) - start;
        println!(
            "{name} scratch_allocations_per10000={count} frame_bytes={}",
            output.len()
        );
    }
    Ok(())
}
