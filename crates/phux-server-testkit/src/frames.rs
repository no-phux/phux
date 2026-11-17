//! Constructors for the frames and commands the suites send most.

use phux_protocol::ids::ResourceId;
use phux_protocol::input::InputEvent;
use phux_protocol::input::key::KeyEvent;
use phux_protocol::wire::frame::{Command, FrameKind, Scope};
use tokio::net::UnixStream;

use crate::send_frame;

/// Send `COMMAND { request_id, command }` without awaiting its reply.
pub async fn send_command(stream: &mut UnixStream, request_id: u32, command: Command) {
    send_frame(
        stream,
        &FrameKind::Command {
            request_id,
            command,
        },
    )
    .await;
}

/// `GET_SCREEN` of `terminal_id`: plain text, no scrollback.
#[must_use]
pub const fn get_screen(terminal_id: ResourceId) -> Command {
    Command::GetScreen {
        terminal_id,
        request_scrollback: None,
        cells: false,
        format: 0,
    }
}

/// `KILL_RESOURCE` of `terminal_id` with no operation id.
#[must_use]
pub const fn kill(terminal_id: ResourceId) -> Command {
    Command::KillResource {
        terminal_id,
        operation_id: None,
    }
}

#[must_use]
pub const fn input_key(terminal_id: ResourceId, event: KeyEvent) -> FrameKind {
    FrameKind::InputKey { terminal_id, event }
}

#[must_use]
pub fn set_metadata(request_id: u32, scope: Scope, key: &str, value: Vec<u8>) -> FrameKind {
    FrameKind::SetMetadata {
        request_id,
        scope,
        key: key.to_owned(),
        value,
    }
}

#[must_use]
pub fn get_metadata(request_id: u32, scope: Scope, key: &str) -> FrameKind {
    FrameKind::GetMetadata {
        request_id,
        scope,
        key: key.to_owned(),
    }
}

#[must_use]
pub fn subscribe_metadata(scope: Scope, key: &str) -> FrameKind {
    FrameKind::SubscribeMetadata {
        scope,
        key: key.to_owned(),
    }
}

#[must_use]
pub const fn subscribe_events(terminal: Option<ResourceId>, after_seq: Option<u64>) -> FrameKind {
    FrameKind::SubscribeEvents {
        terminal,
        after_seq,
    }
}

#[must_use]
pub const fn route_input(terminal_id: ResourceId, event: InputEvent) -> Command {
    Command::RouteInput { terminal_id, event }
}

/// `KILL_RESOURCES` of `ids` with no operation id.
#[must_use]
pub const fn kill_all(ids: Vec<ResourceId>) -> Command {
    Command::KillResources {
        ids,
        operation_id: None,
    }
}
