//! [`FrameKind`] — the decoded wire frame — with its type-byte table and
//! its encode/decode entry points.

use bytes::BytesMut;

use crate::caps::{
    BootstrapCodec, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities,
    Compression, OutputMode, ServerCapabilities,
};
use crate::ids::{BootstrapId, ClientId, GroupId, ResourceId, SatelliteHost, StreamId};
use crate::input::InputEvent;
use crate::input::focus::FocusEvent;
use crate::input::key::KeyEvent;
use crate::input::mouse::MouseEvent;
use crate::input::paste::PasteEvent;
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;
use crate::wire::framing::LENGTH_PREFIX_LEN;
use crate::wire::info::{SessionSnapshot, encode_client_id, encode_session_snapshot};

use super::directory::{DirectoryListingResult, encode_directory_listing, encode_list_directory};
use super::{
    ActorRef, AgentEvent, AttachTarget, CloseReason, Command, CommandResult, DetachReason,
    ErrorCode, EventStamp, HistoryRejectionReason, HistoryTombstoneReason, MAX_FRAME_LEN,
    MoveResult, Scope, SpawnResource, SpawnResult, TYPE_ATTACH, TYPE_ATTACH_READY, TYPE_ATTACHED,
    TYPE_BELL, TYPE_BOOTSTRAP_BEGIN, TYPE_BOOTSTRAP_CHUNK, TYPE_BOOTSTRAP_READY,
    TYPE_BOOTSTRAP_TOMBSTONE, TYPE_COMMAND, TYPE_COMMAND_RESULT, TYPE_DELETE_METADATA, TYPE_DETACH,
    TYPE_DETACHED, TYPE_DIRECTORY_LISTING, TYPE_ERROR, TYPE_EVENT, TYPE_FRAME_ACK,
    TYPE_FRAME_COMPRESSED, TYPE_GET_METADATA, TYPE_HELLO, TYPE_HELLO_OK, TYPE_HISTORY_PAGE,
    TYPE_HISTORY_REJECTED, TYPE_HISTORY_REQUEST, TYPE_HISTORY_TOMBSTONE, TYPE_INPUT_FOCUS,
    TYPE_INPUT_KEY, TYPE_INPUT_MOUSE, TYPE_INPUT_PASTE, TYPE_INPUT_TERMINAL_REPLY,
    TYPE_LIST_DIRECTORY, TYPE_LIST_METADATA, TYPE_METADATA_CHANGED, TYPE_METADATA_KEYS,
    TYPE_METADATA_VALUE, TYPE_MOVE_RESOURCE, TYPE_PING, TYPE_PONG, TYPE_RESIZE_TERMINAL,
    TYPE_RESOURCE_CLOSED, TYPE_RESOURCE_MOVED, TYPE_RESOURCE_OUTPUT, TYPE_RESOURCE_SPAWNED,
    TYPE_SET_METADATA, TYPE_SPAWN_RESOURCE, TYPE_SUBSCRIBE_EVENTS, TYPE_SUBSCRIBE_METADATA,
    TYPE_VIEWPORT_RESIZE, TombstoneReason, ViewportInfo, encode_actor_ref, encode_agent_event,
    encode_attach_target, encode_bootstrap_codec, encode_bootstrap_profile, encode_command,
    encode_command_result, encode_env, encode_focus_event, encode_key_event, encode_mouse_event,
    encode_move_result, encode_paste_event, encode_scope, encode_spawn_result, encode_string_list,
    encode_terminal_id, encode_viewport_info,
};

