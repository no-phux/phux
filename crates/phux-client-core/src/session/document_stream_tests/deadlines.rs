//! Staging expires on progress, not age; a fenced document whose recut never
//! begins is reported; and every update reclaims what stalled elsewhere.

use phux_protocol::wire::frame::TombstoneReason;
use phux_protocol::{BootstrapId, BootstrapProfile, BootstrapStreamProfile, ResourceId, StreamId};

use super::super::kernel_rig::RigAdapter;
use super::super::{
    EffectBuffer, KernelEffect, KernelError, KernelInput, KernelStatus,
    MAX_BOOTSTRAP_STAGING_BYTES, MAX_BOOTSTRAP_STAGING_IDLE_MS, SessionKernel,
};
use super::{documents, generation, stream, surface};
use crate::engine::CanonicalGeometry;

type Kernel = SessionKernel<RigAdapter>;
type Outcome = Result<(), KernelError<std::convert::Infallible>>;

const IDLE: u64 = MAX_BOOTSTRAP_STAGING_IDLE_MS;

fn kernel() -> Kernel {
    SessionKernel::new(RigAdapter::default(), BootstrapProfile::SynthesizedVtRaw)
}

fn ids(raw: u64) -> (StreamId, BootstrapId) {
    (stream(), generation(raw))
}

fn begin(kernel: &mut Kernel, now_ms: u64, id: &ResourceId, raw: u64) -> Outcome {
    let terminal = *id != surface();
    kernel.update_at(
        now_ms,
        KernelInput::BootstrapBegin {
            terminal_id: id,
            stream_id: stream(),
            bootstrap_id: generation(raw),
            profile: if terminal {
                BootstrapStreamProfile::SynthesizedVtRaw
            } else {
                BootstrapStreamProfile::PixelSurfacePngV1
            },
            geometry: if terminal {
                CanonicalGeometry { cols: 80, rows: 24 }
            } else {
                CanonicalGeometry { cols: 0, rows: 0 }
            },
            base_seq: 4,
        },
        &mut EffectBuffer::new(),
    )
}

fn chunk(kernel: &mut Kernel, now_ms: u64, chunk_seq: u32, payload: &[u8]) -> Outcome {
    let (stream_id, bootstrap_id) = ids(1);
    kernel.update_at(
        now_ms,
        KernelInput::BootstrapChunk {
            terminal_id: &surface(),
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        },
        &mut EffectBuffer::new(),
    )
}

fn ready(kernel: &mut Kernel, now_ms: u64, raw: u64, effects: &mut EffectBuffer) -> Outcome {
    let (stream_id, bootstrap_id) = ids(raw);
    kernel.update_at(
        now_ms,
        KernelInput::BootstrapReady {
            terminal_id: &surface(),
            stream_id,
            bootstrap_id,
            history_cursor: None,
        },
        effects,
    )
}

fn tombstone(kernel: &mut Kernel, now_ms: u64, raw: u64) {
    let (stream_id, bootstrap_id) = ids(raw);
    kernel
        .update_at(
            now_ms,
            KernelInput::Tombstone {
                terminal_id: &surface(),
                stream_id,
                bootstrap_id,
                reason: TombstoneReason::OutboundGap,
                last_valid_seq: 4,
            },
            &mut EffectBuffer::new(),
        )
        .unwrap();
}

fn resyncs(effects: &EffectBuffer) -> Vec<(ResourceId, TombstoneReason)> {
    effects
        .as_slice()
        .iter()
        .filter_map(|effect| match effect {
            KernelEffect::Status(KernelStatus::ResyncRequired {
                terminal_id,
                reason,
                ..
            }) => Some((terminal_id.clone(), *reason)),
            _ => None,
        })
        .collect()
}

