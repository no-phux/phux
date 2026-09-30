//! The connected lane's tick: feed what the driver read, then publish.
//!
//! Every frame still walks the per-frame hooks `phux_client_feed_frame`
//! walks (epoch fence, retired-close suppression, profile validation,
//! agent-generation tracking) on the owning thread. What changes is how
//! terminal output reaches the engine: a contiguous run of `RESOURCE_OUTPUT`
//! frames is fed as one batch, so a flood costs one owner-thread apply and
//! one grid publication per run instead of one per frame. Any other frame
//! ends the run first, so the engine sees frames in wire order.

use std::mem;

use phux_client_runtime::control::ControlError;
use phux_protocol::wire::frame::FrameKind;

use super::client::Client;
use super::error::BridgeError;
use super::{
    apply_server_frame, control_error, decode_server_frame, follow_negotiated_session,
    validate_runtime_frame,
};

/// The most frames one output run carries into one engine apply. It matches
/// the driver's per-read ceiling, so a run is at most a read's worth.
const RUN_MAX_FRAMES: usize = 256;
/// The most encoded bytes one output run carries. A full inbound queue can
/// hold 32 MiB; cutting runs here bounds one owner-thread round trip (and
/// the owning thread's wait on it) to about a mebibyte of VT, and publishes
/// progress at least that often.
const RUN_MAX_BYTES: usize = 1024 * 1024;

/// Drain one snapshot of the driver's queue and feed it.
///
/// The snapshot is the fairness bound: frames the driver reads while this
/// runs wait for the next poll, so one call never outlasts the inbound
/// queue's ceiling (128 frames and 32 MiB, plus one read) however fast the
/// server floods, and the embedder's event loop gets its turn in between.
///
/// Drain lifecycle events after taking the batch but before decoding it: a
/// loss and the replacement socket's `HELLO_OK` may have accumulated between
/// polls, and old subscription tombstones must not filter frames from that
/// replacement connection.
pub(crate) fn poll_connected(client: &mut Client) -> Result<bool, BridgeError> {
    let (epoch, frames) = client.take_inbound();
    let mut attached = client.process_runtime_events()?;
    let mut run = OutputRun::default();
    let fed = feed_snapshot(client, epoch, frames, &mut run, &mut attached);
    // Frames already in the run precede whatever stopped the loop, so they
    // are applied, and any error of theirs outranks it.
    attached |= run.flush(client, epoch)?;
    fed?;
    attached |= client.process_runtime_events()?;
    Ok(attached)
}

fn feed_snapshot(
    client: &mut Client,
    epoch: u64,
    frames: Vec<bytes::Bytes>,
    run: &mut OutputRun,
    attached: &mut bool,
) -> Result<(), BridgeError> {
    for bytes in frames {
        let Some(frame) = decode_queued(client, epoch, &bytes)? else {
            break;
        };
        if joins_output_run(client, &frame) {
            validate_runtime_frame(client, &frame)?;
            if run.push(frame, bytes.len()) {
                *attached |= run.flush(client, epoch)?;
            }
            continue;
        }
        *attached |= run.flush(client, epoch)?;
        *attached |= apply_server_frame(client, frame, Some(epoch))?;
    }
    Ok(())
}

/// Decode one queued frame, or `None` once its connection has ended. The
/// epoch is checked again under the guard that applies the frame.
fn decode_queued(
    client: &Client,
    epoch: u64,
    bytes: &[u8],
) -> Result<Option<FrameKind>, BridgeError> {
    if !client.control().accepts_inbound(epoch) {
        return Ok(None);
    }
    decode_server_frame(client, bytes).map(Some)
}

/// Terminal output whose only per-frame hook is participant validation.
/// Agent streams keep the per-frame path: their records drive workspace
/// projections that later frames' validation may read.
fn joins_output_run(client: &Client, frame: &FrameKind) -> bool {
    matches!(
        frame,
        FrameKind::ResourceOutput { terminal_id, .. } if !client.is_agent_stream(terminal_id)
    )
}

/// Validated output frames waiting to be applied as one engine batch.
#[derive(Default)]
struct OutputRun {
    frames: Vec<FrameKind>,
    bytes: usize,
}

impl OutputRun {
    /// Add one frame of `len` encoded bytes; true once the run is full.
    fn push(&mut self, frame: FrameKind, len: usize) -> bool {
        self.frames.push(frame);
        self.bytes = self.bytes.saturating_add(len);
        self.frames.len() >= RUN_MAX_FRAMES || self.bytes >= RUN_MAX_BYTES
    }

    /// Apply the run through one control-plane batch, then drain the events
    /// it raised, exactly as `apply_server_frame` does for one frame. A run
    /// whose connection ended meanwhile is dropped whole: every frame in it
    /// came from that connection.
    fn flush(&mut self, client: &mut Client, epoch: u64) -> Result<bool, BridgeError> {
        if self.frames.is_empty() {
            return Ok(false);
        }
        self.bytes = 0;
        let frames = mem::take(&mut self.frames);
        let runtime_result = {
            let mut control = client.control();
            if !control.accepts_inbound(epoch) {
                return Ok(false);
            }
            let result = control.feed_batch(frames);
            if let Err(ControlError::Refused(message)) = &result {
                control.fail(message.clone());
            }
            drop(control);
            result.map_err(control_error)
        };
        let attached = client.process_runtime_events()?;
        runtime_result?;
        follow_negotiated_session(client)?;
        Ok(attached)
    }
}

