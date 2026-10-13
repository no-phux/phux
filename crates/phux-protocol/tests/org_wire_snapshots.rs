//! Snapshot tests for the organization commands, results, and events
//! (`docs/spec/L1.md` §5.7): one golden hex dump per shape, so a byte change
//! to any of them surfaces in review. The frames outside §5.7 keep their
//! goldens in `frame_wire_snapshots.rs`.

#![allow(
    clippy::unwrap_used,
    clippy::missing_const_for_fn,
    reason = "fixtures: unwrap a known-good key; helpers stay plain fns"
)]

use bytes::BytesMut;
use phux_protocol::ids::{
    ActivationId, ArrangementKey, AuthorityId, ClientId, IdempotencyKey, LeaseEpoch, PrincipalId,
    ResourceId, ResourceKey, ResourceKind, Revision, SatelliteHost, ServerInstance, SessionId,
    SessionKey, WorkspaceKey,
};
use phux_protocol::kinds::Verb;
use phux_protocol::wire::frame::{
    ActivationInfo, ActorRef, AgentEvent, ArrangementDoc, ArrangementGroup, ArrangementNode,
    ArrangementOwner, ArrangementValue, Command, CommandResult, CommandValue, Committed,
    Delegation, EntryRef, EntryTarget, ExecState, ExitPolicy, FrameKind, LaunchMode, LaunchSpec,
    LiveTarget, MoveItem, OrgFilter, OrganizationPage, Outcome, RecordRevision, Refusal,
    ResourceRecord, SessionLifetime, SessionRecord, SplitDirection, TerminateMode,
};

/// `xxd`-style dump, the same rendering `frame_wire_snapshots.rs` uses.
fn hex_dump(bytes: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut out = String::new();
    for (chunk_idx, chunk) in bytes.chunks(16).enumerate() {
        let _ = write!(out, "{:08x} |", chunk_idx * 16);
        for (i, b) in chunk.iter().enumerate() {
            if i == 8 {
                out.push(' ');
            }
            let _ = write!(out, " {b:02x}");
        }
        for i in 0..16 - chunk.len() {
            if chunk.len() + i == 8 {
                out.push(' ');
            }
            out.push_str("   ");
        }
        out.push_str(" |");
        for b in chunk {
            out.push(if (0x20..=0x7e).contains(b) {
                char::from(*b)
            } else {
                '.'
            });
        }
        out.push('\n');
    }
    out
}

fn dump_frame(frame: &FrameKind) -> String {
    let mut buf = BytesMut::new();
    frame.encode(&mut buf);
    let (decoded, rest) = FrameKind::decode(&buf).unwrap();
    assert!(rest.is_empty());
    assert_eq!(&decoded, frame, "the golden frame round-trips");
    hex_dump(&buf)
}

fn op() -> IdempotencyKey {
    IdempotencyKey::new([0x0A; 16]).unwrap()
}
fn session() -> SessionKey {
    SessionKey::new([0x5E; 16]).unwrap()
}
fn other_session() -> SessionKey {
    SessionKey::new([0x5F; 16]).unwrap()
}
fn resource() -> ResourceKey {
    ResourceKey::new([0x7E; 16]).unwrap()
}
fn arrangement() -> ArrangementKey {
    ArrangementKey::new([0xA7; 16]).unwrap()
}
const fn rev(n: u64) -> Revision {
    Revision::new(n)
}
fn target() -> LiveTarget {
    LiveTarget {
        resource: ResourceId::local(0x2A),
        instance: ServerInstance::new([0x11; 16]),
        activation: ActivationId::new(3),
    }
}
fn launch() -> LaunchSpec {
    LaunchSpec::new(vec!["zsh".to_owned(), "-l".to_owned()])
        .with_cwd(Some("/src".to_owned()))
        .with_env(vec![("LANG".to_owned(), "C".to_owned())])
}
fn document() -> ArrangementDoc {
    ArrangementDoc {
        groups: vec![ArrangementGroup {
            id: [0x61; 16],
            name: Some("main".to_owned()),
            tree: ArrangementNode::Split {
                dir: SplitDirection::Horizontal,
                ratio: 0.5,
                first: Box::new(ArrangementNode::Leaf(EntryRef {
                    entry: [0xE1; 16],
                    target: EntryTarget::Member(resource()),
                })),
                second: Box::new(ArrangementNode::Leaf(EntryRef {
                    entry: [0xE2; 16],
                    target: EntryTarget::Satellite {
                        host: SatelliteHost::new("edge"),
                        id: 9,
                    },
                })),
            },
        }],
    }
}
fn holder() -> ActorRef {
    ActorRef::new(ClientId::new(3)).with_principal(PrincipalId::new([0x91; 16]))
}
const fn command(command: Command) -> FrameKind {
    FrameKind::Command {
        request_id: 0x21,
        command,
    }
}
const fn result(result: CommandResult) -> FrameKind {
    FrameKind::CommandResult {
        request_id: 0x21,
        result,
    }
}
const fn event(event: AgentEvent) -> FrameKind {
    FrameKind::Event {
        terminal: None,
        event,
        stamp: None,
    }
}

