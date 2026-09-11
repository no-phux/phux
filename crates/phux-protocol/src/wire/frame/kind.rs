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
    AgentEvent, AttachTarget, CloseReason, Command, CommandResult, DetachReason, ErrorCode,
    HistoryRejectionReason, HistoryTombstoneReason, MAX_FRAME_LEN, MoveResult, Scope,
    SpawnResource, SpawnResult, TYPE_ATTACH, TYPE_ATTACH_READY, TYPE_ATTACHED, TYPE_BELL,
    TYPE_BOOTSTRAP_BEGIN, TYPE_BOOTSTRAP_CHUNK, TYPE_BOOTSTRAP_READY, TYPE_BOOTSTRAP_TOMBSTONE,
    TYPE_COMMAND, TYPE_COMMAND_RESULT, TYPE_DELETE_METADATA, TYPE_DETACH, TYPE_DETACHED,
    TYPE_DIRECTORY_LISTING, TYPE_ERROR, TYPE_EVENT, TYPE_FRAME_ACK, TYPE_FRAME_COMPRESSED,
    TYPE_GET_METADATA, TYPE_HELLO, TYPE_HELLO_OK, TYPE_HISTORY_PAGE, TYPE_HISTORY_REJECTED,
    TYPE_HISTORY_REQUEST, TYPE_HISTORY_TOMBSTONE, TYPE_INPUT_FOCUS, TYPE_INPUT_KEY,
    TYPE_INPUT_MOUSE, TYPE_INPUT_PASTE, TYPE_INPUT_TERMINAL_REPLY, TYPE_LIST_DIRECTORY,
    TYPE_LIST_METADATA, TYPE_METADATA_CHANGED, TYPE_METADATA_KEYS, TYPE_METADATA_VALUE,
    TYPE_MOVE_RESOURCE, TYPE_PING, TYPE_PONG, TYPE_RESIZE_TERMINAL, TYPE_RESOURCE_CLOSED,
    TYPE_RESOURCE_MOVED, TYPE_RESOURCE_OUTPUT, TYPE_RESOURCE_SPAWNED, TYPE_SET_METADATA,
    TYPE_SPAWN_RESOURCE, TYPE_SUBSCRIBE_EVENTS, TYPE_SUBSCRIBE_METADATA, TYPE_VIEWPORT_RESIZE,
    TombstoneReason, ViewportInfo, encode_agent_event, encode_attach_target,
    encode_bootstrap_codec, encode_bootstrap_profile, encode_command, encode_command_result,
    encode_env, encode_focus_event, encode_key_event, encode_mouse_event, encode_move_result,
    encode_paste_event, encode_scope, encode_spawn_result, encode_string_list, encode_terminal_id,
    encode_viewport_info,
};