/// Decoded wire frame (`docs/spec/proto.md` §7).
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FrameKind {
    /// `HELLO` — client to server handshake (`docs/spec/proto.md` §6.1). A
    /// missing or truncated capability record is malformed.
    Hello {
        /// Free-form client identifier (e.g. `"phux-client 0.1.0"`).
        client_name: String,
        /// Highest protocol major version the client supports.
        protocol_major: u16,
        /// Highest protocol minor version the client supports.
        protocol_minor: u16,
        /// Highest protocol patch version the client supports.
        protocol_patch: u16,
        /// Client capability advertisement (SPEC §6.2).
        client_caps: ClientCapabilities,
    },
    /// `HELLO_OK` — server handshake acknowledgement (`docs/spec/proto.md`
    /// §6.1). Selects one [`BootstrapProfile`] for the connection's lifetime.
    HelloOk {
        /// Selected major version (wire-breaking axis pre-1.0).
        protocol_major: u16,
        /// Selected minor version.
        protocol_minor: u16,
        /// Selected patch version.
        protocol_patch: u16,
        /// The conformance tiers the server mounts; intersect with the
        /// client's `layers` for the negotiated tier set.
        server_caps: ServerCapabilities,
        /// Opaque server identity bytes (SPEC §6.1).
        server_id: Vec<u8>,
        /// Explicit synchronization profile selected for this connection.
        selected_profile: BootstrapProfile,
        /// Negotiated bootstrap/history payload bounds.
        bootstrap_limits: BootstrapLimits,
    },

    /// `PING` — liveness probe (`docs/spec/proto.md` §7.4), echoed in `PONG`.
    Ping {
        /// Opaque nonce echoed by the peer in `PONG`.
        nonce: u64,
    },
    /// `PONG` — liveness response (`docs/spec/proto.md` §7.4).
    Pong {
        /// Nonce echoed from the corresponding `PING`.
        nonce: u64,
    },

    /// `RESOURCE_OUTPUT` — live terminal content (`docs/spec/L1.md` §4.1).
    ///
    /// `seq` is contiguous and non-wrapping within one `(stream_id,
    /// bootstrap_id)`. Under `NativeState`, `bytes` are the exact PTY bytes
    /// and MUST NOT be rewritten.
    ResourceOutput {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription receiving this output.
        stream_id: StreamId,
        /// Published replica generation this output extends.
        bootstrap_id: BootstrapId,
        /// Monotonic stream sequence.
        seq: u64,
        /// Opaque VT bytes.
        bytes: bytes::Bytes,
    },

    /// `ATTACH` — client requests to attach to a session (`docs/spec/L1.md` §7).
    Attach {
        /// Client-chosen correlation id echoed by `ATTACHED` and `ATTACH_READY`.
        attach_id: u32,
        /// Which session to attach to.
        target: AttachTarget,
        /// Client viewport dimensions at attach time.
        viewport: ViewportInfo,
        /// Whether to send scrollback as part of the attach sequence.
        request_scrollback: bool,
        /// Upper bound on scrollback lines the client will accept.
        scrollback_limit_lines: u32,
        /// Attach intent for every Terminal the attach subscribes (field 6,
        /// ADR-0127). `None` writes no field and means `{ PRIMARY, NEVER }`.
        /// Send `Some` only to a server advertising `ATTACH_ROLES`.
        role_policy: Option<super::RolePolicy>,
    },

    /// `DETACH` — client signals clean departure (`docs/spec/proto.md` §7.2).
    Detach,

    /// `INPUT_KEY` — client forwards a structured key event (`docs/spec/input.md` §2).
    InputKey {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Structured key event; libghostty atoms inside.
        event: KeyEvent,
    },

    /// `INPUT_MOUSE` — client forwards a mouse event (`docs/spec/input.md` §3).
    InputMouse {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Structured mouse event; coordinates are terminal-local pixels.
        event: MouseEvent,
    },

    /// `INPUT_FOCUS` — client reports focus change on its host window
    /// (`docs/spec/input.md` §4).
    InputFocus {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Whether the client window gained or lost focus.
        event: FocusEvent,
    },

    /// `INPUT_PASTE` — client forwards a paste payload (`docs/spec/input.md` §5).
    InputPaste {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Paste payload plus trust classification.
        event: PasteEvent,
    },

    /// `INPUT_TERMINAL_REPLY` — opaque bytes the client's terminal emulator
    /// generated in response to output (`docs/spec/input.md` §6), written
    /// byte-identically to the terminal's input lane.
    InputTerminalReply {
        /// Attached target terminal.
        terminal_id: ResourceId,
        /// Non-empty opaque PTY reply bytes. NUL and non-UTF-8 are valid.
        bytes: bytes::Bytes,
    },

    /// `FRAME_ACK` — cumulative acknowledgement (`docs/spec/proto.md` §8.2),
    /// valid only for the `SynthesizedVtStateSync` profile.
    FrameAck {
        /// Acked terminal.
        terminal_id: ResourceId,
        /// Logical subscription whose `StateSync` reference advances.
        stream_id: StreamId,
        /// Replica generation whose reference advances.
        bootstrap_id: BootstrapId,
        /// Highest contiguous `RESOURCE_OUTPUT.seq` applied.
        seq: u64,
    },

    /// `VIEWPORT_RESIZE` — the attached client's outer terminal changed
    /// size (`docs/spec/proto.md` §7.1 / §10.5). The connection identifies
    /// the client.
    ViewportResize {
        /// New outer-terminal metrics.
        viewport: ViewportInfo,
    },

    /// `ATTACHED` — metadata inventory for an accepted attach.
    ///
    /// Terminal content follows separately through bootstrap streams. This
    /// frame does not mean those streams are renderable; `ATTACH_READY` marks
    /// the aggregate boundary after every pane is READY or closed.
    Attached {
        /// Client-chosen correlation id from `ATTACH`.
        attach_id: u32,
        /// Full graph of sessions/windows/panes plus initial focus.
        snapshot: SessionSnapshot,
        /// Server-allocated client identifier for this attachment.
        initial_client_id: ClientId,
    },
    /// `ATTACH_READY` — every stream created by one `ATTACH` is READY or closed.
    AttachReady {
        /// Client-chosen correlation id from `ATTACH`.
        attach_id: u32,
    },

    /// `DETACHED` — server confirms detach and closes the transport
    /// (`docs/spec/proto.md` §7.2). With that close, the only ending a
    /// consumer may act on; an `ERROR` never ends an attach (§9).
    Detached {
        /// Why the attach ended, or `None` when unstated or unrecognised.
        /// A consumer MUST NOT infer [`DetachReason::Requested`] from absence.
        reason: Option<DetachReason>,
        /// Diagnostic text, not a contract; empty when none was sent.
        message: String,
    },

    /// `BOOTSTRAP_BEGIN` — declares one replacement replica generation.
    BootstrapBegin {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// New generation for this stream.
        bootstrap_id: BootstrapId,
        /// Concrete stream profile. Its variants encode the `codec` and
        /// `output_mode` fields without permitting native `StateSync`.
        profile: BootstrapStreamProfile,
        /// Authoritative PTY width at the actor cut.
        cols: u16,
        /// Authoritative PTY height at the actor cut.
        rows: u16,
        /// Actor cut sequence; first live output is `base_seq + 1`.
        base_seq: u64,
    },
    /// `BOOTSTRAP_CHUNK` — one bounded opaque checkpoint fragment.
    BootstrapChunk {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation.
        bootstrap_id: BootstrapId,
        /// Zero-based contiguous chunk sequence.
        chunk_seq: u32,
        /// Opaque engine/compatibility bytes.
        payload: bytes::Bytes,
    },
    /// `BOOTSTRAP_READY` — prior chunks reach the selected codec's READY boundary.
    BootstrapReady {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation now safe to publish.
        bootstrap_id: BootstrapId,
        /// Opaque newest-to-oldest history cursor, if retained history exists.
        history_cursor: Option<bytes::Bytes>,
    },
    /// `HISTORY_REQUEST` — request the next bounded history suffix page.
    HistoryRequest {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation that issued the cursor.
        bootstrap_id: BootstrapId,
        /// Opaque cursor returned by READY or a previous page.
        cursor: bytes::Bytes,
        /// Requested response byte budget; zero receives `HISTORY_REJECTED`.
        max_bytes: u32,
        /// Requested row budget; zero receives `HISTORY_REJECTED`.
        max_rows: u32,
    },
    /// `HISTORY_PAGE` — one independently decodable opaque history page.
    HistoryPage {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation that issued the cursor.
        bootstrap_id: BootstrapId,
        /// Non-zero page sequence within this generation-bound cursor lineage.
        page_seq: u64,
        /// Opaque cursor consumed by this response.
        cursor: bytes::Bytes,
        /// Cursor for the next older page; absence means this payload ends in FINISH.
        next_cursor: Option<bytes::Bytes>,
        /// Opaque selected-codec page bytes.
        payload: bytes::Bytes,
        /// Number of history rows encoded by this payload.
        rows: u32,
    },
    /// `BOOTSTRAP_TOMBSTONE` — permanently invalidates one generation.
    BootstrapTombstone {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Invalidated generation.
        bootstrap_id: BootstrapId,
        /// Why continuity could not be preserved.
        reason: TombstoneReason,
        /// Highest live sequence known valid for this generation.
        last_valid_seq: u64,
    },
    /// `HISTORY_TOMBSTONE` — invalidates one progressive history cursor only.
    HistoryTombstone {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation that issued the cursor.
        bootstrap_id: BootstrapId,
        /// Opaque invalidated history cursor.
        cursor: bytes::Bytes,
        /// Why progressive history for the cursor ended.
        reason: HistoryTombstoneReason,
    },
    /// `HISTORY_REJECTED` — retryable refusal that preserves cursor continuity.
    HistoryRejected {
        /// Target terminal.
        terminal_id: ResourceId,
        /// Logical subscription.
        stream_id: StreamId,
        /// Replica generation that issued the cursor.
        bootstrap_id: BootstrapId,
        /// Opaque history cursor that was not advanced.
        cursor: bytes::Bytes,
        /// Why the request did not begin.
        reason: HistoryRejectionReason,
        /// Non-zero byte limit required for a retry.
        required_bytes: u32,
        /// Non-zero row limit required for a retry.
        required_rows: u32,
    },

    /// `BELL` — terminal received a bell character (`docs/spec/L1.md` §1.2).
    Bell {
        /// Terminal that bell'd.
        terminal_id: ResourceId,
    },

    /// `ERROR` — structured error (`docs/spec/proto.md` §9). A fatal error
    /// MUST be followed by `DETACHED { PROTOCOL_ERROR }` and transport close.
    Error {
        /// The `COMMAND` this error answers, or `None` when uncorrelated.
        request_id: Option<u32>,
        /// Structured error code.
        code: ErrorCode,
        /// Short human-readable message.
        message: String,
    },

    // L3 metadata frames (SPEC §7.4). Values are opaque bytes; the server
    // MUST NOT emit `MetadataChanged` to a non-L3 consumer (SPEC §16.4).
    /// `GET_METADATA` — read the value at `(scope, key)`
    /// (`docs/spec/L3.md` §1); answered by `METADATA_VALUE`.
    GetMetadata {
        /// Correlates this request with its `METADATA_VALUE` reply.
        request_id: u32,
        /// Where to look the key up.
        scope: Scope,
        /// UTF-8 key name. Convention: `phux.<consumer>.<name>/<version>`
        /// per SPEC §17 (non-normative).
        key: String,
    },

    /// `SET_METADATA` — atomically write `value` at `(scope, key)` and
    /// broadcast `METADATA_CHANGED` (`docs/spec/L3.md` §1). A server MAY cap
    /// value size with [`ErrorCode::ResourceExhausted`].
    SetMetadata {
        /// Request correlation id.
        request_id: u32,
        /// Where to write the key.
        scope: Scope,
        /// UTF-8 key name.
        key: String,
        /// Opaque value bytes. The server MUST NOT interpret them.
        value: Vec<u8>,
    },

    /// `DELETE_METADATA` — idempotently remove `key` from `scope`
    /// (`docs/spec/L3.md` §1); a tombstone is broadcast iff the key existed.
    DeleteMetadata {
        /// Request correlation id.
        request_id: u32,
        /// Where to delete the key.
        scope: Scope,
        /// UTF-8 key name.
        key: String,
    },

    /// `LIST_METADATA` — list the key names in `scope` (`docs/spec/L3.md`
    /// §1); answered by `METADATA_KEYS`.
    ListMetadata {
        /// Correlates this request with its `METADATA_KEYS` reply.
        request_id: u32,
        /// Where to list keys from.
        scope: Scope,
    },

    /// `SUBSCRIBE_METADATA` — idempotently opt into `METADATA_CHANGED` for
    /// `(scope, key)` (`docs/spec/L3.md` §1); implicit teardown on detach.
    SubscribeMetadata {
        /// Scope to watch.
        scope: Scope,
        /// Specific key to watch. The subscriber receives
        /// `MetadataChanged` iff the event's `(scope, key)` matches.
        key: String,
    },

    /// `METADATA_CHANGED` — `(scope, key)` was written (`Some`) or deleted
    /// (`None`) (`docs/spec/L3.md` §1). The value rides inline.
    MetadataChanged {
        /// Scope the change happened in.
        scope: Scope,
        /// Key that changed.
        key: String,
        /// New value, or `None` for a deletion (tombstone).
        value: Option<Vec<u8>>,
        /// The connection whose write caused the change, or `None` when the
        /// server made it. Additive field 4 (ADR-0123), absent when `None`.
        actor: Option<ActorRef>,
    },

    /// `METADATA_VALUE` — reply to `GET_METADATA` (`docs/spec/L3.md` §1).
    MetadataValue {
        /// Correlates this reply with a prior `GET_METADATA.request_id`.
        request_id: u32,
        /// `Some(bytes)` when the key was present, `None` when absent.
        value: Option<Vec<u8>>,
    },

    /// `METADATA_KEYS` — reply to `LIST_METADATA` (`docs/spec/L3.md` §1).
    /// Servers SHOULD sort keys; clients MUST NOT rely on the order.
    MetadataKeys {
        /// Correlates this reply with a prior `LIST_METADATA.request_id`.
        request_id: u32,
        /// Key names present in the requested scope.
        keys: Vec<String>,
    },

    /// `LIST_DIRECTORY` — list the child directories of `path` on the
    /// serving host (`docs/spec/L3.md` §4). Gated on
    /// [`ServerFeature::ListDirectory`](crate::caps::ServerFeature::ListDirectory).
    ListDirectory {
        /// Correlates this request with its `DIRECTORY_LISTING` reply.
        request_id: u32,
        /// Absolute path, `~` / `~/rest`, or empty (the serving user's home).
        path: String,
        /// Satellite to list on, relayed by a federation hub
        /// (`docs/spec/L3.md` §4.1); `None` is the serving host. Gated on
        /// [`ServerFeature::ListDirectoryHost`](crate::caps::ServerFeature::ListDirectoryHost).
        host: Option<SatelliteHost>,
    },

    /// `DIRECTORY_LISTING` — server reply to a prior `LIST_DIRECTORY`
    /// (`docs/spec/L3.md` §4): the listing, or a typed refusal.
    DirectoryListing {
        /// Correlates this reply with a prior `LIST_DIRECTORY.request_id`.
        request_id: u32,
        /// The listing, or why it could not be produced.
        result: DirectoryListingResult,
    },

    /// `SPAWN_RESOURCE` — spawn a new resource under `group`
    /// (`docs/spec/L1.md` §1 / §10.1); answered by `RESOURCE_SPAWNED`.
    SpawnResource {
        /// Correlates this request with its `RESOURCE_SPAWNED` reply.
        request_id: u32,
        /// Group under which to spawn.
        group: GroupId,
        /// Command + argv, or `None` for the server's default shell.
        command: Option<Vec<String>>,
        /// Working directory, or `None` for the server's default.
        cwd: Option<String>,
        /// Environment, or `None` to inherit the server's. `Some(vec![])`
        /// starts with an empty environment.
        env: Option<Vec<(String, String)>>,
        /// `TERM` override, or `None` for the server default. An explicit
        /// `TERM` in `env` still wins.
        term: Option<String>,
        /// Satellite host to spawn on via a federation hub (field 7,
        /// ADR-0007 / L1 §9.1), or `None` for the receiving server.
        satellite: Option<SatelliteHost>,
        /// Existing Terminal whose window hosts the new one (field 8): an
        /// ownership address, not focus or geometry.
        owner_terminal: Option<ResourceId>,
        /// Opaque [`RESOURCE_AGENT_SESSION_KEY`](super::RESOURCE_AGENT_SESSION_KEY)
        /// bytes installed atomically before the Terminal is visible (field 9).
        agent_session: Option<Vec<u8>>,
        /// `(cols, rows)` to create the grid and PTY at (field 10), so the
        /// follow-up resize does not invalidate the first bootstrap. Gated on
        /// [`ServerFeature::SpawnInitialSize`](crate::caps::ServerFeature::SpawnInitialSize);
        /// a zero axis is ignored.
        initial_size: Option<(u16, u16)>,
        /// Kind, parent, and agent facet (fields 11-14, `docs/spec/L1.md`
        /// §1.2). `None` is a plain Terminal spawn and writes none of them.
        resource: Option<Box<SpawnResource>>,
    },

    /// `RESOURCE_SPAWNED` — reply to `SPAWN_RESOURCE` (`docs/spec/L1.md` §1 /
    /// §10.1) with the new id or a typed [`SpawnError`](super::SpawnError).
    ResourceSpawned {
        /// Correlates this reply with a prior `SpawnResource.request_id`.
        request_id: u32,
        /// Either the freshly allocated Terminal, or a structured error.
        result: SpawnResult,
    },

    /// `MOVE_RESOURCE` — re-parent a live Terminal into the window owning
    /// `owner_terminal` (`docs/spec/L1.md` §1 / §10.1; ADR-0056).
    ///
    /// The id and all process state are unchanged; layout stays client L3.
    /// Local-only: satellite ids are refused with
    /// [`MoveError::UnsupportedSatelliteRoute`](super::MoveError::UnsupportedSatelliteRoute).
    /// Senders MUST first see the `MOVE_RESOURCE` feature bit.
    MoveResource {
        /// Correlates this request with the eventual `ResourceMoved`.
        request_id: u32,
        /// The Terminal to re-parent.
        terminal: ResourceId,
        /// Existing Terminal whose owning window becomes the destination.
        owner_terminal: ResourceId,
    },

    /// `RESOURCE_MOVED` — reply to `MOVE_RESOURCE` (ADR-0056).
    ResourceMoved {
        /// Correlates this reply with a prior `MoveResource.request_id`.
        request_id: u32,
        /// Either the moved Terminal, or a structured error.
        result: MoveResult,
    },

    /// `RESOURCE_CLOSED` — a resource ended (`docs/spec/L1.md` §1 / §10.1).
    ResourceClosed {
        /// The resource that ended.
        terminal_id: ResourceId,
        /// Process exit code, or `None` for signals / unknown.
        exit_status: Option<i32>,
        /// Why it closed. Field 3, omitted for [`CloseReason::Unknown`].
        reason: CloseReason,
        /// The terminating signal, if any. Field 4, absent when `None`.
        signal: Option<i32>,
    },

    /// `RESIZE_TERMINAL` — per-Terminal PTY resize (`docs/spec/L1.md` §1 /
    /// §10.2), alongside `VIEWPORT_RESIZE`. A zero axis SHOULD be a no-op.
    ResizeTerminal {
        /// Target Terminal.
        terminal_id: ResourceId,
        /// New width in cells.
        cols: u16,
        /// New height in cells.
        rows: u16,
    },

    /// `COMMAND` — control-plane request envelope (`docs/spec/L1.md` §5,
    /// ADR-0021). Other frames MAY interleave before its `COMMAND_RESULT`.
    Command {
        /// Correlates this request with the eventual `CommandResult`.
        request_id: u32,
        /// The command to execute.
        command: Command,
    },

    /// `COMMAND_RESULT` — reply to a prior [`FrameKind::Command`].
    CommandResult {
        /// Correlates this reply with a prior `Command.request_id`.
        request_id: u32,
        /// The command's outcome.
        result: CommandResult,
    },

    /// `SUBSCRIBE_EVENTS` — idempotently opt into the pushed [`AgentEvent`]
    /// stream (`docs/spec/L1.md` §7.5). A pure registration: it does not
    /// attach, resize, or snapshot. Implicit teardown on detach.
    SubscribeEvents {
        /// Per-Terminal scope, or `None` for every Terminal the client may
        /// observe.
        terminal: Option<ResourceId>,
        /// Journal cursor (ADR-0123): replay retained events with a greater
        /// `seq` before going live, or report `journal_gap`. `None` is
        /// live-only.
        after_seq: Option<u64>,
    },

    /// `EVENT` — one pushed [`AgentEvent`] (`docs/spec/L1.md` §7.5).
    Event {
        /// The Terminal this event concerns, or `None` if server-scoped.
        terminal: Option<ResourceId>,
        /// The event payload.
        event: AgentEvent,
        /// The journal stamp (fields 3-6, ADR-0123), or `None` for an
        /// unjournaled event: everything from a server without
        /// `EVENT_JOURNAL`, and a `journal_gap` notice.
        stamp: Option<Box<EventStamp>>,
    },
}