#[allow(
    clippy::too_many_lines,
    reason = "one fixture per organization command, kept as one flat table"
)]
fn command_fixtures() -> Vec<(&'static str, FrameKind)> {
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
                expected: rev(4),
                name: "play".to_owned(),
            }),
        ),
        (
            "snap_org_session_set_lifetime",
            command(Command::SessionSetLifetime {
                operation: op(),
                session: session(),
                expected: rev(4),
                lifetime: SessionLifetime::Persistent,
            }),
        ),
        (
            "snap_org_session_delete",
            command(Command::SessionDelete {
                operation: op(),
                session: session(),
                expected: rev(4),
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
                expected: rev(2),
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
                expected: rev(2),
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
            command(Command::InboxResolve {
                operation: op(),
                session: session(),
                expected: rev(4),
            }),
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

#[allow(clippy::too_many_lines, reason = "one fixture per organization result")]
fn result_fixtures() -> Vec<(&'static str, FrameKind)> {
    let refused = |refusal| result(CommandResult::Refused(refusal));
    let page = OrganizationPage::new(AuthorityId::new([0xAA; 16]).unwrap(), rev(40), 77)
        .with_inbox(Some(other_session()))
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

#[allow(
    clippy::too_many_lines,
    reason = "one fixture per organization event, kept as one flat table"
)]
fn event_fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        (
            "snap_org_event_session_created",
            event(AgentEvent::SessionCreated {
                session: session(),
                name: "work".to_owned(),
                lifetime: SessionLifetime::Persistent,
                revision: rev(1),
            }),
        ),
        (
            "snap_org_event_session_renamed",
            event(AgentEvent::SessionRenamed {
                session: session(),
                name: "play".to_owned(),
                revision: rev(2),
            }),
        ),
        (
            "snap_org_event_session_deleted",
            event(AgentEvent::SessionDeleted {
                session: session(),
                name: "play".to_owned(),
                revision: rev(3),
            }),
        ),
        (
            "snap_org_event_session_lifetime_changed",
            event(AgentEvent::SessionLifetimeChanged {
                session: session(),
                lifetime: SessionLifetime::Ephemeral,
                revision: rev(3),
            }),
        ),
        (
            "snap_org_event_membership_changed",
            event(AgentEvent::MembershipChanged {
                resource: resource(),
                from: session(),
                to: other_session(),
                from_revision: rev(5),
                to_revision: rev(8),
            }),
        ),
        (
            "snap_org_event_resource_created",
            event(AgentEvent::ResourceCreated {
                resource: resource(),
                session: session(),
                kind: ResourceKind::Terminal,
            }),
        ),
        (
            "snap_org_event_resource_removed",
            event(AgentEvent::ResourceRemoved {
                resource: resource(),
                session: session(),
                kind: ResourceKind::AgentSession,
            }),
        ),
        (
            "snap_org_event_activation_changed",
            event(AgentEvent::ActivationChanged {
                resource: resource(),
                activation: ActivationId::new(3),
                state: ExecState::Completed,
                outcome: Some(Outcome::Signaled(9)),
            }),
        ),
        (
            "snap_org_event_arrangement_changed",
            event(AgentEvent::ArrangementChanged {
                arrangement: arrangement(),
                owner: ArrangementOwner::Workspace(WorkspaceKey::new([0xB0; 16]).unwrap()),
                revision: Some(rev(2)),
            }),
        ),
        (
            "snap_org_event_arrangement_deleted",
            event(AgentEvent::ArrangementChanged {
                arrangement: arrangement(),
                owner: ArrangementOwner::Session(session()),
                revision: None,
            }),
        ),
        (
            "snap_org_event_lease_changed",
            event(AgentEvent::LeaseChanged {
                resource: resource(),
                epoch: LeaseEpoch::new(6),
                holder: Some(holder().with_on_behalf_of(PrincipalId::new([0x92; 16]))),
            }),
        ),
        (
            "snap_org_event_intake_changed",
            event(AgentEvent::IntakeChanged {
                session: session(),
                awaiting_organization: false,
                revision: rev(6),
            }),
        ),
        (
            "snap_org_event_resource_renamed",
            event(AgentEvent::ResourceRenamed {
                resource: resource(),
                name: Some("logs".to_owned()),
                revision: rev(3),
            }),
        ),
    ]
}

/// Every organization fixture round-trips and matches its golden.
#[test]
fn org_wire_snapshots_match_goldens() {
    for (name, frame) in command_fixtures()
        .into_iter()
        .chain(result_fixtures())
        .chain(event_fixtures())
    {
        insta::assert_snapshot!(name, dump_frame(&frame));
    }
}
