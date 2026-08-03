//! Deterministic capture-race and frame-fault contract matrix (phux-slogic.5.4).

#![allow(clippy::expect_used, reason = "tests")]
#![allow(clippy::panic, reason = "tests")]
#![allow(
    clippy::too_many_lines,
    reason = "fault tables keep each recovery trace linear"
)]

mod common;

use std::collections::BTreeSet;

use bytes::Bytes;
use common::fault::{Fault, FaultOutcome, FaultScript, FaultStep, Milestone, WireTranscript};
use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{BootstrapId, StreamId, TerminalId};
use phux_protocol::wire::frame::{
    FrameKind, HistoryRejectionReason, HistoryTombstoneReason, TombstoneReason,
};

const FIRST_POST_FENCE: u64 = 7;
const FINAL_RECORD: u64 = 15;

const fn terminal() -> TerminalId {
    TerminalId::local(1)
}

const fn stream() -> StreamId {
    StreamId::new(1).expect("non-zero stream")
}

const fn generation(raw: u64) -> BootstrapId {
    BootstrapId::new(raw).expect("non-zero generation")
}

fn record(number: u64) -> Bytes {
    Bytes::from(format!("pty:{number:04}\n"))
}

fn decode_record(payload: &[u8]) -> Result<u64, String> {
    let text = std::str::from_utf8(payload).map_err(|error| error.to_string())?;
    let number = text
        .strip_prefix("pty:")
        .and_then(|text| text.strip_suffix('\n'))
        .ok_or_else(|| format!("invalid numbered record {text:?}"))?;
    number.parse::<u64>().map_err(|error| error.to_string())
}

const fn begin(id: BootstrapId, base_seq: u64, cols: u16, rows: u16) -> FrameKind {
    FrameKind::BootstrapBegin {
        terminal_id: terminal(),
        stream_id: stream(),
        bootstrap_id: id,
        profile: BootstrapStreamProfile::SynthesizedVtRaw,
        cols,
        rows,
        base_seq,
    }
}

fn chunk(id: BootstrapId, chunk_seq: u32, number: u64) -> FrameKind {
    FrameKind::BootstrapChunk {
        terminal_id: terminal(),
        stream_id: stream(),
        bootstrap_id: id,
        chunk_seq,
        payload: record(number),
    }
}

fn ready(id: BootstrapId, with_history: bool) -> FrameKind {
    FrameKind::BootstrapReady {
        terminal_id: terminal(),
        stream_id: stream(),
        bootstrap_id: id,
        history_cursor: with_history.then(|| Bytes::from_static(b"cursor-1")),
    }
}

fn output(id: BootstrapId, seq: u64) -> FrameKind {
    FrameKind::TerminalOutput {
        terminal_id: terminal(),
        stream_id: stream(),
        bootstrap_id: id,
        seq,
        bytes: record(seq),
    }
}

fn checkpoint(id: BootstrapId, base_seq: u64, cols: u16, rows: u16) -> Vec<FrameKind> {
    let mut frames = vec![begin(id, base_seq, cols, rows)];
    for number in 0..=base_seq {
        frames.push(chunk(
            id,
            u32::try_from(number).expect("bounded checkpoint sequence"),
            number,
        ));
    }
    frames.push(ready(id, true));
    frames
}

const fn milestone(frame: &FrameKind) -> Milestone {
    match frame {
        FrameKind::BootstrapBegin { .. } => Milestone::BootstrapBegin,
        FrameKind::BootstrapChunk { chunk_seq, .. } => Milestone::CaptureRecord(*chunk_seq),
        FrameKind::BootstrapReady { .. } => Milestone::BootstrapReady,
        FrameKind::TerminalOutput { seq, .. } => Milestone::LiveOutput(*seq),
        FrameKind::HistoryPage { page_seq, .. } => Milestone::HistoryPage(*page_seq),
        FrameKind::BootstrapTombstone { .. } => Milestone::BootstrapTombstone,
        FrameKind::HistoryTombstone { .. } => Milestone::HistoryTombstone,
        _ => Milestone::Reconnect,
    }
}