impl InputEvent {
    /// Wrap this event in the matching `INPUT_*` [`FrameKind`] addressed to
    /// `terminal_id`. Lives here so `crate::input` stays a leaf.
    #[must_use]
    pub fn into_frame(self, terminal_id: ResourceId) -> FrameKind {
        match self {
            Self::Key(event) => FrameKind::InputKey { terminal_id, event },
            Self::Mouse(event) => FrameKind::InputMouse { terminal_id, event },
            Self::Focus(event) => FrameKind::InputFocus { terminal_id, event },
            Self::Paste(event) => FrameKind::InputPaste { terminal_id, event },
        }
    }
}

impl FrameKind {
    /// Type discriminant from `docs/spec/proto.md` §7.
    #[must_use]
    pub const fn type_byte(&self) -> u8 {
        match self {
            Self::Hello { .. } => TYPE_HELLO,
            Self::HelloOk { .. } => TYPE_HELLO_OK,
            Self::Ping { .. } => TYPE_PING,
            Self::Pong { .. } => TYPE_PONG,
            Self::ResourceOutput { .. } => TYPE_RESOURCE_OUTPUT,
            Self::Attach { .. } => TYPE_ATTACH,
            Self::Detach => TYPE_DETACH,
            Self::InputKey { .. } => TYPE_INPUT_KEY,
            Self::InputMouse { .. } => TYPE_INPUT_MOUSE,
            Self::InputFocus { .. } => TYPE_INPUT_FOCUS,
            Self::InputPaste { .. } => TYPE_INPUT_PASTE,
            Self::InputTerminalReply { .. } => TYPE_INPUT_TERMINAL_REPLY,
            Self::FrameAck { .. } => TYPE_FRAME_ACK,
            Self::ViewportResize { .. } => TYPE_VIEWPORT_RESIZE,
            Self::Attached { .. } => TYPE_ATTACHED,
            Self::AttachReady { .. } => TYPE_ATTACH_READY,
            Self::Detached { .. } => TYPE_DETACHED,
            Self::HistoryRequest { .. } => TYPE_HISTORY_REQUEST,
            Self::BootstrapBegin { .. } => TYPE_BOOTSTRAP_BEGIN,
            Self::BootstrapChunk { .. } => TYPE_BOOTSTRAP_CHUNK,
            Self::BootstrapReady { .. } => TYPE_BOOTSTRAP_READY,
            Self::HistoryPage { .. } => TYPE_HISTORY_PAGE,
            Self::BootstrapTombstone { .. } => TYPE_BOOTSTRAP_TOMBSTONE,
            Self::HistoryTombstone { .. } => TYPE_HISTORY_TOMBSTONE,
            Self::HistoryRejected { .. } => TYPE_HISTORY_REJECTED,
            Self::Bell { .. } => TYPE_BELL,
            Self::Error { .. } => TYPE_ERROR,
            Self::GetMetadata { .. } => TYPE_GET_METADATA,
            Self::SetMetadata { .. } => TYPE_SET_METADATA,
            Self::DeleteMetadata { .. } => TYPE_DELETE_METADATA,
            Self::ListMetadata { .. } => TYPE_LIST_METADATA,
            Self::SubscribeMetadata { .. } => TYPE_SUBSCRIBE_METADATA,
            Self::MetadataChanged { .. } => TYPE_METADATA_CHANGED,
            Self::MetadataValue { .. } => TYPE_METADATA_VALUE,
            Self::MetadataKeys { .. } => TYPE_METADATA_KEYS,
            Self::ListDirectory { .. } => TYPE_LIST_DIRECTORY,
            Self::DirectoryListing { .. } => TYPE_DIRECTORY_LISTING,
            Self::SpawnResource { .. } => TYPE_SPAWN_RESOURCE,
            Self::MoveResource { .. } => TYPE_MOVE_RESOURCE,
            Self::ResourceMoved { .. } => TYPE_RESOURCE_MOVED,
            Self::ResourceSpawned { .. } => TYPE_RESOURCE_SPAWNED,
            Self::ResourceClosed { .. } => TYPE_RESOURCE_CLOSED,
            Self::ResizeTerminal { .. } => TYPE_RESIZE_TERMINAL,
            Self::Command { .. } => TYPE_COMMAND,
            Self::CommandResult { .. } => TYPE_COMMAND_RESULT,
            Self::SubscribeEvents { .. } => TYPE_SUBSCRIBE_EVENTS,
            Self::Event { .. } => TYPE_EVENT,
        }
    }

