//! L1 resource lifecycle, spawn/move/close, and command fixtures.

use phux_protocol::ids::{GroupId, ResourceId, ResourceKind, SatelliteHost};
use phux_protocol::wire::frame::{
    AgentEvent, CloseReason, Command, CommandResult, ErrorCode, FrameKind, MoveError, MoveResult,
    SpawnError, SpawnResource, SpawnResult,
};

#[allow(clippy::too_many_lines)]
pub(super) fn fixtures() -> Vec<(&'static str, FrameKind)> {
    vec![
        // L1 Terminal lifecycle frames.
        (
            // The minimum SPAWN_RESOURCE: request_id, default group, every
            // optional field absent. Reads as "spawn the server's default
            // shell in its default cwd, inheriting its env."
            "snap_spawn_terminal_minimal",
            FrameKind::SpawnResource {
                request_id: 0x0000_0001,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                initial_size: None,
                resource: None,
            },
        ),
        (
            // All optional fields populated; exercises the env-pair encoding
            // and length-prefixed command list.
            "snap_spawn_terminal_full",
            FrameKind::SpawnResource {
                request_id: 0x0000_0002,
                group: GroupId::new(1),
                command: Some(vec!["zsh".to_owned(), "-i".to_owned()]),
                cwd: Some("/home/u/src".to_owned()),
                env: Some(vec![
                    ("TERM".to_owned(), "xterm-256color".to_owned()),
                    ("LANG".to_owned(), "en_US.UTF-8".to_owned()),
                ]),
                term: None,
                satellite: None,
                owner_terminal: Some(ResourceId::local(42)),
                agent_session: Some(
                    br#"{"plugin_id":"com.phux.agents","native_id":"session-42"}"#.to_vec(),
                ),
                initial_size: Some((132, 43)),
                resource: None,
            },
        ),
        (
            // The first-class `term` field (phux-ign): field id 6, a bare
            // UTF-8 string. Distinct from the `TERM` env pair above — this is
            // the typed per-spawn override.
            "snap_spawn_terminal_term_field",
            FrameKind::SpawnResource {
                request_id: 0x0000_0003,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: Some("ghostty".to_owned()),
                satellite: None,
                owner_terminal: None,
                agent_session: None,
                initial_size: None,
                resource: None,
            },
        ),
        (
            // An AgentSession spawn: fields 11 (kind), 12 (parent), 13
            // (provider), 14 (native_id) and none of the PTY-shape fields.
            "snap_spawn_terminal_agent_session",
            FrameKind::SpawnResource {
                request_id: 0x0000_0004,
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
                    SpawnResource::agent_session(ResourceId::local(0x0000_002A), "claude")
                        .with_native_id(Some("session-42".to_owned())),
                )),
            },
        ),
        // pane_spawned events: a root Terminal keeps the empty body; an
        // AgentSession child carries its kind and parent as TLV fields.
        (
            "snap_event_pane_spawned_terminal",
            FrameKind::Event {
                terminal: Some(ResourceId::local(0x0000_002A)),
                event: AgentEvent::ResourceSpawned {
                    kind: ResourceKind::Terminal,
                    parent: None,
                },
                stamp: None,
            },
        ),
        (
            "snap_event_pane_spawned_agent_session",
            FrameKind::Event {
                terminal: Some(ResourceId::local(0x0000_002B)),
                event: AgentEvent::ResourceSpawned {
                    kind: ResourceKind::AgentSession,
                    parent: Some(ResourceId::local(0x0000_002A)),
                },
                stamp: None,
            },
        ),
        (
            "snap_terminal_spawned_err_unsupported_kind",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_000C,
                result: SpawnResult::Err(SpawnError::UnsupportedKind),
            },
        ),
        (
            "snap_terminal_spawned_err_parent_not_found",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_000D,
                result: SpawnResult::Err(SpawnError::ParentNotFound),
            },
        ),
        (
            "snap_terminal_spawned_err_parent_kind_mismatch",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_000E,
                result: SpawnResult::Err(SpawnError::ParentKindMismatch),
            },
        ),
        (
            "snap_terminal_spawned_ok",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_0001,
                result: SpawnResult::Ok(ResourceId::local(0x0000_002A)),
            },
        ),
        // ADR-0109: a bound reply is the `Ok` bytes above plus field 3, the
        // 16-byte instance token, so an older decoder reads the same `Ok`.
        (
            "snap_terminal_spawned_ok_bound",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_0001,
                result: SpawnResult::OkBound {
                    id: ResourceId::local(0x0000_002A),
                    instance: phux_protocol::ids::ServerInstance::new([0xA5; 16]),
                },
            },
        ),
        (
            "snap_spawn_terminal_bind_instance",
            FrameKind::SpawnResource {
                request_id: 0x0000_0010,
                group: GroupId::new(1),
                command: None,
                cwd: None,
                env: None,
                term: None,
                satellite: Some(SatelliteHost::new("edge")),
                owner_terminal: None,
                agent_session: None,
                initial_size: None,
                resource: Some(Box::new(SpawnResource::default().with_bind_instance(true))),
            },
        ),
        (
            "snap_command_kill_resource_if",
            FrameKind::Command {
                request_id: 0x0000_0011,
                command: Command::KillResourceIf {
                    terminal_id: ResourceId::satellite("edge", 0x0000_0009),
                    precondition:
                        phux_protocol::wire::frame::KillPrecondition::spawned_and_unattached(
                            phux_protocol::ids::ServerInstance::new([0xA5; 16]),
                        ),
                    operation_id: None,
                },
            },
        ),
        (
            "snap_command_perf_control_trace_stop",
            FrameKind::Command {
                request_id: 0x0000_0012,
                command: Command::PerfControl {
                    op: phux_protocol::wire::frame::PerfOp::TraceStop,
                },
            },
        ),
        (
            "snap_command_result_error_precondition_failed",
            FrameKind::CommandResult {
                request_id: 0x0000_0011,
                result: CommandResult::Error {
                    code: ErrorCode::PreconditionFailed,
                    message: "attached".to_owned(),
                },
            },
        ),
        (
            "snap_terminal_spawned_err_group_not_found",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_0007,
                result: SpawnResult::Err(SpawnError::GroupNotFound),
            },
        ),
        (
            "snap_terminal_spawned_err_spawn_failed",
            FrameKind::ResourceSpawned {
                request_id: 0x0000_0008,
                result: SpawnResult::Err(SpawnError::SpawnFailed("no pty available".to_owned())),
            },
        ),
        (
            "snap_move_terminal",
            FrameKind::MoveResource {
                request_id: 0x0000_0009,
                terminal: ResourceId::local(0x0000_002A),
                owner_terminal: ResourceId::local(0x0000_0007),
            },
        ),
        (
            "snap_terminal_moved_ok",
            FrameKind::ResourceMoved {
                request_id: 0x0000_0009,
                result: MoveResult::Ok(ResourceId::local(0x0000_002A)),
            },
        ),
        (
            "snap_terminal_moved_err_move_failed",
            FrameKind::ResourceMoved {
                request_id: 0x0000_000A,
                result: MoveResult::Err(MoveError::MoveFailed("no such terminal".to_owned())),
            },
        ),
        (
            "snap_terminal_moved_err_unsupported_satellite_route",
            FrameKind::ResourceMoved {
                request_id: 0x0000_000B,
                result: MoveResult::Err(MoveError::UnsupportedSatelliteRoute),
            },
        ),
        (
            "snap_terminal_closed_with_exit_code",
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(0x0000_002A),
                exit_status: Some(0),
                reason: CloseReason::Unknown,
                signal: None,
            },
        ),
        (
            // `exit_status = None` covers "killed by signal / unknown cause".
            "snap_terminal_closed_signal_unknown",
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(0x0000_002A),
                exit_status: None,
                reason: CloseReason::Unknown,
                signal: None,
            },
        ),
        (
            // A stated reason rides as additive field 3; `Unknown` above is
            // the absent-field shape the two goldens before it pin.
            "snap_terminal_closed_parent_closed",
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(0x0000_002B),
                exit_status: None,
                reason: CloseReason::ParentClosed,
                signal: None,
            },
        ),
        (
            "snap_terminal_closed_killed_with_exit_code",
            FrameKind::ResourceClosed {
                terminal_id: ResourceId::local(0x0000_002A),
                exit_status: Some(-9),
                reason: CloseReason::Killed,
                signal: None,
            },
        ),
    ]
}
