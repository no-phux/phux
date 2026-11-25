//! Encoded samples of the server's stream, metadata, host, spawn,
//! lifecycle, and forwarding-reply frames.

use super::{
    AgentEvent, BootstrapStreamProfile, Bytes, CloseReason, CommandResult, CommandValue,
    DirectoryEntry, DirectoryErrorCode, DirectoryListing, DirectoryListingError, EventStamp,
    FrameKind, HistoryRejectionReason, HistoryTombstoneReason, MoveResult, PathErrorCode, PathKind,
    PathQueryError, PathRow, PathStatus, Scope, ServerInstance, SpawnResult, TombstoneReason,
    actor, generation, key, stream, terminal,
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
    vec![
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

pub(super) fn spawn_samples() -> Vec<FrameKind> {
    let instance = ServerInstance::new([0xA5; 16]);
    vec![
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
        FrameKind::CommandResult {
            request_id: 1,
            result: CommandResult::Ok,
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

const fn ok_with(value: CommandValue) -> FrameKind {
    FrameKind::CommandResult {
        request_id: 1,
        result: CommandResult::OkWith(value),
    }
}

/// The port-forward, ssh-agent, and service-HTTP replies, each chunk both
/// mid-stream at its largest accepted size and as the empty last one.
pub(super) fn reply_samples() -> Vec<FrameKind> {
    use crate::forward::{ForwardChunk, MAX_AGENT_MESSAGE_BYTES, MAX_FORWARD_CHUNK_BYTES};
    use crate::service_http::{MAX_SERVICE_HTTP_READ_BYTES, ServiceHttpChunk, ServiceHttpHead};
    vec![
        ok_with(CommandValue::ForwardOpened(u64::MAX)),
        ok_with(CommandValue::ForwardChunk(ForwardChunk {
            bytes: vec![0x5a; MAX_FORWARD_CHUNK_BYTES],
            eof: false,
        })),
        ok_with(CommandValue::ForwardChunk(ForwardChunk {
            bytes: Vec::new(),
            eof: true,
        })),
        FrameKind::Event {
            terminal: None,
            event: AgentEvent::SshAgentRequest {
                id: u64::MAX,
                message: vec![13; MAX_AGENT_MESSAGE_BYTES],
            },
            stamp: None,
        },
        FrameKind::Event {
            terminal: Some(crate::ids::ResourceId::local(7)),
            event: AgentEvent::OpenUrl {
                id: u64::MAX,
                url: "https://phux.sh/".to_owned(),
            },
            stamp: None,
        },
        ok_with(CommandValue::ServiceHttpHead(ServiceHttpHead {
            exchange: 9,
            status: 401,
            headers: vec![("www-authenticate".to_owned(), "Basic".to_owned())],
        })),
        ok_with(CommandValue::ServiceHttpChunk(ServiceHttpChunk {
            bytes: vec![0x5a; MAX_SERVICE_HTTP_READ_BYTES],
            end: false,
        })),
        ok_with(CommandValue::ServiceHttpChunk(ServiceHttpChunk {
            bytes: Vec::new(),
            end: true,
        })),
    ]
}