#[cfg(test)]
mod tests {
    use super::{RUN_MAX_FRAMES, poll_connected};
    use crate::c::test_support;
    use crate::c::*;
    use phux_protocol::ResourceId;
    use phux_protocol::ids::{BootstrapId, StreamId};

    fn output(terminal: u32, seq: u64, text: &str) -> Vec<u8> {
        let mut encoded = bytes::BytesMut::new();
        FrameKind::ResourceOutput {
            terminal_id: ResourceId::local(terminal),
            stream_id: StreamId::new(1).expect("stream"),
            bootstrap_id: BootstrapId::new(1).expect("generation"),
            seq,
            bytes: text.as_bytes().to_vec().into(),
        }
        .encode(&mut encoded);
        encoded.to_vec()
    }

    fn encode(frame: &FrameKind) -> Vec<u8> {
        let mut encoded = bytes::BytesMut::new();
        frame.encode(&mut encoded);
        encoded.to_vec()
    }

    fn attached() -> Box<PhuxClient> {
        // SAFETY: the fixture hands back a uniquely owned heap client.
        unsafe { Box::from_raw(test_support::attached_client(&[])) }
    }

    fn queue(client: &PhuxClient, frames: Vec<Vec<u8>>) {
        client
            .inner
            .control()
            .queue_inbound_batch(frames.into_iter().map(Into::into).collect())
            .expect("room for the read");
    }

    fn generation(client: &PhuxClient) -> u64 {
        client
            .inner
            .control()
            .publication()
            .generation(&ResourceId::local(1))
            .expect("the bootstrapped terminal is published")
    }

    fn text(client: &PhuxClient) -> String {
        client
            .inner
            .projection(&ResourceId::local(1))
            .expect("published")
            .text()
    }

    /// Where `needle` sits in the grid; the grid's order is wire order.
    fn at(client: &PhuxClient, needle: &str) -> usize {
        let text = text(client);
        text.find(needle)
            .unwrap_or_else(|| panic!("{needle:?} missing from {text:?}"))
    }

    /// A multi-frame read is one engine batch and one publication, not one
    /// per frame; the grid still ends where frame-by-frame feeding would.
    #[test]
    fn a_multi_frame_read_publishes_once() {
        let mut client = attached();
        let before = generation(&client);
        queue(
            &client,
            (1..=8)
                .map(|seq| output(1, seq, &format!("\r\nline {seq}")))
                .collect(),
        );
        poll_connected(&mut client.inner).expect("poll");
        assert_eq!(generation(&client) - before, 1, "one publication per read");
        assert!(at(&client, "line 1") < at(&client, "line 8"));
    }

    /// A frame with its own hooks ends the run before it, so the engine and
    /// the projections see wire order: output, bell, output is two batches.
    #[test]
    fn a_non_output_frame_splits_the_run_in_wire_order() {
        let mut client = attached();
        let before = generation(&client);
        queue(
            &client,
            vec![
                output(1, 1, "\r\nfirst"),
                encode(&FrameKind::Bell {
                    terminal_id: ResourceId::local(1),
                }),
                output(1, 2, "\r\nsecond"),
            ],
        );
        poll_connected(&mut client.inner).expect("poll");
        assert_eq!(generation(&client) - before, 2);
        assert!(at(&client, "first") < at(&client, "second"));
    }

    /// A read longer than one run's frame ceiling is cut, not refused.
    #[test]
    fn a_run_is_cut_at_its_frame_ceiling() {
        let mut client = attached();
        let before = generation(&client);
        let count = u64::try_from(RUN_MAX_FRAMES).expect("fits") + 1;
        queue(
            &client,
            (1..=count)
                .map(|seq| output(1, seq, &format!("\r\n{seq}")))
                .collect(),
        );
        poll_connected(&mut client.inner).expect("poll");
        assert_eq!(generation(&client) - before, 2);
        assert!(text(&client).contains(&format!("\n{count}")));
    }

    /// A frame that fails validation mid-read still lets the output before
    /// it land, as frame-by-frame feeding did, and fails the poll.
    #[test]
    fn output_before_an_invalid_frame_is_applied() {
        let mut client = attached();
        queue(
            &client,
            vec![
                output(1, 1, "\r\napplied"),
                output(1, 2, "\r\nalso applied"),
                output(99, 1, "\r\noutside the attach"),
                output(1, 3, "\r\nnever"),
            ],
        );
        assert!(poll_connected(&mut client.inner).is_err());
        assert!(at(&client, "applied") < at(&client, "also applied"));
        assert!(!text(&client).contains("never"));
    }
}