fn delivered(outcome: FaultOutcome, transcript: &WireTranscript, context: &str) -> Vec<FrameKind> {
    match outcome {
        FaultOutcome::Delivered(frames) => frames,
        other => transcript.fail(format!("{context}: expected delivery, got {other:?}")),
    }
}

#[derive(Debug)]
struct Pending {
    id: BootstrapId,
    base_seq: u64,
    cols: u16,
    rows: u16,
    next_chunk: u32,
    checkpoint: Vec<u64>,
    bytes: Vec<u8>,
}

#[derive(Debug, Default)]
struct Replica {
    pending: Option<Pending>,
    active: Option<BootstrapId>,
    geometry: Option<(u16, u16)>,
    last_seq: u64,
    records: Vec<u64>,
    bytes: Vec<u8>,
    tombstoned: BTreeSet<BootstrapId>,
    history_pages: Vec<u64>,
    history_ended: Option<HistoryTombstoneReason>,
    last_rejected_cursor: Option<Bytes>,
    diagnostics: Vec<String>,
}

impl Replica {
    fn apply(&mut self, frame: FrameKind, transcript: &mut WireTranscript) -> Result<(), String> {
        let result = match frame {
            FrameKind::BootstrapBegin {
                bootstrap_id,
                base_seq,
                cols,
                rows,
                ..
            } => {
                if self.tombstoned.contains(&bootstrap_id) {
                    Err(format!(
                        "stale BEGIN for tombstoned generation {bootstrap_id}"
                    ))
                } else {
                    self.pending = Some(Pending {
                        id: bootstrap_id,
                        base_seq,
                        cols,
                        rows,
                        next_chunk: 0,
                        checkpoint: Vec::new(),
                        bytes: Vec::new(),
                    });
                    Ok(())
                }
            }
            FrameKind::BootstrapChunk {
                bootstrap_id,
                chunk_seq,
                payload,
                ..
            } => {
                let pending = self
                    .pending
                    .as_mut()
                    .ok_or_else(|| "chunk without BEGIN".to_owned())?;
                if pending.id != bootstrap_id || self.tombstoned.contains(&bootstrap_id) {
                    Err(format!("stale chunk for generation {bootstrap_id}"))
                } else if chunk_seq != pending.next_chunk {
                    Err(format!(
                        "checkpoint chunk gap/duplicate: expected={} actual={chunk_seq}",
                        pending.next_chunk
                    ))
                } else {
                    let number = decode_record(&payload)?;
                    pending.checkpoint.push(number);
                    pending.bytes.extend_from_slice(&payload);
                    pending.next_chunk += 1;
                    Ok(())
                }
            }
            FrameKind::BootstrapReady { bootstrap_id, .. } => {
                let pending = self
                    .pending
                    .take()
                    .ok_or_else(|| "READY without BEGIN".to_owned())?;
                if pending.id != bootstrap_id || self.tombstoned.contains(&bootstrap_id) {
                    Err(format!("stale READY for generation {bootstrap_id}"))
                } else if pending.checkpoint.len() as u64 != pending.base_seq + 1 {
                    Err(format!(
                        "READY before complete checkpoint: expected={} actual={}",
                        pending.base_seq + 1,
                        pending.checkpoint.len()
                    ))
                } else {
                    self.records = pending.checkpoint;
                    self.bytes = pending.bytes;
                    self.active = Some(bootstrap_id);
                    self.geometry = Some((pending.cols, pending.rows));
                    self.last_seq = pending.base_seq;
                    Ok(())
                }
            }
            FrameKind::TerminalOutput {
                bootstrap_id,
                seq,
                bytes,
                ..
            } => {
                if self.active != Some(bootstrap_id) || self.tombstoned.contains(&bootstrap_id) {
                    transcript.note(format!(
                        "ignored stale TERMINAL_OUTPUT generation={bootstrap_id} seq={seq}"
                    ));
                    Ok(())
                } else if seq != self.last_seq + 1 {
                    Err(format!(
                        "live sequence gap/duplicate: expected={} actual={seq}",
                        self.last_seq + 1
                    ))
                } else {
                    let number = decode_record(&bytes)?;
                    if number == seq {
                        self.records.push(number);
                        self.bytes.extend_from_slice(&bytes);
                        self.last_seq = seq;
                        Ok(())
                    } else {
                        Err(format!(
                            "live payload/sequence mismatch: payload={number} seq={seq}"
                        ))
                    }
                }
            }
            FrameKind::BootstrapTombstone {
                bootstrap_id,
                last_valid_seq,
                ..
            } => {
                self.tombstoned.insert(bootstrap_id);
                if self.active == Some(bootstrap_id) {
                    self.active = None;
                }
                if self
                    .pending
                    .as_ref()
                    .is_some_and(|pending| pending.id == bootstrap_id)
                {
                    self.pending = None;
                }
                transcript.note(format!(
                    "generation {bootstrap_id} tombstoned at seq {last_valid_seq}"
                ));
                Ok(())
            }
            FrameKind::HistoryPage {
                bootstrap_id,
                page_seq,
                payload,
                ..
            } => {
                if self.active == Some(bootstrap_id) {
                    decode_record(&payload)?;
                    self.history_pages.push(page_seq);
                } else {
                    transcript.note(format!(
                        "ignored stale HISTORY_PAGE generation={bootstrap_id} page={page_seq}"
                    ));
                }
                Ok(())
            }
            FrameKind::HistoryTombstone { reason, .. } => {
                self.history_ended = Some(reason);
                Ok(())
            }
            FrameKind::HistoryRejected { cursor, reason, .. } => {
                self.last_rejected_cursor = Some(cursor);
                self.diagnostics
                    .push(format!("history rejected: {reason:?}"));
                Ok(())
            }
            other => Err(format!("unexpected frame: {other:?}")),
        };
        transcript.note(format!("apply result={result:?}"));
        result
    }
}

