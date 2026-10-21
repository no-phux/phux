#![allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

const SERVER_A: [u8; 16] = [0xAA; 16];
const SERVER_B: [u8; 16] = [0xBB; 16];

fn tid(n: u32) -> ResourceId {
    ResourceId::local(n)
}

fn operation_id(n: u64) -> InputOperationId {
    let mut bytes = [0_u8; 16];
    bytes[8..].copy_from_slice(&n.to_be_bytes());
    InputOperationId::new(bytes).expect("non-zero test operation id")
}

fn submit(
    journal: &mut InputReplayJournal,
    terminal_id: ResourceId,
    events: Vec<InputEvent>,
) -> Result<(), ReplayReport> {
    static NEXT_ID: AtomicU64 = AtomicU64::new(1);
    journal.submit_at(
        operation_id(NEXT_ID.fetch_add(1, Ordering::Relaxed)),
        terminal_id,
        events,
        100,
    )
}

fn paste(text: &str) -> Vec<InputEvent> {
    use phux_protocol::input::paste::{PasteEvent, PasteTrust};
    vec![InputEvent::Paste(PasteEvent {
        trust: PasteTrust::Untrusted,
        data: text.as_bytes().to_vec(),
    })]
}

fn enter() -> Vec<InputEvent> {
    use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
    vec![InputEvent::Key(KeyEvent {
        action: KeyAction::Press,
        key: PhysicalKey::Enter,
        mods: ModSet::empty(),
        consumed_mods: ModSet::empty(),
        composing: false,
        text: None,
        unshifted_codepoint: None,
    })]
}

fn armed_journal() -> InputReplayJournal {
    let mut journal = InputReplayJournal::new();
    assert!(journal.begin_connection(Some(&SERVER_A), true).is_empty());
    journal
}

/// Pull the outstanding attempt's frame pieces or panic.
fn send_one(journal: &mut InputReplayJournal, next: &mut u32) -> (u32, InputOperationId) {
    let (reports, frame) = journal.next_frame(next);
    assert!(reports.is_empty(), "{reports:?}");
    match frame.expect("an attempt is due") {
        FrameKind::Command {
            request_id,
            command:
                Command::ApplyInput {
                    operation_id,
                    terminal_id: _,
                    events: _,
                },
        } => (request_id, operation_id),
        other => panic!("not an APPLY_INPUT: {other:?}"),
    }
}

fn request_id_for_terminal(frames: &[FrameKind], expected: &ResourceId) -> u32 {
    frames
        .iter()
        .find_map(|frame| match frame {
            FrameKind::Command {
                request_id,
                command: Command::ApplyInput { terminal_id, .. },
            } if terminal_id == expected => Some(*request_id),
            _ => None,
        })
        .expect("terminal attempt")
}

// ---- the id is the contract -------------------------------------

/// A resend after a lost socket reuses the SAME operation id under a
/// fresh request id. This is the whole point of the journal: the id is
/// what lets the server's dedupe cache answer instead of writing twice.
#[test]
fn a_replay_reuses_the_operation_id_and_not_the_request_id() {
    let mut journal = armed_journal();
    let operation_id = operation_id(42);
    let payload = paste("ship it");
    journal
        .submit_at(operation_id, tid(1), payload.clone(), 100)
        .expect("queue");
    let mut next = 1_u32;
    let (reports, first_frames) = journal.next_frames_at(&mut next, 110);
    assert!(reports.is_empty());
    let first_request = command_parts(&first_frames[0]).0;
    let first_events = command_parts(&first_frames[0]).1.to_vec();
    let first_op = match &first_frames[0] {
        FrameKind::Command {
            command: Command::ApplyInput { operation_id, .. },
            ..
        } => *operation_id,
        other => panic!("not APPLY_INPUT: {other:?}"),
    };

    journal.connection_lost();
    let reports = journal.begin_connection_at(Some(&SERVER_A), true, 120);
    assert!(reports.is_empty(), "{reports:?}");

    let (reports, second_frames) = journal.next_frames_at(&mut next, 130);
    assert!(reports.is_empty());
    let second_request = command_parts(&second_frames[0]).0;
    let second_events = command_parts(&second_frames[0]).1;
    let second_op = match &second_frames[0] {
        FrameKind::Command {
            command: Command::ApplyInput { operation_id, .. },
            ..
        } => *operation_id,
        other => panic!("not APPLY_INPUT: {other:?}"),
    };
    assert_eq!(first_op, second_op, "a retry must never mint a fresh id");
    assert_eq!(first_op, operation_id);
    assert_eq!(
        first_events, payload,
        "the payload is immutable across replay"
    );
    assert_eq!(second_events, payload.as_slice());
    assert_ne!(
        first_request, second_request,
        "the request id is connection-local and must not be reused"
    );
}

