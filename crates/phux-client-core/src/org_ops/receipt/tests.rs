#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test assertions"
)]
use super::*;
use crate::org_ops::{OrgJournal, OrgReport};
use phux_protocol::ids::{ResourceKey, ResourceKind, Revision, SessionKey};
use phux_protocol::wire::frame::{Committed, ExitPolicy, LaunchMode, LaunchSpec, SessionLifetime};

fn operation(n: u8) -> OperationId {
    OperationId::new([n; 16]).unwrap()
}
fn session() -> SessionKey {
    SessionKey::new([9; 16]).unwrap()
}
fn result(receipt: Committed) -> CommandResult {
    CommandResult::OkWith(CommandValue::Committed(Box::new(receipt)))
}
fn settle(command: Command, result: &CommandResult) -> OrgReport {
    let mut journal = OrgJournal::default();
    let original = *command.organization_operation().unwrap();
    journal.submit(command, 0).unwrap();
    assert!(journal.begin_connection(true, 0).is_empty());
    assert_eq!(journal.next_frames(&mut || 7).len(), 1);
    let report = journal.resolve(7, result).unwrap();
    assert_eq!(report.operation, original);
    assert!(
        journal.next_frames(&mut || 8).is_empty(),
        "missing evidence never schedules another creation"
    );
    report
}
fn create() -> Command {
    Command::SessionCreate {
        operation: operation(1),
        name: "work".into(),
        lifetime: SessionLifetime::Persistent,
        attrs: Vec::new(),
        launch: None,
    }
}
fn ensure() -> Command {
    Command::SessionEnsure {
        operation: operation(1),
        name: "work".into(),
        lifetime: SessionLifetime::Persistent,
        attrs: Vec::new(),
        launch: None,
    }
}

#[test]
fn journal_preserves_original_operation_when_receipt_acknowledges_a_foreign_create() {
    let report = settle(
        create(),
        &result(Committed::new(operation(2), Revision::new(1)).with_session(Some(session()))),
    );
    assert!(matches!(report.outcome, OrgOutcome::Unknown { .. }));
    assert_eq!(report.operation, operation(1));
}

#[test]
fn missing_created_identity_or_ensure_resolution_is_unknown_not_refusal_or_commit_success() {
    for (command, receipt) in [
        (create(), Committed::new(operation(1), Revision::new(1))),
        (
            ensure(),
            Committed::new(operation(1), Revision::new(1)).with_session(Some(session())),
        ),
    ] {
        assert!(matches!(
            settle(command, &result(receipt)).outcome,
            OrgOutcome::Unknown { .. }
        ));
    }
}

#[test]
fn ensure_existing_and_created_keep_exact_key_and_never_require_observed_execution() {
    for created in [false, true] {
        let mut receipt =
            Committed::new(operation(1), Revision::new(1)).with_session(Some(session()));
        receipt.ensured_created = Some(created);
        let OrgOutcome::Committed(actual) = settle(ensure(), &result(receipt)).outcome else {
            panic!("complete resolution");
        };
        assert_eq!(actual.session, Some(session()));
        assert_eq!(actual.ensured_created, Some(created));
        assert!(actual.target.is_none(), "ensuring is not execution");
    }
    assert!(matches!(
        settle(
            create(),
            &result(Committed::new(operation(1), Revision::new(1)).with_session(Some(session())))
        )
        .outcome,
        OrgOutcome::Committed(_)
    ));
}

#[test]
fn dormant_resource_commit_needs_resource_identity_not_a_live_target() {
    let command = || Command::ResourceCreate {
        operation: operation(1),
        session: session(),
        kind: ResourceKind::Terminal,
        launch: Some(Box::new(LaunchSpec::new(vec!["/bin/sh".into()]))),
        name: None,
        on_exit: ExitPolicy::Retain,
        launch_mode: LaunchMode::Deferred,
        placement: None,
    };
    let receipt = Committed::new(operation(1), Revision::new(1));
    assert!(matches!(
        settle(command(), &result(receipt.clone())).outcome,
        OrgOutcome::Unknown { .. }
    ));
    let resource = ResourceKey::new([3; 16]).unwrap();
    let OrgOutcome::Committed(actual) =
        settle(command(), &result(receipt.with_resource(Some(resource)))).outcome
    else {
        panic!("dormant create is committed intent");
    };
    assert_eq!(actual.resource, Some(resource));
    assert!(actual.target.is_none());
}

#[test]
fn bare_acceptance_or_a_read_value_cannot_become_created_success() {
    for reply in [
        CommandResult::Ok,
        CommandResult::Accepted {
            operation: operation(1),
        },
        CommandResult::OkWith(CommandValue::Json("not a creation receipt".into())),
    ] {
        assert!(matches!(
            settle(create(), &reply).outcome,
            OrgOutcome::Unknown { .. }
        ));
    }
}

#[test]
fn generic_acknowledged_admission_remains_distinct_and_rejects_foreign_operation() {
    let rename = || Command::SessionRename {
        operation: operation(1),
        session: session(),
        name: "renamed".into(),
    };
    assert!(matches!(
        settle(
            rename(),
            &CommandResult::Accepted {
                operation: operation(1)
            }
        )
        .outcome,
        OrgOutcome::Accepted
    ));
    assert!(matches!(
        settle(
            rename(),
            &CommandResult::Accepted {
                operation: operation(2)
            }
        )
        .outcome,
        OrgOutcome::Unknown { .. }
    ));
}