fn apply_clean(
    replica: &mut Replica,
    script: &mut FaultScript,
    frame: FrameKind,
    transcript: &mut WireTranscript,
) {
    for frame in delivered(
        script.transmit(milestone(&frame), frame, transcript),
        transcript,
        "clean path",
    ) {
        if let Err(error) = replica.apply(frame, transcript) {
            transcript.fail(error);
        }
    }
}

const fn tombstone(id: BootstrapId, reason: TombstoneReason, last_valid_seq: u64) -> FrameKind {
    FrameKind::BootstrapTombstone {
        terminal_id: terminal(),
        stream_id: stream(),
        bootstrap_id: id,
        reason,
        last_valid_seq,
    }
}

fn history_page(id: BootstrapId, page_seq: u64, cursor: &'static [u8]) -> FrameKind {
    FrameKind::HistoryPage {
        terminal_id: terminal(),
        stream_id: stream(),
        bootstrap_id: id,
        page_seq,
        cursor: Bytes::from_static(cursor),
        next_cursor: Some(Bytes::from_static(b"next-cursor")),
        payload: record(100 + page_seq),
        rows: 1,
    }
}

fn assert_exact_partition(replica: &Replica, transcript: &WireTranscript, through: u64) {
    let expected = (0..=through).collect::<Vec<_>>();
    transcript.assert_eq(
        &replica.records,
        &expected,
        "checkpoint/post-fence partition",
    );
    let expected_bytes = (0..=through)
        .flat_map(|number| record(number).to_vec())
        .collect::<Vec<_>>();
    transcript.assert_eq(
        &replica.bytes,
        &expected_bytes,
        "every PTY byte appears exactly once in checkpoint or post-fence output",
    );
    let unique = replica.records.iter().copied().collect::<BTreeSet<_>>();
    transcript.assert_eq(
        &unique.len(),
        &replica.records.len(),
        "every numbered PTY record appears exactly once",
    );
}

