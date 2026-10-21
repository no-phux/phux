//! Encoded samples of the stream, metadata, host, spawn, and lifecycle frames.

use super::{
    AgentEvent, BootstrapStreamProfile, Bytes, CloseReason, Command, CommandResult, DirectoryEntry,
    DirectoryErrorCode, DirectoryListing, DirectoryListingError, EventStamp, FrameKind, GroupId,
    HistoryRejectionReason, HistoryTombstoneReason, MoveResult, PathErrorCode, PathKind,
    PathQueryError, PathRow, PathStatus, ResourceId, SatelliteHost, Scope, ServerInstance,
    SpawnResource, SpawnResult, TombstoneReason, actor, generation, key, stream, terminal,
};

#[allow(
    clippy::too_many_lines,
    reason = "one sample per bootstrap/history frame shape, kept as one flat table"
)]
pub(super) fn stream_samples() -> Vec<FrameKind> {
    let cursor = Bytes::from_static(b"cursor");
    vec![
        FrameKind::ResourceOutput {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            seq: 1,
            bytes: Bytes::from_static(b"hi"),
        },
        FrameKind::FrameAck {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            seq: 1,
        },
        FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            profile: BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        },
        FrameKind::BootstrapBegin {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            profile: BootstrapStreamProfile::AgentEventsJsonlResumeV1 { after_seq: 5 },
            cols: 0,
            rows: 0,
            base_seq: 8,
        },
        FrameKind::BootstrapChunk {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            chunk_seq: 0,
            payload: Bytes::from_static(b"chunk"),
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            history_cursor: None,
        },
        FrameKind::BootstrapReady {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            history_cursor: Some(cursor.clone()),
        },
        FrameKind::HistoryRequest {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            cursor: cursor.clone(),
            max_bytes: 1024,
            max_rows: 10,
        },
        history_page(None),
        history_page(Some(Bytes::from_static(b"older"))),
        FrameKind::BootstrapTombstone {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            reason: TombstoneReason::Resize,
            last_valid_seq: 4,
        },
        FrameKind::HistoryTombstone {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            cursor: cursor.clone(),
            reason: HistoryTombstoneReason::Pruned,
        },
        FrameKind::HistoryRejected {
            terminal_id: terminal(),
            stream_id: stream(),
            bootstrap_id: generation(),
            cursor,
            reason: HistoryRejectionReason::TooSmall,
            required_bytes: 2048,
            required_rows: 20,
        },
    ]
}

pub(super) fn history_page(next_cursor: Option<Bytes>) -> FrameKind {
    FrameKind::HistoryPage {
        terminal_id: terminal(),
        stream_id: stream(),
        bootstrap_id: generation(),
        page_seq: 1,
        cursor: Bytes::from_static(b"cursor"),
        next_cursor,
        payload: Bytes::from_static(b"rows"),
        rows: 2,
    }
}

pub(super) fn metadata_samples() -> Vec<FrameKind> {
    let key_name = || "phux.example/v1".to_owned();
    vec![
        FrameKind::GetMetadata {
            request_id: 1,
            scope: Scope::Global,
            key: key_name(),
        },
        FrameKind::SetMetadata {
            request_id: 1,
            scope: Scope::Resource(terminal()),
            key: key_name(),
            value: b"{}".to_vec(),
        },
        FrameKind::DeleteMetadata {
            request_id: 1,
            scope: Scope::Group(GroupId::new(1)),
            key: key_name(),
        },
        FrameKind::ListMetadata {
            request_id: 1,
            scope: Scope::Global,
        },
        FrameKind::SubscribeMetadata {
            scope: Scope::Global,
            key: key_name(),
        },
        FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: key_name(),
            value: None,
            actor: None,
        },
        FrameKind::MetadataChanged {
            scope: Scope::Global,
            key: key_name(),
            value: Some(b"{}".to_vec()),
            actor: Some(actor()),
        },
        FrameKind::MetadataValue {
            request_id: 1,
            value: None,
        },
        FrameKind::MetadataValue {
            request_id: 1,
            value: Some(b"{}".to_vec()),
        },
        FrameKind::MetadataKeys {
            request_id: 1,
            keys: vec![key_name()],
        },
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "success and refusal shapes of the host-path frames, kept as one flat table"
)]
pub(super) fn host_samples() -> Vec<FrameKind> {
    let edge = || Some(SatelliteHost::new("edge"));
    vec![
        FrameKind::ListDirectory {
            request_id: 1,
            path: String::new(),
            host: None,
        },
        FrameKind::ListDirectory {
            request_id: 1,
            path: "/home".to_owned(),
            host: edge(),
        },
        FrameKind::DirectoryListing {
            request_id: 1,
            result: Ok(DirectoryListing {
                path: "/".to_owned(),
                parent: None,
                entries: Vec::new(),
                truncated: false,
            }),
        },
        FrameKind::DirectoryListing {
            request_id: 1,
            result: Ok(DirectoryListing {
                path: "/home/u".to_owned(),
                parent: Some("/home".to_owned()),
                entries: vec![DirectoryEntry {
                    name: "src".to_owned(),
                    is_symlink: true,
                }],
                truncated: true,
            }),
        },
        FrameKind::DirectoryListing {
            request_id: 1,
            result: Err(DirectoryListingError {
                path: "/root".to_owned(),
                code: DirectoryErrorCode::PermissionDenied,
                message: "denied".to_owned(),
            }),
        },
        FrameKind::PathQuery {
            request_id: 1,
            root: "/".to_owned(),
            query: String::new(),
            recursive: false,
            host: None,
        },
        FrameKind::PathQuery {
            request_id: 1,
            root: "/home".to_owned(),
            query: "src".to_owned(),
            recursive: true,
            host: edge(),
        },
        FrameKind::PathResults {
            request_id: 1,
            result: Ok(crate::wire::frame::PathResults {
                root: "/".to_owned(),
                parent: None,
                rows: Vec::new(),
                status: PathStatus::Complete,
            }),
        },
        FrameKind::PathResults {
            request_id: 1,
            result: Ok(crate::wire::frame::PathResults {
                root: "/home".to_owned(),
                parent: Some("/".to_owned()),
                rows: vec![PathRow {
                    path: "/home/u".to_owned(),
                    kind: PathKind::Directory,
                }],
                status: PathStatus::Truncated,
            }),
        },
        FrameKind::PathResults {
            request_id: 1,
            result: Err(PathQueryError {
                root: "/nope".to_owned(),
                code: PathErrorCode::NotFound,
                message: "missing".to_owned(),
            }),
        },
    ]
}

