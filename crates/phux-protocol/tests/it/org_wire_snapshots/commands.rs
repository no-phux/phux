//! One golden fixture per organization command.

use super::{
    arrangement, command, document, launch, op, other_session, resource, rev, session, target,
};
use phux_protocol::ids::{IdempotencyKey, ResourceKind};
use phux_protocol::wire::frame::{
    Command, ExitPolicy, FrameKind, LaunchMode, MoveItem, OrgFilter, RecordRevision,
    SessionLifetime, TerminateMode,
};

#[allow(
    clippy::too_many_lines,
    reason = "one fixture per organization command, kept as one flat table"
)]
pub(super) fn command_fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        (
            "snap_org_get_organization",
            command(Command::GetOrganization {
                filter: OrgFilter::Session(session()),
                page_size: 100,
                cursor: Some(b"c1".to_vec()),
            }),
        ),
        (
            "snap_org_session_create",
            command(Command::SessionCreate {
                operation: op(),
                name: "work".to_owned(),
                lifetime: SessionLifetime::Ephemeral,
                attrs: vec![("project".to_owned(), "phux".to_owned())],
                launch: Some(Box::new(launch())),
            }),
        ),
        (
            "snap_org_session_rename",
            command(Command::SessionRename {
                operation: op(),
                session: session(),
                name: "play".to_owned(),
            }),
        ),
        (
            "snap_org_session_set_project",
            command(Command::SessionSetProject {
                operation: op(),
                session: session(),
                project: Some("phux".to_owned()),
            }),
        ),
        (
            "snap_org_session_clear_project",
            command(Command::SessionSetProject {
                operation: op(),
                session: session(),
                project: None,
            }),
        ),
        (
            "snap_org_session_set_lifetime",
            command(Command::SessionSetLifetime {
                operation: op(),
                session: session(),
                lifetime: SessionLifetime::Persistent,
            }),
        ),
        (
            "snap_org_session_delete",
            command(Command::SessionDelete {
                operation: op(),
                session: session(),
            }),
        ),
        (
            "snap_org_session_kill",
            command(Command::SessionKill {
                operation: op(),
                session: session(),
            }),
        ),
        (
            "snap_org_resource_create",
            command(Command::ResourceCreate {
                operation: op(),
                session: session(),
                kind: ResourceKind::Terminal,
                launch: Some(Box::new(launch())),
                name: Some("build".to_owned()),
                on_exit: ExitPolicy::Retain,
                launch_mode: LaunchMode::Immediate,
            }),
        ),
        (
            "snap_org_resource_create_deferred",
            command(Command::ResourceCreate {
                operation: op(),
                session: session(),
                kind: ResourceKind::Terminal,
                launch: Some(Box::new(launch())),
                name: Some("build".to_owned()),
                on_exit: ExitPolicy::Retain,
                launch_mode: LaunchMode::Deferred,
            }),
        ),
        (
            "snap_org_resource_move",
            command(Command::ResourceMove {
                operation: op(),
                resource: resource(),
                from: session(),
                from_expected: rev(4),
                to: other_session(),
                to_expected: rev(7),
            }),
        ),
        (
            "snap_org_resource_move_many",
            command(Command::ResourceMoveMany {
                operation: op(),
                items: vec![MoveItem {
                    resource: resource(),
                    from: session(),
                    from_expected: rev(4),
                }],
                to: other_session(),
                to_expected: rev(7),
            }),
        ),
        (
            "snap_org_resource_rename",
            command(Command::ResourceRename {
                operation: op(),
                resource: resource(),
                name: Some("logs".to_owned()),
            }),
        ),
        (
            "snap_org_resource_terminate",
            command(Command::ResourceTerminate {
                operation: op(),
                target: target(),
                mode: TerminateMode::Force,
            }),
        ),
        (
            "snap_org_resource_remove",
            command(Command::ResourceRemove {
                operation: op(),
                resource: resource(),
            }),
        ),
        (
            "snap_org_resource_relaunch",
            command(Command::ResourceRelaunch {
                operation: op(),
                resource: resource(),
                expected: rev(2),
            }),
        ),
        (
            "snap_org_inbox_resolve",
            command(Command::intake_resolve(op(), session(), rev(4))),
        ),
        (
            "snap_org_organization_undo",
            command(Command::OrganizationUndo {
                operation: op(),
                undo: IdempotencyKey::new([0x0B; 16]).unwrap(),
                expected: vec![RecordRevision {
                    key: *session().as_bytes(),
                    revision: rev(5),
                }],
            }),
        ),
        (
            "snap_org_arrangement_get",
            command(Command::ArrangementGet {
                arrangement: arrangement(),
            }),
        ),
        (
            "snap_org_arrangement_replace",
            command(Command::ArrangementReplace {
                operation: op(),
                arrangement: arrangement(),
                expected: rev(1),
                document: document(),
            }),
        ),
        (
            "snap_org_delegate_agent",
            command(Command::DelegateAgent {
                operation: op(),
                session: session(),
                ttl_secs: 3600,
            }),
        ),
        (
            "snap_org_revoke_delegation",
            command(Command::RevokeDelegation {
                operation: op(),
                credential_id: "delegated:0f0f".to_owned(),
            }),
        ),
    ]
}