/// Document stalls: `(resource, reason, attempt)`, attempt `0` for lost.
fn stalls(effects: &EffectBuffer) -> Vec<(ResourceId, TombstoneReason, u32)> {
    effects
        .as_slice()
        .iter()
        .filter_map(|effect| match effect {
            KernelEffect::Status(KernelStatus::DocumentStalled {
                terminal_id,
                reason,
                attempt,
            }) => Some((terminal_id.clone(), *reason, *attempt)),
            KernelEffect::Status(KernelStatus::DocumentLost {
                terminal_id,
                reason,
            }) => Some((terminal_id.clone(), *reason, 0)),
            _ => None,
        })
        .collect()
}

#[test]
fn a_large_cut_on_a_slow_link_lands_while_it_keeps_moving() {
    // 16 MiB at 64 KiB/s: over four minutes, far past the idle window.
    let mut kernel = kernel();
    begin(&mut kernel, 0, &surface(), 1).unwrap();
    let piece = vec![7_u8; 256 * 1024];
    for chunk_seq in 0..64_u32 {
        chunk(
            &mut kernel,
            u64::from(chunk_seq + 1) * 4_000,
            chunk_seq,
            &piece,
        )
        .unwrap();
    }
    let mut effects = EffectBuffer::new();
    assert_eq!(
        kernel.expire_bootstrap_staging(256_000 + IDLE, &mut effects),
        0
    );
    ready(&mut kernel, 256_000 + IDLE, 1, &mut effects).unwrap();
    assert_eq!(documents(&effects)[0].bytes.len(), 16 * 1024 * 1024);
}

#[test]
fn a_cut_that_stops_or_trickles_still_expires() {
    // Silent past the idle window since its last chunk.
    let mut kernel = kernel();
    begin(&mut kernel, 0, &surface(), 1).unwrap();
    chunk(&mut kernel, 20_000, 0, &vec![1; 1024 * 1024]).unwrap();
    let mut effects = EffectBuffer::new();
    assert_eq!(
        kernel.expire_bootstrap_staging(20_000 + IDLE, &mut effects),
        0
    );
    assert_eq!(
        kernel.expire_bootstrap_staging(20_001 + IDLE, &mut effects),
        1
    );
    // A document stalls on its own; nothing else resyncs.
    assert!(resyncs(&effects).is_empty());
    assert_eq!(stalls(&effects), [(surface(), TombstoneReason::Other, 1)]);

    // A byte every few seconds never idles, but falls below the minimum rate.
    let mut kernel = self::kernel();
    begin(&mut kernel, 0, &surface(), 1).unwrap();
    let mut chunk_seq = 0;
    let refused = loop {
        let now_ms = u64::from(chunk_seq + 1) * 5_000;
        if let Err(error) = chunk(&mut kernel, now_ms, chunk_seq, b"x") {
            break (now_ms, error);
        }
        chunk_seq += 1;
    };
    assert!(matches!(
        refused,
        (35_000, KernelError::BootstrapStagingLimitExceeded)
    ));
}

#[test]
fn a_fenced_document_whose_recut_never_begins_is_reported_once() {
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    begin(&mut kernel, 0, &surface(), 1).unwrap();
    ready(&mut kernel, 0, 1, &mut effects).unwrap();
    tombstone(&mut kernel, 1_000, 1);
    assert_eq!(kernel.next_deadline_in_ms(), Some(IDLE + 1));
    assert_eq!(
        kernel.expire_bootstrap_staging(1_000 + IDLE, &mut effects),
        0
    );
    assert_eq!(
        kernel.expire_bootstrap_staging(1_001 + IDLE, &mut effects),
        1
    );
    assert_eq!(
        stalls(&effects),
        [(surface(), TombstoneReason::OutboundGap, 1)]
    );
    assert!(resyncs(&effects).is_empty());
    // The re-attach's recut is awaited twice as long.
    assert_eq!(kernel.next_deadline_in_ms(), Some(2 * IDLE + 1));

    // A server that recuts at once is never reported.
    let mut kernel = self::kernel();
    begin(&mut kernel, 0, &surface(), 1).unwrap();
    ready(&mut kernel, 0, 1, &mut effects).unwrap();
    tombstone(&mut kernel, 0, 1);
    begin(&mut kernel, 0, &surface(), 2).unwrap();
    ready(&mut kernel, 0, 2, &mut effects).unwrap();
    assert_eq!(kernel.expire_bootstrap_staging(9 * IDLE, &mut effects), 0);
    assert_eq!(kernel.next_deadline_in_ms(), None);
}