pub(super) fn spawn(resource: Option<SpawnResource>) -> FrameKind {
    FrameKind::SpawnResource {
        request_id: 1,
        group: GroupId::new(1),
        command: None,
        cwd: None,
        env: None,
        term: None,
        satellite: None,
        owner_terminal: None,
        agent_session: None,
        initial_size: None,
        resource: resource.map(Box::new),
    }
}

pub(super) fn spawn_samples() -> Vec<FrameKind> {
    let full_terminal = FrameKind::SpawnResource {
        request_id: 2,
        group: GroupId::new(1),
        command: Some(vec!["zsh".to_owned(), "-i".to_owned()]),
        cwd: Some("/home/u".to_owned()),
        env: Some(vec![("LANG".to_owned(), "C".to_owned())]),
        term: Some("ghostty".to_owned()),
        satellite: Some(SatelliteHost::new("edge")),
        owner_terminal: Some(ResourceId::local(8)),
        agent_session: Some(b"{}".to_vec()),
        initial_size: Some((132, 43)),
        resource: Some(Box::new(
            SpawnResource::default()
                .with_bind_instance(true)
                .with_retain_secs(Some(600))
                .with_idempotency_key(Some(key())),
        )),
    };
    let agent_session = SpawnResource::agent_session(terminal(), "claude")
        .with_native_id(Some("session-42".to_owned()));
    let instance = ServerInstance::new([0xA5; 16]);
    vec![
        spawn(None),
        full_terminal,
        spawn(Some(agent_session)),
        FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::Ok(terminal()),
        },
        FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::OkBound {
                id: terminal(),
                instance,
            },
        },
        FrameKind::ResourceSpawned {
            request_id: 1,
            result: SpawnResult::Replayed {
                id: terminal(),
                instance: Some(instance),
            },
        },
        FrameKind::MoveResource {
            request_id: 1,
            terminal: terminal(),
            owner_terminal: ResourceId::local(8),
        },
        FrameKind::ResourceMoved {
            request_id: 1,
            result: MoveResult::Ok(terminal()),
        },
    ]
}

#[allow(
    clippy::too_many_lines,
    reason = "every optional-field shape of the lifecycle frames, kept as one flat table"
)]
pub(super) fn lifecycle_samples() -> Vec<FrameKind> {
    let stamp = EventStamp::new(0x0102, 0x0189)
        .with_actor(Some(actor()))
        .with_operation_id(Some(key()));
    vec![
        FrameKind::Bell {
            terminal_id: terminal(),
        },
        FrameKind::ResourceClosed {
            terminal_id: terminal(),
            exit_status: None,
            reason: CloseReason::Unknown,
            signal: None,
        },
        FrameKind::ResourceClosed {
            terminal_id: terminal(),
            exit_status: Some(-9),
            reason: CloseReason::Killed,
            signal: Some(9),
        },
        FrameKind::Command {
            request_id: 1,
            command: Command::Upgrade,
        },
        FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::Ok,
        },
        FrameKind::SubscribeEvents {
            terminal: None,
            after_seq: None,
        },
        FrameKind::SubscribeEvents {
            terminal: Some(terminal()),
            after_seq: Some(41),
        },
        FrameKind::Event {
            terminal: None,
            event: AgentEvent::SourceGap { dropped: 3 },
            stamp: None,
        },
        FrameKind::Event {
            terminal: Some(terminal()),
            event: AgentEvent::SourceGap { dropped: 3 },
            stamp: Some(Box::new(stamp)),
        },
    ]
}
