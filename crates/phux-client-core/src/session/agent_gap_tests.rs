//! A server whose lagged consumer fell behind evicted records tombstones the
//! live `AgentSession` generation with `OUTBOUND_GAP`, then publishes the full
//! ring on the same stream under a new bootstrap id.

#![allow(clippy::unwrap_used, reason = "tests")]

use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{BootstrapId, BootstrapProfile, BootstrapStreamProfile, ResourceId, StreamId};

use super::agent_stream::AgentRecordMode;
use super::kernel_rig::RigAdapter;
use super::{AgentStreamInfo, EffectBuffer, KernelEffect, KernelInput, SessionKernel};
use crate::engine::CanonicalGeometry;

fn agent() -> ResourceId {
    ResourceId::local(2)
}

fn stream() -> StreamId {
    StreamId::new(3).unwrap()
}

fn generation(raw: u64) -> BootstrapId {
    BootstrapId::new(raw).unwrap()
}

fn lines(seqs: std::ops::RangeInclusive<u64>) -> String {
    use std::fmt::Write as _;
    let mut out = String::new();
    for seq in seqs {
        writeln!(
            out,
            r#"{{"seq":{seq},"ts_ms":1,"type":"stop","data":{{}}}}"#
        )
        .unwrap();
    }
    out
}

/// Publish a full agent generation at `base_seq` carrying `records`.
fn publish(
    kernel: &mut SessionKernel<RigAdapter>,
    bootstrap_id: BootstrapId,
    base_seq: u64,
    records: &str,
    effects: &mut EffectBuffer,
) {
    let agent = agent();
    kernel
        .update(
            KernelInput::BootstrapBegin {
                terminal_id: &agent,
                stream_id: stream(),
                bootstrap_id,
                profile: BootstrapStreamProfile::AgentEventsJsonlV1,
                geometry: CanonicalGeometry { cols: 0, rows: 0 },
                base_seq,
            },
            effects,
        )
        .unwrap();
    if !records.is_empty() {
        kernel
            .update(
                KernelInput::BootstrapChunk {
                    terminal_id: &agent,
                    stream_id: stream(),
                    bootstrap_id,
                    chunk_seq: 0,
                    payload: records.as_bytes(),
                },
                effects,
            )
            .unwrap();
    }
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &agent,
                stream_id: stream(),
                bootstrap_id,
                history_cursor: None,
            },
            effects,
        )
        .unwrap();
}

fn live(
    kernel: &mut SessionKernel<RigAdapter>,
    bootstrap_id: BootstrapId,
    seq: u64,
    effects: &mut EffectBuffer,
) -> bool {
    let records = lines(seq..=seq);
    kernel
        .update(
            KernelInput::ResourceOutput {
                terminal_id: &agent(),
                stream_id: stream(),
                bootstrap_id,
                seq,
                payload: records.as_bytes(),
            },
            effects,
        )
        .is_ok()
}

fn logged(kernel: &SessionKernel<RigAdapter>) -> Vec<u64> {
    kernel
        .agent_session(&agent())
        .unwrap()
        .log
        .records()
        .map(|record| record.seq)
        .collect()
}

#[test]
fn an_agent_gap_tombstone_then_full_replacement_publishes_the_retained_ring() {
    let mut kernel = SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw);
    let mut effects = EffectBuffer::new();
    publish(&mut kernel, generation(4), 0, "", &mut effects);
    assert!(live(&mut kernel, generation(4), 1, &mut effects));
    assert!(live(&mut kernel, generation(4), 2, &mut effects));
    kernel
        .update(
            KernelInput::Tombstone {
                terminal_id: &agent(),
                stream_id: stream(),
                bootstrap_id: generation(4),
                reason: TombstoneReason::OutboundGap,
                last_valid_seq: 2,
            },
            &mut effects,
        )
        .unwrap();
    assert!(
        !live(&mut kernel, generation(4), 3, &mut effects),
        "the retired generation takes no more output"
    );

    effects.clear();
    publish(&mut kernel, generation(5), 7, &lines(6..=7), &mut effects);
    let publication = effects.as_slice().iter().find_map(|effect| match effect {
        KernelEffect::AgentPublication(publication) => Some(publication),
        _ => None,
    });
    let publication = publication.expect("the replacement publishes");
    assert_eq!(publication.mode, AgentRecordMode::Replace);
    assert_eq!(
        publication.stream,
        AgentStreamInfo {
            stream_id: stream(),
            bootstrap_id: generation(5),
            last_seq: 7,
        }
    );
    assert_eq!(logged(&kernel), [6, 7], "the log is the retained ring");
    assert!(live(&mut kernel, generation(5), 8, &mut effects));
    assert_eq!(logged(&kernel), [6, 7, 8]);
}
