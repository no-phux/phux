//! Owner-thread perf counters, measured as deltas around fed frames.
//!
//! These counters (`phux_client_runtime::perf`) are process-wide statics, so
//! a delta is only this test's if nothing else in the process feeds an
//! engine. That is why this is its own test binary: `cargo test` (the
//! `crate-check` and `test-cargo` lanes) runs a binary's tests as threads of
//! ONE process, and inside `control_plane` every other embedded-engine test
//! bumped the same counters mid-measurement (phux-pzni). nextest's
//! process-per-test model hid that; a binary holding only counter-delta
//! tests is isolated under either runner. Add a test here only if it reads
//! counter deltas, and never a test that does not.

#![cfg(feature = "engine")]
#![allow(clippy::expect_used, reason = "test assertions")]
#![allow(clippy::unwrap_used, reason = "test assertions")]
#![allow(clippy::panic, reason = "test assertions")]

use bytes::BytesMut;
use phux_protocol::ids::{BootstrapId, StreamId};
use phux_protocol::wire::frame::FrameKind;

#[path = "support/embedded.rs"]
mod embedded;
use embedded::*;

/// The delivery shape sets the owner-thread cost, and the `runtime.*`
/// counters show it: a transport read fed as one batch is one apply round
/// trip and one grid publication; the same frames fed one at a time are one
/// round trip each, and publish per frame only for a consumer that reads
/// every frame. Unread, they cost one projection, pulled by the next read.
#[test]
fn a_batched_read_publishes_once_and_frame_at_a_time_publishes_per_read() {
    use phux_client_runtime::perf::{ACQUIRED, APPLY_BATCHES, CAUGHT_UP, DEFERRED, PUBLISHED};
    const FRAMES: u64 = 8;
    let client = embedded_with_history(None);
    let frame = |seq: u64| {
        let mut encoded = BytesMut::new();
        FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: StreamId::new(1).unwrap(),
            bootstrap_id: BootstrapId::new(1).unwrap(),
            seq,
            bytes: format!("\r\nline {seq}").into_bytes().into(),
        }
        .encode(&mut encoded);
        encoded.to_vec()
    };

    let _ = client.acquire(&terminal()).unwrap();
    let (batches, published) = (APPLY_BATCHES.get(), PUBLISHED.get());
    let read: Vec<_> = (1..=FRAMES).map(frame).collect();
    client
        .with_control(|plane| plane.feed_bytes_batch(&read))
        .unwrap();
    assert_eq!(APPLY_BATCHES.get() - batches, 1, "one round trip per read");
    assert_eq!(PUBLISHED.get() - published, 1, "one publication per read");

    // A consumer that reads every frame is published every frame.
    let (batches, published) = (APPLY_BATCHES.get(), PUBLISHED.get());
    for seq in FRAMES + 1..=2 * FRAMES {
        let _ = client.acquire(&terminal()).unwrap();
        client
            .with_control(|plane| plane.feed_bytes(&frame(seq)))
            .unwrap();
    }
    assert_eq!(APPLY_BATCHES.get() - batches, FRAMES);
    assert_eq!(PUBLISHED.get() - published, FRAMES);

    // Nobody reads: every frame after the first unread one is deferred, and
    // the read that follows pulls one projection of the final state.
    let _ = client.acquire(&terminal()).unwrap();
    let (batches, published, deferred, acquired) = (
        APPLY_BATCHES.get(),
        PUBLISHED.get(),
        DEFERRED.get(),
        ACQUIRED.get(),
    );
    for seq in 2 * FRAMES + 1..=3 * FRAMES {
        client
            .with_control(|plane| plane.feed_bytes(&frame(seq)))
            .unwrap();
    }
    assert_eq!(APPLY_BATCHES.get() - batches, FRAMES);
    assert_eq!(PUBLISHED.get() - published, 1, "only the first frame");
    assert_eq!(DEFERRED.get() - deferred, FRAMES - 1);
    let caught_up = CAUGHT_UP.get();
    assert!(
        client
            .acquire(&terminal())
            .unwrap()
            .row_text(3)
            .starts_with("line 24")
    );
    assert_eq!(CAUGHT_UP.get() - caught_up, 1);
    assert_eq!(PUBLISHED.get() - published, 2, "the read pulled one more");
    assert_eq!(ACQUIRED.get() - acquired, 1);
}
