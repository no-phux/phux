//! Producer, attach, state-reply, bootstrap, and event-journal fixtures.

use phux_protocol::caps::BootstrapStreamProfile;
use phux_protocol::ids::{
    BootstrapId, ClientId, GroupId, IdempotencyKey, ResourceId, ResourceKind, SatelliteHost,
    SessionId, StreamId, WindowId,
};
use phux_protocol::wire::frame::{
    ActorRef, AgentEvent, CloseReason, Command, CommandResult, CommandValue, ControlAction,
    ErrorCode, EventStamp, FrameKind, ResourceLifecycle, Scope, SpawnError, SpawnResource,
    SpawnResult,
};
use phux_protocol::wire::info::{
    AgentFacet, ExitFacet, HostInventory, HostSessionInfo, ResourceInfo, SessionInfo,
    SessionSnapshot,
};

#[allow(clippy::too_many_lines)]
pub(super) fn fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        // APPEND_RESOURCE_OUTPUT (tag 0x1a): the producer verb, one complete
        // AgentEventsJsonlV1 record.
        (
            "snap_command_append_resource_output",
            FrameKind::Command {
                request_id: 0x0000_0010,
                command: Command::AppendResourceOutput {
                    terminal_id: ResourceId::local(0x0000_002B),
                    bytes: b"{\"type\":\"prompt\",\"data\":{\"len\":12}}\n".to_vec(),
                },
            },
        ),
        // COMMAND_RESULT error codes minted for producer-fed resources.
        (
            "snap_command_result_error_wrong_resource_kind",
            FrameKind::CommandResult {
                request_id: 0x0000_0010,
                result: CommandResult::Error {
                    code: ErrorCode::WrongResourceKind,
                    message: "terminals are fed by their pty".to_owned(),
                },
            },
        ),
        (
            "snap_command_result_error_not_producer",
            FrameKind::CommandResult {
                request_id: 0x0000_0011,
                result: CommandResult::Error {
                    code: ErrorCode::NotProducer,
                    message: String::new(),
                },
            },
        ),
        (
            "snap_command_result_error_record_invalid",
            FrameKind::CommandResult {
                request_id: 0x0000_0012,
                result: CommandResult::Error {
                    code: ErrorCode::RecordInvalid,
                    message: "unknown record type".to_owned(),
                },
            },
        ),
        (
            "snap_command_result_error_overflow",
            FrameKind::CommandResult {
                request_id: 0x0000_0013,
                result: CommandResult::Error {
                    code: ErrorCode::Overflow,
                    message: String::new(),
                },
            },
        ),
        // ATTACHED carrying the trailing resource-facet list: one Terminal
        // and one AgentSession child bound to it.
        (
            "snap_attached_with_agent_session_facets",
            FrameKind::Attached {
                attach_id: 1,
                initial_client_id: ClientId::new(7),
                snapshot: SessionSnapshot::new(
                    SessionId::new(1),
                    WindowId::new(10),
                    ResourceId::local(0x2A),
                )
                .with_resources(vec![
                    ResourceInfo::new(ResourceId::local(0x2A), WindowId::new(10), 80, 24),
                    ResourceInfo::resource(ResourceId::local(0x2B), ResourceKind::AgentSession)
                        .with_parent(Some(ResourceId::local(0x2A)))
                        .with_agent(Some(
                            AgentFacet::new("claude", "working")
                                .with_native_id(Some("session-42".to_owned())),
                        )),
                ]),
            },
        ),
        // GET_STATE reply from a federation hub carrying the trailing
        // host-session inventory: no resource facets (a zero-count facet
        // list anchors the position), one reachable satellite with one
        // session, one unreachable satellite.
        (
            "snap_command_result_state_with_host_inventory",
            FrameKind::CommandResult {
                request_id: 0x0000_0014,
                result: CommandResult::OkWith(CommandValue::State(
                    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
                        .with_sessions(vec![
                            SessionInfo::new(SessionId::new(1), "work").with_window_count(1),
                        ])
                        .with_hosts(vec![
                            HostInventory::reachable(
                                SatelliteHost::new("edge"),
                                vec![
                                    HostSessionInfo::new(SessionId::new(1), "build")
                                        .with_window_count(2)
                                        .with_pane_count(3)
                                        .with_attached_client_count(1)
                                        .with_active_resource(Some(ResourceId::satellite(
                                            SatelliteHost::new("edge"),
                                            7,
                                        ))),
                                ],
                            ),
                            HostInventory::unreachable(SatelliteHost::new("down"), "link is down"),
                        ]),
                )),
            },
        ),
        // GET_STATE reply carrying all three trailing lists in wire
        // order (ADR-0105): a zero-count resource-facet list, one
        // satellite in the host inventory, then one keep-empty session row.
        (
            "snap_command_result_state_with_keep_empty_session",
            FrameKind::CommandResult {
                request_id: 0x0000_0015,
                result: CommandResult::OkWith(CommandValue::State(
                    SessionSnapshot::new(SessionId::new(2), WindowId::new(0), ResourceId::local(0))
                        .with_sessions(vec![
                            SessionInfo::new(SessionId::new(1), "work").with_window_count(1),
                            SessionInfo::new(SessionId::new(2), "parked").with_keep_empty(true),
                        ])
                        .with_hosts(vec![HostInventory::reachable(
                            SatelliteHost::new("edge"),
                            vec![HostSessionInfo::new(SessionId::new(1), "build")],
                        )]),
                )),
            },
        ),
        // GET_STATE reply with a keep-empty session and no hosts: the facet
        // and host lists are written as zero-count anchors.
        (
            "snap_command_result_state_with_keep_empty_no_hosts",
            FrameKind::CommandResult {
                request_id: 0x0000_0016,
                result: CommandResult::OkWith(CommandValue::State(
                    SessionSnapshot::new(SessionId::new(2), WindowId::new(0), ResourceId::local(0))
                        .with_sessions(vec![
                            SessionInfo::new(SessionId::new(2), "parked").with_keep_empty(true),
                        ]),
                )),
            },
        ),
        // BOOTSTRAP_BEGIN for an AgentSession stream: codec tag 3, raw
        // output mode, no grid.
        (
            "snap_bootstrap_begin_agent_events_jsonl_v1",
            FrameKind::BootstrapBegin {
                terminal_id: ResourceId::local(0x2B),
                stream_id: StreamId::new(1).unwrap(),
                bootstrap_id: BootstrapId::new(1).unwrap(),
                profile: BootstrapStreamProfile::AgentEventsJsonlV1,
                cols: 0,
                rows: 0,
                base_seq: 0,
            },
        ),
        // 0.9.0-draft.15 (ADR-0123): a journaled event with every stamp
        // field, a subscription with a cursor, both gap events, an expired
        // lease, and an attributed metadata change.
        (
            "snap_event_journal_stamped",
            FrameKind::Event {
                terminal: Some(ResourceId::local(0x2B)),
                event: AgentEvent::ResourceSpawned {
                    kind: ResourceKind::Terminal,
                    parent: None,
                },
                stamp: Some(Box::new(
                    EventStamp::new(0x0102, 0x0000_0189_0000_0000)
                        .with_actor(Some(
                            ActorRef::new(ClientId::new(7))
                                .with_credential_id(Some("cred".to_owned()))
                                .with_client_name(Some("cli".to_owned())),
                        ))
                        .with_operation_id(IdempotencyKey::new([0x5A; 16])),
                )),
            },
        ),
        (
            "snap_subscribe_events_after_seq",
            FrameKind::SubscribeEvents {
                terminal: Some(ResourceId::local(0x2B)),
                after_seq: Some(0x0102),
            },
        ),
        (
            "snap_event_journal_gap",
            FrameKind::Event {
                terminal: None,
                event: AgentEvent::JournalGap {
                    first_missing: 0x10,
                    last_missing: 0x20,
                },
                stamp: None,
            },
        ),
        (
            "snap_event_source_gap",
            FrameKind::Event {
                terminal: Some(ResourceId::local(0x2B)),
                event: AgentEvent::SourceGap { dropped: 3 },
                stamp: Some(Box::new(EventStamp::new(0x21, 0))),
            },
        ),
        (
            "snap_event_terminal_control_expired",
            FrameKind::Event {
                terminal: Some(ResourceId::local(0x2B)),
                event: AgentEvent::TerminalControl {
                    lifecycle: ResourceLifecycle::Running,
                    exit_status: None,
                    input_holder: None,
                    action: ControlAction::Expired,
                    actor: None,
                },
                stamp: None,
            },
        ),
        (
            "snap_metadata_changed_with_actor",
            FrameKind::MetadataChanged {
                scope: Scope::Global,
                key: "phux.tui.layout/v1/1".to_owned(),
                value: Some(b"{}".to_vec()),
                actor: Some(ActorRef::new(ClientId::new(7))),
            },
        ),
        // 0.9.0-draft.15 (ADR-0124): a signal death, a retained spawn, and
        // a snapshot whose extension block carries an exited and a held
        // resource.
        (
            "snap_terminal_closed_signal",
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(0x2B),
                exit_status: None,
                reason: CloseReason::Exited,
                signal: Some(9),
            },
        ),
        (
            "snap_spawn_terminal_retain_and_idempotency_key",
            FrameKind::SpawnResource {
                request_id: 0x0000_0017,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                initial_size: None,
                resource: Some(Box::new(
                    SpawnResource::default()
                        .with_retain_secs(Some(600))
                        .with_idempotency_key(IdempotencyKey::new([0x5A; 16])),
                )),
            },
        ),
        (
            "snap_command_result_state_with_resource_state",
            FrameKind::CommandResult {
                request_id: 0x0000_0018,
                result: CommandResult::OkWith(CommandValue::State(
                    SessionSnapshot::new(SessionId::new(1), WindowId::new(1), ResourceId::local(1))
                        .with_resources(vec![
                            ResourceInfo::new(ResourceId::local(1), WindowId::new(1), 80, 24)
                                .with_lifecycle(ResourceLifecycle::Exited)
                                .with_exit(Some(
                                    ExitFacet::new(0x0100, 0x0200).with_exit_status(Some(0)),
                                )),
                            ResourceInfo::new(ResourceId::local(2), WindowId::new(1), 80, 24)
                                .with_input_holder(Some(ClientId::new(7))),
                            ResourceInfo::new(ResourceId::local(3), WindowId::new(1), 80, 24),
                        ]),
                )),
            },
        ),
        // 0.9.0-draft.15 (ADR-0126): a replayed spawn and a key conflict.
        (
            "snap_terminal_spawned_replayed",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_0017,
                result: SpawnResult::Replayed {
                    id: ResourceId::local(0x2B),
                    instance: None,
                },
            },
        ),
        (
            "snap_terminal_spawned_err_idempotency_conflict",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_0017,
                result: SpawnResult::Err(SpawnError::IdempotencyConflict),
            },
        ),
        (
            "snap_terminal_resize_standard",
            FrameKind::ResizeTerminal {
                terminal_id: ResourceId::local(0x0000_002A),
                cols: 80,
                rows: 24,
                cell_px: None,
            },
        ),
        (
            "snap_terminal_resize_cell_px",
            FrameKind::ResizeTerminal {
                terminal_id: ResourceId::local(0x0000_002A),
                cols: 80,
                rows: 24,
                cell_px: Some((9, 18)),
            },
        ),
    ]
}
