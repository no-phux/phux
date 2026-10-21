//! Incarnation binding, the retry horizon, and result verdicts.

use super::*;

// ---- point 5: incarnation binding --------------------------------

/// An attempted operation must not be replayed against a different
/// incarnation: the dedupe cache died with the old process, so the only
/// honest verdict is unknown.
#[test]
fn an_attempted_op_strands_unknown_when_the_incarnation_changes() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("ship it")).expect("queue");
    let mut next = 1_u32;
    let _ = send_one(&mut journal, &mut next);

    journal.connection_lost();
    let reports = journal.begin_connection(Some(&SERVER_B), true);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].disposition, ReplayDisposition::Unknown);
    let (more, frame) = journal.next_frame(&mut next);
    assert!(more.is_empty() && frame.is_none());
}

/// A never-sent operation carries no incarnation binding: it is simply
/// sent to whichever server is there now. Nothing was ever written, so
/// there is nothing to double.
#[test]
fn a_never_sent_op_survives_an_incarnation_change() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("queued while offline")).expect("queue");
    journal.connection_lost();
    let reports = journal.begin_connection(Some(&SERVER_B), true);
    assert!(reports.is_empty(), "{reports:?}");
    let mut next = 1_u32;
    let (_, op) = send_one(&mut journal, &mut next);
    let _ = op;
}

/// A reconnected server without `ACKNOWLEDGED_INPUT` (or with a malformed
/// incarnation id) can honor nothing: everything strands, by the
/// attempted/never-sent rule.
#[test]
fn a_server_without_the_feature_strands_everything() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("attempted")).expect("queue");
    let mut next = 1_u32;
    let _ = send_one(&mut journal, &mut next);
    submit(&mut journal, tid(1), paste("never sent")).expect("queue");

    journal.connection_lost();
    let reports = journal.begin_connection(Some(&SERVER_A), false);
    let dispositions: Vec<_> = reports.iter().map(|r| r.disposition).collect();
    assert_eq!(
        dispositions,
        vec![ReplayDisposition::Unknown, ReplayDisposition::Refused]
    );
    assert!(!journal.active());
}

#[test]
fn a_short_server_id_is_not_a_usable_incarnation() {
    let mut journal = InputReplayJournal::new();
    submit(&mut journal, tid(1), paste("queued")).expect("queue");
    let reports = journal.begin_connection(Some(&[0xAA; 4]), true);
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].disposition, ReplayDisposition::Refused);
    assert!(!journal.active());
}

// ---- the horizon --------------------------------------------------

/// An operation older than the horizon is never resent — the server's
/// dedupe record may be evicted, so a resend could write twice. It
/// strands by the attempted/never-sent rule instead.
#[test]
fn the_horizon_strands_instead_of_resending() {
    let mut journal = armed_journal();
    journal
        .submit_at(operation_id(1), tid(1), paste("stale"), 1_000)
        .expect("queue");
    let mut next = 1_u32;
    let (reports, frame) = journal.next_frame_at(
        &mut next,
        1_000 + u64::try_from(INPUT_RETRY_HORIZON.as_millis()).expect("fits"),
    );
    assert!(frame.is_none());
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].disposition, ReplayDisposition::Refused);
}

/// The same expiry applies at reconnect: a stale attempted op is
/// unknown, and the fresh one behind it is refused so it cannot execute
/// against ambiguous pane state.
#[test]
fn reconnect_expiry_fences_later_input_on_that_terminal() {
    let mut journal = armed_journal();
    journal
        .submit_at(operation_id(1), tid(1), paste("stale"), 1_000)
        .expect("queue");
    let mut next = 1_u32;
    let _ = journal.next_frame_at(&mut next, 1_100);
    journal
        .submit_at(operation_id(2), tid(1), paste("fresh"), 600_999)
        .expect("queue");

    journal.connection_lost();
    let reports = journal.begin_connection_at(Some(&SERVER_A), true, 601_000);
    assert_eq!(reports.len(), 2);
    assert_eq!(reports[0].disposition, ReplayDisposition::Unknown);
    assert_eq!(reports[1].disposition, ReplayDisposition::Refused);
    let (more, frame) = journal.next_frame_at(&mut next, 601_000);
    assert!(more.is_empty());
    assert!(frame.is_none(), "later input must remain fenced");
}