#[test]
fn replay_policy_never_downgrades_to_fire_and_forget_input() {
    let mut journal = armed_journal();
    journal
        .submit_at(operation_id(43), tid(1), enter(), 100)
        .expect("queue acknowledged key batch");
    let mut next = 1_u32;

    for now_ms in [110, 130] {
        let (reports, frames) = journal.next_frames_at(&mut next, now_ms);
        assert!(reports.is_empty());
        assert!(matches!(
            frames.as_slice(),
            [FrameKind::Command {
                command: Command::ApplyInput { .. },
                ..
            }]
        ));
        journal.connection_lost();
        assert!(
            journal
                .begin_connection_at(Some(&SERVER_A), true, now_ms + 1)
                .is_empty()
        );
    }
}

/// Only one attempt is outstanding at a time; the second queued paste
/// goes on the wire only after the first resolves.
#[test]
fn attempts_are_serialized_in_submission_order() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("first")).expect("queue");
    submit(&mut journal, tid(1), paste("second")).expect("queue");
    let mut next = 1_u32;
    let (request, _) = send_one(&mut journal, &mut next);
    let (reports, frame) = journal.next_frame(&mut next);
    assert!(reports.is_empty() && frame.is_none(), "{frame:?}");

    let report = journal
        .resolve(request, &CommandResult::Ok)
        .expect("owned request id");
    assert_eq!(report.disposition, ReplayDisposition::Delivered);

    let (second_request, _) = send_one(&mut journal, &mut next);
    assert!(journal.owns(second_request));
}

#[test]
fn delayed_paste_keeps_later_paste_and_enter_in_terminal_order() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("A")).expect("queue A");
    let mut next = 1;
    let (a_request, _) = send_one(&mut journal, &mut next);
    submit(&mut journal, tid(1), paste("B")).expect("queue B");
    submit(&mut journal, tid(1), enter()).expect("queue Enter");
    let (_, blocked) = journal.next_frames(&mut next);
    assert!(blocked.is_empty(), "one Terminal has one active operation");

    journal
        .resolve(a_request, &CommandResult::Ok)
        .expect("resolve A");
    let (_, b_frames) = journal.next_frames(&mut next);
    assert_eq!(b_frames.len(), 1);
    let b_request = command_parts(&b_frames[0]).0;
    assert!(matches!(
        command_parts(&b_frames[0]).1[0],
        InputEvent::Paste(_)
    ));

    journal
        .resolve(b_request, &CommandResult::Ok)
        .expect("resolve B");
    let (_, enter_frames) = journal.next_frames(&mut next);
    assert_eq!(enter_frames.len(), 1);
    assert!(matches!(
        command_parts(&enter_frames[0]).1[0],
        InputEvent::Key(_)
    ));
}

#[test]
fn an_unrelated_terminal_progresses_while_first_terminal_waits() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("held")).expect("queue held");
    let mut next = 1;
    let _ = send_one(&mut journal, &mut next);
    submit(&mut journal, tid(2), paste("other")).expect("queue other");
    let (_, frames) = journal.next_frames(&mut next);
    assert_eq!(frames.len(), 1);
    assert_eq!(command_parts(&frames[0]).2, &tid(2));
}

#[test]
fn unsent_suffix_rolls_back_to_definite_refusal_after_send_failure() {
    let mut journal = armed_journal();
    for terminal in 1..=3 {
        submit(&mut journal, tid(terminal), paste(&terminal.to_string())).expect("queue");
    }
    let (_, frames) = journal.next_frames(&mut 1);
    assert_eq!(frames.len(), 3);

    // Sending A failed. A reached the transport API, but B/C never did.
    journal.rollback_unsent(&frames[1..]);
    let reports = journal.drain_unresolved("the connection send failed");
    let dispositions: Vec<_> = reports.iter().map(|report| report.disposition).collect();
    assert_eq!(
        dispositions,
        vec![
            ReplayDisposition::Unknown,
            ReplayDisposition::Refused,
            ReplayDisposition::Refused,
        ]
    );
}