#[test]
fn pauses_at_every_capture_record_preserve_the_atomic_fence() {
    let source = checkpoint(generation(1), FIRST_POST_FENCE - 1, 80, 24);
    for pause_index in 0..source.len() {
        let mut transcript = WireTranscript::new();
        transcript.note(format!("case pause_index={pause_index}"));
        let pause_at = milestone(&source[pause_index]);
        let mut script = FaultScript::new([FaultStep::new(pause_at, Fault::Pause)]);
        let mut replica = Replica::default();
        let mut paused = None;

        for (index, frame) in source.iter().cloned().enumerate() {
            match script.transmit(milestone(&frame), frame, &mut transcript) {
                FaultOutcome::Delivered(frames) => {
                    for frame in frames {
                        if let Err(error) = replica.apply(frame, &mut transcript) {
                            transcript.fail(error);
                        }
                    }
                }
                FaultOutcome::Paused(token) if index == pause_index => paused = Some(token),
                other => transcript.fail(format!("unexpected capture outcome {other:?}")),
            }
            if index == pause_index {
                for seq in FIRST_POST_FENCE..=FINAL_RECORD {
                    transcript.note(format!(
                        "canonical PTY advanced while capture paused: seq={seq}"
                    ));
                }
                let token = paused
                    .take()
                    .unwrap_or_else(|| transcript.fail("pause produced no token"));
                let frame = script
                    .resume(token, &mut transcript)
                    .unwrap_or_else(|error| transcript.fail(error));
                if let Err(error) = replica.apply(frame, &mut transcript) {
                    transcript.fail(error);
                }
            }
        }
        for seq in FIRST_POST_FENCE..=FINAL_RECORD {
            apply_clean(
                &mut replica,
                &mut script,
                output(generation(1), seq),
                &mut transcript,
            );
        }
        script.assert_drained(&transcript);
        assert_exact_partition(&replica, &transcript, FINAL_RECORD);
    }
}

#[test]
fn chunk_faults_converge_by_tombstone_and_full_resync() {
    for fault in [Fault::Drop, Fault::Duplicate, Fault::CorruptPayload] {
        let mut transcript = WireTranscript::new();
        transcript.note(format!("case checkpoint fault={fault:?}"));
        let bad = generation(10);
        let good = generation(11);
        let mut script = FaultScript::new([FaultStep::new(Milestone::CaptureRecord(3), fault)]);
        let mut replica = Replica::default();
        let mut diagnosed = false;

        for frame in checkpoint(bad, FIRST_POST_FENCE - 1, 80, 24) {
            match script.transmit(milestone(&frame), frame, &mut transcript) {
                FaultOutcome::Delivered(frames) => {
                    for frame in frames {
                        if let Err(error) = replica.apply(frame, &mut transcript) {
                            transcript.note(format!("explicit diagnostic: {error}"));
                            diagnosed = true;
                        }
                    }
                }
                FaultOutcome::Dropped => transcript.note("chunk intentionally dropped"),
                other => transcript.fail(format!("unexpected chunk fault outcome {other:?}")),
            }
        }
        transcript.assert(
            diagnosed,
            format_args!("fault produced no explicit diagnostic"),
        );

        apply_clean(
            &mut replica,
            &mut script,
            tombstone(bad, TombstoneReason::CodecFailure, FIRST_POST_FENCE - 1),
            &mut transcript,
        );
        for frame in checkpoint(good, FIRST_POST_FENCE - 1, 80, 24) {
            apply_clean(&mut replica, &mut script, frame, &mut transcript);
        }
        for seq in FIRST_POST_FENCE..=FINAL_RECORD {
            apply_clean(
                &mut replica,
                &mut script,
                output(good, seq),
                &mut transcript,
            );
        }

        let before_stale = replica.records.clone();
        for stale in [
            chunk(bad, 3, 3),
            ready(bad, true),
            output(bad, FINAL_RECORD + 1),
        ] {
            let _ = replica.apply(stale, &mut transcript);
        }
        transcript.assert_eq(
            &replica.records,
            &before_stale,
            "stale generation changed the published replica",
        );
        script.assert_drained(&transcript);
        assert_exact_partition(&replica, &transcript, FINAL_RECORD);
    }
}

