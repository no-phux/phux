#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use super::*;
use phux_protocol::ids::{Revision, SessionKey};

fn op(n: u8) -> OperationId {
    OperationId::new([n; 16]).unwrap()
}

fn skey(n: u8) -> SessionKey {
    SessionKey::new([n; 16]).unwrap()
}

fn rename(n: u8, name: &str) -> Command {
    Command::SessionRename {
        operation: op(n),
        session: skey(1),
        name: name.to_owned(),
    }
}

fn committed(n: u8) -> CommandResult {
    CommandResult::OkWith(CommandValue::Committed(Box::new(Committed::new(
        op(n),
        Revision::new(4),
    ))))
}

fn sequence() -> impl FnMut() -> u32 {
    let mut next = 0;
    move || {
        next += 1;
        next
    }
}

#[test]
fn a_reconnect_resends_the_same_operation_id_and_payload() {
    let mut journal = OrgJournal::default();
    journal.submit(rename(1, "work"), 0).unwrap();
    assert!(
        journal.next_frames(&mut sequence()).is_empty(),
        "no connection"
    );
    assert!(journal.begin_connection(true, 0).is_empty());
    let first = journal.next_frames(&mut sequence());
    assert_eq!(first.len(), 1);
    assert!(journal.next_frames(&mut sequence()).is_empty(), "in flight");

    journal.connection_lost();
    assert!(journal.begin_connection(true, 1_000).is_empty());
    let second = journal.next_frames(&mut sequence());
    let (FrameKind::Command { command: a, .. }, FrameKind::Command { command: b, .. }) =
        (&first[0], &second[0])
    else {
        panic!("commands")
    };
    assert_eq!(a, b, "the resend is byte-identical, operation id included");

    let report = journal.resolve(1, &committed(1)).unwrap();
    assert_eq!(report.operation, op(1));
    assert!(report.outcome.succeeded());
    assert!(journal.is_empty());
}

#[test]
fn a_resubmit_dedupes_and_a_reused_id_with_new_payload_is_refused() {
    let mut journal = OrgJournal::default();
    journal.submit(rename(1, "work"), 0).unwrap();
    journal.submit(rename(1, "work"), 5).unwrap();
    assert_eq!(journal.len(), 1);
    assert_eq!(
        journal.submit(rename(1, "play"), 5),
        Err(SubmitError::DuplicateOperation)
    );
    assert_eq!(
        journal.submit(Command::Shutdown, 0),
        Err(SubmitError::NotAnOperation)
    );
}

#[test]
fn past_the_horizon_an_attempted_operation_is_unknown_and_never_resent() {
    let mut journal = OrgJournal::new(1_000);
    journal.submit(rename(1, "work"), 0).unwrap();
    journal.submit(rename(2, "other"), 0).unwrap();
    journal.begin_connection(true, 0);
    let frames = journal.next_frames(&mut sequence());
    assert_eq!(frames.len(), 2);
    journal.resolve(2, &committed(2));
    journal.connection_lost();
    let reports = journal.begin_connection(true, 5_000);
    assert_eq!(reports.len(), 1);
    assert!(matches!(reports[0].outcome, OrgOutcome::Unknown { .. }));
    assert!(journal.next_frames(&mut sequence()).is_empty());
}

#[test]
fn a_server_without_organization_strands_sent_as_unknown_and_unsent_as_not_sent() {
    let mut journal = OrgJournal::default();
    journal.submit(rename(1, "work"), 0).unwrap();
    journal.begin_connection(true, 0);
    journal.next_frames(&mut sequence());
    journal.connection_lost();
    journal.submit(rename(2, "other"), 0).unwrap();
    let reports = journal.begin_connection(false, 10);
    assert_eq!(reports.len(), 2);
    assert!(matches!(reports[0].outcome, OrgOutcome::Unknown { .. }));
    assert!(matches!(reports[1].outcome, OrgOutcome::NotSent { .. }));
}

#[test]
fn another_authority_strands_attempted_operations() {
    let authority = |n| AuthorityId::new([n; 16]).unwrap();
    let mut journal = OrgJournal::default();
    journal.submit(rename(1, "work"), 0).unwrap();
    journal.begin_connection(true, 0);
    assert!(journal.set_authority(authority(1)).is_empty());
    journal.next_frames(&mut sequence());
    journal.connection_lost();
    journal.begin_connection(true, 10);
    let reports = journal.set_authority(authority(2));
    assert_eq!(reports.len(), 1);
    assert!(matches!(reports[0].outcome, OrgOutcome::Unknown { .. }));
}

/// An unreadable reply, or a newer server's error code, does not say whether
/// the command committed: unknown, never an error the caller may retry past.
#[test]
fn an_unreadable_or_newer_error_is_an_unknown_outcome() {
    for code in [ErrorCode::UNREADABLE_REPLY, ErrorCode::Unknown(299)] {
        let outcome = OrgOutcome::from_result(&CommandResult::Error {
            code,
            message: "unreadable".to_owned(),
        });
        assert!(matches!(outcome, OrgOutcome::Unknown { .. }), "{code:?}");
    }
    let refused = OrgOutcome::from_result(&CommandResult::Error {
        code: ErrorCode::Unsupported,
        message: "unsupported".to_owned(),
    });
    assert!(matches!(refused, OrgOutcome::Error { .. }));
}
