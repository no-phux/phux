//! The frame catalog: every [`FrameKind`](super::FrameKind) variant's type
//! byte, spec name, `field.rs` module and decoder, declared once.
//!
//! [`frame_catalog!`] hands the rows to a generator macro. From them the
//! crate builds the `TYPE_*` discriminants (through `wire_tags!`, so a
//! reused byte is a compile error), `FrameKind::type_byte`, the
//! `decode_body` dispatch, and the wire-schema name, module and index.
//!
//! A row reads `Variant = TYPE_NAME 0xNN, "SPEC_NAME", "field_module",
//! decoder;`. Row order is the `docs/spec/wire-schema.json` emission order,
//! not byte order, so a new frame goes at the end. The field module is not
//! derivable from the variant (`PONG` reuses `ping`, `DETACH` has none), so
//! each row names it. Direction is the type byte's high bit: clear is client
//! to server, set is server to client.
//!
//! Bytes that stay unassigned:
//! - `0x15` (`INPUT_SELECTION`) was removed by ADR-0030.
//! - `0x40` (`PANE_DIFF`) was retired by ADR-0013.
//! - `0x91` (`TERMINAL_SNAPSHOT`) is retired by ADR-0070; never decode or
//!   reassign it.
//! - `0x24..=0x29` and `0xA3..=0xA7`: non-Terminal things are a
//!   `ResourceKind`, not a parallel frame family.
//!
//! L3 metadata frames use `0x50..=0x5F` client to server and `0xD0..=0xDF`
//! server to client. `FRAME_COMPRESSED` is no variant, so it is
//! declared beside the catalog rather than in it.