    /// Encode `self`, wrapped in `FRAME_COMPRESSED` when `compression` was
    /// negotiated and deflate actually shrinks it; otherwise a plain
    /// [`Self::encode`]. `scratch` is a reusable buffer for the plain image.
    pub fn encode_compressed(
        &self,
        compression: Compression,
        scratch: &mut BytesMut,
        out: &mut BytesMut,
    ) {
        if compression == Compression::None {
            self.encode(out);
            return;
        }
        scratch.clear();
        self.encode(scratch);
        // The envelope carries the inner frame minus its length prefix.
        let Some(body) = scratch.get(LENGTH_PREFIX_LEN..) else {
            self.encode(out);
            return;
        };
        let Some(deflated) = crate::wire::compress::deflate(body) else {
            out.extend_from_slice(scratch);
            return;
        };
        let Ok(uncompressed_len) = u32::try_from(body.len()) else {
            out.extend_from_slice(scratch);
            return;
        };
        let header_pos = out.len();
        out.extend_from_slice(&[0u8; LENGTH_PREFIX_LEN]);
        let body_start = out.len();
        let mut enc = Encoder::new(out);
        enc.write_u8(TYPE_FRAME_COMPRESSED);
        u8_field(
            &mut enc,
            field::frame_compressed::ALGORITHM,
            compression.as_u8(),
        );
        u32_field(
            &mut enc,
            field::frame_compressed::UNCOMPRESSED_LEN,
            uncompressed_len,
        );
        enc.write_field(field::frame_compressed::PAYLOAD, &deflated);
        let body_len = out.len() - body_start;
        let len_u32 = u32::try_from(body_len).unwrap_or(u32::MAX);
        out[header_pos..header_pos + LENGTH_PREFIX_LEN].copy_from_slice(&len_u32.to_be_bytes());
    }

    /// Encode `self` as a complete length-prefixed frame.
    ///
    /// Writes the four-byte big-endian length header, the type byte, and the
    /// payload. The caller owns the `BytesMut` lifecycle.
    pub fn encode(&self, out: &mut BytesMut) {
        // Reserve four bytes for the length header; backfill once we know how
        // many bytes the type + payload consumed.
        let header_pos = out.len();
        out.extend_from_slice(&[0u8; 4]);

        let body_start = out.len();
        let mut enc = Encoder::new(out);
        enc.write_u8(self.type_byte());
        self.encode_payload(&mut enc);

        // Backfill the length header. The length value excludes the four
        // header bytes themselves but includes the type byte and payload, per
        // SPEC §5.
        let body_len = out.len() - body_start;
        debug_assert!(
            u32::try_from(body_len).is_ok_and(|n| n <= MAX_FRAME_LEN),
            "encoded frame exceeds protocol cap",
        );
        let len_u32 = u32::try_from(body_len).unwrap_or(u32::MAX);
        out[header_pos..header_pos + 4].copy_from_slice(&len_u32.to_be_bytes());
    }