#[test]
fn unknown_result_fences_same_terminal_but_not_another_terminal() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("ambiguous")).expect("queue");
    submit(&mut journal, tid(1), paste("must not run")).expect("queue");
    submit(&mut journal, tid(2), paste("independent")).expect("queue");
    let mut next = 1_u32;
    let (_, frames) = journal.next_frames_at(&mut next, 100);
    let first_request = request_id_for_terminal(&frames, &tid(1));
    let report = journal
        .resolve(
            first_request,
            &CommandResult::Error {
                code: ErrorCode::InputDeliveryUnknown,
                message: "writer stalled".to_owned(),
            },
        )
        .expect("owned result");
    assert_eq!(report.disposition, ReplayDisposition::Unknown);
    let deferred = journal.take_reports();
    assert_eq!(deferred.len(), 1);
    assert_eq!(deferred[0].disposition, ReplayDisposition::Refused);
    assert!(journal.must_order_after(&tid(2)));
    assert!(journal.delivery_fenced(&tid(1)));
    assert!(journal.must_order_after(&tid(1)));
    let rejection = submit(&mut journal, tid(1), enter()).expect_err("fenced input");
    assert_eq!(rejection.disposition, ReplayDisposition::Refused);
    assert!(journal.clear_delivery_fence(&tid(1)));
    assert!(!journal.delivery_fenced(&tid(1)));
    submit(&mut journal, tid(1), enter()).expect("fresh input after authoritative read");
}

#[test]
fn terminal_retirement_reports_pending_work_and_reclaims_its_fence() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("attempted")).expect("queue");
    submit(&mut journal, tid(2), paste("independent")).expect("queue");
    let mut next = 1_u32;
    let (_, frames) = journal.next_frames_at(&mut next, 100);
    let first_request = request_id_for_terminal(&frames, &tid(1));
    let _ = journal.resolve(
        first_request,
        &CommandResult::Error {
            code: ErrorCode::InternalError,
            message: "ambiguous".to_owned(),
        },
    );
    assert!(journal.delivery_fenced(&tid(1)));

    let reports = journal.retire_terminal(&tid(1), "the terminal closed");
    assert!(
        reports.is_empty(),
        "the unknown result already stranded followers"
    );
    assert!(!journal.delivery_fenced(&tid(1)));
    assert!(journal.must_order_after(&tid(2)));
}

// ---- verdicts -----------------------------------------------------

/// The result classification is the mobile reference's: OK is the
/// receipt, `INPUT_DELIVERY_UNKNOWN` is terminal-unknown, anything else
/// wrote nothing.
#[test]
fn verdicts_mirror_the_reference_classification() {
    for (result, expected) in [
        (CommandResult::Ok, ReplayDisposition::Delivered),
        (
            CommandResult::Error {
                code: ErrorCode::InputDeliveryUnknown,
                message: "writer stalled".to_owned(),
            },
            ReplayDisposition::Unknown,
        ),
        (
            CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                message: "another client holds the slot".to_owned(),
            },
            ReplayDisposition::Refused,
        ),
        (
            CommandResult::Error {
                code: ErrorCode::UnsafePaste,
                message: "policy".to_owned(),
            },
            ReplayDisposition::Refused,
        ),
        (
            CommandResult::Error {
                code: ErrorCode::InternalError,
                message: "post-handoff failure".to_owned(),
            },
            ReplayDisposition::Unknown,
        ),
    ] {
        let mut journal = armed_journal();
        submit(&mut journal, tid(1), paste("x")).expect("queue");
        let mut next = 1_u32;
        let (request, _) = send_one(&mut journal, &mut next);
        let report = journal.resolve(request, &result).expect("owned");
        assert_eq!(report.disposition, expected, "{result:?}");
    }
}