fn fenced_then_output_elsewhere(host_ticks: bool) -> EffectBuffer {
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    begin(&mut kernel, 0, &surface(), 1).unwrap();
    ready(&mut kernel, 0, 1, &mut effects).unwrap();
    if host_ticks {
        kernel.expire_bootstrap_staging(0, &mut effects);
    }
    tombstone(&mut kernel, 0, 1);
    kernel
        .update_at(
            IDLE + 1,
            KernelInput::ResourceOutput {
                terminal_id: &ResourceId::local(77),
                stream_id: stream(),
                bootstrap_id: generation(1),
                seq: 1,
                payload: b"",
            },
            &mut effects,
        )
        .unwrap_err();
    effects
}

#[test]
fn without_a_host_timer_any_update_reports_an_overdue_recut() {
    let reported = fenced_then_output_elsewhere(false);
    assert_eq!(
        stalls(&reported),
        [(surface(), TombstoneReason::OutboundGap, 1)]
    );
    // A host that ticks owns the deadlines; its tick reports them.
    assert!(stalls(&fenced_then_output_elsewhere(true)).is_empty());
}

#[test]
fn a_stalled_cut_from_an_earlier_connection_stops_holding_the_ceiling() {
    // A terminal cut holding the whole byte budget went silent.
    let mut kernel = kernel();
    let stale = ResourceId::local(1);
    begin(&mut kernel, 0, &stale, 1).unwrap();
    kernel
        .lanes
        .terminal_mut(&stale)
        .and_then(|state| state.staging.as_mut())
        .unwrap()
        .accepted_bytes = MAX_BOOTSTRAP_STAGING_BYTES;
    begin(&mut kernel, IDLE - 1, &surface(), 1).unwrap();
    assert!(matches!(
        chunk(&mut kernel, IDLE, 0, b"x"),
        Err(KernelError::BootstrapStagingLimitExceeded)
    ));

    // Past the idle window, the next update retires it and reports it.
    let mut kernel = self::kernel();
    begin(&mut kernel, 0, &stale, 1).unwrap();
    kernel
        .lanes
        .terminal_mut(&stale)
        .and_then(|state| state.staging.as_mut())
        .unwrap()
        .accepted_bytes = MAX_BOOTSTRAP_STAGING_BYTES;
    let mut effects = EffectBuffer::new();
    kernel
        .update_at(
            IDLE + 1,
            KernelInput::BootstrapBegin {
                terminal_id: &surface(),
                stream_id: stream(),
                bootstrap_id: generation(1),
                profile: BootstrapStreamProfile::PixelSurfacePngV1,
                geometry: CanonicalGeometry { cols: 0, rows: 0 },
                base_seq: 4,
            },
            &mut effects,
        )
        .unwrap();
    assert_eq!(
        resyncs(&effects),
        [(stale.clone(), TombstoneReason::CodecFailure)]
    );
    assert!(kernel.staging(&stale).is_none());
    chunk(&mut kernel, IDLE + 1, 0, b"x").unwrap();
    // A host that ticks owns expiry: an update leaves the cut to its tick.
    let mut kernel = self::kernel();
    begin(&mut kernel, 0, &stale, 1).unwrap();
    let mut effects = EffectBuffer::new();
    assert_eq!(kernel.expire_bootstrap_staging(0, &mut effects), 0);
    begin(&mut kernel, IDLE + 1, &surface(), 1).unwrap();
    assert!(kernel.staging(&stale).is_some());
    assert_eq!(kernel.next_deadline_in_ms(), Some(0));
    assert_eq!(kernel.expire_bootstrap_staging(IDLE + 1, &mut effects), 1);
}

