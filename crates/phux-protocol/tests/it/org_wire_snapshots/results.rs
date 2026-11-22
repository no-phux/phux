//! One golden fixture per organization result.

use super::{
    arrangement, document, holder, op, other_session, resource, result, rev, session, target,
};
use phux_protocol::ids::{ActivationId, AuthorityId, ResourceId, ServerInstance, SessionId};
use phux_protocol::kinds::Verb;
use phux_protocol::wire::frame::{
    ActivationInfo, ArrangementOwner, ArrangementValue, CommandResult, CommandValue, Committed,
    Delegation, ExecState, FrameKind, OrganizationPage, Outcome, RecordRevision, Refusal,
    ResourceRecord, SessionRecord,
};

#[allow(clippy::too_many_lines, reason = "one fixture per organization result")]
pub(super) fn result_fixtures() -> Vec<(&'static str, FrameKind)> {
    let refused = |refusal| result(CommandResult::Refused(refusal));
    let page = OrganizationPage::new(AuthorityId::new([0xAA; 16]).unwrap(), rev(40), 77)
        .with_intake(Some(other_session()))
        .with_sessions(vec![
            SessionRecord::new(session(), "work", rev(4))
                .with_members(1)
                .with_live(Some(SessionId::new(1))),
        ])
        .with_resources(vec![
            ResourceRecord::new(resource(), session(), rev(2))
                .with_activation(Some(
                    ActivationInfo::new(
                        ActivationId::new(3),
                        ServerInstance::new([0x11; 16]),
                        ExecState::Completed,
                    )
                    .with_outcome(Some(Outcome::Exited(0))),
                ))
                .with_live(Some(ResourceId::local(0x2A))),
        ])
        .with_totals(1, 1, 0)
        .with_next_cursor(Some(b"n".to_vec()));
    vec![
        (
            "snap_org_result_accepted",
            result(CommandResult::Accepted { operation: op() }),
        ),
        (
            "snap_org_result_refused_unauthorized",
            refused(Refusal::Unauthorized {
                verb: Verb::Organize,
            }),
        ),
        (
            "snap_org_result_refused_not_found",
            refused(Refusal::NotFound),
        ),
        (
            "snap_org_result_refused_conflict",
            refused(Refusal::Conflict { current: rev(9) }),
        ),
        (
            "snap_org_result_refused_stale",
            refused(Refusal::Stale {
                current: Some(ActivationId::new(4)),
            }),
        ),
        (
            "snap_org_result_refused_not_empty",
            refused(Refusal::NotEmpty { members: 2 }),
        ),
        (
            "snap_org_result_refused_unsupported",
            refused(Refusal::Unsupported {
                capability: "workspaces".to_owned(),
            }),
        ),
        (
            "snap_org_result_refused_invalid_argument",
            refused(Refusal::InvalidArgument {
                field: "resource".to_owned(),
                reason: "running".to_owned(),
            }),
        ),
        (
            "snap_org_result_refused_name_taken",
            refused(Refusal::NameTaken),
        ),
        (
            "snap_org_result_refused_execution_uncertain",
            refused(Refusal::ExecutionUncertain),
        ),
        (
            "snap_org_result_refused_lease_held",
            refused(Refusal::LeaseHeld { holder: holder() }),
        ),
        (
            "snap_org_result_refused_duplicate_operation",
            refused(Refusal::DuplicateOperation),
        ),
        (
            "snap_org_result_refused_store_failure",
            refused(Refusal::StoreFailure {
                reason: "disk full".to_owned(),
            }),
        ),
        (
            "snap_org_result_committed",
            result(CommandResult::OkWith(CommandValue::Committed(Box::new(
                Committed::new(op(), rev(5))
                    .with_resource(Some(resource()))
                    .with_target(Some(target()))
                    .with_revisions(vec![RecordRevision {
                        key: *session().as_bytes(),
                        revision: rev(5),
                    }]),
            )))),
        ),
        (
            "snap_org_result_committed_kill",
            result(CommandResult::OkWith(CommandValue::Committed(Box::new(
                Committed::new(op(), rev(0)).with_members(Some(3)),
            )))),
        ),
        (
            "snap_org_result_arrangement",
            result(CommandResult::OkWith(CommandValue::Arrangement(Box::new(
                ArrangementValue::new(
                    arrangement(),
                    ArrangementOwner::Session(session()),
                    rev(1),
                    document(),
                ),
            )))),
        ),
        (
            "snap_org_result_delegation",
            result(CommandResult::OkWith(CommandValue::Delegation(Box::new(
                Delegation::new(
                    "delegated:0f0f".to_owned(),
                    session(),
                    "/run/phux/agent-0f0f.sock".to_owned(),
                    1_700_000_000_000,
                ),
            )))),
        ),
        (
            "snap_org_result_organization_page",
            result(CommandResult::OkWith(CommandValue::OrganizationPage(
                Box::new(page),
            ))),
        ),
    ]
}