#[test]
fn ordinary_error_on_a_retry_never_claims_safe_to_retype() {
    for code in [ErrorCode::ResourceExhausted, ErrorCode::InputNotWritten] {
        let mut journal = armed_journal();
        submit(&mut journal, tid(1), paste("x")).expect("queue");
        let mut next = 1;
        let _ = send_one(&mut journal, &mut next);
        journal.connection_lost();
        assert!(journal.begin_connection(Some(&SERVER_A), true).is_empty());
        let (retry_request, _) = send_one(&mut journal, &mut next);
        let report = journal
            .resolve(
                retry_request,
                &CommandResult::Error {
                    code,
                    message: "retry refusal".to_owned(),
                },
            )
            .expect("retry result");
        assert_eq!(report.disposition, ReplayDisposition::Unknown, "{code:?}");
        assert!(!report.notice_line().contains("safe to retype"));
    }
}

#[test]
fn oversized_key_text_is_refused_before_it_reaches_transport() {
    use phux_protocol::wire::frame::MAX_APPLY_INPUT_COMMAND_BODY;

    let InputEvent::Key(mut key) = enter().remove(0) else {
        panic!("enter helper must return a key")
    };
    key.text = Some("x".repeat(MAX_APPLY_INPUT_COMMAND_BODY));
    let mut journal = armed_journal();
    let report =
        submit(&mut journal, tid(1), vec![InputEvent::Key(key)]).expect_err("oversized key text");
    assert_eq!(report.disposition, ReplayDisposition::Refused);
    assert!(report.message.contains("protocol cap"));
}

/// A result for a request id the journal does not own is not consumed —
/// it belongs to some other correlation and must fall through to
/// whatever owns it.
#[test]
fn foreign_request_ids_are_not_consumed() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("x")).expect("queue");
    let mut next = 1_u32;
    let (request, _) = send_one(&mut journal, &mut next);
    assert!(journal.resolve(request + 7, &CommandResult::Ok).is_none());
    assert!(journal.owns(request), "the real attempt must stay pending");
}

/// Final teardown: whatever is left resolves by the attempted/never-sent
/// rule so the user hears about every journaled paste exactly once.
#[test]
fn drain_unresolved_reports_every_op_once() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("attempted")).expect("queue");
    let mut next = 1_u32;
    let _ = send_one(&mut journal, &mut next);
    submit(&mut journal, tid(2), paste("never sent")).expect("queue");
    let reports = journal.drain_unresolved("the reconnect window closed");
    let dispositions: Vec<_> = reports.iter().map(|r| r.disposition).collect();
    assert_eq!(
        dispositions,
        vec![ReplayDisposition::Unknown, ReplayDisposition::Refused]
    );
    assert!(journal.drain_unresolved("again").is_empty());
}

/// The session-switch shape: same connection re-enters `main_loop`, so
/// `begin_connection` runs again with the SAME incarnation while an
/// attempt is outstanding (its reply was discarded by the switch drain).
/// The op must be resent under its original id, not stranded — the
/// server's cache answers it.
#[test]
fn a_same_incarnation_reentry_replays_an_outstanding_attempt() {
    let mut journal = armed_journal();
    submit(&mut journal, tid(1), paste("mid-switch")).expect("queue");
    let mut next = 1_u32;
    let (_, first_op) = send_one(&mut journal, &mut next);

    // No connection_lost: the socket survived; only the correlation was
    // dropped by the drain.
    let reports = journal.begin_connection(Some(&SERVER_A), true);
    assert!(reports.is_empty(), "{reports:?}");
    let (_, second_op) = send_one(&mut journal, &mut next);
    assert_eq!(first_op, second_op);
}