#[test]
fn history_resize_pruning_and_reconnect_have_bounded_outcomes() {
    let mut transcript = WireTranscript::new();
    let first = generation(20);
    let resized = generation(21);
    let reconnected = generation(22);
    let mut replica = Replica::default();
    let mut clean = FaultScript::clean();

    for frame in checkpoint(first, FIRST_POST_FENCE - 1, 80, 24) {
        apply_clean(&mut replica, &mut clean, frame, &mut transcript);
    }

    let mut delayed = FaultScript::new([FaultStep::new(Milestone::HistoryPage(1), Fault::Pause)]);
    let token = match delayed.transmit(
        Milestone::HistoryPage(1),
        history_page(first, 1, b"cursor-1"),
        &mut transcript,
    ) {
        FaultOutcome::Paused(token) => token,
        other => transcript.fail(format!("history was not delayed: {other:?}")),
    };
    for seq in FIRST_POST_FENCE..=10 {
        apply_clean(
            &mut replica,
            &mut clean,
            output(first, seq),
            &mut transcript,
        );
    }
    let page = delayed
        .resume(token, &mut transcript)
        .unwrap_or_else(|error| transcript.fail(error));
    replica
        .apply(page, &mut transcript)
        .unwrap_or_else(|error| transcript.fail(error));
    delayed.assert_drained(&transcript);

    let mut stalled = FaultScript::new([FaultStep::new(
        Milestone::HistoryPage(2),
        Fault::MailboxSaturation,
    )]);
    let unchanged_cursor = Bytes::from_static(b"next-cursor");
    let stalled_outcome = stalled.transmit(
        Milestone::HistoryPage(2),
        history_page(first, 2, b"next-cursor"),
        &mut transcript,
    );
    transcript.assert(
        matches!(stalled_outcome, FaultOutcome::MailboxSaturation),
        format_args!("stalled history did not report bounded mailbox saturation"),
    );
    replica
        .apply(
            FrameKind::HistoryRejected {
                terminal_id: terminal(),
                stream_id: stream(),
                bootstrap_id: first,
                cursor: unchanged_cursor.clone(),
                reason: HistoryRejectionReason::Busy,
                required_bytes: 1,
                required_rows: 1,
            },
            &mut transcript,
        )
        .unwrap_or_else(|error| transcript.fail(error));
    transcript.assert_eq(
        &replica.last_rejected_cursor,
        &Some(unchanged_cursor.clone()),
        "stalled history advanced its cursor",
    );
    apply_clean(&mut replica, &mut clean, output(first, 11), &mut transcript);
    stalled.assert_drained(&transcript);

    replica
        .apply(
            FrameKind::HistoryTombstone {
                terminal_id: terminal(),
                stream_id: stream(),
                bootstrap_id: first,
                cursor: unchanged_cursor,
                reason: HistoryTombstoneReason::Pruned,
            },
            &mut transcript,
        )
        .unwrap_or_else(|error| transcript.fail(error));
    transcript.assert_eq(
        &replica.history_ended,
        &Some(HistoryTombstoneReason::Pruned),
        "pruning did not explicitly end history",
    );
    transcript.assert_eq(
        &replica.active,
        &Some(first),
        "history pruning invalidated live state",
    );

    apply_clean(
        &mut replica,
        &mut clean,
        tombstone(first, TombstoneReason::Resize, 11),
        &mut transcript,
    );
    for frame in checkpoint(resized, 11, 132, 43) {
        apply_clean(&mut replica, &mut clean, frame, &mut transcript);
    }
    transcript.assert_eq(&replica.geometry, &Some((132, 43)), "resize geometry");
    for seq in 12..=13 {
        apply_clean(
            &mut replica,
            &mut clean,
            output(resized, seq),
            &mut transcript,
        );
    }

    apply_clean(
        &mut replica,
        &mut clean,
        tombstone(resized, TombstoneReason::RelayReconnect, 13),
        &mut transcript,
    );
    for frame in checkpoint(reconnected, 13, 132, 43) {
        apply_clean(&mut replica, &mut clean, frame, &mut transcript);
    }
    for seq in 14..=FINAL_RECORD {
        apply_clean(
            &mut replica,
            &mut clean,
            output(reconnected, seq),
            &mut transcript,
        );
    }
    clean.assert_drained(&transcript);
    assert_exact_partition(&replica, &transcript, FINAL_RECORD);
}