    /// Write the field-tagged payload of `self`; the type byte is already out.
    ///
    /// One arm per SPEC §7 catalog entry, each a single call into the
    /// `encode_*` helper that owns that frame's field order. The table stays
    /// in one place so it can be read against the decoder's dispatch.
    #[allow(
        clippy::too_many_lines,
        reason = "one call per arm over the whole SPEC §7 catalog; the arms belong in a single table that mirrors the decoder's"
    )]
    fn encode_payload(&self, enc: &mut Encoder<'_>) {
        match self {
            Self::Hello {
                client_name,
                protocol_major,
                protocol_minor,
                protocol_patch,
                client_caps,
            } => Self::encode_hello(
                enc,
                client_name,
                *protocol_major,
                *protocol_minor,
                *protocol_patch,
                client_caps,
            ),
            Self::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                server_caps,
                server_id,
                selected_profile,
                bootstrap_limits,
            } => {
                Self::encode_hello_ok_version(
                    enc,
                    *protocol_major,
                    *protocol_minor,
                    *protocol_patch,
                );
                Self::encode_hello_ok_negotiation(
                    enc,
                    *server_caps,
                    server_id,
                    *selected_profile,
                    *bootstrap_limits,
                );
            }
            // `Ping` and `Pong` share a single-`u64` nonce field; merged to
            // satisfy `clippy::match_same_arms`.
            Self::Ping { nonce } | Self::Pong { nonce } => Self::encode_nonce(enc, *nonce),
            Self::ResourceOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => Self::encode_terminal_output(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                *seq,
                bytes,
            ),
            Self::Attach {
                attach_id,
                target,
                viewport,
                request_scrollback,
                scrollback_limit_lines,
                role_policy,
            } => {
                Self::encode_attach(
                    enc,
                    *attach_id,
                    target,
                    viewport,
                    *request_scrollback,
                    *scrollback_limit_lines,
                );
                if let Some(policy) = role_policy {
                    u8_field(enc, field::attach::ROLE_POLICY, policy.to_u8());
                }
            }
            // `Detach` is a unit variant: type byte only, no fields.
            Self::Detach => {}
            Self::Detached { reason, message } => Self::encode_detached(enc, *reason, message),
            Self::InputKey { terminal_id, event } => {
                Self::encode_input_key(enc, terminal_id, event);
            }
            Self::InputMouse { terminal_id, event } => {
                Self::encode_input_mouse(enc, terminal_id, event);
            }
            Self::InputFocus { terminal_id, event } => {
                Self::encode_input_focus(enc, terminal_id, *event);
            }
            Self::InputPaste { terminal_id, event } => {
                Self::encode_input_paste(enc, terminal_id, event);
            }
            Self::InputTerminalReply { terminal_id, bytes } => {
                Self::encode_input_terminal_reply(enc, terminal_id, bytes);
            }
            Self::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            } => Self::encode_frame_ack(enc, terminal_id, *stream_id, *bootstrap_id, *seq),
            Self::ViewportResize { viewport } => Self::encode_viewport_resize(enc, viewport),
            Self::Attached {
                attach_id,
                snapshot,
                initial_client_id,
            } => Self::encode_attached(enc, *attach_id, snapshot, *initial_client_id),
            Self::AttachReady { attach_id } => Self::encode_attach_ready(enc, *attach_id),
            Self::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                Self::encode_bootstrap_begin_profile(enc, *profile, *cols, *rows, *base_seq);
            }
            Self::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => Self::encode_bootstrap_chunk(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                *chunk_seq,
                payload,
            ),
            Self::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => Self::encode_bootstrap_ready(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                history_cursor.as_deref(),
            ),
            Self::HistoryRequest {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                max_bytes,
                max_rows,
            } => Self::encode_history_request(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                cursor,
                *max_bytes,
                *max_rows,
            ),
            Self::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                cursor,
                next_cursor,
                payload,
                rows,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                Self::encode_history_page_body(
                    enc,
                    cursor,
                    next_cursor.as_deref(),
                    payload,
                    *page_seq,
                    *rows,
                );
            }
            Self::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => Self::encode_bootstrap_tombstone(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                *reason,
                *last_valid_seq,
            ),
            Self::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => Self::encode_history_tombstone(
                enc,
                terminal_id,
                *stream_id,
                *bootstrap_id,
                cursor,
                *reason,
            ),
            Self::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => {
                generation_fields(enc, terminal_id, *stream_id, *bootstrap_id);
                Self::encode_history_rejected_reason(
                    enc,
                    cursor,
                    *reason,
                    *required_bytes,
                    *required_rows,
                );
            }
            Self::Bell { terminal_id } => Self::encode_bell(enc, terminal_id),
            Self::Error {
                request_id,
                code,
                message,
            } => Self::encode_error(enc, *request_id, *code, message),
            // GET / DELETE share `{request_id, scope, key}`; merged to
            // satisfy `clippy::match_same_arms`. The wire bodies are
            // intentionally identical — the discriminating type byte is
            // emitted before this match arm runs.
            Self::GetMetadata {
                request_id,
                scope,
                key,
            }
            | Self::DeleteMetadata {
                request_id,
                scope,
                key,
            } => Self::encode_get_or_delete_metadata(enc, *request_id, scope, key),
            Self::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } => Self::encode_set_metadata(enc, *request_id, scope, key, value),
            Self::ListMetadata { request_id, scope } => {
                Self::encode_list_metadata(enc, *request_id, scope);
            }
            Self::SubscribeMetadata { scope, key } => {
                Self::encode_subscribe_metadata(enc, scope, key);
            }
            Self::MetadataChanged {
                scope,
                key,
                value,
                actor,
            } => {
                Self::encode_metadata_changed(enc, scope, key, value.as_deref());
                if let Some(actor) = actor {
                    enc.write_field_with(field::metadata_changed::ACTOR, |e| {
                        encode_actor_ref(actor, e);
                    });
                }
            }
            Self::MetadataValue { request_id, value } => {
                Self::encode_metadata_value(enc, *request_id, value.as_deref());
            }
            Self::MetadataKeys { request_id, keys } => {
                Self::encode_metadata_keys(enc, *request_id, keys);
            }
            Self::ListDirectory {
                request_id,
                path,
                host,
            } => {
                encode_list_directory(enc, *request_id, path, host.as_ref());
            }
            Self::DirectoryListing { request_id, result } => {
                encode_directory_listing(enc, *request_id, result);
            }
            Self::SpawnResource {
                request_id,
                group,
                command,
                cwd,
                env,
                term,
                satellite,
                owner_terminal,
                agent_session,
                initial_size,
                resource,
            } => {
                Self::encode_spawn_terminal_request(enc, *request_id, *group);
                Self::encode_spawn_terminal_process(
                    enc,
                    command.as_deref(),
                    cwd.as_deref(),
                    env.as_deref(),
                    term.as_deref(),
                );
                Self::encode_spawn_terminal_placement(
                    enc,
                    satellite.as_ref(),
                    owner_terminal.as_ref(),
                    agent_session.as_deref(),
                    *initial_size,
                );
                if let Some(resource) = resource.as_deref() {
                    Self::encode_spawn_terminal_resource(enc, resource);
                }
            }
            Self::ResourceSpawned { request_id, result } => {
                Self::encode_terminal_spawned(enc, *request_id, result);
            }
            Self::MoveResource {
                request_id,
                terminal,
                owner_terminal,
            } => Self::encode_move_terminal(enc, *request_id, terminal, owner_terminal),
            Self::ResourceMoved { request_id, result } => {
                Self::encode_terminal_moved(enc, *request_id, result);
            }
            Self::ResourceClosed {
                terminal_id,
                exit_status,
                reason,
                signal,
            } => {
                Self::encode_terminal_closed(enc, terminal_id, *exit_status, *reason);
                if let Some(signal) = signal {
                    write_i32_field(enc, field::terminal_closed::SIGNAL, *signal);
                }
            }
            Self::ResizeTerminal {
                terminal_id,
                cols,
                rows,
            } => Self::encode_terminal_resize(enc, terminal_id, *cols, *rows),
            Self::Command {
                request_id,
                command,
            } => Self::encode_command_frame(enc, *request_id, command),
            Self::CommandResult { request_id, result } => {
                Self::encode_command_result_frame(enc, *request_id, result);
            }
            Self::SubscribeEvents {
                terminal,
                after_seq,
            } => {
                Self::encode_subscribe_events(enc, terminal.as_ref());
                if let Some(after_seq) = after_seq {
                    u64_field(enc, field::subscribe_events::AFTER_SEQ, *after_seq);
                }
            }
            Self::Event {
                terminal,
                event,
                stamp,
            } => {
                Self::encode_event(enc, terminal.as_ref(), event);
                if let Some(stamp) = stamp {
                    encode_event_stamp(enc, stamp);
                }
            }
        }
    }

    /// Write the `HELLO` payload (`docs/spec/proto.md` §6.1).
    fn encode_hello(
        enc: &mut Encoder<'_>,
        client_name: &str,
        protocol_major: u16,
        protocol_minor: u16,
        protocol_patch: u16,
        client_caps: &ClientCapabilities,
    ) {
        // String fields ride as raw UTF-8 bytes — the field is already
        // length-delimited by the TLV header, so no inner length prefix.
        enc.write_field(field::hello::CLIENT_NAME, client_name.as_bytes());
        u16_field(enc, field::hello::PROTOCOL_MAJOR, protocol_major);
        u16_field(enc, field::hello::PROTOCOL_MINOR, protocol_minor);
        u16_field(enc, field::hello::PROTOCOL_PATCH, protocol_patch);
        // ClientCapabilities remains a positional sub-record inside its
        // top-level TLV field. Protocol 0.7 fixes the complete order:
        // legacy render caps, palette presence/value, profile set,
        // exact native codec set/features, then receive bounds.
        enc.write_field_with(field::hello::CLIENT_CAPS, |e| {
            e.write_u8(client_caps.color_support.as_wire());
            e.write_u8(client_caps.layers.as_wire());
            e.write_u8(client_caps.image_protocols.as_wire());
            e.write_u8(client_caps.kbd_protocols.as_wire());
            e.write_u8(u8::from(client_caps.hyperlinks));
            e.write_u8(client_caps.output_mode.as_wire());
            if let Some(colors) = client_caps.default_colors {
                e.write_u8(1);
                e.write_u8(colors.foreground.r);
                e.write_u8(colors.foreground.g);
                e.write_u8(colors.foreground.b);
                e.write_u8(colors.background.r);
                e.write_u8(colors.background.g);
                e.write_u8(colors.background.b);
            } else {
                e.write_u8(0);
            }
            e.write_u8(client_caps.bootstrap.profiles.as_wire());
            e.write_u64_be(client_caps.bootstrap.native_codecs.as_wire());
            e.write_u32_be(client_caps.bootstrap.native_features.as_wire());
            e.write_u32_be(client_caps.bootstrap.limits.max_chunk_bytes());
            e.write_u32_be(client_caps.bootstrap.limits.max_history_page_bytes());
        });
        // Additive top-level field, omitted when the client accepts nothing
        // compressed — which is every local consumer, so a UDS HELLO stays
        // byte-identical to what protocol 0.8 has always emitted.
        if !client_caps.compression.is_empty() {
            u8_field(
                enc,
                field::hello::COMPRESSION,
                client_caps.compression.bits(),
            );
        }
        // Additive top-level field that `phux stdio-bridge` stamps on the
        // HELLO it relays. Ordinary clients leave it absent.
        if let Some(origin) = client_caps.ssh_origin {
            enc.write_field_with(field::hello::SSH_ORIGIN, |e| {
                crate::wire::ssh_origin::encode_ssh_origin(&origin, e);
            });
        }
        if client_caps.quic_streams {
            u8_field(enc, field::hello::QUIC_STREAMS, 1);
        }
    }

    /// Write the exact protocol version `HELLO_OK` admits the peer at.
    fn encode_hello_ok_version(
        enc: &mut Encoder<'_>,
        protocol_major: u16,
        protocol_minor: u16,
        protocol_patch: u16,
    ) {
        u16_field(enc, field::hello_ok::PROTOCOL_MAJOR, protocol_major);
        u16_field(enc, field::hello_ok::PROTOCOL_MINOR, protocol_minor);
        u16_field(enc, field::hello_ok::PROTOCOL_PATCH, protocol_patch);
    }

    /// Write the terms `HELLO_OK` negotiates: caps, identity, profile, bounds.
    fn encode_hello_ok_negotiation(
        enc: &mut Encoder<'_>,
        server_caps: ServerCapabilities,
        server_id: &[u8],
        selected_profile: BootstrapProfile,
        bootstrap_limits: BootstrapLimits,
    ) {
        enc.write_field_with(field::hello_ok::SERVER_CAPS, |e| {
            e.write_u8(server_caps.layers.as_wire());
            if !server_caps.features.is_empty() {
                e.write_u32_be(server_caps.features.as_wire());
            }
        });
        // server_id is opaque bytes; the field is already
        // length-delimited so the raw bytes are the value.
        enc.write_field(field::hello_ok::SERVER_ID, server_id);
        enc.write_field_with(field::hello_ok::SELECTED_PROFILE, |e| {
            encode_bootstrap_profile(selected_profile, e);
        });
        u32_field(
            enc,
            field::hello_ok::MAX_CHUNK_BYTES,
            bootstrap_limits.max_chunk_bytes(),
        );
        u32_field(
            enc,
            field::hello_ok::MAX_HISTORY_PAGE_BYTES,
            bootstrap_limits.max_history_page_bytes(),
        );
        // Same discipline as HELLO's offer field: omitted when nothing was
        // selected, so an uncompressed connection's HELLO_OK is unchanged.
        if server_caps.compression != Compression::None {
            u8_field(
                enc,
                field::hello_ok::COMPRESSION,
                server_caps.compression.as_u8(),
            );
        }
    }

    /// Write the single nonce field shared by `PING` and `PONG`.
    fn encode_nonce(enc: &mut Encoder<'_>, nonce: u64) {
        u64_field(enc, field::ping::NONCE, nonce);
    }

    /// Write the `RESOURCE_OUTPUT` payload: VT bytes bound to a generation.
    fn encode_terminal_output(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
        bytes: &[u8],
    ) {
        id_field(enc, field::terminal_output::TERMINAL_ID, terminal_id);
        u64_field(enc, field::terminal_output::SEQ, seq);
        enc.write_field(field::terminal_output::BYTES, bytes);
        u64_field(enc, field::terminal_output::STREAM_ID, stream_id.get());
        u64_field(
            enc,
            field::terminal_output::BOOTSTRAP_ID,
            bootstrap_id.get(),
        );
    }

    /// Write the `ATTACH` payload.
    fn encode_attach(
        enc: &mut Encoder<'_>,
        attach_id: u32,
        target: &AttachTarget,
        viewport: &ViewportInfo,
        request_scrollback: bool,
        scrollback_limit_lines: u32,
    ) {
        enc.write_field_with(field::attach::TARGET, |e| encode_attach_target(target, e));
        enc.write_field_with(field::attach::VIEWPORT, |e| {
            encode_viewport_info(viewport, e);
        });
        u8_field(
            enc,
            field::attach::REQUEST_SCROLLBACK,
            u8::from(request_scrollback),
        );
        u32_field(
            enc,
            field::attach::SCROLLBACK_LIMIT_LINES,
            scrollback_limit_lines,
        );
        u32_field(enc, field::attach::ATTACH_ID, attach_id);
    }

    /// Write the `DETACHED` payload.
    ///
    /// Both fields are optional-absent (field.rs allocation discipline): an
    /// unstated reason and an empty message encode as nothing at all, which
    /// keeps the common acknowledge-a-clean-detach frame byte-identical to
    /// what every 0.7.0 peer already emits.
    fn encode_detached(enc: &mut Encoder<'_>, reason: Option<DetachReason>, message: &str) {
        if let Some(reason) = reason {
            u8_field(enc, field::detached::REASON, reason.as_wire());
        }
        if !message.is_empty() {
            enc.write_field(field::detached::MESSAGE, message.as_bytes());
        }
    }

    /// Write the `INPUT_KEY` payload.
    fn encode_input_key(enc: &mut Encoder<'_>, terminal_id: &ResourceId, event: &KeyEvent) {
        id_field(enc, field::input_key::TERMINAL_ID, terminal_id);
        enc.write_field_with(field::input_key::EVENT, |e| encode_key_event(event, e));
    }

    /// Write the `INPUT_MOUSE` payload.
    fn encode_input_mouse(enc: &mut Encoder<'_>, terminal_id: &ResourceId, event: &MouseEvent) {
        id_field(enc, field::input_mouse::TERMINAL_ID, terminal_id);
        enc.write_field_with(field::input_mouse::EVENT, |e| encode_mouse_event(event, e));
    }

    /// Write the `INPUT_FOCUS` payload.
    fn encode_input_focus(enc: &mut Encoder<'_>, terminal_id: &ResourceId, event: FocusEvent) {
        id_field(enc, field::input_focus::TERMINAL_ID, terminal_id);
        u8_field(enc, field::input_focus::EVENT, encode_focus_event(event));
    }

    /// Write the `INPUT_PASTE` payload.
    fn encode_input_paste(enc: &mut Encoder<'_>, terminal_id: &ResourceId, event: &PasteEvent) {
        id_field(enc, field::input_paste::TERMINAL_ID, terminal_id);
        enc.write_field_with(field::input_paste::EVENT, |e| encode_paste_event(event, e));
    }

    /// Write the `INPUT_TERMINAL_REPLY` payload.
    fn encode_input_terminal_reply(enc: &mut Encoder<'_>, terminal_id: &ResourceId, bytes: &[u8]) {
        id_field(enc, field::input_terminal_reply::TERMINAL_ID, terminal_id);
        enc.write_field(field::input_terminal_reply::BYTES, bytes);
    }

    /// Write the `FRAME_ACK` payload.
    fn encode_frame_ack(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        seq: u64,
    ) {
        id_field(enc, field::frame_ack::TERMINAL_ID, terminal_id);
        u64_field(enc, field::frame_ack::SEQ, seq);
        u64_field(enc, field::frame_ack::STREAM_ID, stream_id.get());
        u64_field(enc, field::frame_ack::BOOTSTRAP_ID, bootstrap_id.get());
    }

    /// Write the `VIEWPORT_RESIZE` payload.
    fn encode_viewport_resize(enc: &mut Encoder<'_>, viewport: &ViewportInfo) {
        enc.write_field_with(field::viewport_resize::VIEWPORT, |e| {
            encode_viewport_info(viewport, e);
        });
    }

    /// Write the `ATTACHED` payload.
    fn encode_attached(
        enc: &mut Encoder<'_>,
        attach_id: u32,
        snapshot: &SessionSnapshot,
        initial_client_id: ClientId,
    ) {
        enc.write_field_with(field::attached::SNAPSHOT, |e| {
            encode_session_snapshot(snapshot, e);
        });
        enc.write_field_with(field::attached::INITIAL_CLIENT_ID, |e| {
            encode_client_id(initial_client_id, e);
        });
        u32_field(enc, field::attached::ATTACH_ID, attach_id);
    }

    /// Write the `ATTACH_READY` payload.
    fn encode_attach_ready(enc: &mut Encoder<'_>, attach_id: u32) {
        u32_field(enc, field::attach_ready::ATTACH_ID, attach_id);
    }

    /// Write the profile half of `BOOTSTRAP_BEGIN`: codec, geometry, base seq.
    ///
    /// The stream profile projects onto the wire as a `(codec, output_mode)`
    /// pair; native state always means raw, byte-identical PTY continuation.
    fn encode_bootstrap_begin_profile(
        enc: &mut Encoder<'_>,
        profile: BootstrapStreamProfile,
        cols: u16,
        rows: u16,
        base_seq: u64,
    ) {
        let (codec, output_mode) = match profile {
            BootstrapStreamProfile::NativeState { codec } => {
                (BootstrapCodec::Native(codec), OutputMode::Raw)
            }
            BootstrapStreamProfile::SynthesizedVtRaw => {
                (BootstrapCodec::SynthesizedVtV1, OutputMode::Raw)
            }
            BootstrapStreamProfile::SynthesizedVtStateSync => {
                (BootstrapCodec::SynthesizedVtV1, OutputMode::StateSync)
            }
            BootstrapStreamProfile::AgentEventsJsonlV1 => {
                (BootstrapCodec::AgentEventsJsonlV1, OutputMode::Raw)
            }
        };
        enc.write_field_with(field::bootstrap_begin::CODEC, |e| {
            encode_bootstrap_codec(codec, e);
        });
        u16_field(enc, field::bootstrap_begin::COLS, cols);
        u16_field(enc, field::bootstrap_begin::ROWS, rows);
        u8_field(
            enc,
            field::bootstrap_begin::OUTPUT_MODE,
            output_mode.as_wire(),
        );
        u64_field(enc, field::bootstrap_begin::BASE_SEQ, base_seq);
    }

    /// Write the `BOOTSTRAP_CHUNK` payload.
    fn encode_bootstrap_chunk(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        chunk_seq: u32,
        payload: &[u8],
    ) {
        generation_fields(enc, terminal_id, stream_id, bootstrap_id);
        u32_field(enc, field::bootstrap_chunk::CHUNK_SEQ, chunk_seq);
        enc.write_field(field::bootstrap_chunk::PAYLOAD, payload);
    }

    /// Write the `BOOTSTRAP_READY` payload.
    fn encode_bootstrap_ready(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        history_cursor: Option<&[u8]>,
    ) {
        generation_fields(enc, terminal_id, stream_id, bootstrap_id);
        if let Some(cursor) = history_cursor {
            enc.write_field(field::bootstrap_ready::HISTORY_CURSOR, cursor);
        }
    }

    /// Write the `HISTORY_REQUEST` payload.
    fn encode_history_request(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: &[u8],
        max_bytes: u32,
        max_rows: u32,
    ) {
        generation_fields(enc, terminal_id, stream_id, bootstrap_id);
        enc.write_field(field::history_request::CURSOR, cursor);
        u32_field(enc, field::history_request::MAX_BYTES, max_bytes);
        u32_field(enc, field::history_request::MAX_ROWS, max_rows);
    }

    /// Write the page half of `HISTORY_PAGE`: cursors, payload, and counts.
    fn encode_history_page_body(
        enc: &mut Encoder<'_>,
        cursor: &[u8],
        next_cursor: Option<&[u8]>,
        payload: &[u8],
        page_seq: u64,
        rows: u32,
    ) {
        enc.write_field(field::history_page::CURSOR, cursor);
        if let Some(next) = next_cursor {
            enc.write_field(field::history_page::NEXT_CURSOR, next);
        }
        enc.write_field(field::history_page::PAYLOAD, payload);
        u64_field(enc, field::history_page::PAGE_SEQ, page_seq);
        u32_field(enc, field::history_page::ROWS, rows);
    }

    /// Write the `BOOTSTRAP_TOMBSTONE` payload.
    fn encode_bootstrap_tombstone(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        reason: TombstoneReason,
        last_valid_seq: u64,
    ) {
        generation_fields(enc, terminal_id, stream_id, bootstrap_id);
        u8_field(enc, field::bootstrap_tombstone::REASON, reason.as_wire());
        u64_field(
            enc,
            field::bootstrap_tombstone::LAST_VALID_SEQ,
            last_valid_seq,
        );
    }

    /// Write the `HISTORY_TOMBSTONE` payload.
    fn encode_history_tombstone(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
        cursor: &[u8],
        reason: HistoryTombstoneReason,
    ) {
        generation_fields(enc, terminal_id, stream_id, bootstrap_id);
        enc.write_field(field::history_tombstone::CURSOR, cursor);
        u8_field(enc, field::history_tombstone::REASON, reason.as_wire());
    }

    /// Write why `HISTORY_REJECTED` refused the cursor, and what it would need.
    fn encode_history_rejected_reason(
        enc: &mut Encoder<'_>,
        cursor: &[u8],
        reason: HistoryRejectionReason,
        required_bytes: u32,
        required_rows: u32,
    ) {
        enc.write_field(field::history_rejected::CURSOR, cursor);
        u8_field(enc, field::history_rejected::REASON, reason.as_wire());
        u32_field(enc, field::history_rejected::REQUIRED_BYTES, required_bytes);
        u32_field(enc, field::history_rejected::REQUIRED_ROWS, required_rows);
    }

    /// Write the `BELL` payload.
    fn encode_bell(enc: &mut Encoder<'_>, terminal_id: &ResourceId) {
        id_field(enc, field::bell::TERMINAL_ID, terminal_id);
    }

    /// Write the `ERROR` payload.
    fn encode_error(
        enc: &mut Encoder<'_>,
        request_id: Option<u32>,
        code: ErrorCode,
        message: &str,
    ) {
        // Optional request_id: absent field = None.
        if let Some(id) = request_id {
            u32_field(enc, field::error::REQUEST_ID, id);
        }
        u16_field(enc, field::error::CODE, code.as_wire());
        enc.write_field(field::error::MESSAGE, message.as_bytes());
    }

    /// Write the `{request_id, scope, key}` body shared by GET and DELETE.
    fn encode_get_or_delete_metadata(
        enc: &mut Encoder<'_>,
        request_id: u32,
        scope: &Scope,
        key: &str,
    ) {
        u32_field(enc, field::get_metadata::REQUEST_ID, request_id);
        enc.write_field_with(field::get_metadata::SCOPE, |e| encode_scope(scope, e));
        enc.write_field(field::get_metadata::KEY, key.as_bytes());
    }

    /// Write the `SET_METADATA` payload.
    fn encode_set_metadata(
        enc: &mut Encoder<'_>,
        request_id: u32,
        scope: &Scope,
        key: &str,
        value: &[u8],
    ) {
        u32_field(enc, field::set_metadata::REQUEST_ID, request_id);
        enc.write_field_with(field::set_metadata::SCOPE, |e| encode_scope(scope, e));
        enc.write_field(field::set_metadata::KEY, key.as_bytes());
        enc.write_field(field::set_metadata::VALUE, value);
    }

    /// Write the `LIST_METADATA` payload.
    fn encode_list_metadata(enc: &mut Encoder<'_>, request_id: u32, scope: &Scope) {
        u32_field(enc, field::list_metadata::REQUEST_ID, request_id);
        enc.write_field_with(field::list_metadata::SCOPE, |e| encode_scope(scope, e));
    }

    /// Write the `SUBSCRIBE_METADATA` payload.
    fn encode_subscribe_metadata(enc: &mut Encoder<'_>, scope: &Scope, key: &str) {
        enc.write_field_with(field::subscribe_metadata::SCOPE, |e| encode_scope(scope, e));
        enc.write_field(field::subscribe_metadata::KEY, key.as_bytes());
    }

    /// Write the `METADATA_CHANGED` payload.
    fn encode_metadata_changed(
        enc: &mut Encoder<'_>,
        scope: &Scope,
        key: &str,
        value: Option<&[u8]>,
    ) {
        enc.write_field_with(field::metadata_changed::SCOPE, |e| encode_scope(scope, e));
        enc.write_field(field::metadata_changed::KEY, key.as_bytes());
        // Optional value: absent field = tombstone (None).
        if let Some(v) = value {
            enc.write_field(field::metadata_changed::VALUE, v);
        }
    }

    /// Write the `METADATA_VALUE` payload.
    fn encode_metadata_value(enc: &mut Encoder<'_>, request_id: u32, value: Option<&[u8]>) {
        u32_field(enc, field::metadata_value::REQUEST_ID, request_id);
        // Optional value: absent field = key absent (None).
        if let Some(v) = value {
            enc.write_field(field::metadata_value::VALUE, v);
        }
    }

    /// Write the `METADATA_KEYS` payload.
    fn encode_metadata_keys(enc: &mut Encoder<'_>, request_id: u32, keys: &[String]) {
        u32_field(enc, field::metadata_keys::REQUEST_ID, request_id);
        // The keys list is one field whose value is a positional u32
        // count + N length-prefixed strings (present even when empty).
        enc.write_field_with(field::metadata_keys::KEYS, |e| {
            debug_assert!(
                u32::try_from(keys.len()).is_ok(),
                "metadata keys list length exceeds u32",
            );
            let len = u32::try_from(keys.len()).unwrap_or(u32::MAX);
            e.write_u32_be(len);
            for k in keys {
                e.write_str(k);
            }
        });
    }

    /// Write the request identity that opens the `SPAWN_RESOURCE` payload.
    fn encode_spawn_terminal_request(enc: &mut Encoder<'_>, request_id: u32, group: GroupId) {
        u32_field(enc, field::spawn_terminal::REQUEST_ID, request_id);
        u32_field(enc, field::spawn_terminal::GROUP, group.get());
    }

    /// Write the process shape `SPAWN_RESOURCE` asks the server to launch.
    ///
    /// Optional command/cwd/env: absent field = None. An empty list
    /// (`Some(vec![])`) stays distinct: a present field with a zero count.
    fn encode_spawn_terminal_process(
        enc: &mut Encoder<'_>,
        command: Option<&[String]>,
        cwd: Option<&str>,
        env_vars: Option<&[(String, String)]>,
        term: Option<&str>,
    ) {
        if let Some(cmd) = command {
            enc.write_field_with(field::spawn_terminal::COMMAND, |e| {
                encode_string_list(cmd, e);
            });
        }
        if let Some(c) = cwd {
            enc.write_field(field::spawn_terminal::CWD, c.as_bytes());
        }
        if let Some(vars) = env_vars {
            enc.write_field_with(field::spawn_terminal::ENV, |e| encode_env(vars, e));
        }
        if let Some(t) = term {
            enc.write_field(field::spawn_terminal::TERM, t.as_bytes());
        }
    }

    /// Write where the spawned terminal lands: host, owner, session, size.
    fn encode_spawn_terminal_placement(
        enc: &mut Encoder<'_>,
        satellite: Option<&SatelliteHost>,
        owner_terminal: Option<&ResourceId>,
        agent_session: Option<&[u8]>,
        initial_size: Option<(u16, u16)>,
    ) {
        if let Some(host) = satellite {
            enc.write_field(field::spawn_terminal::SATELLITE, host.as_str().as_bytes());
        }
        if let Some(owner) = owner_terminal {
            id_field(enc, field::spawn_terminal::OWNER_TERMINAL, owner);
        }
        if let Some(value) = agent_session {
            enc.write_field(field::spawn_terminal::AGENT_SESSION, value);
        }
        if let Some((cols, rows)) = initial_size {
            enc.write_field_with(field::spawn_terminal::INITIAL_SIZE, |e| {
                e.write_u16_be(cols);
                e.write_u16_be(rows);
            });
        }
    }

    /// Write what kind of resource the spawn creates and, for a child kind,
    /// its binding and facet (fields 11-14).
    ///
    /// `kind` is written only when it is not the Terminal default, so a
    /// Terminal spawn's byte image is identical to one encoded before the
    /// field existed; the other three are absent-is-`None` optionals.
    fn encode_spawn_terminal_resource(enc: &mut Encoder<'_>, resource: &SpawnResource) {
        if !resource.kind.is_terminal() {
            enc.write_field(field::spawn_terminal::KIND, &[resource.kind.as_wire()]);
        }
        if let Some(parent) = &resource.parent {
            id_field(enc, field::spawn_terminal::PARENT, parent);
        }
        if let Some(provider) = &resource.provider {
            enc.write_field(field::spawn_terminal::PROVIDER, provider.as_bytes());
        }
        if let Some(native_id) = &resource.native_id {
            enc.write_field(field::spawn_terminal::NATIVE_ID, native_id.as_bytes());
        }
        if resource.bind_instance {
            enc.write_field(field::spawn_terminal::BIND_INSTANCE, &[1]);
        }
        if let Some(secs) = resource.retain_secs {
            u32_field(enc, field::spawn_terminal::RETAIN_SECS, secs);
        }
        if let Some(key) = &resource.idempotency_key {
            enc.write_field(field::spawn_terminal::IDEMPOTENCY_KEY, key.as_bytes());
        }
    }

    /// Write the `RESOURCE_SPAWNED` payload. A bound result adds field 3 and
    /// a replayed one field 4, so a plain reply keeps the bytes it always
    /// had.
    fn encode_terminal_spawned(enc: &mut Encoder<'_>, request_id: u32, result: &SpawnResult) {
        u32_field(enc, field::terminal_spawned::REQUEST_ID, request_id);
        enc.write_field_with(field::terminal_spawned::RESULT, |e| {
            encode_spawn_result(result, e);
        });
        if let Some(instance) = result.instance() {
            enc.write_field_with(field::terminal_spawned::INSTANCE, |e| {
                super::encode_server_instance(&instance, e);
            });
        }
        if result.is_replayed() {
            enc.write_field(field::terminal_spawned::REPLAYED, &[1]);
        }
    }

    /// Write the `MOVE_RESOURCE` payload.
    fn encode_move_terminal(
        enc: &mut Encoder<'_>,
        request_id: u32,
        terminal: &ResourceId,
        owner_terminal: &ResourceId,
    ) {
        u32_field(enc, field::move_terminal::REQUEST_ID, request_id);
        id_field(enc, field::move_terminal::TERMINAL, terminal);
        id_field(enc, field::move_terminal::OWNER_TERMINAL, owner_terminal);
    }

    /// Write the `RESOURCE_MOVED` payload.
    fn encode_terminal_moved(enc: &mut Encoder<'_>, request_id: u32, result: &MoveResult) {
        u32_field(enc, field::terminal_moved::REQUEST_ID, request_id);
        enc.write_field_with(field::terminal_moved::RESULT, |e| {
            encode_move_result(result, e);
        });
    }

    /// Write the `RESOURCE_CLOSED` payload.
    fn encode_terminal_closed(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        exit_status: Option<i32>,
        reason: CloseReason,
    ) {
        id_field(enc, field::terminal_closed::TERMINAL_ID, terminal_id);
        // Optional exit status: absent field = signal / unknown.
        if let Some(status) = exit_status {
            u32_field(
                enc,
                field::terminal_closed::EXIT_STATUS,
                u32::from_be_bytes(status.to_be_bytes()),
            );
        }
        // Optional reason: absent field = unstated, so an unstated reason
        // leaves the body byte-identical to a pre-reason encoder's.
        if !reason.is_unknown() {
            enc.write_field(field::terminal_closed::REASON, &[reason.as_wire()]);
        }
    }

    /// Write the `RESIZE_TERMINAL` payload.
    fn encode_terminal_resize(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        cols: u16,
        rows: u16,
    ) {
        id_field(enc, field::terminal_resize::TERMINAL_ID, terminal_id);
        u16_field(enc, field::terminal_resize::COLS, cols);
        u16_field(enc, field::terminal_resize::ROWS, rows);
    }

    /// Write the `COMMAND` payload.
    fn encode_command_frame(enc: &mut Encoder<'_>, request_id: u32, command: &Command) {
        u32_field(enc, field::command::REQUEST_ID, request_id);
        enc.write_field_with(field::command::COMMAND, |e| encode_command(command, e));
    }

    /// Write the `COMMAND_RESULT` payload.
    fn encode_command_result_frame(enc: &mut Encoder<'_>, request_id: u32, result: &CommandResult) {
        u32_field(enc, field::command_result::REQUEST_ID, request_id);
        enc.write_field_with(field::command_result::RESULT, |e| {
            encode_command_result(result, e);
        });
    }

    /// Write the `SUBSCRIBE_EVENTS` payload.
    fn encode_subscribe_events(enc: &mut Encoder<'_>, terminal: Option<&ResourceId>) {
        // Optional terminal scope: absent field = server-scoped None.
        if let Some(t) = terminal {
            id_field(enc, field::subscribe_events::TERMINAL, t);
        }
    }

    /// Write the `EVENT` payload's scope and event (fields 1-2).
    fn encode_event(enc: &mut Encoder<'_>, terminal: Option<&ResourceId>, event: &AgentEvent) {
        if let Some(t) = terminal {
            id_field(enc, field::event::TERMINAL, t);
        }
        enc.write_field_with(field::event::EVENT, |e| encode_agent_event(event, e));
    }

    /// Decode a single frame from `input`. Returns the decoded frame and the
    /// unconsumed tail of `input`.
    pub fn decode(input: &[u8]) -> Result<(Self, &[u8]), DecodeError> {
        Decoder::new(input).read_frame()
    }

    /// Decode one frame using the payload limits negotiated in `HELLO_OK`.
    ///
    /// Bootstrap/history payload lengths are rejected against `limits` while
    /// still borrowed from the input, before an owned payload copy is made.
    pub fn decode_with_limits(
        input: &[u8],
        limits: BootstrapLimits,
    ) -> Result<(Self, &[u8]), DecodeError> {
        Decoder::with_bootstrap_limits(input, limits).read_frame()
    }
}

