//! [`FrameKind`] — the decoded wire frame — with its type-byte table and
//! its encode/decode entry points.

use bytes::BytesMut;

use crate::caps::{
    BootstrapCodec, BootstrapLimits, BootstrapProfile, BootstrapStreamProfile, ClientCapabilities,
    OutputMode, ServerCapabilities,
};
use crate::ids::{BootstrapId, ClientId, GroupId, SatelliteHost, StreamId, TerminalId};
use crate::input::InputEvent;
use crate::input::focus::FocusEvent;
use crate::input::key::KeyEvent;
use crate::input::mouse::MouseEvent;
use crate::input::paste::PasteEvent;
use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;
use crate::wire::info::{SessionSnapshot, encode_client_id, encode_session_snapshot};

use super::{
    AgentEvent, AttachTarget, Command, CommandResult, DetachReason, ErrorCode,
    HistoryRejectionReason, HistoryTombstoneReason, MAX_FRAME_LEN, MoveResult, Scope, SpawnResult,
    TYPE_ATTACH, TYPE_ATTACH_READY, TYPE_ATTACHED, TYPE_BELL, TYPE_BOOTSTRAP_BEGIN,
    TYPE_BOOTSTRAP_CHUNK, TYPE_BOOTSTRAP_READY, TYPE_BOOTSTRAP_TOMBSTONE, TYPE_COMMAND,
    TYPE_COMMAND_RESULT, TYPE_DELETE_METADATA, TYPE_DETACH, TYPE_DETACHED, TYPE_ERROR, TYPE_EVENT,
    TYPE_FRAME_ACK, TYPE_GET_METADATA, TYPE_HELLO, TYPE_HELLO_OK, TYPE_HISTORY_PAGE,
    TYPE_HISTORY_REJECTED, TYPE_HISTORY_REQUEST, TYPE_HISTORY_TOMBSTONE, TYPE_INPUT_FOCUS,
    TYPE_INPUT_KEY, TYPE_INPUT_MOUSE, TYPE_INPUT_PASTE, TYPE_INPUT_TERMINAL_REPLY,
    TYPE_LIST_METADATA, TYPE_METADATA_CHANGED, TYPE_METADATA_KEYS, TYPE_METADATA_VALUE,
    TYPE_MOVE_TERMINAL, TYPE_PING, TYPE_PONG, TYPE_SET_METADATA, TYPE_SPAWN_TERMINAL,
    TYPE_SUBSCRIBE_EVENTS, TYPE_SUBSCRIBE_METADATA, TYPE_TERMINAL_CLOSED, TYPE_TERMINAL_MOVED,
    TYPE_TERMINAL_OUTPUT, TYPE_TERMINAL_RESIZE, TYPE_TERMINAL_SPAWNED, TYPE_VIEWPORT_RESIZE,
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
/// explicit bootstrap/profile/history frames from ADR-0070. `TerminalOutput`
/// remains VT bytes, now bound to a non-zero stream and bootstrap generation.
///
/// [ADR-0013]: https://github.com/phall1/phux/blob/main/ADR/0013-libghostty-bytes-on-wire.md
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

    /// `TERMINAL_OUTPUT` — live terminal content (`docs/spec/L1.md` §4.1).
    ///
    /// `stream_id` and `bootstrap_id` bind every live frame to one published
    /// replica generation. `seq` is checked, non-wrapping, and contiguous
    /// within that pair. Under `NativeState`, `bytes` are the exact PTY bytes
    /// and MUST NOT be color- or capability-rewritten. Compatibility profiles
    /// may carry raw or synthesized VT according to the selected profile.
    TerminalOutput {
        /// Target terminal.
        terminal_id: TerminalId,
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
    /// Wire shape: tagged [`TerminalId`] followed by the encoded [`KeyEvent`].
    InputKey {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Structured key event; libghostty atoms inside.
        event: KeyEvent,
    },

    /// `INPUT_MOUSE` — client forwards a mouse event (`docs/spec/input.md` §3).
    InputMouse {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Structured mouse event; coordinates are terminal-local pixels.
        event: MouseEvent,
    },

    /// `INPUT_FOCUS` — client reports focus change on its host window
    /// (`docs/spec/input.md` §4).
    InputFocus {
        /// Target terminal.
        terminal_id: TerminalId,
        /// Whether the client window gained or lost focus.
        event: FocusEvent,
    },

    /// `INPUT_PASTE` — client forwards a paste payload (`docs/spec/input.md` §5).
    InputPaste {
        /// Target terminal.
        terminal_id: TerminalId,
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
        terminal_id: TerminalId,
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
        terminal_id: TerminalId,
        /// Logical subscription whose `StateSync` reference advances.
        stream_id: StreamId,
        /// Replica generation whose reference advances.
        bootstrap_id: BootstrapId,
        /// Highest contiguous `TERMINAL_OUTPUT.seq` applied.
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
        terminal_id: TerminalId,
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
        terminal_id: TerminalId,
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
        terminal_id: TerminalId,
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
        terminal_id: TerminalId,
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
        terminal_id: TerminalId,
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
        terminal_id: TerminalId,
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
        terminal_id: TerminalId,
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
        terminal_id: TerminalId,
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
        terminal_id: TerminalId,
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

    // -------------------------------------------------------------------------
    // L1 Terminal lifecycle frames — SPEC §7.2 / §10.1 (phux-4li.10).
    //
    // Unblocks split-pane / kill-pane (was warn+bell in phux-4li.5) and the
    // per-pane `ioctl(TIOCSWINSZ)` half of phux-4li.9's SIGWINCH wire-up.
    // The server-side handler + client-side emission land in follow-up
    // tickets; this enum allocation is the wire substrate they build on.
    // -------------------------------------------------------------------------
    /// `SPAWN_TERMINAL` — client requests a new Terminal under `group`
    /// (`docs/spec/L1.md` §1 / §10.1).
    ///
    /// Async: the server replies with [`FrameKind::TerminalSpawned`]
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
    SpawnTerminal {
        /// Correlates this request with the eventual `TerminalSpawned`.
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
        owner_terminal: Option<TerminalId>,
        /// Opaque [`TERMINAL_AGENT_SESSION_KEY`](super::TERMINAL_AGENT_SESSION_KEY) bytes to install atomically on
        /// the new local Terminal before it becomes visible to other clients.
        /// Additive optional field id 9; old servers ignore it, after which a
        /// new launcher still performs its ordinary SET/GET confirmation.
        agent_session: Option<Vec<u8>>,
        /// `(cols, rows)` the new Terminal's grid and PTY are created at,
        /// or `None` to take the server's default (80x24 in the reference
        /// server). A layout-owning consumer knows the tile the new leaf
        /// will occupy before it has an id for it, and passing that here
        /// is strictly better than letting the pane bootstrap at a default
        /// and then reflowing it: the post-spawn `TERMINAL_RESIZE` becomes
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
    },

    /// `TERMINAL_SPAWNED` — server reply to a prior `SpawnTerminal`
    /// (`docs/spec/L1.md` §1 / §10.1).
    ///
    /// Correlated to the originating request by `request_id`. `result`
    /// carries either the freshly allocated [`TerminalId`] or a structured
    /// [`SpawnError`](super::SpawnError). The structured error is deliberately separate from
    /// the generic [`FrameKind::Error`] catch-all so command-correlated
    /// failures stay typed end-to-end (matching the
    /// `METADATA_VALUE` precedent from phux-4li.8).
    TerminalSpawned {
        /// Correlates this reply with a prior `SpawnTerminal.request_id`.
        request_id: u32,
        /// Either the freshly allocated Terminal, or a structured error.
        result: SpawnResult,
    },

    /// `MOVE_TERMINAL` — re-parent a live Terminal into the window owning
    /// `owner_terminal` (`docs/spec/L1.md` §1 / §10.1; ADR-0056).
    ///
    /// Async: the server replies with [`FrameKind::TerminalMoved`]
    /// correlated by `request_id`. `owner_terminal` is an ownership
    /// address exactly as in `SPAWN_TERMINAL`: the destination window may
    /// belong to a different session, and the frame conveys no split
    /// direction, ratio, or focus — layout stays a client-written L3
    /// concern. The pane's process, PTY, scrollback, metadata, and agent
    /// record are untouched, and its `TerminalId` is stable across the
    /// move. Local-only: a satellite-tagged Terminal on either end is
    /// refused with [`MoveError::UnsupportedSatelliteRoute`](super::MoveError::UnsupportedSatelliteRoute). Senders
    /// MUST first see the `MOVE_TERMINAL` feature bit in
    /// `HELLO_OK.server_caps` (`crate::caps::MOVE_TERMINAL`).
    MoveTerminal {
        /// Correlates this request with the eventual `TerminalMoved`.
        request_id: u32,
        /// The Terminal to re-parent.
        terminal: TerminalId,
        /// Existing Terminal whose owning window becomes the destination.
        owner_terminal: TerminalId,
    },

    /// `TERMINAL_MOVED` — server reply to a prior `MoveTerminal`
    /// (`docs/spec/L1.md` §1 / §10.1; ADR-0056).
    ///
    /// Correlated by `request_id`. `result` carries the moved Terminal's
    /// unchanged [`TerminalId`] or a structured [`MoveError`](super::MoveError) — typed
    /// end-to-end for the same reason as [`FrameKind::TerminalSpawned`].
    TerminalMoved {
        /// Correlates this reply with a prior `MoveTerminal.request_id`.
        request_id: u32,
        /// Either the moved Terminal, or a structured error.
        result: MoveResult,
    },

    /// `TERMINAL_CLOSED` — server notifies clients that a Terminal exited
    /// (`docs/spec/L1.md` §1 / §10.1).
    ///
    /// Emitted when the underlying PTY exits, whether by `_exit(n)`, by
    /// signal, or via a `KILL_TERMINAL` command. `exit_status = Some(n)`
    /// reports the process's exit code; `None` covers signal kills and
    /// unknown-cause exits (a deliberately compact subset of SPEC §10.1's
    /// `ExitStatus` tagged union — the wider tagged union grows in a
    /// follow-up wire bump if the additional structure proves
    /// load-bearing).
    TerminalClosed {
        /// The Terminal that exited.
        terminal_id: TerminalId,
        /// Process exit code (`_exit(n)`), or `None` for signals / unknown.
        exit_status: Option<i32>,
    },

    /// `TERMINAL_RESIZE` — client signals a per-Terminal PTY resize
    /// (`docs/spec/L1.md` §1 / §10.2).
    ///
    /// Sent in addition to (not in place of) `VIEWPORT_RESIZE`: the
    /// outer-viewport frame conveys the client's smallest-common-bounding-
    /// box; this frame conveys the resolved per-pane dimensions after the
    /// client's layout walk. The server's PTY layer drives
    /// `ioctl(TIOCSWINSZ)` from this. Implementations SHOULD treat `cols`
    /// or `rows` of zero as a no-op rather than a kernel error (the
    /// codec round-trips zero faithfully).
    TerminalResize {
        /// Target Terminal.
        terminal_id: TerminalId,
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
        terminal: Option<TerminalId>,
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
        terminal: Option<TerminalId>,
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
    pub fn into_frame(self, terminal_id: TerminalId) -> FrameKind {
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
            Self::TerminalOutput { .. } => TYPE_TERMINAL_OUTPUT,
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
            Self::SpawnTerminal { .. } => TYPE_SPAWN_TERMINAL,
            Self::MoveTerminal { .. } => TYPE_MOVE_TERMINAL,
            Self::TerminalMoved { .. } => TYPE_TERMINAL_MOVED,
            Self::TerminalSpawned { .. } => TYPE_TERMINAL_SPAWNED,
            Self::TerminalClosed { .. } => TYPE_TERMINAL_CLOSED,
            Self::TerminalResize { .. } => TYPE_TERMINAL_RESIZE,
            Self::Command { .. } => TYPE_COMMAND,
            Self::CommandResult { .. } => TYPE_COMMAND_RESULT,
            Self::SubscribeEvents { .. } => TYPE_SUBSCRIBE_EVENTS,
            Self::Event { .. } => TYPE_EVENT,
        }
    }

    /// Encode `self` as a complete length-prefixed frame.
    ///
    /// Writes the four-byte big-endian length header, the type byte, and the
    /// payload. The caller owns the `BytesMut` lifecycle.
    #[allow(
        clippy::too_many_lines,
        reason = "single match over the SPEC §7 catalog; splitting would scatter the encoder/decoder symmetry"
    )]
    pub fn encode(&self, out: &mut BytesMut) {
        // Reserve four bytes for the length header; backfill once we know how
        // many bytes the type + payload consumed.
        let header_pos = out.len();
        out.extend_from_slice(&[0u8; 4]);

        let body_start = out.len();
        let mut enc = Encoder::new(out);
        enc.write_u8(self.type_byte());

        match self {
            Self::Hello {
                client_name,
                protocol_major,
                protocol_minor,
                protocol_patch,
                client_caps,
            } => {
                // String fields ride as raw UTF-8 bytes — the field is already
                // length-delimited by the TLV header, so no inner length prefix.
                enc.write_field(field::hello::CLIENT_NAME, client_name.as_bytes());
                enc.write_field_with(field::hello::PROTOCOL_MAJOR, |e| {
                    e.write_u16_be(*protocol_major);
                });
                enc.write_field_with(field::hello::PROTOCOL_MINOR, |e| {
                    e.write_u16_be(*protocol_minor);
                });
                enc.write_field_with(field::hello::PROTOCOL_PATCH, |e| {
                    e.write_u16_be(*protocol_patch);
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
            }
            Self::HelloOk {
                protocol_major,
                protocol_minor,
                protocol_patch,
                server_caps,
                server_id,
                selected_profile,
                bootstrap_limits,
            } => {
                enc.write_field_with(field::hello_ok::PROTOCOL_MAJOR, |e| {
                    e.write_u16_be(*protocol_major);
                });
                enc.write_field_with(field::hello_ok::PROTOCOL_MINOR, |e| {
                    e.write_u16_be(*protocol_minor);
                });
                enc.write_field_with(field::hello_ok::PROTOCOL_PATCH, |e| {
                    e.write_u16_be(*protocol_patch);
                });
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
                    encode_bootstrap_profile(*selected_profile, e);
                });
                enc.write_field_with(field::hello_ok::MAX_CHUNK_BYTES, |e| {
                    e.write_u32_be(bootstrap_limits.max_chunk_bytes());
                });
                enc.write_field_with(field::hello_ok::MAX_HISTORY_PAGE_BYTES, |e| {
                    e.write_u32_be(bootstrap_limits.max_history_page_bytes());
                });
            }
            // `Ping` and `Pong` share a single-`u64` nonce field; merged to
            // satisfy `clippy::match_same_arms`.
            Self::Ping { nonce } | Self::Pong { nonce } => {
                enc.write_field_with(field::ping::NONCE, |e| e.write_u64_be(*nonce));
            }
            Self::TerminalOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => {
                enc.write_field_with(field::terminal_output::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field_with(field::terminal_output::SEQ, |e| e.write_u64_be(*seq));
                enc.write_field(field::terminal_output::BYTES, bytes);
                enc.write_field_with(field::terminal_output::STREAM_ID, |e| {
                    e.write_u64_be(stream_id.get());
                });
                enc.write_field_with(field::terminal_output::BOOTSTRAP_ID, |e| {
                    e.write_u64_be(bootstrap_id.get());
                });
            }
            Self::Attach {
                attach_id,
                target,
                viewport,
                request_scrollback,
                scrollback_limit_lines,
            } => {
                enc.write_field_with(field::attach::TARGET, |e| encode_attach_target(target, e));
                enc.write_field_with(field::attach::VIEWPORT, |e| {
                    encode_viewport_info(viewport, e);
                });
                enc.write_field_with(field::attach::REQUEST_SCROLLBACK, |e| {
                    e.write_u8(u8::from(*request_scrollback));
                });
                enc.write_field_with(field::attach::SCROLLBACK_LIMIT_LINES, |e| {
                    e.write_u32_be(*scrollback_limit_lines);
                });
                enc.write_field_with(field::attach::ATTACH_ID, |e| e.write_u32_be(*attach_id));
            }
            // `Detach` is a unit variant: type byte only, no fields.
            Self::Detach => {}
            Self::Detached { reason, message } => {
                // Both fields are optional-absent (field.rs allocation
                // discipline): an unstated reason and an empty message encode
                // as nothing at all, which keeps the common
                // acknowledge-a-clean-detach frame byte-identical to what
                // every 0.7.0 peer already emits.
                if let Some(reason) = reason {
                    enc.write_field_with(field::detached::REASON, |e| {
                        e.write_u8(reason.as_wire());
                    });
                }
                if !message.is_empty() {
                    enc.write_field(field::detached::MESSAGE, message.as_bytes());
                }
            }
            Self::InputKey { terminal_id, event } => {
                enc.write_field_with(field::input_key::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field_with(field::input_key::EVENT, |e| encode_key_event(event, e));
            }
            Self::InputMouse { terminal_id, event } => {
                enc.write_field_with(field::input_mouse::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field_with(field::input_mouse::EVENT, |e| encode_mouse_event(event, e));
            }
            Self::InputFocus { terminal_id, event } => {
                enc.write_field_with(field::input_focus::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field_with(field::input_focus::EVENT, |e| {
                    e.write_u8(encode_focus_event(*event));
                });
            }
            Self::InputPaste { terminal_id, event } => {
                enc.write_field_with(field::input_paste::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field_with(field::input_paste::EVENT, |e| encode_paste_event(event, e));
            }
            Self::InputTerminalReply { terminal_id, bytes } => {
                enc.write_field_with(field::input_terminal_reply::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field(field::input_terminal_reply::BYTES, bytes.as_ref());
            }
            Self::FrameAck {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
            } => {
                enc.write_field_with(field::frame_ack::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field_with(field::frame_ack::SEQ, |e| e.write_u64_be(*seq));
                enc.write_field_with(field::frame_ack::STREAM_ID, |e| {
                    e.write_u64_be(stream_id.get());
                });
                enc.write_field_with(field::frame_ack::BOOTSTRAP_ID, |e| {
                    e.write_u64_be(bootstrap_id.get());
                });
            }
            Self::ViewportResize { viewport } => {
                enc.write_field_with(field::viewport_resize::VIEWPORT, |e| {
                    encode_viewport_info(viewport, e);
                });
            }
            Self::Attached {
                attach_id,
                snapshot,
                initial_client_id,
            } => {
                enc.write_field_with(field::attached::SNAPSHOT, |e| {
                    encode_session_snapshot(snapshot, e);
                });
                enc.write_field_with(field::attached::INITIAL_CLIENT_ID, |e| {
                    encode_client_id(*initial_client_id, e);
                });
                enc.write_field_with(field::attached::ATTACH_ID, |e| e.write_u32_be(*attach_id));
            }
            Self::AttachReady { attach_id } => {
                enc.write_field_with(field::attach_ready::ATTACH_ID, |e| {
                    e.write_u32_be(*attach_id);
                });
            }
            Self::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => {
                enc.write_field_with(field::bootstrap_begin::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field_with(field::bootstrap_begin::STREAM_ID, |e| {
                    e.write_u64_be(stream_id.get());
                });
                enc.write_field_with(field::bootstrap_begin::BOOTSTRAP_ID, |e| {
                    e.write_u64_be(bootstrap_id.get());
                });
                let (codec, output_mode) = match profile {
                    BootstrapStreamProfile::NativeState { codec } => {
                        (BootstrapCodec::Native(*codec), OutputMode::Raw)
                    }
                    BootstrapStreamProfile::SynthesizedVtRaw => {
                        (BootstrapCodec::SynthesizedVtV1, OutputMode::Raw)
                    }
                    BootstrapStreamProfile::SynthesizedVtStateSync => {
                        (BootstrapCodec::SynthesizedVtV1, OutputMode::StateSync)
                    }
                };
                enc.write_field_with(field::bootstrap_begin::CODEC, |e| {
                    encode_bootstrap_codec(codec, e);
                });
                enc.write_field_with(field::bootstrap_begin::COLS, |e| e.write_u16_be(*cols));
                enc.write_field_with(field::bootstrap_begin::ROWS, |e| e.write_u16_be(*rows));
                enc.write_field_with(field::bootstrap_begin::OUTPUT_MODE, |e| {
                    e.write_u8(output_mode.as_wire());
                });
                enc.write_field_with(field::bootstrap_begin::BASE_SEQ, |e| {
                    e.write_u64_be(*base_seq);
                });
            }
            Self::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => {
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
                    e.write_u32_be(*chunk_seq);
                });
                enc.write_field(field::bootstrap_chunk::PAYLOAD, payload);
            }
            Self::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => {
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
            Self::HistoryRequest {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                max_bytes,
                max_rows,
            } => {
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
                    e.write_u32_be(*max_bytes);
                });
                enc.write_field_with(field::history_request::MAX_ROWS, |e| {
                    e.write_u32_be(*max_rows);
                });
            }
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
                enc.write_field_with(field::history_page::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field_with(field::history_page::STREAM_ID, |e| {
                    e.write_u64_be(stream_id.get());
                });
                enc.write_field_with(field::history_page::BOOTSTRAP_ID, |e| {
                    e.write_u64_be(bootstrap_id.get());
                });
                enc.write_field(field::history_page::CURSOR, cursor);
                if let Some(next) = next_cursor {
                    enc.write_field(field::history_page::NEXT_CURSOR, next);
                }
                enc.write_field(field::history_page::PAYLOAD, payload);
                enc.write_field_with(field::history_page::PAGE_SEQ, |e| {
                    e.write_u64_be(*page_seq);
                });
                enc.write_field_with(field::history_page::ROWS, |e| {
                    e.write_u32_be(*rows);
                });
            }
            Self::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => {
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
                    e.write_u64_be(*last_valid_seq);
                });
            }
            Self::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => {
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
            Self::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => {
                enc.write_field_with(field::history_rejected::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field_with(field::history_rejected::STREAM_ID, |e| {
                    e.write_u64_be(stream_id.get());
                });
                enc.write_field_with(field::history_rejected::BOOTSTRAP_ID, |e| {
                    e.write_u64_be(bootstrap_id.get());
                });
                enc.write_field(field::history_rejected::CURSOR, cursor);
                enc.write_field_with(field::history_rejected::REASON, |e| {
                    e.write_u8(reason.as_wire());
                });
                enc.write_field_with(field::history_rejected::REQUIRED_BYTES, |e| {
                    e.write_u32_be(*required_bytes);
                });
                enc.write_field_with(field::history_rejected::REQUIRED_ROWS, |e| {
                    e.write_u32_be(*required_rows);
                });
            }
            Self::Bell { terminal_id } => {
                enc.write_field_with(field::bell::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
            }
            Self::Error {
                request_id,
                code,
                message,
            } => {
                // Optional request_id: absent field = None.
                if let Some(id) = request_id {
                    enc.write_field_with(field::error::REQUEST_ID, |e| e.write_u32_be(*id));
                }
                enc.write_field_with(field::error::CODE, |e| e.write_u16_be(code.as_wire()));
                enc.write_field(field::error::MESSAGE, message.as_bytes());
            }
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
            } => {
                enc.write_field_with(field::get_metadata::REQUEST_ID, |e| {
                    e.write_u32_be(*request_id);
                });
                enc.write_field_with(field::get_metadata::SCOPE, |e| encode_scope(scope, e));
                enc.write_field(field::get_metadata::KEY, key.as_bytes());
            }
            Self::SetMetadata {
                request_id,
                scope,
                key,
                value,
            } => {
                enc.write_field_with(field::set_metadata::REQUEST_ID, |e| {
                    e.write_u32_be(*request_id);
                });
                enc.write_field_with(field::set_metadata::SCOPE, |e| encode_scope(scope, e));
                enc.write_field(field::set_metadata::KEY, key.as_bytes());
                enc.write_field(field::set_metadata::VALUE, value);
            }
            Self::ListMetadata { request_id, scope } => {
                enc.write_field_with(field::list_metadata::REQUEST_ID, |e| {
                    e.write_u32_be(*request_id);
                });
                enc.write_field_with(field::list_metadata::SCOPE, |e| encode_scope(scope, e));
            }
            Self::SubscribeMetadata { scope, key } => {
                enc.write_field_with(field::subscribe_metadata::SCOPE, |e| encode_scope(scope, e));
                enc.write_field(field::subscribe_metadata::KEY, key.as_bytes());
            }
            Self::MetadataChanged { scope, key, value } => {
                enc.write_field_with(field::metadata_changed::SCOPE, |e| encode_scope(scope, e));
                enc.write_field(field::metadata_changed::KEY, key.as_bytes());
                // Optional value: absent field = tombstone (None).
                if let Some(v) = value.as_deref() {
                    enc.write_field(field::metadata_changed::VALUE, v);
                }
            }
            Self::MetadataValue { request_id, value } => {
                enc.write_field_with(field::metadata_value::REQUEST_ID, |e| {
                    e.write_u32_be(*request_id);
                });
                // Optional value: absent field = key absent (None).
                if let Some(v) = value.as_deref() {
                    enc.write_field(field::metadata_value::VALUE, v);
                }
            }
            Self::MetadataKeys { request_id, keys } => {
                enc.write_field_with(field::metadata_keys::REQUEST_ID, |e| {
                    e.write_u32_be(*request_id);
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
            Self::SpawnTerminal {
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
            } => {
                enc.write_field_with(field::spawn_terminal::REQUEST_ID, |e| {
                    e.write_u32_be(*request_id);
                });
                enc.write_field_with(field::spawn_terminal::GROUP, |e| {
                    e.write_u32_be(group.get());
                });
                // Optional command/cwd/env: absent field = None. An empty list
                // (`Some(vec![])`) stays distinct: a present field with a zero
                // count.
                if let Some(cmd) = command.as_deref() {
                    enc.write_field_with(field::spawn_terminal::COMMAND, |e| {
                        encode_string_list(cmd, e);
                    });
                }
                if let Some(c) = cwd.as_deref() {
                    enc.write_field(field::spawn_terminal::CWD, c.as_bytes());
                }
                if let Some(env) = env.as_deref() {
                    enc.write_field_with(field::spawn_terminal::ENV, |e| encode_env(env, e));
                }
                if let Some(t) = term.as_deref() {
                    enc.write_field(field::spawn_terminal::TERM, t.as_bytes());
                }
                if let Some(host) = satellite.as_ref() {
                    enc.write_field(field::spawn_terminal::SATELLITE, host.as_str().as_bytes());
                }
                if let Some(owner) = owner_terminal.as_ref() {
                    enc.write_field_with(field::spawn_terminal::OWNER_TERMINAL, |e| {
                        encode_terminal_id(owner, e);
                    });
                }
                if let Some(value) = agent_session {
                    enc.write_field(field::spawn_terminal::AGENT_SESSION, value);
                }
                if let Some((cols, rows)) = initial_size {
                    enc.write_field_with(field::spawn_terminal::INITIAL_SIZE, |e| {
                        e.write_u16_be(*cols);
                        e.write_u16_be(*rows);
                    });
                }
            }
            Self::TerminalSpawned { request_id, result } => {
                enc.write_field_with(field::terminal_spawned::REQUEST_ID, |e| {
                    e.write_u32_be(*request_id);
                });
                enc.write_field_with(field::terminal_spawned::RESULT, |e| {
                    encode_spawn_result(result, e);
                });
            }
            Self::MoveTerminal {
                request_id,
                terminal,
                owner_terminal,
            } => {
                enc.write_field_with(field::move_terminal::REQUEST_ID, |e| {
                    e.write_u32_be(*request_id);
                });
                enc.write_field_with(field::move_terminal::TERMINAL, |e| {
                    encode_terminal_id(terminal, e);
                });
                enc.write_field_with(field::move_terminal::OWNER_TERMINAL, |e| {
                    encode_terminal_id(owner_terminal, e);
                });
            }
            Self::TerminalMoved { request_id, result } => {
                enc.write_field_with(field::terminal_moved::REQUEST_ID, |e| {
                    e.write_u32_be(*request_id);
                });
                enc.write_field_with(field::terminal_moved::RESULT, |e| {
                    encode_move_result(result, e);
                });
            }
            Self::TerminalClosed {
                terminal_id,
                exit_status,
            } => {
                enc.write_field_with(field::terminal_closed::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                // Optional exit status: absent field = signal / unknown.
                if let Some(status) = exit_status {
                    enc.write_field_with(field::terminal_closed::EXIT_STATUS, |e| {
                        e.write_u32_be(u32::from_be_bytes(status.to_be_bytes()));
                    });
                }
            }
            Self::TerminalResize {
                terminal_id,
                cols,
                rows,
            } => {
                enc.write_field_with(field::terminal_resize::TERMINAL_ID, |e| {
                    encode_terminal_id(terminal_id, e);
                });
                enc.write_field_with(field::terminal_resize::COLS, |e| e.write_u16_be(*cols));
                enc.write_field_with(field::terminal_resize::ROWS, |e| e.write_u16_be(*rows));
            }
            Self::Command {
                request_id,
                command,
            } => {
                enc.write_field_with(field::command::REQUEST_ID, |e| e.write_u32_be(*request_id));
                enc.write_field_with(field::command::COMMAND, |e| encode_command(command, e));
            }
            Self::CommandResult { request_id, result } => {
                enc.write_field_with(field::command_result::REQUEST_ID, |e| {
                    e.write_u32_be(*request_id);
                });
                enc.write_field_with(field::command_result::RESULT, |e| {
                    encode_command_result(result, e);
                });
            }
            Self::SubscribeEvents { terminal } => {
                // Optional terminal scope: absent field = server-scoped None.
                if let Some(t) = terminal.as_ref() {
                    enc.write_field_with(field::subscribe_events::TERMINAL, |e| {
                        encode_terminal_id(t, e);
                    });
                }
            }
            Self::Event { terminal, event } => {
                if let Some(t) = terminal.as_ref() {
                    enc.write_field_with(field::event::TERMINAL, |e| encode_terminal_id(t, e));
                }
                enc.write_field_with(field::event::EVENT, |e| encode_agent_event(event, e));
            }
        }

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