#[test]
fn lag_saturation_and_disconnect_are_client_local_and_recover_at_every_milestone() {
    for fault in [Fault::BroadcastLag, Fault::MailboxSaturation] {
        let mut transcript = WireTranscript::new();
        transcript.note(format!("case slow-client fault={fault:?}"));
        let first = generation(30);
        let recovered = generation(31);
        let mut slow = Replica::default();
        let mut peer = Replica::default();
        let mut clean = FaultScript::clean();
        for frame in checkpoint(first, FIRST_POST_FENCE - 1, 80, 24) {
            apply_clean(&mut slow, &mut clean, frame.clone(), &mut transcript);
            apply_clean(&mut peer, &mut clean, frame, &mut transcript);
        }
        for seq in FIRST_POST_FENCE..=9 {
            apply_clean(&mut slow, &mut clean, output(first, seq), &mut transcript);
            apply_clean(&mut peer, &mut clean, output(first, seq), &mut transcript);
        }

        let canonical_before = (0..=10).collect::<Vec<_>>();
        let mut pressure = FaultScript::new([FaultStep::new(Milestone::LiveOutput(10), fault)]);
        let outcome = pressure.transmit(
            Milestone::LiveOutput(10),
            output(first, 10),
            &mut transcript,
        );
        transcript.assert(
            matches!(
                outcome,
                FaultOutcome::BroadcastLag | FaultOutcome::MailboxSaturation
            ),
            format_args!("pressure did not remain client-local: {outcome:?}"),
        );
        apply_clean(&mut peer, &mut clean, output(first, 10), &mut transcript);
        transcript.assert_eq(
            &peer.records,
            &canonical_before,
            "peer delivery under pressure",
        );
        transcript.assert_eq(
            &(0..=10).collect::<Vec<_>>(),
            &canonical_before,
            "canonical terminal changed under client pressure",
        );

        apply_clean(
            &mut slow,
            &mut clean,
            tombstone(first, TombstoneReason::OutboundGap, 9),
            &mut transcript,
        );
        for frame in checkpoint(recovered, 10, 80, 24) {
            apply_clean(&mut slow, &mut clean, frame, &mut transcript);
        }
        for seq in 11..=FINAL_RECORD {
            apply_clean(
                &mut slow,
                &mut clean,
                output(recovered, seq),
                &mut transcript,
            );
            apply_clean(&mut peer, &mut clean, output(first, seq), &mut transcript);
        }
        pressure.assert_drained(&transcript);
        assert_exact_partition(&slow, &transcript, FINAL_RECORD);
        assert_exact_partition(&peer, &transcript, FINAL_RECORD);
    }

    let milestones = checkpoint(generation(40), FIRST_POST_FENCE - 1, 80, 24);
    for disconnect_index in 0..milestones.len() {
        let mut transcript = WireTranscript::new();
        transcript.note(format!("case disconnect_index={disconnect_index}"));
        let first = generation(40);
        let recovered = generation(41);
        let disconnect_at = milestone(&milestones[disconnect_index]);
        let mut script = FaultScript::new([FaultStep::new(disconnect_at, Fault::Disconnect)]);
        let mut replica = Replica::default();
        let mut disconnected = false;

        for (index, frame) in milestones.iter().cloned().enumerate() {
            if disconnected {
                transcript.note(format!("transport dropped remainder index={index}"));
                continue;
            }
            match script.transmit(milestone(&frame), frame, &mut transcript) {
                FaultOutcome::Delivered(frames) => {
                    for frame in frames {
                        if let Err(error) = replica.apply(frame, &mut transcript) {
                            transcript.fail(error);
                        }
                    }
                }
                FaultOutcome::Disconnected if index == disconnect_index => disconnected = true,
                other => transcript.fail(format!("unexpected disconnect outcome {other:?}")),
            }
        }
        transcript.assert(disconnected, format_args!("transport never disconnected"));
        replica.pending = None;
        replica.active = None;
        transcript.note("reconnect starts an independent full bootstrap generation");
        let mut clean = FaultScript::clean();
        for frame in checkpoint(recovered, FIRST_POST_FENCE - 1, 80, 24) {
            apply_clean(&mut replica, &mut clean, frame, &mut transcript);
        }
        for stale in [ready(first, true), output(first, FIRST_POST_FENCE)] {
            let _ = replica.apply(stale, &mut transcript);
        }
        for seq in FIRST_POST_FENCE..=FINAL_RECORD {
            apply_clean(
                &mut replica,
                &mut clean,
                output(recovered, seq),
                &mut transcript,
            );
        }
        script.assert_drained(&transcript);
        clean.assert_drained(&transcript);
        assert_exact_partition(&replica, &transcript, FINAL_RECORD);
    }
}
