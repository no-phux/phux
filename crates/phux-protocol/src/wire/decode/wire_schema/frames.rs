//! Each `FrameKind` variant's schema name and index, from an exhaustive match.

use super::FrameKind;

/// Number of [`FrameKind`] variants; [`frame_entry`] indexes them `0..`.
pub(super) const FRAME_VARIANTS: usize = 49;

/// `(variant index, spec name, field.rs module)` for one frame. Exhaustive,
/// so a new variant is a compile error here until it is named.
#[allow(
    clippy::too_many_lines,
    reason = "one arm per FrameKind variant, kept as one flat table"
)]
pub(super) const fn frame_entry(frame: &FrameKind) -> (usize, &'static str, &'static str) {
    match frame {
        FrameKind::Hello { .. } => (0, "HELLO", "hello"),
        FrameKind::HelloOk { .. } => (1, "HELLO_OK", "hello_ok"),
        FrameKind::Ping { .. } => (2, "PING", "ping"),
        FrameKind::Pong { .. } => (3, "PONG", "ping"),
        FrameKind::ResourceOutput { .. } => (4, "RESOURCE_OUTPUT", "terminal_output"),
        FrameKind::Attach { .. } => (5, "ATTACH", "attach"),
        FrameKind::Detach => (6, "DETACH", ""),
        FrameKind::InputKey { .. } => (7, "INPUT_KEY", "input_key"),
        FrameKind::InputMouse { .. } => (8, "INPUT_MOUSE", "input_mouse"),
        FrameKind::InputFocus { .. } => (9, "INPUT_FOCUS", "input_focus"),
        FrameKind::InputPaste { .. } => (10, "INPUT_PASTE", "input_paste"),
        FrameKind::InputTerminalReply { .. } => {
            (11, "INPUT_TERMINAL_REPLY", "input_terminal_reply")
        }
        FrameKind::FrameAck { .. } => (12, "FRAME_ACK", "frame_ack"),
        FrameKind::ViewportResize { .. } => (13, "VIEWPORT_RESIZE", "viewport_resize"),
        FrameKind::Attached { .. } => (14, "ATTACHED", "attached"),
        FrameKind::AttachReady { .. } => (15, "ATTACH_READY", "attach_ready"),
        FrameKind::Detached { .. } => (16, "DETACHED", "detached"),
        FrameKind::BootstrapBegin { .. } => (17, "BOOTSTRAP_BEGIN", "bootstrap_begin"),
        FrameKind::BootstrapChunk { .. } => (18, "BOOTSTRAP_CHUNK", "bootstrap_chunk"),
        FrameKind::BootstrapReady { .. } => (19, "BOOTSTRAP_READY", "bootstrap_ready"),
        FrameKind::HistoryRequest { .. } => (20, "HISTORY_REQUEST", "history_request"),
        FrameKind::HistoryPage { .. } => (21, "HISTORY_PAGE", "history_page"),
        FrameKind::BootstrapTombstone { .. } => (22, "BOOTSTRAP_TOMBSTONE", "bootstrap_tombstone"),
        FrameKind::HistoryTombstone { .. } => (23, "HISTORY_TOMBSTONE", "history_tombstone"),
        FrameKind::HistoryRejected { .. } => (24, "HISTORY_REJECTED", "history_rejected"),
        FrameKind::Bell { .. } => (25, "BELL", "bell"),
        FrameKind::Error { .. } => (26, "ERROR", "error"),
        FrameKind::GetMetadata { .. } => (27, "GET_METADATA", "get_metadata"),
        FrameKind::SetMetadata { .. } => (28, "SET_METADATA", "set_metadata"),
        FrameKind::DeleteMetadata { .. } => (29, "DELETE_METADATA", "get_metadata"),
        FrameKind::ListMetadata { .. } => (30, "LIST_METADATA", "list_metadata"),
        FrameKind::SubscribeMetadata { .. } => (31, "SUBSCRIBE_METADATA", "subscribe_metadata"),
        FrameKind::MetadataChanged { .. } => (32, "METADATA_CHANGED", "metadata_changed"),
        FrameKind::MetadataValue { .. } => (33, "METADATA_VALUE", "metadata_value"),
        FrameKind::MetadataKeys { .. } => (34, "METADATA_KEYS", "metadata_keys"),
        FrameKind::ListDirectory { .. } => (35, "LIST_DIRECTORY", "list_directory"),
        FrameKind::DirectoryListing { .. } => (36, "DIRECTORY_LISTING", "directory_listing"),
        FrameKind::PathQuery { .. } => (37, "PATH_QUERY", "path_query"),
        FrameKind::PathResults { .. } => (38, "PATH_RESULTS", "path_results"),
        FrameKind::SpawnResource { .. } => (39, "SPAWN_RESOURCE", "spawn_terminal"),
        FrameKind::ResourceSpawned { .. } => (40, "RESOURCE_SPAWNED", "terminal_spawned"),
        FrameKind::MoveResource { .. } => (41, "MOVE_RESOURCE", "move_terminal"),
        FrameKind::ResourceMoved { .. } => (42, "RESOURCE_MOVED", "terminal_moved"),
        FrameKind::ResourceClosed { .. } => (43, "RESOURCE_CLOSED", "terminal_closed"),
        FrameKind::ResizeTerminal { .. } => (44, "RESIZE_TERMINAL", "terminal_resize"),
        FrameKind::Command { .. } => (45, "COMMAND", "command"),
        FrameKind::CommandResult { .. } => (46, "COMMAND_RESULT", "command_result"),
        FrameKind::SubscribeEvents { .. } => (47, "SUBSCRIBE_EVENTS", "subscribe_events"),
        FrameKind::Event { .. } => (48, "EVENT", "event"),
    }
}

/// `FRAME_COMPRESSED` is no `FrameKind` variant: the envelope is sampled by
/// compressing one, so it is named here.
pub(super) const FRAME_COMPRESSED: (&str, &str) = ("FRAME_COMPRESSED", "frame_compressed");