/// Decoded wire frame.
///
/// The phux-6yl.4 scaffold populated `Hello`, `Ping`, and `PaneDiff`. The
/// phux-4az pass added the message-catalog variants needed for the attach
/// lifecycle. Protocol 0.7 replaces the retired synthesized snapshot frame with
/// explicit bootstrap/profile/history frames from ADR-0070. `ResourceOutput`
/// remains VT bytes, now bound to a non-zero stream and bootstrap generation.
///
/// [ADR-0013]: https://github.com/no-phux/phux/blob/main/ADR/0013-libghostty-bytes-on-wire.md
#[derive(Debug, Clone, PartialEq)]
#[non_exhaustive]
pub enum FrameKind {
    /// `HELLO` — client to server handshake (`docs/spec/proto.md` §6.1).
    ///
    /// Carries the client's identifier, exact protocol version, and required
    /// protocol-0.7 [`ClientCapabilities`] record. A missing or truncated
    /// capability record is malformed; protocol 0.6 and 0.7 use their
    /// major/minor admission boundary rather than compatibility defaults.
    Hello {
        /// Free-form client identifier (e.g. `"phux-client 0.1.0"`).
        client_name: String,
        /// Highest protocol major version the client supports.
        protocol_major: u16,
        /// Highest protocol minor version the client supports.
        protocol_minor: u16,
        /// Highest protocol patch version the client supports.
        protocol_patch: u16,
        /// Client capability advertisement (SPEC §6.2). Drives server-side
        /// VT byte-stream downsampling via [`crate::caps::ColorSupport`].
        client_caps: ClientCapabilities,
    },
    /// `HELLO_OK` — server handshake acknowledgement (`docs/spec/proto.md` §6.1).
    ///
    /// Protocol 0.7 selects one explicit [`BootstrapProfile`] and exact
    /// negotiated payload limits. Native selection contains the immutable
    /// libghostty codec and required feature intersection; compatibility
    /// selection contains its `OutputMode`. No later frame can switch profile.
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
        /// Opaque server identity bytes (SPEC §6.1). Not interpreted by
        /// the client today; reserved for reconnect / multi-server routing.
        server_id: Vec<u8>,
        /// Explicit synchronization profile selected for this connection.
        selected_profile: BootstrapProfile,
        /// Negotiated bootstrap/history payload bounds.
        bootstrap_limits: BootstrapLimits,
    },

    /// `PING` — liveness probe (`docs/spec/proto.md` §7.4). The peer MUST echo `nonce`
    /// back in a `PONG` frame.
    Ping {
        /// Opaque nonce echoed by the peer in `PONG`.
        nonce: u64,
    },
    /// `PONG` — liveness response (`docs/spec/proto.md` §7.4). Echoes the
    /// nonce from a prior [`FrameKind::Ping`].
    Pong {
        /// Nonce echoed from the corresponding `PING`.
        nonce: u64,
    },

    /// `RESOURCE_OUTPUT` — live terminal content (`docs/spec/L1.md` §4.1).
    ///
    /// `stream_id` and `bootstrap_id` bind every live frame to one published
    /// replica generation. `seq` is checked, non-wrapping, and contiguous
    /// within that pair. Under `NativeState`, `bytes` are the exact PTY bytes
    /// and MUST NOT be color- or capability-rewritten. Compatibility profiles
    /// may carry raw or synthesized VT according to the selected profile.
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
    ///
    /// Conforms to SPEC §13 as of phux-i58: `target` tagged union plus
    /// viewport metrics plus scrollback negotiation.
    Attach {
        /// Client-chosen correlation id echoed by `ATTACHED` and `ATTACH_READY`.
        attach_id: u32,
        /// Which session to attach to. Tagged union with four variants.
        target: AttachTarget,
        /// Client viewport dimensions at attach time.
        viewport: ViewportInfo,
        /// Whether the client wants the server to send scrollback as part of
        /// the attach sequence.
        request_scrollback: bool,
        /// Upper bound on scrollback lines the client will accept.
        ///
        /// The server caps its own retention at `min(server_cap, this)`.
        scrollback_limit_lines: u32,
    },

    /// `DETACH` — client signals clean departure (`docs/spec/proto.md` §7.2).
    ///
    /// Carries no fields in the phux-4az scaffold; SPEC §7.3 also keeps it
    /// empty (the `DetachReason` is sent in `DETACHED` from the server).
    Detach,

    /// `INPUT_KEY` — client forwards a structured key event (`docs/spec/input.md` §2).
    ///
    /// Wire shape: tagged [`ResourceId`] followed by the encoded [`KeyEvent`].
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

    /// `INPUT_TERMINAL_REPLY` — opaque bytes generated by the client's
    /// terminal emulator in response to server output (`docs/spec/input.md` §6).
    ///
    /// This is not user paste or a structured input atom. The attached client
    /// sends the bytes byte-identically and the server writes them directly to
    /// the terminal's ordered encoded-input lane.
    InputTerminalReply {
        /// Attached target terminal.
        terminal_id: ResourceId,
        /// Non-empty opaque PTY reply bytes. NUL and non-UTF-8 are valid.
        bytes: bytes::Bytes,
    },

    /// `FRAME_ACK` — cumulative `StateSync` acknowledgement
    /// (`docs/spec/proto.md` §8.2).
    ///
    /// Valid only for the `SynthesizedVtStateSync` profile. Raw profiles never
    /// acknowledge live output; reliable ordering plus the bootstrap READY
    /// boundary releases raw bytes without an extra RTT.
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
    /// size (`docs/spec/proto.md` §7.1 / §10.5).
    ///
    /// The connection itself identifies which client this resize belongs
    /// to — there is no `client_id` field on the wire (consistent with
    /// `ATTACH` / `INPUT_*` / etc., which also rely on the connection's
    /// implicit identity). The server uses this to update the resolved
    /// terminal dimensions for the client's currently-attached terminal.
    ///
    /// `viewport` reuses the [`ViewportInfo`] shape from `ATTACH`. SPEC
    /// §10.5 additionally defines `cell_w`/`cell_h`/`padding_*` for
    /// pixel-precise mouse encoding; those grow alongside the mouse
    /// encoder rework and don't gate the byc.4hp wiring.
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
    /// (`docs/spec/proto.md` §7.2).
    ///
    /// With the transport close that follows it, this is the *only* ending a
    /// consumer may act on: receipt of an `ERROR` never terminates an attach
    /// (proto.md §9). Both fields are additive and optional-absent, so a
    /// server that predates `0.7.0-draft.7` still round-trips as
    /// `{ reason: None, message: "" }`.
    Detached {
        /// Why the attach ended, or `None` when the peer stated no reason —
        /// either because it predates the field or because it sent a
        /// [`DetachReason`] this build does not recognise. A consumer MUST
        /// NOT infer [`DetachReason::Requested`] from absence.
        reason: Option<DetachReason>,
        /// Human-readable detail; empty when the peer sent none. Never a
        /// substitute for `reason` — it is diagnostic text, not a contract.
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

    /// `ERROR` — server-to-client structured error (`docs/spec/proto.md` §9).
    ///
    /// Carries a numeric [`ErrorCode`] plus a human-readable UTF-8
    /// `message`. `request_id` is `Some(_)` when the error correlates with
    /// a prior `COMMAND` per SPEC §14, and `None` for spontaneous server
    /// errors (e.g. malformed `ATTACH`, fatal protocol violations).
    ///
    /// A fatal error MUST be followed by `DETACHED { reason:
    /// PROTOCOL_ERROR }` and transport close.
    Error {
        /// Correlates this error with a prior `COMMAND`'s `request_id`,
        /// if applicable. `None` for non-command-correlated errors.
        request_id: Option<u32>,
        /// Structured error code; see [`ErrorCode`].
        code: ErrorCode,
        /// Human-readable, UTF-8, free-form message. Implementations
        /// SHOULD keep this short enough to log inline.
        message: String,
    },

    // -------------------------------------------------------------------------
    // L3 metadata frames — SPEC §7.4 / §11.L3 (phux-4li.2). Reserved for
    // consumers that declare `Layer::L3` in `HELLO.client_caps.layers`; the
    // server MUST NOT emit `MetadataChanged` to a non-L3 consumer (SPEC
    // §16.4). The server's K/V store treats values as opaque bytes.
    //
    // Reply paths (GET → value, LIST → keys) are intentionally NOT yet
    // wire-encoded as dedicated frames in v0.1 of L3: SPEC §11 already
    // defines the generic `COMMAND` / `COMMAND_RESULT` envelope for that
    // pattern, and lighting up `COMMAND` is a sibling ticket. v0.1 servers
    // expose the GET / LIST functions as server-side Rust APIs (see
    // `phux_server::state::ServerState`); the wire reply path lands when
    // `COMMAND` does. `MetadataChanged` is independently load-bearing for
    // the ADR-0019 layout-coordination story and ships here.
    // -------------------------------------------------------------------------
    /// `GET_METADATA` — client requests the value at `(scope, key)`
    /// (`docs/spec/L3.md` §1 / §11.L3).
    ///
    /// The reply is currently a server-side function return; the wire
    /// reply path will ride the generic `COMMAND_RESULT` envelope when
    /// it lands. `request_id` is carried so the future reply correlates.
    GetMetadata {
        /// Correlates this request with the eventual `COMMAND_RESULT`.
        request_id: u32,
        /// Where to look the key up.
        scope: Scope,
        /// UTF-8 key name. Convention: `phux.<consumer>.<name>/<version>`
        /// per SPEC §17 (non-normative).
        key: String,
    },

    /// `SET_METADATA` — client writes `value` at `(scope, key)`
    /// (`docs/spec/L3.md` §1 / §11.L3).
    ///
    /// Atomic write: the server stores `value` and broadcasts
    /// `MetadataChanged { scope, key, value: Some(value) }` to every
    /// subscriber matching `(scope, key)`. Implementations MAY enforce a
    /// per-key size limit (recommended: 256 KiB) and reply with
    /// [`ErrorCode::ResourceExhausted`] if exceeded.
    SetMetadata {
        /// Correlates this request with the eventual `COMMAND_RESULT`.
        request_id: u32,
        /// Where to write the key.
        scope: Scope,
        /// UTF-8 key name.
        key: String,
        /// Opaque value bytes. The server MUST NOT interpret them.
        value: Vec<u8>,
    },

    /// `DELETE_METADATA` — client removes `key` from `scope`
    /// (`docs/spec/L3.md` §1 / §11.L3).
    ///
    /// Idempotent: deleting a missing key is not an error. The server
    /// broadcasts `MetadataChanged { scope, key, value: None }` (a
    /// tombstone) to subscribers iff the key existed before the call.
    DeleteMetadata {
        /// Correlates this request with the eventual `COMMAND_RESULT`.
        request_id: u32,
        /// Where to delete the key.
        scope: Scope,
        /// UTF-8 key name.
        key: String,
    },

    /// `LIST_METADATA` — client requests the set of key names in `scope`
    /// (`docs/spec/L3.md` §1 / §11.L3).
    ///
    /// Returns key names only — values are not part of the listing. As
    /// with `GET_METADATA`, the wire reply path is deferred to the
    /// `COMMAND_RESULT` envelope; v0.1 servers expose LIST as a Rust
    /// function return.
    ListMetadata {
        /// Correlates this request with the eventual `COMMAND_RESULT`.
        request_id: u32,
        /// Where to list keys from.
        scope: Scope,
    },

    /// `SUBSCRIBE_METADATA` — client opts into `MetadataChanged` events
    /// matching `(scope, key)` (`docs/spec/L3.md` §1).
    ///
    /// A single subscribe per `(scope, key)` is enough; the server keys
    /// subscribers by `(client, scope, key)` so re-subscribes are
    /// idempotent. Unsubscription is implicit on detach (see
    /// `phux_server::state::ServerState::detach`); a future
    /// `UNSUBSCRIBE_METADATA` ticket may add explicit teardown.
    SubscribeMetadata {
        /// Scope to watch.
        scope: Scope,
        /// Specific key to watch. The subscriber receives
        /// `MetadataChanged` iff the event's `(scope, key)` matches.
        key: String,
    },

    /// `METADATA_CHANGED` — server notifies a subscriber that
    /// `(scope, key)` was written or deleted (`docs/spec/L3.md` §1).
    ///
    /// `value` is `Some(new_bytes)` on a SET and `None` on a DELETE
    /// (the tombstone case). Subscribers MAY re-issue `GET_METADATA`
    /// after receiving the notification; the value is also carried
    /// inline for the common-case path where the subscriber just
    /// wants the new bytes (SPEC §7.4 leaves this latitude — "the
    /// value itself is not carried" was the v0.1 sketch; phux-4li.2
    /// lifts it because the layout coordination use case
    /// (ADR-0019) is a read-on-every-change pattern and the round
    /// trip is wasteful).
    MetadataChanged {
        /// Scope the change happened in.
        scope: Scope,
        /// Key that changed.
        key: String,
        /// New value, or `None` for a deletion (tombstone).
        value: Option<Vec<u8>>,
    },

    /// `METADATA_VALUE` — server reply to a prior `GET_METADATA`
    /// (`docs/spec/L3.md` §1 / §11.L3). Allocated by phux-4li.8.
    ///
    /// Correlated to the originating request by `request_id`. `value` is
    /// `Some(bytes)` when the key was present at the time of the lookup
    /// and `None` when the key was absent (no tombstone distinction —
    /// "absent" subsumes "never written" and "explicitly deleted").
    ///
    /// Design choice (phux-4li.8): a dedicated reply frame rather than
    /// the generic `COMMAND_RESULT` envelope sketched in SPEC §11. The
    /// envelope would have forced design closure on every L1/L2 COMMAND
    /// payload before any L3 consumer needs the reply path; for v0.1 the
    /// metadata family is already opinionated (`METADATA_CHANGED` carries
    /// value inline, departing from the §7.4 sketch) so an ad-hoc
    /// dedicated reply is consistent. The generic envelope ships when
    /// `COMMAND` does, and does not need to subsume `METADATA_VALUE`.
    MetadataValue {
        /// Correlates this reply with a prior `GET_METADATA.request_id`.
        request_id: u32,
        /// `Some(bytes)` when the key was present, `None` when absent.
        value: Option<Vec<u8>>,
    },

    /// `METADATA_KEYS` — server reply to a prior `LIST_METADATA`
    /// (`docs/spec/L3.md` §1 / §11.L3). Allocated by phux-4li.8.
    ///
    /// Correlated to the originating request by `request_id`. Carries
    /// the set of key names present in the requested scope. Server
    /// implementations SHOULD return keys in lexicographic order so
    /// snapshots and tests round-trip stably; clients MUST NOT rely on
    /// any particular ordering for correctness.
    MetadataKeys {
        /// Correlates this reply with a prior `LIST_METADATA.request_id`.
        request_id: u32,
        /// Keys present in the requested scope; values are NOT included
        /// (clients fetch them separately via `GET_METADATA`).
        keys: Vec<String>,
    },

    /// `LIST_DIRECTORY` — client asks the serving server for the child
    /// directories of `path` on its own host (`docs/spec/L3.md` §4).
    ///
    /// Answered by [`FrameKind::DirectoryListing`] correlated by
    /// `request_id`. Gated on
    /// [`ServerFeature::ListDirectory`](crate::caps::ServerFeature::ListDirectory).
    ListDirectory {
        /// Correlates this request with its `DIRECTORY_LISTING` reply.
        request_id: u32,
        /// Absolute path, `~` / `~/rest`, or empty (the serving user's home).
        path: String,
        /// List on this satellite instead of the serving host: a federation
        /// hub relays the request over its link and answers with the
        /// satellite's listing (`docs/spec/L3.md` §4.1). `None` (field
        /// absent) is the serving host, byte-identical to the pre-field
        /// frame. Gated on
        /// [`ServerFeature::ListDirectoryHost`](crate::caps::ServerFeature::ListDirectoryHost):
        /// an older server skips the field and lists its own host.
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

    // -------------------------------------------------------------------------
    // L1 Terminal lifecycle frames — SPEC §7.2 / §10.1 (phux-4li.10).
    //
    // Unblocks split-pane / kill-pane (was warn+bell in phux-4li.5) and the
    // per-pane `ioctl(TIOCSWINSZ)` half of phux-4li.9's SIGWINCH wire-up.
    // The server-side handler + client-side emission land in follow-up
    // tickets; this enum allocation is the wire substrate they build on.
    // -------------------------------------------------------------------------
    /// `SPAWN_RESOURCE` — client requests a new Terminal under `group`
    /// (`docs/spec/L1.md` §1 / §10.1).
    ///
    /// Async: the server replies with [`FrameKind::ResourceSpawned`]
    /// correlated by `request_id`. `command = None` means "use the server's
    /// default shell" (the same convention as
    /// `AttachTarget::CreateIfMissing.command = None`). `cwd = None` means
    /// "use the server's default working directory" — typically the user's
    /// `$HOME`; the exact policy is implementation-defined. `env = None`
    /// inherits the server's environment as-is; `env = Some([])` is
    /// distinct (start with an empty environment).
    ///
    /// v0.1 servers expose a single default Group at
    /// `GroupId(1)` (SPEC §7.4 L2-dependency note). Other group
    /// ids MAY surface as [`SpawnError::GroupNotFound`](super::SpawnError::GroupNotFound) inside the
    /// reply frame's [`SpawnResult::Err`] arm.
    SpawnResource {
        /// Correlates this request with the eventual `ResourceSpawned`.
        request_id: u32,
        /// Group under which to spawn the new Terminal.
        group: GroupId,
        /// Command + argv, or `None` to invoke the server's default shell.
        command: Option<Vec<String>>,
        /// Working directory for the new Terminal, or `None` for the
        /// server's default.
        cwd: Option<String>,
        /// Environment variables for the new Terminal, or `None` to
        /// inherit the server's environment. `Some(vec![])` is distinct
        /// from `None`: it starts with an empty environment.
        env: Option<Vec<(String, String)>>,
        /// First-class `TERM` override for the new Terminal, or `None` to
        /// defer to the server's `defaults.term` (and ultimately the
        /// compiled-in `DEFAULT_TERM`). A bare `env` entry for `TERM`
        /// still wins over this field on the server, but this gives
        /// consumers a typed knob without hand-rolling the env pair.
        term: Option<String>,
        /// Satellite host to spawn on (phux-v45.6, ADR-0007 / L1 §9.1),
        /// or `None` to spawn on the receiving server — the only shape a
        /// non-federated consumer ever sends. A federation hub routes a
        /// `Some(host)` spawn over its outbound link to that satellite
        /// and replies with the new Terminal re-tagged
        /// `Satellite { host, id }`; a non-hub server (or a hub whose
        /// registry lacks `host`) replies
        /// [`SpawnError::UnsupportedSatelliteRoute`](super::SpawnError::UnsupportedSatelliteRoute), and a hub whose
        /// link to `host` is down replies
        /// [`SpawnError::SatelliteUnreachable`](super::SpawnError::SatelliteUnreachable). Encoded as optional
        /// field id 7 (absent = `None`), so the field is wire-additive:
        /// a pre-phux-v45.6 body decodes as `None`.
        satellite: Option<SatelliteHost>,
        /// Existing Terminal whose owning window must host the new Terminal.
        /// This is an ownership address, not focus or geometry: clients still
        /// write layout metadata to place the returned leaf. `None` preserves
        /// the legacy attached-client / most-recent-session policy. Encoded as
        /// additive optional field id 8.
        owner_terminal: Option<ResourceId>,
        /// Opaque [`RESOURCE_AGENT_SESSION_KEY`](super::RESOURCE_AGENT_SESSION_KEY) bytes to install atomically on
        /// the new local Terminal before it becomes visible to other clients.
        /// Additive optional field id 9; old servers ignore it, after which a
        /// new launcher still performs its ordinary SET/GET confirmation.
        agent_session: Option<Vec<u8>>,
        /// `(cols, rows)` the new Terminal's grid and PTY are created at,
        /// or `None` to take the server's default (80x24 in the reference
        /// server). A layout-owning consumer knows the tile the new leaf
        /// will occupy before it has an id for it, and passing that here
        /// is strictly better than letting the pane bootstrap at a default
        /// and then reflowing it: the post-spawn `RESIZE_TERMINAL` becomes
        /// a no-op instead of invalidating the bootstrap generation the
        /// server just built (bead phux-a5xj).
        ///
        /// Additive optional field id 10, gated on
        /// [`ServerFeature::SpawnInitialSize`](crate::caps::ServerFeature::SpawnInitialSize):
        /// a server that predates the field ignores it and spawns at its
        /// default, which is exactly the pre-field behavior, so sending it
        /// unadvertised is degrading rather than dangerous — but the bit is
        /// what lets a client know whether the resize it sends next is
        /// redundant. A zero on either axis is ignored by the receiver.
        initial_size: Option<(u16, u16)>,
        /// What kind of resource to spawn and, for a child kind, its binding
        /// and facet: the additive fields 11 (`kind`), 12 (`parent`),
        /// 13 (`provider`), and 14 (`native_id`) of `docs/spec/L1.md` §1.2.
        /// `None` is the plain Terminal spawn: no field is written, and a
        /// body that carries none of the four decodes to `None`, so a
        /// Terminal spawn's bytes are unchanged from before the fields
        /// existed. See [`SpawnResource`] for the per-kind rules the
        /// decoder enforces and the feature bit that gates the fields.
        resource: Option<Box<SpawnResource>>,
    },

    /// `RESOURCE_SPAWNED` — server reply to a prior `SpawnResource`
    /// (`docs/spec/L1.md` §1 / §10.1).
    ///
    /// Correlated to the originating request by `request_id`. `result`
    /// carries either the freshly allocated [`ResourceId`] or a structured
    /// [`SpawnError`](super::SpawnError). The structured error is deliberately separate from
    /// the generic [`FrameKind::Error`] catch-all so command-correlated
    /// failures stay typed end-to-end (matching the
    /// `METADATA_VALUE` precedent from phux-4li.8).
    ResourceSpawned {
        /// Correlates this reply with a prior `SpawnResource.request_id`.
        request_id: u32,
        /// Either the freshly allocated Terminal, or a structured error.
        result: SpawnResult,
    },

    /// `MOVE_RESOURCE` — re-parent a live Terminal into the window owning
    /// `owner_terminal` (`docs/spec/L1.md` §1 / §10.1; ADR-0056).
    ///
    /// Async: the server replies with [`FrameKind::ResourceMoved`]
    /// correlated by `request_id`. `owner_terminal` is an ownership
    /// address exactly as in `SPAWN_RESOURCE`: the destination window may
    /// belong to a different session, and the frame conveys no split
    /// direction, ratio, or focus — layout stays a client-written L3
    /// concern. The pane's process, PTY, scrollback, metadata, and agent
    /// record are untouched, and its `ResourceId` is stable across the
    /// move. Local-only: a satellite-tagged Terminal on either end is
    /// refused with [`MoveError::UnsupportedSatelliteRoute`](super::MoveError::UnsupportedSatelliteRoute). Senders
    /// MUST first see the `MOVE_RESOURCE` feature bit in
    /// `HELLO_OK.server_caps` (`crate::caps::MOVE_RESOURCE`).
    MoveResource {
        /// Correlates this request with the eventual `ResourceMoved`.
        request_id: u32,
        /// The Terminal to re-parent.
        terminal: ResourceId,
        /// Existing Terminal whose owning window becomes the destination.
        owner_terminal: ResourceId,
    },

    /// `RESOURCE_MOVED` — server reply to a prior `MoveResource`
    /// (`docs/spec/L1.md` §1 / §10.1; ADR-0056).
    ///
    /// Correlated by `request_id`. `result` carries the moved Terminal's
    /// unchanged [`ResourceId`] or a structured [`MoveError`](super::MoveError) — typed
    /// end-to-end for the same reason as [`FrameKind::ResourceSpawned`].
    ResourceMoved {
        /// Correlates this reply with a prior `MoveResource.request_id`.
        request_id: u32,
        /// Either the moved Terminal, or a structured error.
        result: MoveResult,
    },

    /// `RESOURCE_CLOSED` — server notifies clients that a Terminal exited
    /// (`docs/spec/L1.md` §1 / §10.1).
    ///
    /// Emitted when the underlying PTY exits, whether by `_exit(n)`, by
    /// signal, or via a `KILL_RESOURCE` command. `exit_status = Some(n)`
    /// reports the process's exit code; `None` covers signal kills and
    /// unknown-cause exits (a deliberately compact subset of SPEC §10.1's
    /// `ExitStatus` tagged union — the wider tagged union grows in a
    /// follow-up wire bump if the additional structure proves
    /// load-bearing).
    ResourceClosed {
        /// The Terminal that exited.
        terminal_id: ResourceId,
        /// Process exit code (`_exit(n)`), or `None` for signals / unknown.
        exit_status: Option<i32>,
        /// Why the resource closed. Additive optional field id 3: the
        /// encoder omits the field for [`CloseReason::Unknown`] and the
        /// decoder reads an absent field as `Unknown`, so a body from a peer
        /// that predates the field carries no reason rather than failing.
        reason: CloseReason,
    },

    /// `RESIZE_TERMINAL` — client signals a per-Terminal PTY resize
    /// (`docs/spec/L1.md` §1 / §10.2).
    ///
    /// Sent in addition to (not in place of) `VIEWPORT_RESIZE`: the
    /// outer-viewport frame conveys the client's smallest-common-bounding-
    /// box; this frame conveys the resolved per-pane dimensions after the
    /// client's layout walk. The server's PTY layer drives
    /// `ioctl(TIOCSWINSZ)` from this. Implementations SHOULD treat `cols`
    /// or `rows` of zero as a no-op rather than a kernel error (the
    /// codec round-trips zero faithfully).
    ResizeTerminal {
        /// Target Terminal.
        terminal_id: ResourceId,
        /// New width in cells.
        cols: u16,
        /// New height in cells.
        rows: u16,
    },

    /// `COMMAND` — the generic control-plane request envelope
    /// (`docs/spec/L1.md` §5, ADR-0021).
    ///
    /// Carries a typed [`Command`] correlated to its eventual
    /// [`FrameKind::CommandResult`] by `request_id`. Asynchronous: the
    /// server MAY interleave other frames before the result (SPEC §5).
    Command {
        /// Correlates this request with the eventual `CommandResult`.
        request_id: u32,
        /// The command to execute.
        command: Command,
    },

    /// `COMMAND_RESULT` — reply to a prior [`FrameKind::Command`]
    /// (`docs/spec/L1.md` §5, ADR-0021).
    ///
    /// Correlated to the originating request by `request_id`.
    CommandResult {
        /// Correlates this reply with a prior `Command.request_id`.
        request_id: u32,
        /// The command's outcome.
        result: CommandResult,
    },

    // -------------------------------------------------------------------------
    // Agent-event frames — SPEC §7.5 / §10.3 (phux-y2t / ADR-0022 'events').
    // The push half of the agent surface; an additive accelerator of the
    // CLI poll-floor `wait`. `SUBSCRIBE_EVENTS` (C→S `0x41`) opts a client
    // into the stream; `EVENT` (S→C `0xB3`) carries each extensible tagged
    // event. The taxonomy is forward-compat (TLV body) — see [`AgentEvent`].
    // -------------------------------------------------------------------------
    /// `SUBSCRIBE_EVENTS` — client opts into the server-pushed
    /// [`AgentEvent`] stream (`docs/spec/L1.md` §7.5).
    ///
    /// `terminal` scopes the subscription:
    /// - `Some(id)` — only events for that Terminal (per-pane).
    /// - `None` — every event the server emits for any Terminal the
    ///   client may observe (server-scoped), e.g. `pane_spawned` /
    ///   `pane_closed` across the session.
    ///
    /// Idempotent: re-subscribing the same scope is a no-op. Unsubscription
    /// is implicit on detach (matching `SUBSCRIBE_METADATA`); a future
    /// `UNSUBSCRIBE_EVENTS` ticket may add explicit teardown. The
    /// subscription does NOT itself attach, resize, or send a snapshot —
    /// it is purely a push registration, so an agent can `watch` a Terminal
    /// without disturbing the live session.
    SubscribeEvents {
        /// Per-Terminal scope, or `None` for every Terminal the client may
        /// observe.
        terminal: Option<ResourceId>,
    },

    /// `EVENT` — server pushes one [`AgentEvent`] to a subscribed client
    /// (`docs/spec/L1.md` §7.5).
    ///
    /// `terminal` identifies the Terminal the event concerns, or `None`
    /// for a server-scoped event with no single owning Terminal. The
    /// `event` body is TLV-encoded (`tag: u8` + length-prefixed bytes) so
    /// an older client skips unrecognised event kinds via
    /// [`AgentEvent::Unknown`] rather than failing the parse.
    Event {
        /// The Terminal this event concerns, or `None` if server-scoped.
        terminal: Option<ResourceId>,
        /// The event payload.
        event: AgentEvent,
    },
}

impl InputEvent {
    /// Wrap this event in the matching per-atom input [`FrameKind`]
    /// addressed to `terminal_id` (`INPUT_KEY` / `INPUT_MOUSE` /
    /// `INPUT_FOCUS` / `INPUT_PASTE`). Used by the attach loop to ship a
    /// parsed event to its focused pane.
    ///
    /// Lives here rather than next to [`InputEvent`] itself so that
    /// `crate::input` stays a leaf of the wire layer: frames know about
    /// input atoms, input atoms know nothing about frames.
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

    /// Encode `self`, wrapping it in a `FRAME_COMPRESSED` envelope when the
    /// connection negotiated a compression and the transform actually pays.
    ///
    /// Falls back to a plain [`Self::encode`] whenever it does not — nothing
    /// was negotiated, the body is small, or it did not shrink. That fallback
    /// is not a degraded mode: compression is per frame, so a receiver sees an
    /// ordinary frame and a wrapped one interchangeably on the same
    /// connection, and a keystroke echo never pays a compressor.
    ///
    /// `scratch` is a caller-owned buffer for the uncompressed image, reused
    /// across calls so a per-frame allocation does not replace the syscall
    /// this is meant to make cheaper.
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
        // The envelope carries the inner frame's body: its type byte and
        // payload, i.e. everything past the four-byte length prefix. Carrying
        // the prefix too would duplicate a length the envelope already knows.
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
        enc.write_field_with(field::frame_compressed::ALGORITHM, |e| {
            e.write_u8(compression.as_u8());
        });
        enc.write_field_with(field::frame_compressed::UNCOMPRESSED_LEN, |e| {
            e.write_u32_be(uncompressed_len);
        });
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
            } => Self::encode_attach(
                enc,
                *attach_id,
                target,
                viewport,
                *request_scrollback,
                *scrollback_limit_lines,
            ),
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
                Self::encode_bootstrap_begin_stream(enc, terminal_id, *stream_id, *bootstrap_id);
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
                Self::encode_history_page_stream(enc, terminal_id, *stream_id, *bootstrap_id);
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
                Self::encode_history_rejected_stream(enc, terminal_id, *stream_id, *bootstrap_id);
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
            Self::MetadataChanged { scope, key, value } => {
                Self::encode_metadata_changed(enc, scope, key, value.as_deref());
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
            } => Self::encode_terminal_closed(enc, terminal_id, *exit_status, *reason),
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
            Self::SubscribeEvents { terminal } => {
                Self::encode_subscribe_events(enc, terminal.as_ref());
            }
            Self::Event { terminal, event } => {
                Self::encode_event(enc, terminal.as_ref(), event);
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
        enc.write_field_with(field::hello::PROTOCOL_MAJOR, |e| {
            e.write_u16_be(protocol_major);
        });
        enc.write_field_with(field::hello::PROTOCOL_MINOR, |e| {
            e.write_u16_be(protocol_minor);
        });
        enc.write_field_with(field::hello::PROTOCOL_PATCH, |e| {
            e.write_u16_be(protocol_patch);
        });
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
            enc.write_field_with(field::hello::COMPRESSION, |e| {
                e.write_u8(client_caps.compression.bits());
            });
        }
        // Additive top-level field that `phux stdio-bridge` stamps on the
        // HELLO it relays. Ordinary clients leave it absent.
        if let Some(origin) = client_caps.ssh_origin {
            enc.write_field_with(field::hello::SSH_ORIGIN, |e| {
                crate::wire::ssh_origin::encode_ssh_origin(&origin, e);
            });
        }
    }

    /// Write the exact protocol version `HELLO_OK` admits the peer at.
    fn encode_hello_ok_version(
        enc: &mut Encoder<'_>,
        protocol_major: u16,
        protocol_minor: u16,
        protocol_patch: u16,
    ) {
        enc.write_field_with(field::hello_ok::PROTOCOL_MAJOR, |e| {
            e.write_u16_be(protocol_major);
        });
        enc.write_field_with(field::hello_ok::PROTOCOL_MINOR, |e| {
            e.write_u16_be(protocol_minor);
        });
        enc.write_field_with(field::hello_ok::PROTOCOL_PATCH, |e| {
            e.write_u16_be(protocol_patch);
        });
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
        enc.write_field_with(field::hello_ok::MAX_CHUNK_BYTES, |e| {
            e.write_u32_be(bootstrap_limits.max_chunk_bytes());
        });
        enc.write_field_with(field::hello_ok::MAX_HISTORY_PAGE_BYTES, |e| {
            e.write_u32_be(bootstrap_limits.max_history_page_bytes());
        });
        // Same discipline as HELLO's offer field: omitted when nothing was
        // selected, so an uncompressed connection's HELLO_OK is unchanged.
        if server_caps.compression != Compression::None {
            enc.write_field_with(field::hello_ok::COMPRESSION, |e| {
                e.write_u8(server_caps.compression.as_u8());
            });
        }
    }

    /// Write the single nonce field shared by `PING` and `PONG`.
    fn encode_nonce(enc: &mut Encoder<'_>, nonce: u64) {
        enc.write_field_with(field::ping::NONCE, |e| e.write_u64_be(nonce));
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
        enc.write_field_with(field::terminal_output::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::terminal_output::SEQ, |e| e.write_u64_be(seq));
        enc.write_field(field::terminal_output::BYTES, bytes);
        enc.write_field_with(field::terminal_output::STREAM_ID, |e| {
            e.write_u64_be(stream_id.get());
        });
        enc.write_field_with(field::terminal_output::BOOTSTRAP_ID, |e| {
            e.write_u64_be(bootstrap_id.get());
        });
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
        enc.write_field_with(field::attach::REQUEST_SCROLLBACK, |e| {
            e.write_u8(u8::from(request_scrollback));
        });
        enc.write_field_with(field::attach::SCROLLBACK_LIMIT_LINES, |e| {
            e.write_u32_be(scrollback_limit_lines);
        });
        enc.write_field_with(field::attach::ATTACH_ID, |e| e.write_u32_be(attach_id));
    }

    /// Write the `DETACHED` payload.
    ///
    /// Both fields are optional-absent (field.rs allocation discipline): an
    /// unstated reason and an empty message encode as nothing at all, which
    /// keeps the common acknowledge-a-clean-detach frame byte-identical to
    /// what every 0.7.0 peer already emits.
    fn encode_detached(enc: &mut Encoder<'_>, reason: Option<DetachReason>, message: &str) {
        if let Some(reason) = reason {
            enc.write_field_with(field::detached::REASON, |e| {
                e.write_u8(reason.as_wire());
            });
        }
        if !message.is_empty() {
            enc.write_field(field::detached::MESSAGE, message.as_bytes());
        }
    }

    /// Write the `INPUT_KEY` payload.
    fn encode_input_key(enc: &mut Encoder<'_>, terminal_id: &ResourceId, event: &KeyEvent) {
        enc.write_field_with(field::input_key::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::input_key::EVENT, |e| encode_key_event(event, e));
    }

    /// Write the `INPUT_MOUSE` payload.
    fn encode_input_mouse(enc: &mut Encoder<'_>, terminal_id: &ResourceId, event: &MouseEvent) {
        enc.write_field_with(field::input_mouse::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::input_mouse::EVENT, |e| encode_mouse_event(event, e));
    }

    /// Write the `INPUT_FOCUS` payload.
    fn encode_input_focus(enc: &mut Encoder<'_>, terminal_id: &ResourceId, event: FocusEvent) {
        enc.write_field_with(field::input_focus::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::input_focus::EVENT, |e| {
            e.write_u8(encode_focus_event(event));
        });
    }

    /// Write the `INPUT_PASTE` payload.
    fn encode_input_paste(enc: &mut Encoder<'_>, terminal_id: &ResourceId, event: &PasteEvent) {
        enc.write_field_with(field::input_paste::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::input_paste::EVENT, |e| encode_paste_event(event, e));
    }

    /// Write the `INPUT_TERMINAL_REPLY` payload.
    fn encode_input_terminal_reply(enc: &mut Encoder<'_>, terminal_id: &ResourceId, bytes: &[u8]) {
        enc.write_field_with(field::input_terminal_reply::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
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
        enc.write_field_with(field::frame_ack::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::frame_ack::SEQ, |e| e.write_u64_be(seq));
        enc.write_field_with(field::frame_ack::STREAM_ID, |e| {
            e.write_u64_be(stream_id.get());
        });
        enc.write_field_with(field::frame_ack::BOOTSTRAP_ID, |e| {
            e.write_u64_be(bootstrap_id.get());
        });
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
        enc.write_field_with(field::attached::ATTACH_ID, |e| e.write_u32_be(attach_id));
    }

    /// Write the `ATTACH_READY` payload.
    fn encode_attach_ready(enc: &mut Encoder<'_>, attach_id: u32) {
        enc.write_field_with(field::attach_ready::ATTACH_ID, |e| {
            e.write_u32_be(attach_id);
        });
    }

    /// Write the generation binding that opens the `BOOTSTRAP_BEGIN` payload.
    fn encode_bootstrap_begin_stream(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
    ) {
        enc.write_field_with(field::bootstrap_begin::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::bootstrap_begin::STREAM_ID, |e| {
            e.write_u64_be(stream_id.get());
        });
        enc.write_field_with(field::bootstrap_begin::BOOTSTRAP_ID, |e| {
            e.write_u64_be(bootstrap_id.get());
        });
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
        enc.write_field_with(field::bootstrap_begin::COLS, |e| e.write_u16_be(cols));
        enc.write_field_with(field::bootstrap_begin::ROWS, |e| e.write_u16_be(rows));
        enc.write_field_with(field::bootstrap_begin::OUTPUT_MODE, |e| {
            e.write_u8(output_mode.as_wire());
        });
        enc.write_field_with(field::bootstrap_begin::BASE_SEQ, |e| {
            e.write_u64_be(base_seq);
        });
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
        enc.write_field_with(field::bootstrap_chunk::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::bootstrap_chunk::STREAM_ID, |e| {
            e.write_u64_be(stream_id.get());
        });
        enc.write_field_with(field::bootstrap_chunk::BOOTSTRAP_ID, |e| {
            e.write_u64_be(bootstrap_id.get());
        });
        enc.write_field_with(field::bootstrap_chunk::CHUNK_SEQ, |e| {
            e.write_u32_be(chunk_seq);
        });
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
        enc.write_field_with(field::bootstrap_ready::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::bootstrap_ready::STREAM_ID, |e| {
            e.write_u64_be(stream_id.get());
        });
        enc.write_field_with(field::bootstrap_ready::BOOTSTRAP_ID, |e| {
            e.write_u64_be(bootstrap_id.get());
        });
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
        enc.write_field_with(field::history_request::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::history_request::STREAM_ID, |e| {
            e.write_u64_be(stream_id.get());
        });
        enc.write_field_with(field::history_request::BOOTSTRAP_ID, |e| {
            e.write_u64_be(bootstrap_id.get());
        });
        enc.write_field(field::history_request::CURSOR, cursor);
        enc.write_field_with(field::history_request::MAX_BYTES, |e| {
            e.write_u32_be(max_bytes);
        });
        enc.write_field_with(field::history_request::MAX_ROWS, |e| {
            e.write_u32_be(max_rows);
        });
    }

    /// Write the generation binding that opens the `HISTORY_PAGE` payload.
    fn encode_history_page_stream(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
    ) {
        enc.write_field_with(field::history_page::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::history_page::STREAM_ID, |e| {
            e.write_u64_be(stream_id.get());
        });
        enc.write_field_with(field::history_page::BOOTSTRAP_ID, |e| {
            e.write_u64_be(bootstrap_id.get());
        });
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
        enc.write_field_with(field::history_page::PAGE_SEQ, |e| {
            e.write_u64_be(page_seq);
        });
        enc.write_field_with(field::history_page::ROWS, |e| {
            e.write_u32_be(rows);
        });
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
        enc.write_field_with(field::bootstrap_tombstone::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::bootstrap_tombstone::STREAM_ID, |e| {
            e.write_u64_be(stream_id.get());
        });
        enc.write_field_with(field::bootstrap_tombstone::BOOTSTRAP_ID, |e| {
            e.write_u64_be(bootstrap_id.get());
        });
        enc.write_field_with(field::bootstrap_tombstone::REASON, |e| {
            e.write_u8(reason.as_wire());
        });
        enc.write_field_with(field::bootstrap_tombstone::LAST_VALID_SEQ, |e| {
            e.write_u64_be(last_valid_seq);
        });
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
        enc.write_field_with(field::history_tombstone::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::history_tombstone::STREAM_ID, |e| {
            e.write_u64_be(stream_id.get());
        });
        enc.write_field_with(field::history_tombstone::BOOTSTRAP_ID, |e| {
            e.write_u64_be(bootstrap_id.get());
        });
        enc.write_field(field::history_tombstone::CURSOR, cursor);
        enc.write_field_with(field::history_tombstone::REASON, |e| {
            e.write_u8(reason.as_wire());
        });
    }

    /// Write the generation binding that opens the `HISTORY_REJECTED` payload.
    fn encode_history_rejected_stream(
        enc: &mut Encoder<'_>,
        terminal_id: &ResourceId,
        stream_id: StreamId,
        bootstrap_id: BootstrapId,
    ) {
        enc.write_field_with(field::history_rejected::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::history_rejected::STREAM_ID, |e| {
            e.write_u64_be(stream_id.get());
        });
        enc.write_field_with(field::history_rejected::BOOTSTRAP_ID, |e| {
            e.write_u64_be(bootstrap_id.get());
        });
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
        enc.write_field_with(field::history_rejected::REASON, |e| {
            e.write_u8(reason.as_wire());
        });
        enc.write_field_with(field::history_rejected::REQUIRED_BYTES, |e| {
            e.write_u32_be(required_bytes);
        });
        enc.write_field_with(field::history_rejected::REQUIRED_ROWS, |e| {
            e.write_u32_be(required_rows);
        });
    }

    /// Write the `BELL` payload.
    fn encode_bell(enc: &mut Encoder<'_>, terminal_id: &ResourceId) {
        enc.write_field_with(field::bell::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
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
            enc.write_field_with(field::error::REQUEST_ID, |e| e.write_u32_be(id));
        }
        enc.write_field_with(field::error::CODE, |e| e.write_u16_be(code.as_wire()));
        enc.write_field(field::error::MESSAGE, message.as_bytes());
    }

    /// Write the `{request_id, scope, key}` body shared by GET and DELETE.
    fn encode_get_or_delete_metadata(
        enc: &mut Encoder<'_>,
        request_id: u32,
        scope: &Scope,
        key: &str,
    ) {
        enc.write_field_with(field::get_metadata::REQUEST_ID, |e| {
            e.write_u32_be(request_id);
        });
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
        enc.write_field_with(field::set_metadata::REQUEST_ID, |e| {
            e.write_u32_be(request_id);
        });
        enc.write_field_with(field::set_metadata::SCOPE, |e| encode_scope(scope, e));
        enc.write_field(field::set_metadata::KEY, key.as_bytes());
        enc.write_field(field::set_metadata::VALUE, value);
    }

    /// Write the `LIST_METADATA` payload.
    fn encode_list_metadata(enc: &mut Encoder<'_>, request_id: u32, scope: &Scope) {
        enc.write_field_with(field::list_metadata::REQUEST_ID, |e| {
            e.write_u32_be(request_id);
        });
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
        enc.write_field_with(field::metadata_value::REQUEST_ID, |e| {
            e.write_u32_be(request_id);
        });
        // Optional value: absent field = key absent (None).
        if let Some(v) = value {
            enc.write_field(field::metadata_value::VALUE, v);
        }
    }

    /// Write the `METADATA_KEYS` payload.
    fn encode_metadata_keys(enc: &mut Encoder<'_>, request_id: u32, keys: &[String]) {
        enc.write_field_with(field::metadata_keys::REQUEST_ID, |e| {
            e.write_u32_be(request_id);
        });
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
        enc.write_field_with(field::spawn_terminal::REQUEST_ID, |e| {
            e.write_u32_be(request_id);
        });
        enc.write_field_with(field::spawn_terminal::GROUP, |e| {
            e.write_u32_be(group.get());
        });
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
            enc.write_field_with(field::spawn_terminal::OWNER_TERMINAL, |e| {
                encode_terminal_id(owner, e);
            });
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
            enc.write_field_with(field::spawn_terminal::PARENT, |e| {
                encode_terminal_id(parent, e);
            });
        }
        if let Some(provider) = &resource.provider {
            enc.write_field(field::spawn_terminal::PROVIDER, provider.as_bytes());
        }
        if let Some(native_id) = &resource.native_id {
            enc.write_field(field::spawn_terminal::NATIVE_ID, native_id.as_bytes());
        }
    }

    /// Write the `RESOURCE_SPAWNED` payload.
    fn encode_terminal_spawned(enc: &mut Encoder<'_>, request_id: u32, result: &SpawnResult) {
        enc.write_field_with(field::terminal_spawned::REQUEST_ID, |e| {
            e.write_u32_be(request_id);
        });
        enc.write_field_with(field::terminal_spawned::RESULT, |e| {
            encode_spawn_result(result, e);
        });
    }

    /// Write the `MOVE_RESOURCE` payload.
    fn encode_move_terminal(
        enc: &mut Encoder<'_>,
        request_id: u32,
        terminal: &ResourceId,
        owner_terminal: &ResourceId,
    ) {
        enc.write_field_with(field::move_terminal::REQUEST_ID, |e| {
            e.write_u32_be(request_id);
        });
        enc.write_field_with(field::move_terminal::TERMINAL, |e| {
            encode_terminal_id(terminal, e);
        });
        enc.write_field_with(field::move_terminal::OWNER_TERMINAL, |e| {
            encode_terminal_id(owner_terminal, e);
        });
    }

    /// Write the `RESOURCE_MOVED` payload.
    fn encode_terminal_moved(enc: &mut Encoder<'_>, request_id: u32, result: &MoveResult) {
        enc.write_field_with(field::terminal_moved::REQUEST_ID, |e| {
            e.write_u32_be(request_id);
        });
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
        enc.write_field_with(field::terminal_closed::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        // Optional exit status: absent field = signal / unknown.
        if let Some(status) = exit_status {
            enc.write_field_with(field::terminal_closed::EXIT_STATUS, |e| {
                e.write_u32_be(u32::from_be_bytes(status.to_be_bytes()));
            });
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
        enc.write_field_with(field::terminal_resize::TERMINAL_ID, |e| {
            encode_terminal_id(terminal_id, e);
        });
        enc.write_field_with(field::terminal_resize::COLS, |e| e.write_u16_be(cols));
        enc.write_field_with(field::terminal_resize::ROWS, |e| e.write_u16_be(rows));
    }

    /// Write the `COMMAND` payload.
    fn encode_command_frame(enc: &mut Encoder<'_>, request_id: u32, command: &Command) {
        enc.write_field_with(field::command::REQUEST_ID, |e| e.write_u32_be(request_id));
        enc.write_field_with(field::command::COMMAND, |e| encode_command(command, e));
    }

    /// Write the `COMMAND_RESULT` payload.
    fn encode_command_result_frame(enc: &mut Encoder<'_>, request_id: u32, result: &CommandResult) {
        enc.write_field_with(field::command_result::REQUEST_ID, |e| {
            e.write_u32_be(request_id);
        });
        enc.write_field_with(field::command_result::RESULT, |e| {
            encode_command_result(result, e);
        });
    }

    /// Write the `SUBSCRIBE_EVENTS` payload.
    fn encode_subscribe_events(enc: &mut Encoder<'_>, terminal: Option<&ResourceId>) {
        // Optional terminal scope: absent field = server-scoped None.
        if let Some(t) = terminal {
            enc.write_field_with(field::subscribe_events::TERMINAL, |e| {
                encode_terminal_id(t, e);
            });
        }
    }

    /// Write the `EVENT` payload.
    fn encode_event(enc: &mut Encoder<'_>, terminal: Option<&ResourceId>, event: &AgentEvent) {
        if let Some(t) = terminal {
            enc.write_field_with(field::event::TERMINAL, |e| encode_terminal_id(t, e));
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