/// Write one tagged `ResourceId` field.
fn id_field(enc: &mut Encoder<'_>, field_id: u32, id: &ResourceId) {
    enc.write_field_with(field_id, |e| encode_terminal_id(id, e));
}

/// Write fields 1-3 (resource, stream, generation) that open every
/// bootstrap and history frame (`docs/spec/L1.md` §4.3).
fn generation_fields(
    enc: &mut Encoder<'_>,
    terminal_id: &ResourceId,
    stream_id: StreamId,
    bootstrap_id: BootstrapId,
) {
    id_field(enc, field::bootstrap_begin::TERMINAL_ID, terminal_id);
    u64_field(enc, field::bootstrap_begin::STREAM_ID, stream_id.get());
    u64_field(
        enc,
        field::bootstrap_begin::BOOTSTRAP_ID,
        bootstrap_id.get(),
    );
}

/// Write one fixed-width `u8` field.
fn u8_field(enc: &mut Encoder<'_>, field_id: u32, value: u8) {
    enc.write_field_with(field_id, |e| e.write_u8(value));
}

/// Write one fixed-width `u16` field.
fn u16_field(enc: &mut Encoder<'_>, field_id: u32, value: u16) {
    enc.write_field_with(field_id, |e| e.write_u16_be(value));
}