/// Invoke `$generator! { rows }` with every frame catalog row.
macro_rules! frame_catalog {
    ($generator:ident) => {
        $generator! {
/// Discriminant for `HELLO` (client to server, `docs/protocol.md#handshake`).
Hello = TYPE_HELLO 0x01, "HELLO", "hello", decode_hello;
/// Discriminant for `HELLO_OK` (server to client, `docs/protocol.md#handshake`).
HelloOk = TYPE_HELLO_OK 0x80, "HELLO_OK", "hello_ok", decode_hello_ok;
/// Discriminant for `PING` (client to server, `docs/protocol.md#handshake`).
Ping = TYPE_PING 0x7F, "PING", "ping", decode_ping;
/// Discriminant for `PONG` (server to client, `docs/protocol.md#handshake`).
Pong = TYPE_PONG 0xFF, "PONG", "ping", decode_pong;
/// Discriminant for generation-bound `RESOURCE_OUTPUT` (server to client,
/// `docs/protocol.md#attach-and-output`). Native-profile payloads are byte-identical raw PTY
/// bytes and are never capability-rewritten.
ResourceOutput = TYPE_RESOURCE_OUTPUT 0x90, "RESOURCE_OUTPUT", "terminal_output",
    decode_terminal_output;
/// Discriminant for `ATTACH` (client to server, `docs/protocol.md`).
Attach = TYPE_ATTACH 0x02, "ATTACH", "attach", decode_attach;
/// Discriminant for `DETACH` (client to server, `docs/protocol.md`).
Detach = TYPE_DETACH 0x03, "DETACH", "", decode_detach;
/// Discriminant for `INPUT_KEY` (client to server, `docs/protocol.md#input`).
InputKey = TYPE_INPUT_KEY 0x10, "INPUT_KEY", "input_key", decode_input_key;
/// Discriminant for `INPUT_MOUSE` (client to server, `docs/protocol.md#input`).
InputMouse = TYPE_INPUT_MOUSE 0x12, "INPUT_MOUSE", "input_mouse", decode_input_mouse;
/// Discriminant for `INPUT_FOCUS` (client to server, `docs/protocol.md#input`).
InputFocus = TYPE_INPUT_FOCUS 0x14, "INPUT_FOCUS", "input_focus", decode_input_focus;
/// Discriminant for `INPUT_PASTE` (client to server, `docs/protocol.md#input`).
InputPaste = TYPE_INPUT_PASTE 0x11, "INPUT_PASTE", "input_paste", decode_input_paste;
/// Discriminant for `INPUT_TERMINAL_REPLY` (client to server,
/// `docs/protocol.md#terminal-queries`).
InputTerminalReply = TYPE_INPUT_TERMINAL_REPLY 0x17, "INPUT_TERMINAL_REPLY",
    "input_terminal_reply", decode_input_terminal_reply;
/// Discriminant for StateSync-only `FRAME_ACK` (client to server,
/// `docs/protocol.md#generations-publication-and-history`).
///
/// Cumulative within one `(ResourceId, StreamId, BootstrapId)` after the
/// client applies the acknowledged transition. Raw profiles never send it.
FrameAck = TYPE_FRAME_ACK 0x21, "FRAME_ACK", "frame_ack", decode_frame_ack;
/// Discriminant for `VIEWPORT_RESIZE` (client to server, `docs/protocol.md`):
/// the outer terminal changed size; payload is the
/// [`ViewportInfo`] shape `ATTACH` carries.
ViewportResize = TYPE_VIEWPORT_RESIZE 0x20, "VIEWPORT_RESIZE", "viewport_resize",
    decode_viewport_resize;
/// Discriminant for `ATTACHED` (server to client, `docs/protocol.md#attach-and-output`).
Attached = TYPE_ATTACHED 0x81, "ATTACHED", "attached", decode_attached;
/// Discriminant for `ATTACH_READY` (server to client, `docs/protocol.md#attach-and-output`).
AttachReady = TYPE_ATTACH_READY 0x83, "ATTACH_READY", "attach_ready", decode_attach_ready;
/// Discriminant for `DETACHED` (server to client, `docs/protocol.md#commands-events-resources`).
Detached = TYPE_DETACHED 0x82, "DETACHED", "detached", decode_detached;
/// Discriminant for `BOOTSTRAP_BEGIN` (server to client, `docs/protocol.md#attach-and-output`).
BootstrapBegin = TYPE_BOOTSTRAP_BEGIN 0x93, "BOOTSTRAP_BEGIN", "bootstrap_begin",
    decode_bootstrap_begin;
/// Discriminant for `BOOTSTRAP_CHUNK` (server to client, `docs/protocol.md#attach-and-output`).
BootstrapChunk = TYPE_BOOTSTRAP_CHUNK 0x94, "BOOTSTRAP_CHUNK", "bootstrap_chunk",
    decode_bootstrap_chunk;
/// Discriminant for `BOOTSTRAP_READY` (server to client, `docs/protocol.md#attach-and-output`).
BootstrapReady = TYPE_BOOTSTRAP_READY 0x95, "BOOTSTRAP_READY", "bootstrap_ready",
    decode_bootstrap_ready;
/// Discriminant for `HISTORY_REQUEST` (client to server, `docs/protocol.md`).
HistoryRequest = TYPE_HISTORY_REQUEST 0x16, "HISTORY_REQUEST", "history_request",
    decode_history_request;
/// Discriminant for `HISTORY_PAGE` (server to client, `docs/protocol.md`).
HistoryPage = TYPE_HISTORY_PAGE 0x96, "HISTORY_PAGE", "history_page", decode_history_page;
/// Discriminant for `BOOTSTRAP_TOMBSTONE` (server to client, `docs/protocol.md`).
BootstrapTombstone = TYPE_BOOTSTRAP_TOMBSTONE 0x97, "BOOTSTRAP_TOMBSTONE",
    "bootstrap_tombstone", decode_bootstrap_tombstone;
/// Discriminant for cursor-scoped `HISTORY_TOMBSTONE` (server to client,
/// `docs/protocol.md#generations-publication-and-history`).
HistoryTombstone = TYPE_HISTORY_TOMBSTONE 0x98, "HISTORY_TOMBSTONE", "history_tombstone",
    decode_history_tombstone;
/// Discriminant for retryable cursor-scoped `HISTORY_REJECTED` (server to
/// client, `docs/protocol.md#generations-publication-and-history`).
HistoryRejected = TYPE_HISTORY_REJECTED 0x99, "HISTORY_REJECTED", "history_rejected",
    decode_history_rejected;
/// Discriminant for `BELL` (server to client, `docs/protocol.md#commands-events-resources`).
Bell = TYPE_BELL 0xB0, "BELL", "bell", decode_bell;
/// Discriminant for `ERROR` (server to client, `docs/protocol.md`). Fatal
/// errors MUST be followed by `DETACHED { PROTOCOL_ERROR }` and transport close.
Error = TYPE_ERROR 0xC1, "ERROR", "error", decode_error;
/// Discriminant for `GET_METADATA` (client to server, `docs/protocol.md`).
GetMetadata = TYPE_GET_METADATA 0x50, "GET_METADATA", "get_metadata", decode_get_metadata;
/// Discriminant for `SET_METADATA` (client to server, `docs/protocol.md`).
SetMetadata = TYPE_SET_METADATA 0x51, "SET_METADATA", "set_metadata", decode_set_metadata;
/// Discriminant for `DELETE_METADATA` (client to server, `docs/protocol.md`).
DeleteMetadata = TYPE_DELETE_METADATA 0x52, "DELETE_METADATA", "get_metadata",
    decode_delete_metadata;
/// Discriminant for `LIST_METADATA` (client to server, `docs/protocol.md`).
ListMetadata = TYPE_LIST_METADATA 0x53, "LIST_METADATA", "list_metadata",
    decode_list_metadata;
/// Discriminant for `SUBSCRIBE_METADATA` (client to server, `docs/protocol.md`).
SubscribeMetadata = TYPE_SUBSCRIBE_METADATA 0x54, "SUBSCRIBE_METADATA", "subscribe_metadata",
    decode_subscribe_metadata;
/// Discriminant for `METADATA_CHANGED` (server to client, `docs/protocol.md`).
MetadataChanged = TYPE_METADATA_CHANGED 0xD0, "METADATA_CHANGED", "metadata_changed",
    decode_metadata_changed;
/// Discriminant for `METADATA_VALUE` (server to client, `docs/protocol.md`):
/// the `GET_METADATA` reply, `None` when the key is absent.
MetadataValue = TYPE_METADATA_VALUE 0xD1, "METADATA_VALUE", "metadata_value",
    decode_metadata_value;
/// Discriminant for `METADATA_KEYS` (server to client, `docs/protocol.md`):
/// the `LIST_METADATA` reply, sorted key names only.
MetadataKeys = TYPE_METADATA_KEYS 0xD2, "METADATA_KEYS", "metadata_keys",
    decode_metadata_keys;
/// Discriminant for `LIST_DIRECTORY` (client to server, `docs/protocol.md`);
/// gated on [`ServerFeature::ListDirectory`](crate::caps::ServerFeature::ListDirectory).
ListDirectory = TYPE_LIST_DIRECTORY 0x55, "LIST_DIRECTORY", "list_directory",
    decode_list_directory_frame;
/// Discriminant for `DIRECTORY_LISTING` (server to client, `docs/protocol.md`):
/// the listing or a typed refusal.
DirectoryListing = TYPE_DIRECTORY_LISTING 0xD3, "DIRECTORY_LISTING", "directory_listing",
    decode_directory_listing_frame;
/// `PATH_QUERY` (client to server).
PathQuery = TYPE_PATH_QUERY 0x56, "PATH_QUERY", "path_query", decode_path_query;
/// `PATH_RESULTS` (server to client).
PathResults = TYPE_PATH_RESULTS 0xD4, "PATH_RESULTS", "path_results", decode_path_results;
/// Discriminant for `SPAWN_RESOURCE` (client to server, `docs/protocol.md`);
/// answered by [`TYPE_RESOURCE_SPAWNED`].
SpawnResource = TYPE_SPAWN_RESOURCE 0x22, "SPAWN_RESOURCE", "spawn_terminal",
    decode_spawn_terminal;
/// Discriminant for `RESOURCE_SPAWNED` (server to client): the
/// `SPAWN_RESOURCE` reply carrying a [`SpawnResult`].
ResourceSpawned = TYPE_RESOURCE_SPAWNED 0xA2, "RESOURCE_SPAWNED", "terminal_spawned",
    decode_terminal_spawned;
/// Discriminant for `MOVE_RESOURCE` (client to server, `docs/protocol.md`,
/// ADR-0056): re-parent a live Terminal into the window owning
/// `owner_terminal`, leaving its process and state untouched. Gated on
/// `MOVE_RESOURCE`; answered by [`TYPE_RESOURCE_MOVED`].
MoveResource = TYPE_MOVE_RESOURCE 0x2A, "MOVE_RESOURCE", "move_terminal",
    decode_move_terminal;
/// Discriminant for `RESOURCE_MOVED` (server to client): the
/// `MOVE_RESOURCE` reply carrying a [`MoveResult`].
ResourceMoved = TYPE_RESOURCE_MOVED 0xA8, "RESOURCE_MOVED", "terminal_moved",
    decode_terminal_moved;
/// Discriminant for `RESOURCE_CLOSED` (server to client, `docs/protocol.md`):
/// a resource ended.
ResourceClosed = TYPE_RESOURCE_CLOSED 0xA1, "RESOURCE_CLOSED", "terminal_closed",
    decode_terminal_closed;
/// Discriminant for `RESIZE_TERMINAL` (client to server, `docs/protocol.md`):
/// a per-Terminal PTY resize.
ResizeTerminal = TYPE_RESIZE_TERMINAL 0x23, "RESIZE_TERMINAL", "terminal_resize",
    decode_terminal_resize;
/// Discriminant for `COMMAND` (client to server, `docs/protocol.md#commands-events-resources`).
Command = TYPE_COMMAND 0x31, "COMMAND", "command", decode_command;
/// Discriminant for `COMMAND_RESULT` (server to client, `docs/protocol.md`).
CommandResult = TYPE_COMMAND_RESULT 0xC2, "COMMAND_RESULT", "command_result",
    decode_command_result;
/// Discriminant for `SUBSCRIBE_EVENTS` (client to server, `docs/protocol.md`).
SubscribeEvents = TYPE_SUBSCRIBE_EVENTS 0x41, "SUBSCRIBE_EVENTS", "subscribe_events",
    decode_subscribe_events;
/// Discriminant for `EVENT` (server to client, `docs/protocol.md#commands-events-resources`).
Event = TYPE_EVENT 0xB3, "EVENT", "event", decode_event;
/// Discriminant for `CLIPBOARD_REPLY` (client to server,
/// `docs/program-sequences.md#clipboard-osc-52`).
ClipboardReply = TYPE_CLIPBOARD_REPLY 0x18, "CLIPBOARD_REPLY", "clipboard_reply",
    decode_clipboard_reply;
/// Discriminant for `VIEWPORT_COLORS` (client to server, `docs/protocol.md`):
/// the outer terminal's default colours changed mid-attach.
ViewportColors = TYPE_VIEWPORT_COLORS 0x2B, "VIEWPORT_COLORS", "viewport_colors",
    decode_viewport_colors;
        }
    };
}

/// Declare each catalog row's `TYPE_*` discriminant through `wire_tags!`,
/// which reserves the byte in the `Message` namespace.
macro_rules! frame_type_consts {
    ($(
        $(#[$attr:meta])*
        $variant:ident = $name:ident $tag:literal, $spec:literal, $module:literal, $decode:ident;
    )*) => {
        wire_tags! { Message;
            $(
                $(#[$attr])*
                pub const $name: u8 = $tag;
            )*
        }
    };
}

pub(crate) use frame_catalog;