#[test]
fn rollback_of_replay_suffix_preserves_prior_attempt_uncertainty() {
    let mut journal = armed_journal();
    for terminal in 1..=3 {
        submit(&mut journal, tid(terminal), paste(&terminal.to_string())).expect("queue");
    }
    let _ = journal.next_frames(&mut 1);
    journal.connection_lost();
    assert!(journal.begin_connection(Some(&SERVER_A), true).is_empty());

    let (_, replay_frames) = journal.next_frames(&mut 10);
    assert_eq!(replay_frames.len(), 3);
    journal.rollback_unsent(&replay_frames[1..]);
    let reports = journal.drain_unresolved("the replay send failed");
    assert!(
        reports
            .iter()
            .all(|report| report.disposition == ReplayDisposition::Unknown),
        "every operation retains its prior attempt: {reports:?}"
    );
}

#[test]
fn queue_overflow_is_explicit_and_does_not_consume_capacity() {
    let mut journal = armed_journal();
    let mut refused = None;
    while refused.is_none() {
        let batch = vec![InputEvent::Paste(phux_protocol::input::paste::PasteEvent {
            trust: phux_protocol::input::paste::PasteTrust::Trusted,
            data: vec![b'x'; 60 * 1024],
        })];
        refused = submit(&mut journal, tid(1), batch).err();
    }
    let report = refused.expect("journal byte cap must refuse");
    assert_eq!(report.disposition, ReplayDisposition::Refused);
    assert!(report.notice_line().contains("queue full"));
    let (reports, frames) = journal.next_frames(&mut 1);
    assert_eq!(reports, vec![report]);
    assert_eq!(frames.len(), 1);
    let _ = journal.drain_unresolved("test cleanup");
    assert_eq!(journal.retained_events, 0);
    assert_eq!(journal.retained_bytes, 0);
    assert!(journal.clear_delivery_fence(&tid(1)));
    submit(&mut journal, tid(1), paste("fits")).expect("capacity remains");
}

#[test]
fn queue_event_count_is_bounded_independently_of_payload_bytes() {
    let mut journal = armed_journal();
    for _ in 0..(INPUT_JOURNAL_MAX_EVENTS / phux_protocol::MAX_APPLY_INPUT_EVENTS) {
        submit(
            &mut journal,
            tid(1),
            vec![
                InputEvent::Focus(phux_protocol::input::focus::FocusEvent::Gained);
                phux_protocol::MAX_APPLY_INPUT_EVENTS
            ],
        )
        .expect("within aggregate event cap");
    }
    let report = submit(
        &mut journal,
        tid(1),
        vec![InputEvent::Focus(
            phux_protocol::input::focus::FocusEvent::Gained,
        )],
    )
    .expect_err("aggregate event cap");
    assert_eq!(report.disposition, ReplayDisposition::Refused);
    assert_eq!(journal.retained_events, INPUT_JOURNAL_MAX_EVENTS);
}

#[test]
fn coalesced_mixed_reports_preserve_delivery_certainty() {
    let mut journal = InputReplayJournal::new();
    for index in 0..MAX_DEFERRED_REPORTS {
        journal.defer_report(ReplayReport {
            disposition: ReplayDisposition::Refused,
            operation_id: index.to_string(),
            message: "bounded refusal".to_owned(),
        });
    }
    journal.defer_reports(vec![
        ReplayReport {
            disposition: ReplayDisposition::Unknown,
            operation_id: "unknown".to_owned(),
            message: "socket closed after send".to_owned(),
        },
        ReplayReport {
            disposition: ReplayDisposition::Refused,
            operation_id: "refused".to_owned(),
            message: "queue full".to_owned(),
        },
    ]);

    let reports = journal.take_reports();
    let summaries = &reports[MAX_DEFERRED_REPORTS..];
    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].disposition, ReplayDisposition::Unknown);
    assert!(!summaries[0].notice_line().contains("safe to retype"));
    assert_eq!(summaries[1].disposition, ReplayDisposition::Refused);
    assert!(summaries[1].notice_line().contains("safe to retype"));
    assert!(journal.take_reports().is_empty());
}

fn command_parts(frame: &FrameKind) -> (u32, &[InputEvent], &ResourceId) {
    let FrameKind::Command {
        request_id,
        command:
            Command::ApplyInput {
                terminal_id,
                events,
                ..
            },
    } = frame
    else {
        panic!("not APPLY_INPUT: {frame:?}");
    };
    (*request_id, events, terminal_id)
}

mod stranding;