/// Write one fixed-width `u32` field.
fn u32_field(enc: &mut Encoder<'_>, field_id: u32, value: u32) {
    enc.write_field_with(field_id, |e| e.write_u32_be(value));
}

/// Write one fixed-width `u64` field.
fn u64_field(enc: &mut Encoder<'_>, field_id: u32, value: u64) {
    enc.write_field_with(field_id, |e| e.write_u64_be(value));
}

/// Write one field whose value is an `i32` as its two's-complement `u32`.
fn write_i32_field(enc: &mut Encoder<'_>, field_id: u32, value: i32) {
    u32_field(enc, field_id, u32::from_be_bytes(value.to_be_bytes()));
}

/// Write an `EVENT` journal stamp (fields 3-6, ADR-0123). `seq` and `ts_ms`
/// always travel together; `actor` and `operation_id` only when set.
fn encode_event_stamp(enc: &mut Encoder<'_>, stamp: &EventStamp) {
    u64_field(enc, field::event::SEQ, stamp.seq);
    u64_field(enc, field::event::TS_MS, stamp.ts_ms);
    if let Some(actor) = &stamp.actor {
        enc.write_field_with(field::event::ACTOR, |e| encode_actor_ref(actor, e));
    }
    if let Some(key) = &stamp.operation_id {
        enc.write_field(field::event::OPERATION_ID, key.as_bytes());
    }
}