fn output_to(kernel: &mut Kernel, raw: u64, seq: u64) -> Outcome {
    let (stream_id, bootstrap_id) = ids(raw);
    kernel.update(
        KernelInput::ResourceOutput {
            terminal_id: &surface(),
            stream_id,
            bootstrap_id,
            seq,
            payload: b"doc",
        },
        &mut EffectBuffer::new(),
    )
}

#[test]
fn live_output_reads_the_clock_only_while_a_deadline_is_pending() {
    // The kernel's own clock, as the runtime drives it.
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    let begin_at = |kernel: &mut Kernel, id: &ResourceId, raw: u64| {
        kernel.update(
            KernelInput::BootstrapBegin {
                terminal_id: id,
                stream_id: stream(),
                bootstrap_id: generation(raw),
                profile: BootstrapStreamProfile::PixelSurfacePngV1,
                geometry: CanonicalGeometry { cols: 0, rows: 0 },
                base_seq: 4,
            },
            &mut EffectBuffer::new(),
        )
    };
    begin_at(&mut kernel, &surface(), 1).unwrap();
    kernel
        .update(
            KernelInput::BootstrapReady {
                terminal_id: &surface(),
                stream_id: stream(),
                bootstrap_id: generation(1),
                history_cursor: None,
            },
            &mut effects,
        )
        .unwrap();
    // A sweep with nothing due clears the deadline the cut left behind.
    kernel.next_deadline_ms = Some(0);
    output_to(&mut kernel, 1, 5).unwrap();
    let (reads, sweeps) = (kernel.clock_reads.get(), kernel.deadline_sweeps);
    for seq in 6..1_006 {
        output_to(&mut kernel, 1, seq).unwrap();
    }
    assert_eq!(kernel.clock_reads.get(), reads, "no deadline, no clock");
    // A cut in flight elsewhere: one read and one comparison per update,
    // and no lane scan before it is due.
    begin_at(&mut kernel, &ResourceId::local(10), 1).unwrap();
    let (reads, sweeps_before) = (kernel.clock_reads.get(), kernel.deadline_sweeps);
    for seq in 1_006..2_006 {
        output_to(&mut kernel, 1, seq).unwrap();
    }
    assert_eq!(kernel.clock_reads.get() - reads, 1_000);
    assert_eq!(kernel.deadline_sweeps, sweeps_before);
    assert_eq!(sweeps_before, sweeps, "opening a cut scans nothing");
}

#[test]
fn a_run_of_frames_off_the_current_generation_asks_for_recovery() {
    let mut kernel = kernel();
    let mut effects = EffectBuffer::new();
    begin(&mut kernel, 0, &surface(), 1).unwrap();
    ready(&mut kernel, 0, 1, &mut effects).unwrap();
    let mut last = EffectBuffer::new();
    for seq in 0..64 {
        let (stream_id, other) = ids(9);
        kernel
            .update_at(
                0,
                KernelInput::ResourceOutput {
                    terminal_id: &surface(),
                    stream_id,
                    bootstrap_id: other,
                    seq: seq + 5,
                    payload: b"mis-tagged",
                },
                &mut last,
            )
            .unwrap_err();
        assert_eq!(stalls(&last).is_empty(), seq < 63, "frame {seq}");
    }
    assert_eq!(stalls(&last), [(surface(), TombstoneReason::Other, 1)]);

    // A terminal has no per-resource recovery: it resyncs.
    let mut kernel = self::kernel();
    let pane = ResourceId::local(1);
    begin(&mut kernel, 0, &pane, 1).unwrap();
    for seq in 0..64 {
        kernel
            .update_at(
                0,
                KernelInput::ResourceOutput {
                    terminal_id: &pane,
                    stream_id: stream(),
                    bootstrap_id: generation(9),
                    seq: seq + 5,
                    payload: b"mis-tagged",
                },
                &mut last,
            )
            .unwrap_err();
    }
    assert_eq!(resyncs(&last), [(pane, TombstoneReason::Other)]);
}
