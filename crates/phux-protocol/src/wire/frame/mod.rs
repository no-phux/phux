//! Frame header and `FrameKind` enum.
//!
//! See `docs/spec/proto.md` §5 (framing) and §7 (message catalog).
//!
//! Wire layout (per `docs/spec/proto.md` §5):
//!
//! ```text
//! +-------------------------+
//! | length: u32 big-endian  |   number of bytes that follow
//! +-------------------------+
//! | type:   u8              |   message discriminant from §7
//! +-------------------------+
//! | payload: length-1 bytes |
//! +-------------------------+
//! ```
//!
//! `length` is at least `1` (the type byte) and at most `MAX_FRAME_LEN`.
//!
//! Under [ADR-0013] terminal content rides as raw VT bytes (`RESOURCE_OUTPUT`).
//! There is no structured per-cell diff variant on this enum — earlier
//! drafts carried `PaneDiff` at type byte `0x40`; that slot is retired
//! and `RESOURCE_OUTPUT` (type `0x90` per SPEC §7.2) takes its place.
//!
//! [ADR-0013]: https://github.com/no-phux/phux/blob/main/ADR/0013-libghostty-bytes-on-wire.md

/// Maximum permitted value of the wire-frame `length` field, per `docs/spec/proto.md` §5
/// ("at most `16_777_216` (16 MiB)").
pub const MAX_FRAME_LEN: u32 = 16 * 1024 * 1024;
/// Maximum bytes in one opaque engine-owned history cursor.
pub const MAX_HISTORY_CURSOR_BYTES: usize = 4 * 1024;
/// Hard upper bound for rows requested or reported in one native history page.
pub const MAX_HISTORY_PAGE_ROWS: u32 = 4 * 1024;
/// Maximum bytes in one opaque client terminal-emulator PTY reply.
///
/// This matches the existing 64 KiB input-command bound and keeps a stateful
/// reply below pre-auth buffering limits with ample TLV envelope headroom.
pub const MAX_INPUT_TERMINAL_REPLY_BYTES: usize = 64 * 1024;
/// Maximum payload bytes in one [`Command::PutFile`] chunk. The 8 MiB ceiling
/// leaves ample room below the 16 MiB frame cap for envelope and metadata.
pub const MAX_FILE_UPLOAD_CHUNK: usize = 8 * 1024 * 1024;
/// Maximum completed file size accepted by [`Command::PutFile`].
pub const MAX_FILE_UPLOAD_SIZE: u64 = 64 * 1024 * 1024;
/// Maximum payload bytes in one [`Command::AppendResourceOutput`] call.
///
/// Matches the 64 KiB input-command and terminal-reply bounds: one append is
/// one or more complete records, never a bulk upload, so a producer with more
/// to say sends more appends.
pub const MAX_APPEND_BYTES: usize = 64 * 1024;
/// Maximum bytes in a `SPAWN_RESOURCE.provider` string (field 13): the
/// `integration_id` bound of the `phux.agent-session/v1` record
/// (`docs/spec/L3.md` §3.7.1).
pub const MAX_RESOURCE_PROVIDER_BYTES: usize = 120;
/// Maximum bytes in a `SPAWN_RESOURCE.native_id` string (field 14): the
/// `native_id` bound of the `phux.agent-session/v1` record
/// (`docs/spec/L3.md` §3.7.1).
pub const MAX_RESOURCE_NATIVE_ID_BYTES: usize = 1024;

// -----------------------------------------------------------------------------
// Message discriminants from SPEC §7. Only the variants implemented in this
// scaffold are exposed via `FrameKind`; the remaining IDs are recorded here so
// sibling tasks can wire them up without re-deriving the catalog.
// -----------------------------------------------------------------------------

/// Wire type byte for one SPEC §7 message-catalog entry.
///
/// The public surface stays the `TYPE_*` consts below, each of which takes
/// its value from a variant here. Declaring the catalog as a field-less
/// `#[repr(u8)]` enum — the same protection [`ErrorCode`](super::ErrorCode)
/// already has — makes a duplicated type byte compile error E0081, so two
/// branches can no longer allocate the same discriminant and ship a silent
/// protocol break (phux-ke0c). Retired slots stay out of the enum; their
/// comments below keep the allocation history. A new allocation picks an
/// open value from `docs/spec/appendix-reserved.md` §1 and adds the variant
/// and its const in the same edit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
enum FrameType {
    Hello = 0x01,
    Attach = 0x02,
    Detach = 0x03,
    InputKey = 0x10,
    InputPaste = 0x11,
    InputMouse = 0x12,
    InputFocus = 0x14,
    HistoryRequest = 0x16,
    InputTerminalReply = 0x17,
    FrameAck = 0x21,
    ViewportResize = 0x20,
    Ping = 0x7F,
    HelloOk = 0x80,
    Attached = 0x81,
    Detached = 0x82,
    AttachReady = 0x83,
    Bell = 0xB0,
    Error = 0xC1,
    Pong = 0xFF,
    ResourceOutput = 0x90,
    BootstrapBegin = 0x93,
    BootstrapChunk = 0x94,
    BootstrapReady = 0x95,
    HistoryPage = 0x96,
    BootstrapTombstone = 0x97,
    HistoryTombstone = 0x98,
    HistoryRejected = 0x99,
    FrameCompressed = 0x9A,
    GetMetadata = 0x50,
    SetMetadata = 0x51,
    DeleteMetadata = 0x52,
    ListMetadata = 0x53,
    SubscribeMetadata = 0x54,
    MetadataChanged = 0xD0,
    MetadataValue = 0xD1,
    MetadataKeys = 0xD2,
    ListDirectory = 0x55,
    DirectoryListing = 0xD3,
    SpawnResource = 0x22,
    ResizeTerminal = 0x23,
    MoveResource = 0x2A,
    ResourceMoved = 0xA8,
    ResourceClosed = 0xA1,
    ResourceSpawned = 0xA2,
    Command = 0x31,
    CommandResult = 0xC2,
    SubscribeEvents = 0x41,
    Event = 0xB3,
}

/// Discriminant for `HELLO` (client to server, `docs/spec/proto.md` §6.1).
pub const TYPE_HELLO: u8 = FrameType::Hello as u8;
/// Discriminant for `ATTACH` (client to server, `docs/spec/proto.md` §7.1 / §13).
pub const TYPE_ATTACH: u8 = FrameType::Attach as u8;
/// Discriminant for `DETACH` (client to server, `docs/spec/proto.md` §7.1 / §7.3).
pub const TYPE_DETACH: u8 = FrameType::Detach as u8;
/// Discriminant for `INPUT_KEY` (client to server, `docs/spec/input.md` §2).
pub const TYPE_INPUT_KEY: u8 = FrameType::InputKey as u8;
/// Discriminant for `INPUT_PASTE` (client to server, `docs/spec/input.md` §5).
pub const TYPE_INPUT_PASTE: u8 = FrameType::InputPaste as u8;
/// Discriminant for `INPUT_MOUSE` (client to server, `docs/spec/input.md` §3).
pub const TYPE_INPUT_MOUSE: u8 = FrameType::InputMouse as u8;
/// Discriminant for `INPUT_FOCUS` (client to server, `docs/spec/input.md` §4).
pub const TYPE_INPUT_FOCUS: u8 = FrameType::InputFocus as u8;
// 0x15 was `INPUT_SELECTION`, removed in v0.5.0 (phux-q1ni, ADR-0030):
// selection is a client-side projection over the consumer's own engine, not a
// wire frame. The discriminant is left unassigned.
/// Discriminant for `HISTORY_REQUEST` (client to server, `docs/spec/L1.md` §4.5).
pub const TYPE_HISTORY_REQUEST: u8 = FrameType::HistoryRequest as u8;
/// Discriminant for `INPUT_TERMINAL_REPLY` (client to server,
/// `docs/spec/input.md` §6).
pub const TYPE_INPUT_TERMINAL_REPLY: u8 = FrameType::InputTerminalReply as u8;
/// Discriminant for StateSync-only `FRAME_ACK` (client to server,
/// `docs/spec/proto.md` §8.2).
///
/// Cumulative within one `(ResourceId, StreamId, BootstrapId)` after the
/// client applies the acknowledged transition. Raw profiles never send it.
pub const TYPE_FRAME_ACK: u8 = FrameType::FrameAck as u8;
/// Discriminant for `VIEWPORT_RESIZE` (client to server, `docs/spec/proto.md` §7.1 / §10.5).
///
/// The client emits this when its outer terminal changes size (SIGWINCH on
/// Unix, the GUI resize event on graphical hosts). Payload reuses the
/// [`ViewportInfo`] shape carried by `ATTACH` (§13) — phux-4hp keeps the wire
/// shape minimal and lets future tickets grow the per-cell pixel + padding
/// metrics from SPEC §10.5 when the mouse-encoder needs them.
pub const TYPE_VIEWPORT_RESIZE: u8 = FrameType::ViewportResize as u8;
/// Discriminant for `PING` (client to server, `docs/spec/proto.md` §7.4).
pub const TYPE_PING: u8 = FrameType::Ping as u8;
/// Discriminant for `HELLO_OK` (server to client, `docs/spec/proto.md` §6.1).
pub const TYPE_HELLO_OK: u8 = FrameType::HelloOk as u8;
/// Discriminant for `ATTACHED` (server to client, `docs/spec/L1.md` §8).
pub const TYPE_ATTACHED: u8 = FrameType::Attached as u8;
/// Discriminant for `DETACHED` (server to client, `docs/spec/L1.md` §1 / §7.3).
pub const TYPE_DETACHED: u8 = FrameType::Detached as u8;
/// Discriminant for `ATTACH_READY` (server to client, `docs/spec/L1.md` §8).
pub const TYPE_ATTACH_READY: u8 = FrameType::AttachReady as u8;
/// Discriminant for `BELL` (server to client, `docs/spec/L1.md` §1.2).
pub const TYPE_BELL: u8 = FrameType::Bell as u8;
/// Discriminant for `ERROR` (server to client, `docs/spec/proto.md` §9).
///
/// Carries a structured [`ErrorCode`] plus a human-readable UTF-8 message
/// and an optional `request_id` correlating the error with a prior
/// `COMMAND` (per SPEC §14). Fatal errors MUST be followed by `DETACHED
/// { reason: PROTOCOL_ERROR }` and transport close.
pub const TYPE_ERROR: u8 = FrameType::Error as u8;
/// Discriminant for `PONG` (server to client, `docs/spec/proto.md` §7.4).
pub const TYPE_PONG: u8 = FrameType::Pong as u8;
/// Discriminant for generation-bound `RESOURCE_OUTPUT` (server to client,
/// `docs/spec/L1.md` §4.1). Native-profile payloads are byte-identical raw PTY
/// bytes and are never capability-rewritten.
pub const TYPE_RESOURCE_OUTPUT: u8 = FrameType::ResourceOutput as u8;
// 0x91 was `TERMINAL_SNAPSHOT` through protocol 0.6. It is permanently
// retired by ADR-0070 and MUST NOT be decoded or reassigned.
/// Discriminant for `BOOTSTRAP_BEGIN` (server to client, `docs/spec/L1.md` §4.3).
pub const TYPE_BOOTSTRAP_BEGIN: u8 = FrameType::BootstrapBegin as u8;
/// Discriminant for `BOOTSTRAP_CHUNK` (server to client, `docs/spec/L1.md` §4.3).
pub const TYPE_BOOTSTRAP_CHUNK: u8 = FrameType::BootstrapChunk as u8;
/// Discriminant for `BOOTSTRAP_READY` (server to client, `docs/spec/L1.md` §4.3).
pub const TYPE_BOOTSTRAP_READY: u8 = FrameType::BootstrapReady as u8;
/// Discriminant for `HISTORY_PAGE` (server to client, `docs/spec/L1.md` §4.5).
pub const TYPE_HISTORY_PAGE: u8 = FrameType::HistoryPage as u8;
/// Discriminant for `BOOTSTRAP_TOMBSTONE` (server to client, `docs/spec/L1.md` §4.6).
pub const TYPE_BOOTSTRAP_TOMBSTONE: u8 = FrameType::BootstrapTombstone as u8;
/// Discriminant for cursor-scoped `HISTORY_TOMBSTONE` (server to client,
/// `docs/spec/L1.md` §4.5).
pub const TYPE_HISTORY_TOMBSTONE: u8 = FrameType::HistoryTombstone as u8;
/// Discriminant for retryable cursor-scoped `HISTORY_REJECTED` (server to
/// client, `docs/spec/L1.md` §4.5).
pub const TYPE_HISTORY_REJECTED: u8 = FrameType::HistoryRejected as u8;
/// Discriminant for `FRAME_COMPRESSED` (server to client).
///
/// A negotiated envelope carrying one deflated inner frame
/// (`docs/spec/proto.md` §6.4). Allocated from the `0x9A..=0x9F` hot-path
/// reserve (`docs/spec/appendix-reserved.md` §1) because it wraps the hot
/// path's largest frames.
pub const TYPE_FRAME_COMPRESSED: u8 = FrameType::FrameCompressed as u8;

// -----------------------------------------------------------------------------
// L3 metadata frame discriminants — SPEC §7.4 (phux-4li.2).
//
// Contiguous block 0x50..=0x54 for C→S commands; 0xD0 for the single S→C
// notification. Sits between the L1 hot-path C→S range (0x10..=0x21) and
// the proto SUBSCRIBE slot (0x40). There is no L2 tier (ADR-0030), so the
// tail of the block stays L3: `LIST_DIRECTORY` takes 0x55 and 0x56..=0x5F
// remain open. The S→C side uses 0xD0..=0xDF as a matching block
// (`DIRECTORY_LISTING` takes 0xD3), with `BELL` (0xB0) / `ALERT` (0xB2) and
// `ERROR` (0xC1) already on lower discriminants.
// -----------------------------------------------------------------------------

/// Discriminant for `GET_METADATA` (client to server, `docs/spec/L3.md` §1 / §11.L3).
pub const TYPE_GET_METADATA: u8 = FrameType::GetMetadata as u8;
/// Discriminant for `SET_METADATA` (client to server, `docs/spec/L3.md` §1 / §11.L3).
pub const TYPE_SET_METADATA: u8 = FrameType::SetMetadata as u8;
/// Discriminant for `DELETE_METADATA` (client to server, `docs/spec/L3.md` §1 / §11.L3).
pub const TYPE_DELETE_METADATA: u8 = FrameType::DeleteMetadata as u8;
/// Discriminant for `LIST_METADATA` (client to server, `docs/spec/L3.md` §1 / §11.L3).
pub const TYPE_LIST_METADATA: u8 = FrameType::ListMetadata as u8;
/// Discriminant for `SUBSCRIBE_METADATA` (client to server, `docs/spec/L3.md` §1).
pub const TYPE_SUBSCRIBE_METADATA: u8 = FrameType::SubscribeMetadata as u8;

/// Discriminant for `METADATA_CHANGED` (server to client, `docs/spec/L3.md` §1).
pub const TYPE_METADATA_CHANGED: u8 = FrameType::MetadataChanged as u8;

/// Conventional L3 metadata key holding a session's human-readable name.
///
/// Introduced by the v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027): with
/// the L2 collection tier dissolved and the `RENAME_SESSION` verb removed,
/// a session rename is expressed as a `SET_METADATA` write of this key.
/// The server is authoritative — it intercepts a write of this key under
/// the appropriate scope and applies the registry rename so that `ls` /
/// `attach` keep reading a single source of truth for the name — but the
/// key is a stable, documented convention clients can rely on (the same way
/// `phux.tui.layout/v1` is for TUI layout).
pub const SESSION_NAME_KEY: &str = "phux.session.name/v1";

/// Conventional L3 metadata key requesting creation of a named session
/// *without* attaching.
///
/// Introduced by the v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027) in
/// place of the removed `CREATE_SESSION` verb. The value is a UTF-8 JSON
/// object `{ "name": str, "command": [str]?, "cwd": str? }`. The server
/// intercepts a `SET_METADATA` write of this key under `Scope::Global`,
/// seeds the session + pane (the same machinery `ATTACH { CreateIfMissing }`
/// uses), and records the result. Because `SET_METADATA` carries no reply
/// frame, the caller follows with `GET_STATE` to read back the new session's
/// seed-pane id (the create-without-attach path `phux new --json` and the
/// MCP `phux_new` tool use).
pub const SESSION_CREATE_KEY: &str = "phux.session.create/v1";

/// Conventional L3 metadata key under which the server publishes the result
/// of the most recent [`SESSION_CREATE_KEY`] write for a given session name.
///
/// Because `SET_METADATA` carries no reply frame, the create-without-attach
/// path needs a way to read back the freshly-seeded pane's id. The server
/// writes a UTF-8 JSON object `{ "name": str, "terminal_id": u32 }` to this
/// key (`Scope::Global`) after a successful create; the client `GET`s it and
/// matches on `name`. Introduced by the v0.3.0 "Option B" re-tier (ADR-0019
/// / ADR-0027).
pub const SESSION_CREATE_RESULT_KEY: &str = "phux.session.created/v1";

/// Prefix for one-shot, nonce-correlated session-create result keys.
///
/// New clients include a UUID `request_token` in [`SESSION_CREATE_KEY`]'s
/// JSON value, then read `"{SESSION_CREATE_RESULT_KEY_PREFIX}{request_token}"`.
/// The server consumes this ephemeral Global value on the first GET. The
/// legacy uncorrelated [`SESSION_CREATE_RESULT_KEY`] remains for old clients.
pub const SESSION_CREATE_RESULT_KEY_PREFIX: &str = "phux.session.created/v1/";

/// Conventional L3 metadata key that sets a session's keep-empty mark
/// (ADR-0105).
///
/// Scope: `Global`. Value: `name\0true` or `name\0false` (NUL-separated
/// UTF-8, the same shape as [`SESSION_NAME_KEY`]). The server intercepts the
/// write, applies it to the named session, and broadcasts the applied value
/// to subscribers when the mark actually changed; the value is not stored.
/// Clearing the mark on a session that holds no windows removes that session,
/// which is how an empty session is killed. Advertised by
/// [`ServerFeature::KeepEmptySessions`](crate::caps::ServerFeature::KeepEmptySessions).
pub const SESSION_KEEP_EMPTY_KEY: &str = "phux.session.keep_empty/v1";

/// Encode a [`SESSION_KEEP_EMPTY_KEY`] value: `name\0true` or `name\0false`.
#[must_use]
pub fn encode_session_keep_empty(name: &str, keep: bool) -> Vec<u8> {
    format!("{name}\0{keep}").into_bytes()
}

/// Decode a [`SESSION_KEEP_EMPTY_KEY`] value into the session name and the
/// mark. `None` for anything other than UTF-8 `name\0true` or `name\0false`.
#[must_use]
pub fn decode_session_keep_empty(value: &[u8]) -> Option<(&str, bool)> {
    let (name, flag) = std::str::from_utf8(value).ok()?.split_once('\0')?;
    match flag {
        "true" => Some((name, true)),
        "false" => Some((name, false)),
        _ => None,
    }
}

/// Conventional L3 metadata key holding a Terminal's freeform string tags
/// (ADR-0027 decision point 4, `phux-p0yq`).
///
/// Scope: the Terminal's [`ResourceId`](crate::ids::ResourceId). Value: a UTF-8 JSON array of
/// non-empty, duplicate-free tag strings, e.g. `["build","ci"]`; an empty
/// array or an absent key both mean "no tags". The server stores the bytes
/// without interpreting them ([`docs/spec/L3.md`](../../../docs/spec/L3.md)
/// §3.6); tag *meaning* is the normative client convention this key names, so
/// the `#tag` selector ([ADR-0027](../../../ADR/0027-terminal-references-and-l3-links.md)
/// decision point 5) resolves identically across consumers. Set via
/// `SET_METADATA`, read via `GET_METADATA`/`LIST_METADATA`.
pub const RESOURCE_TAGS_KEY: &str = "phux.tags/v1";

/// Conventional L3 metadata key holding a Terminal's outgoing *link* edges
/// (ADR-0027 decision point 4, `phux-p0yq`).
///
/// Scope: the source Terminal's [`ResourceId`](crate::ids::ResourceId). Value: a UTF-8 JSON array of
/// link records `{ "target": u32, "kind": str }`, where `target` is the
/// linked Terminal's local wire id and `kind` is an OPEN enum — v1 defines
/// `"group"` (a soft grouping edge); unknown kinds are preserved, not
/// rejected, so the vocabulary grows additively. The server stores the bytes
/// opaquely; link *meaning* is the normative client convention this key
/// names. A link is a metadata value, never a second wire identity
/// ([ADR-0027](../../../ADR/0027-terminal-references-and-l3-links.md)).
pub const RESOURCE_LINK_KEY: &str = "phux.link/v1";

/// Conventional L3 metadata key holding a Terminal's declared agent
/// identity and lifecycle record (ADR-0040, `phux-3ert`).
///
/// Scope: the Terminal the agent runs in. Value: a UTF-8 JSON object
/// `{ "name": str, "kind"?: str, "state"?: str, "attention"?: str,
/// "session"?: str }` where `state` and `attention` are OPEN string enums —
/// a consumer reading an unrecognized value treats it as `unknown` /
/// `normal` rather than failing the parse. The server stores the bytes
/// without interpreting them ([`docs/spec/L3.md`](../../../docs/spec/L3.md)
/// §3.7); the *schema* is the normative client convention this key names, so
/// a record written by one integration (CLI, plugin, provider hook) reads
/// identically in every consumer. A consumer that finds this record MUST
/// prefer it over OSC-title or screen-scrape heuristics; heuristics remain
/// the fallback when the key is absent. Set via `SET_METADATA`, cleared via
/// `DELETE_METADATA`, observed via `GET_METADATA`/`SUBSCRIBE_METADATA`.
pub const RESOURCE_AGENT_KEY: &str = "phux.agent/v1";

/// Conventional Terminal-scoped provenance for provider-native session resume.
///
/// Value: bounded UTF-8 JSON `{plugin_id, integration_id, native_id}` owned by
/// ADR-0068. `SPAWN_RESOURCE.agent_session` may install these opaque bytes
/// atomically with a local spawn; ordinary L3 reads and writes use this key.
pub const RESOURCE_AGENT_SESSION_KEY: &str = "phux.agent-session/v1";

/// Conventional Terminal-scoped metadata key for the server-observed
/// foreground process and available-shell answer. Clients may read and
/// subscribe to this key but MUST NOT set or delete it.
pub const RESOURCE_PANE_OCCUPANT_KEY: &str = "phux.pane-occupant/v1";

/// Maximum encoded `phux.agent-session/v1` record accepted by server mutations.
pub const MAX_AGENT_SESSION_RECORD_BYTES: usize = 4 * 1024;

/// Conventional L3 metadata key acting as the config-reload doorbell for
/// attached consumers (phux-foz.5).
///
/// Scope: [`Scope::Global`]. Value: an opaque, writer-chosen nonce (the
/// reference CLI writes a UTF-8 `unix-nanos-pid` string); its only job is
/// to DIFFER from the previous value so the server's equal-bytes SET
/// dedup does not swallow the broadcast. The server stores the bytes
/// without interpreting them. A consumer subscribed to this key treats a
/// non-tombstone `METADATA_CHANGED` as "re-read your local config now":
/// it re-runs its own layered config load and rebuilds its config-derived
/// state in place. The config itself NEVER crosses the wire — each
/// consumer reads its own file — and a consumer whose re-read fails MUST
/// keep its previous configuration (surface the error locally, never
/// half-apply). Tombstones (`DELETE_METADATA`) are ignored. Set via
/// `SET_METADATA`, observed via `SUBSCRIBE_METADATA`. Backs
/// `phux config reload` and the TUI `reload-config` action.
pub const CONFIG_RELOAD_KEY: &str = "phux.config.reload/v1";

/// Discriminant for `METADATA_VALUE` (server to client, `docs/spec/L3.md` §1).
///
/// Reply frame for `GET_METADATA`; correlated by `request_id`. Carries
/// `Option<bytes>` — `Some(bytes)` when the key holds a value,
/// `None` when the key is absent. Allocated by phux-4li.8.
pub const TYPE_METADATA_VALUE: u8 = FrameType::MetadataValue as u8;

/// Discriminant for `METADATA_KEYS` (server to client, `docs/spec/L3.md` §1).
///
/// Reply frame for `LIST_METADATA`; correlated by `request_id`. Carries
/// the lexicographically sorted list of key names present in the
/// requested scope (values are not included; LIST is by-key-name only).
/// Allocated by phux-4li.8.
pub const TYPE_METADATA_KEYS: u8 = FrameType::MetadataKeys as u8;

/// Discriminant for `LIST_DIRECTORY` (client to server, `docs/spec/L3.md` §4).
///
/// A host query: list the child directories of a path on the serving host.
/// Allocated from the open `0x55..=0x5F` tail of the L3 C→S block; gated on
/// [`ServerFeature::ListDirectory`](crate::caps::ServerFeature::ListDirectory).
pub const TYPE_LIST_DIRECTORY: u8 = FrameType::ListDirectory as u8;

/// Discriminant for `DIRECTORY_LISTING` (server to client, `docs/spec/L3.md` §4).
///
/// Reply frame for `LIST_DIRECTORY`; correlated by `request_id`. Carries
/// either the listing or a typed refusal.
pub const TYPE_DIRECTORY_LISTING: u8 = FrameType::DirectoryListing as u8;

// -----------------------------------------------------------------------------
// L1 Terminal lifecycle frame discriminants — SPEC §7.2 / §10.1 (phux-4li.10).
//
// Allocates the SPAWN / CLOSED / RESIZE wire-frames needed to lift split-pane
// and kill-pane out of the `phux-4li.5` warn+bell stubs and to drive per-pane
// `ioctl(TIOCSWINSZ)` from the post-SIGWINCH ReflowDiff (phux-4li.9). The
// server-side handler + client-side emission land in follow-up tickets.
//
// C→S allocations slot into `0x22..=0x23`, the first free pair after
// VIEWPORT_RESIZE (`0x20`) / FRAME_ACK (`0x21`). The 0x14..=0x1F
// hot-path reservation in Appendix B is preserved by skipping past it.
// S→C allocations honour the spec-only reservations carried in SPEC §7.2
// (`0xA1 RESOURCE_CLOSED`) and extend by one (`0xA2 RESOURCE_SPAWNED`)
// for the dedicated SPAWN reply — see SPEC Appendix C for the
// 0.2.0-draft.2 entry.
// -----------------------------------------------------------------------------

/// Discriminant for `SPAWN_RESOURCE` (client to server, `docs/spec/L1.md` §1 / §10.1).
///
/// Carries `{ request_id, group, command: Option<list<str>>,
/// cwd: Option<str>, env: Option<list<(str, str)>> }`. The reply rides on
/// [`TYPE_RESOURCE_SPAWNED`] correlated by `request_id`.
pub const TYPE_SPAWN_RESOURCE: u8 = FrameType::SpawnResource as u8;
/// Discriminant for `RESIZE_TERMINAL` (client to server, `docs/spec/L1.md` §1 / §10.2).
///
/// Per-Terminal PTY resize. Drives `ioctl(TIOCSWINSZ)` server-side; the
/// outer-viewport `VIEWPORT_RESIZE` (`0x20`) remains the
/// minimum-bounding-box signal. Both flow from a single SIGWINCH on the
/// client (phux-4li.9).
pub const TYPE_RESIZE_TERMINAL: u8 = FrameType::ResizeTerminal as u8;

// ---------------------------------------------------------------------------
// `0x24..=0x29` C→S and `0xA3..=0xA7` S→C stay unallocated. The PTY-less
// process family and the port-forward family once pencilled into them are
// subsumed by `ResourceKind` (`docs/spec/appendix-reserved.md` §1): a served
// thing that is not a Terminal is a kind spoken through the existing spawn /
// output / close frames, not a parallel frame family.
// ---------------------------------------------------------------------------

/// Discriminant for `MOVE_RESOURCE` (client to server, `docs/spec/L1.md`
/// §1 / §10.1; ADR-0056).
///
/// Re-parents a live Terminal into the window that currently owns
/// `owner_terminal` — possibly in a different session — without touching
/// its process, PTY, scrollback, or metadata. Ownership addressing only,
/// exactly as `SPAWN_RESOURCE.owner_terminal`: no split direction, ratio,
/// or focus. The reply rides on [`TYPE_RESOURCE_MOVED`] correlated by
/// `request_id`. Gated on the `MOVE_RESOURCE` server feature bit
/// (`crate::caps::MOVE_RESOURCE`); allocated at `0x2A`, past the
/// unallocated `0x24..=0x29` block.
pub const TYPE_MOVE_RESOURCE: u8 = FrameType::MoveResource as u8;
/// Discriminant for `RESOURCE_MOVED` (server to client, `docs/spec/L1.md`
/// §1 / §10.1; ADR-0056).
///
/// Reply frame for `MOVE_RESOURCE`; correlated by `request_id`. Carries a
/// `Result<ResourceId, MoveError>` tagged union — see [`MoveResult`].
/// Allocated at `0xA8`, past the unallocated `0xA3..=0xA7` block.
pub const TYPE_RESOURCE_MOVED: u8 = FrameType::ResourceMoved as u8;

/// Discriminant for `RESOURCE_CLOSED` (server to client, `docs/spec/L1.md` §1 / §10.1).
///
/// Push notification when a Terminal's PTY exits, naturally or via
/// `KILL_RESOURCE`. Honours the spec-only reservation at `0xA1`.
pub const TYPE_RESOURCE_CLOSED: u8 = FrameType::ResourceClosed as u8;
/// Discriminant for `RESOURCE_SPAWNED` (server to client, `docs/spec/L1.md` §1 / §10.1).
///
/// Reply frame for `SPAWN_RESOURCE`; correlated by `request_id`. Carries a
/// `Result<ResourceId, SpawnError>` tagged union — see [`SpawnResult`].
pub const TYPE_RESOURCE_SPAWNED: u8 = FrameType::ResourceSpawned as u8;

// Wire tags for the `SpawnResult` tagged union (SPEC §7.2 / §10.1).
//
// Convention: `Ok = 0x00`, `Err = 0x01` — established here by phux-4li.10
// and reusable by future `Result<T, E>`-shaped reply frames (e.g. when
// `COMMAND_RESULT` lands per SPEC §11). The convention deliberately
// matches the `Option` tag convention (`None = 0x00`, `Some = 0x01`) so
// hex-dump readers do not have to remember a second per-shape table.
/// Wire tag for [`SpawnResult::Ok`].
pub(crate) const SPAWN_RESULT_OK: u8 = 0;
/// Wire tag for [`SpawnResult::Err`].
pub(crate) const SPAWN_RESULT_ERR: u8 = 1;

// Wire tags for the `SpawnError` tagged union (SPEC §7.2 / §10.1).
/// Wire tag for [`SpawnError::GroupNotFound`].
pub(crate) const SPAWN_ERROR_TAG_GROUP_NOT_FOUND: u8 = 0;
/// Wire tag for [`SpawnError::SpawnFailed`].
pub(crate) const SPAWN_ERROR_TAG_SPAWN_FAILED: u8 = 1;
/// Wire tag for [`SpawnError::UnsupportedSatelliteRoute`] (phux-v45.6).
pub(crate) const SPAWN_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE: u8 = 2;
/// Wire tag for [`SpawnError::SatelliteUnreachable`] (phux-v45.6).
pub(crate) const SPAWN_ERROR_TAG_SATELLITE_UNREACHABLE: u8 = 3;
/// Wire tag for [`SpawnError::UnsupportedKind`].
pub(crate) const SPAWN_ERROR_TAG_UNSUPPORTED_KIND: u8 = 4;
/// Wire tag for [`SpawnError::ParentNotFound`].
pub(crate) const SPAWN_ERROR_TAG_PARENT_NOT_FOUND: u8 = 5;
/// Wire tag for [`SpawnError::ParentKindMismatch`].
pub(crate) const SPAWN_ERROR_TAG_PARENT_KIND_MISMATCH: u8 = 6;

// Wire tags for the `MoveResult` / `MoveError` tagged unions (ADR-0056),
// following the `SpawnResult` convention above (`Ok = 0x00`, `Err = 0x01`).
/// Wire tag for [`MoveResult::Ok`].
pub(crate) const MOVE_RESULT_OK: u8 = 0;
/// Wire tag for [`MoveResult::Err`].
pub(crate) const MOVE_RESULT_ERR: u8 = 1;
/// Wire tag for [`MoveError::MoveFailed`].
pub(crate) const MOVE_ERROR_TAG_MOVE_FAILED: u8 = 0;
/// Wire tag for [`MoveError::UnsupportedSatelliteRoute`].
pub(crate) const MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE: u8 = 1;

// Wire tags for the `Scope` tagged union (SPEC §7.4 / §11.L3).
/// Wire tag for [`Scope::Resource`].
pub(crate) const SCOPE_TAG_RESOURCE: u8 = 0;
/// Wire tag for [`Scope::Group`].
pub(crate) const SCOPE_TAG_GROUP: u8 = 1;
/// Wire tag for [`Scope::Global`].
pub(crate) const SCOPE_TAG_GLOBAL: u8 = 2;

// -----------------------------------------------------------------------------
// Control-plane frame discriminants — SPEC §5 (phux-k61 / ADR-0021).
//
// The generic command envelope. `COMMAND` (C→S) carries a typed `Command`
// correlated by `request_id`; `COMMAND_RESULT` (S→C) carries the matching
// `CommandResult`. Allocated from the control-plane ranges reserved in
// Appendix B (`0x31..=0x3F` C→S, `0xC2..=0xCF` S→C; `0xC1` is ERROR).
// ADR-0021 routes the CLI control verbs (`ls`, `kill`) through this rather
// than minting per-verb frames.
// -----------------------------------------------------------------------------

/// Discriminant for `COMMAND` (client to server, `docs/spec/L1.md` §5).
pub const TYPE_COMMAND: u8 = FrameType::Command as u8;
/// Discriminant for `COMMAND_RESULT` (server to client, `docs/spec/L1.md` §5).
pub const TYPE_COMMAND_RESULT: u8 = FrameType::CommandResult as u8;

// -----------------------------------------------------------------------------
// Agent-event frame discriminants — SPEC §7.5 (phux-y2t / ADR-0022 'events').
//
// The push half of the agent surface: a client SUBSCRIBES to a stream of
// extensible tagged lifecycle/activity events, and the server PUSHES `EVENT`
// frames as those events occur. This is an *additive accelerator* of the
// CLI-side poll-floor `wait` (which already shipped over `GET_SCREEN`):
// conditions stay matched client-side; events just cut polling latency.
//
// Allocated from the events reserved ranges in Appendix B: `0x41..=0x4F`
// (C→S) and `0xB3..=0xBF` (S→C). `SUBSCRIBE_EVENTS` takes the first C→S
// slot; `EVENT` takes the first S→C slot.
// -----------------------------------------------------------------------------

/// Discriminant for `SUBSCRIBE_EVENTS` (client to server, `docs/spec/L1.md` §7.5).
pub const TYPE_SUBSCRIBE_EVENTS: u8 = FrameType::SubscribeEvents as u8;
/// Discriminant for `EVENT` (server to client, `docs/spec/L1.md` §7.5).
pub const TYPE_EVENT: u8 = FrameType::Event as u8;

// Wire tags for the `AgentEvent` tagged union (SPEC §7.5 / §10.3).
//
// Each event rides inside the `EVENT` frame as a `tag: u8` followed by a
// length-prefixed `body: bytes`. The length prefix is what makes the
// taxonomy forward-compatible: a decoder that does not recognise `tag`
// reads (and skips) the declared body length and yields
// [`AgentEvent::Unknown`], so a v0.2.x server may add event kinds without
// breaking an older client's frame parse. Tags are allocated sequentially.
/// Wire tag for [`AgentEvent::CommandStarted`].
pub(crate) const EVENT_TAG_COMMAND_STARTED: u8 = 0x00;
/// Wire tag for [`AgentEvent::CommandFinished`].
pub(crate) const EVENT_TAG_COMMAND_FINISHED: u8 = 0x01;
/// Wire tag for [`AgentEvent::TitleChanged`].
pub(crate) const EVENT_TAG_TITLE_CHANGED: u8 = 0x02;
/// Wire tag for [`AgentEvent::Bell`].
pub(crate) const EVENT_TAG_BELL: u8 = 0x03;
/// Wire tag for [`AgentEvent::ResourceSpawned`].
pub(crate) const EVENT_TAG_RESOURCE_SPAWNED: u8 = 0x04;
/// Wire tag for [`AgentEvent::ResourceClosed`].
pub(crate) const EVENT_TAG_RESOURCE_CLOSED: u8 = 0x05;
/// Wire tag for [`AgentEvent::Dirty`].
pub(crate) const EVENT_TAG_DIRTY: u8 = 0x06;
/// Wire tag for [`AgentEvent::Idle`].
pub(crate) const EVENT_TAG_IDLE: u8 = 0x07;
/// Wire tag for [`AgentEvent::TerminalControl`]. The supervisory broadcast
/// (ADR-0033): emitted to every subscriber whenever a Terminal's input lease
/// or process lifecycle changes — who holds the wheel, and `Running` /
/// `Frozen` / `Exited`. The live-dashboard signal and the seed of the
/// recorded audit trail.
pub(crate) const EVENT_TAG_TERMINAL_CONTROL: u8 = 0x08;
/// Wire tag for [`AgentEvent::Asked`]. Appended after `TERMINAL_CONTROL`'s
/// `0x08`; `ASKED` is an additive agent-surface event (phux-2sl6) that carries
/// an agent's pending human-answerable question so a projection consumer can
/// render the waiting prompt without re-deriving it from the grid. Its body
/// is field-tagged TLV (not positional) so the suggestion list and the
/// optional elapsed counter are additive and an older decoder skips the whole
/// event by its length prefix as [`AgentEvent::Unknown`].
pub(crate) const EVENT_TAG_ASKED: u8 = 0x09;
/// Wire tag for [`AgentEvent::CwdChanged`]. Appended after `ASKED`'s `0x09`
/// (phux-foz.4): the scoped Terminal's working directory changed. Sourced
/// server-side from the kernel cwd of the PTY child (the same query the
/// spawn-inheritance path uses), polled at OSC-133 prompt boundaries and
/// output-idle and coalesced on change. Backs the `cwd` status widget.
pub(crate) const EVENT_TAG_CWD_CHANGED: u8 = 0x0a;

/// Wire tag for one [`Command`] variant inside the `COMMAND` envelope
/// (SPEC §5.1), the same compile-time-unique discipline as [`FrameType`]:
/// `docs/spec/appendix-reserved.md` §2 is this enum's allocation registry,
/// and a duplicate tag is compile error E0081 rather than a silent protocol
/// break (phux-ke0c). The freed `0x0a` / `0x0b` slots stay out of the enum;
/// the const docs below keep their history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
enum CommandTag {
    AttachResource = 0x01,
    DetachResource = 0x02,
    KillResource = 0x03,
    GetState = 0x05,
    GetScreen = 0x07,
    RouteInput = 0x08,
    KillResources = 0x09,
    GetTerminalState = 0x0c,
    SubscribeResourceEvents = 0x0d,
    Upgrade = 0x0e,
    AcquireInput = 0x0f,
    ReleaseInput = 0x10,
    SignalTerminal = 0x11,
    ReportAsked = 0x12,
    DetachClients = 0x13,
    ApplyInput = 0x14,
    PutFile = 0x15,
    Shutdown = 0x16,
    ReportAgentState = 0x17,
    GetPerf = 0x18,
    Transcribe = 0x19,
    AppendResourceOutput = 0x1a,
    KillResourceIf = 0x1b,
}

// Wire tags for the `Command` tagged union (SPEC §5.1). Tags follow the
// spec catalog order so the allocation is stable as later verbs land:
// SPAWN=0x00, ATTACH_RESOURCE=0x01, DETACH_RESOURCE=0x02, KILL_RESOURCE=0x03,
// RESIZE_TERMINAL=0x04, GET_STATE=0x05, RUN_HOOK=0x06. v0.1 implements only
// KILL_RESOURCE and GET_STATE (ADR-0021 §3); the rest are reserved and
// decode as `UnknownEnumValue` until wired.
/// Wire tag for [`Command::AttachResource`], taking the `0x01` slot the
/// SPEC §5.1 catalog reserved for `ATTACH_RESOURCE` (phux-v45.7). The
/// per-Terminal output-subscription verb: it wires the caller to receive a
/// profile-selected bootstrap stream plus `RESOURCE_OUTPUT`.
/// The catalog's `role_policy` field is not yet
/// encoded — an absent policy decodes as
/// `RolePolicy { requested_role: PRIMARY, takeover: NEVER }` per SPEC §8.1,
/// which is exactly what this body-less-policy encoding means; the field
/// lands additively behind its own wire bump.
pub(crate) const COMMAND_TAG_ATTACH_RESOURCE: u8 = CommandTag::AttachResource as u8;
/// Wire tag for [`Command::DetachResource`], taking the `0x02` slot the
/// SPEC §5.1 catalog reserved for `DETACH_RESOURCE` (phux-v45.7). Drops the
/// caller's per-Terminal output subscription (the `ATTACH_RESOURCE`
/// counterpart) and its per-Terminal event-stream subscription; the
/// Terminal itself is unaffected.
pub(crate) const COMMAND_TAG_DETACH_RESOURCE: u8 = CommandTag::DetachResource as u8;
/// Wire tag for [`Command::KillResource`].
pub(crate) const COMMAND_TAG_KILL_RESOURCE: u8 = CommandTag::KillResource as u8;
/// Wire tag for [`Command::GetState`].
pub(crate) const COMMAND_TAG_GET_STATE: u8 = CommandTag::GetState as u8;
/// Wire tag for [`Command::GetScreen`]. Appended after `RUN_HOOK`'s
/// reserved `0x06` (SPEC §5.1 catalog order); `GET_SCREEN` is an additive
/// agent-surface command (ADR-0022 §5), not part of the original catalog.
pub(crate) const COMMAND_TAG_GET_SCREEN: u8 = CommandTag::GetScreen as u8;
/// Wire tag for [`Command::RouteInput`]. Appended after `GET_SCREEN`'s
/// `0x07`; `ROUTE_INPUT` is an additive agent-surface command (ADR-0022)
/// that delivers an already-built input event to a Terminal without an
/// attach, subscription, or resize — the write counterpart to the
/// side-effect-free `GET_SCREEN` read.
pub(crate) const COMMAND_TAG_ROUTE_INPUT: u8 = CommandTag::RouteInput as u8;
/// Wire tag for [`Command::KillResources`]. Reuses the `0x09` slot freed
/// when the L2 lifecycle verbs (`CREATE_SESSION` / `KILL_COLLECTION` /
/// `RENAME_SESSION`) were dissolved in the v0.3.0 "Option B" re-tier
/// (ADR-0019 / ADR-0027). `KILL_RESOURCES` is the one irreducible
/// multi-terminal op the dissolved `KILL_COLLECTION` left behind: it
/// destroys a *list* of Terminals atomically under the server's single
/// state lock — all-or-nothing for a local server — replacing the
/// per-session teardown verb with a pure L1 list operation. Grouping
/// (which Terminals form a "session") is now client logic over L3
/// metadata, so the server need only know the resolved ids. The reply
/// rides `COMMAND_RESULT { Ok }` (the async `RESOURCE_CLOSED` frames
/// confirm teardown), the same ack shape `KILL_RESOURCE` uses. Backs
/// `phux kill SESSION`.
pub(crate) const COMMAND_TAG_KILL_RESOURCES: u8 = CommandTag::KillResources as u8;
/// Wire tag for [`Command::GetTerminalState`]. Appended after
/// `RENAME_SESSION`'s `0x0b`; `GET_TERMINAL_STATE` is an additive
/// L2 Collection-aware query (ADR-0015 L2) that returns a comprehensive
/// snapshot of a Terminal's full state: grid, scrollback, cursor, shell
/// metadata, sequence number, and timestamp as a structured JSON
/// object (built server-side; see `handle_get_terminal_state` in
/// phux-server). Unlike `GET_SCREEN` (L1 raw
/// grid), this returns structured state suitable for agent polling and
/// change detection. The reply rides `COMMAND_RESULT { Ok_With(Json(..)) }`.
pub(crate) const COMMAND_TAG_GET_TERMINAL_STATE: u8 = CommandTag::GetTerminalState as u8;
pub(crate) const COMMAND_TAG_SUBSCRIBE_RESOURCE_EVENTS: u8 =
    CommandTag::SubscribeResourceEvents as u8;
/// Wire tag for [`Command::Upgrade`]. Appended after
/// `SUBSCRIBE_RESOURCE_EVENTS`'s `0x0d`; `UPGRADE` is an additive control
/// command (ADR-0032) that triggers a graceful in-place re-exec. It carries no
/// payload — the handoff state blob is built and passed server-side.
pub(crate) const COMMAND_TAG_UPGRADE: u8 = CommandTag::Upgrade as u8;
/// Wire tag for [`Command::AcquireInput`]. The first of the three
/// supervisory verbs (ADR-0033, "take the wheel + kill"): asserts an
/// exclusive input lease over a Terminal so a human/operator can seize the
/// stdin write path from whatever is driving the pane.
pub(crate) const COMMAND_TAG_ACQUIRE_INPUT: u8 = CommandTag::AcquireInput as u8;
/// Wire tag for [`Command::ReleaseInput`]. Drops the input lease held over a
/// Terminal, returning it to `Open` (any subscriber's input passes). ADR-0033.
pub(crate) const COMMAND_TAG_RELEASE_INPUT: u8 = CommandTag::ReleaseInput as u8;
/// Wire tag for [`Command::SignalTerminal`]. Delivers an explicit POSIX
/// signal (interrupt / freeze / resume / terminate / kill) to the process
/// group inside a Terminal — distinct from `KILL_RESOURCE`, which removes the
/// pane. The reversible `Freeze`/`Resume` brake lives here. ADR-0033.
pub(crate) const COMMAND_TAG_SIGNAL_TERMINAL: u8 = CommandTag::SignalTerminal as u8;
/// Wire tag for [`Command::ReportAsked`]. This is the opt-in agent hook
/// ingress for ADR-0036: configured integrations can report the same payload
/// as `AgentEvent::Asked` without writing terminal-title escape sequences.
/// The server validates and emits the normal `EVENT` frame; no new event kind
/// or consumer surface is introduced.
pub(crate) const COMMAND_TAG_REPORT_ASKED: u8 = CommandTag::ReportAsked as u8;
/// Wire tag for [`Command::DetachClients`]. Force-detaches the clients
/// attached to a session (or every attached client when the target session
/// is absent) from *outside* the attach UI — the `phux detach` verb. Unlike
/// `FrameKind::Detach`, which detaches the sending connection, this targets
/// other clients by session name.
pub(crate) const COMMAND_TAG_DETACH_CLIENTS: u8 = CommandTag::DetachClients as u8;
/// Wire tag for [`Command::ApplyInput`].
pub(crate) const COMMAND_TAG_APPLY_INPUT: u8 = CommandTag::ApplyInput as u8;
/// Maximum number of events in one [`Command::ApplyInput`] batch.
pub const MAX_APPLY_INPUT_EVENTS: usize = 256;
/// Maximum encoded bytes in the nested [`Command::ApplyInput`] command body.
pub const MAX_APPLY_INPUT_COMMAND_BODY: usize = 64 * 1024;
/// Wire tag for [`Command::PutFile`].
pub(crate) const COMMAND_TAG_PUT_FILE: u8 = CommandTag::PutFile as u8;
/// Wire tag for [`Command::Shutdown`]. Appended after `PUT_FILE`'s `0x15`;
/// `0x0a` and `0x0b` are freed-and-reserved and MUST NOT be reallocated
/// without a `minor` bump (`appendix-reserved.md`).
pub(crate) const COMMAND_TAG_SHUTDOWN: u8 = CommandTag::Shutdown as u8;
/// Wire tag for [`Command::ReportAgentState`].
pub(crate) const COMMAND_TAG_REPORT_AGENT_STATE: u8 = CommandTag::ReportAgentState as u8;
/// Wire tag for [`Command::GetPerf`]. Appended after `REPORT_AGENT_STATE`.
pub(crate) const COMMAND_TAG_GET_PERF: u8 = CommandTag::GetPerf as u8;
/// Wire tag for [`Command::Transcribe`]. Appended after `GET_PERF`.
pub(crate) const COMMAND_TAG_TRANSCRIBE: u8 = CommandTag::Transcribe as u8;
/// Wire tag for [`Command::AppendResourceOutput`]. Appended after
/// `TRANSCRIBE`; the producer verb of a producer-fed resource, gated on
/// `ServerFeature::ResourceKinds`.
pub(crate) const COMMAND_TAG_APPEND_RESOURCE_OUTPUT: u8 = CommandTag::AppendResourceOutput as u8;
/// Wire tag for [`Command::KillResourceIf`]. Appended after
/// `APPEND_RESOURCE_OUTPUT`; gated on `ServerFeature::ConditionalKill`
/// (ADR-0109). A new tag rather than a field on `KILL_RESOURCE`, so a peer
/// without the feature fails to decode it instead of killing unconditionally.
pub(crate) const COMMAND_TAG_KILL_RESOURCE_IF: u8 = CommandTag::KillResourceIf as u8;

// Wire tags for the `InputEvent` tagged union (ROUTE_INPUT arg). These
// mirror the four `INPUT_*` frame atoms (`docs/spec/input.md`).
/// Wire tag for [`InputEvent::Key`].
pub(crate) const INPUT_EVENT_TAG_KEY: u8 = 0x00;
/// Wire tag for [`InputEvent::Mouse`].
pub(crate) const INPUT_EVENT_TAG_MOUSE: u8 = 0x01;
/// Wire tag for [`InputEvent::Focus`].
pub(crate) const INPUT_EVENT_TAG_FOCUS: u8 = 0x02;
/// Wire tag for [`InputEvent::Paste`].
pub(crate) const INPUT_EVENT_TAG_PASTE: u8 = 0x03;
// 0x04 was the `Selection` input-event tag, removed in v0.5.0 (phux-q1ni).

// Wire tags for the `StateScope` tagged union (SPEC §5.1, GET_STATE arg).
/// Wire tag for [`StateScope::Server`].
pub(crate) const STATE_SCOPE_TAG_SERVER: u8 = 0x00;

// Wire tags for the `CommandResult` tagged union (SPEC §5).
/// Wire tag for [`CommandResult::Ok`].
pub(crate) const COMMAND_RESULT_TAG_OK: u8 = 0x00;
/// Wire tag for [`CommandResult::OkWith`].
pub(crate) const COMMAND_RESULT_TAG_OK_WITH: u8 = 0x01;
/// Wire tag for [`CommandResult::Error`].
pub(crate) const COMMAND_RESULT_TAG_ERROR: u8 = 0x02;

// Wire tags for the `CommandValue` tagged union (SPEC §5).
/// Wire tag for [`CommandValue::ResourceId`].
pub(crate) const COMMAND_VALUE_TAG_RESOURCE_ID: u8 = 0x00;
/// Wire tag for [`CommandValue::GroupId`].
pub(crate) const COMMAND_VALUE_TAG_GROUP_ID: u8 = 0x01;
/// Wire tag for [`CommandValue::State`].
pub(crate) const COMMAND_VALUE_TAG_STATE: u8 = 0x02;
/// Wire tag for [`CommandValue::Json`].
pub(crate) const COMMAND_VALUE_TAG_JSON: u8 = 0x03;
/// Wire tag for [`CommandValue::Bytes`].
pub(crate) const COMMAND_VALUE_TAG_BYTES: u8 = 0x04;
/// Wire tag for [`CommandValue::FileUpload`].
pub(crate) const COMMAND_VALUE_TAG_FILE_UPLOAD: u8 = 0x05;

// Wire tags for the `AttachTarget` tagged union (SPEC §13).
/// Wire tag for [`AttachTarget::Last`].
pub(crate) const ATTACH_TARGET_LAST: u8 = 0;
/// Wire tag for [`AttachTarget::ByName`].
pub(crate) const ATTACH_TARGET_BY_NAME: u8 = 1;
/// Wire tag for [`AttachTarget::ById`].
pub(crate) const ATTACH_TARGET_BY_ID: u8 = 2;
/// Wire tag for [`AttachTarget::CreateIfMissing`].
pub(crate) const ATTACH_TARGET_CREATE_IF_MISSING: u8 = 3;

// -----------------------------------------------------------------------------
// Compile-time uniqueness for the hand-allocated wire tags (phux-ke0c).
//
// The SPEC §7 message catalog and the §5.1 command tags take their values
// from the [`FrameType`] / [`CommandTag`] enums above, so rustc rejects a
// duplicate discriminant outright. The remaining `u8` tags mirror
// data-carrying tagged unions, which cannot be `#[repr(u8)]` casts; their
// consts keep literal values and are checked here instead. A duplicate
// inside any one of these families is the same protocol break as a
// duplicated frame type byte, and no decode match arm would catch it for a
// tag with no consumer yet. The `wire_tag_consts_are_uniqueness_checked`
// test below keeps every literal tag const in one of these lists.
// -----------------------------------------------------------------------------

/// Compile-time assertion that one family of wire tags has no duplicates.
const fn assert_unique_tags(tags: &[u8]) {
    let mut i = 0;
    while i < tags.len() {
        let mut j = i + 1;
        while j < tags.len() {
            assert!(
                tags[i] != tags[j],
                "duplicate wire tag in one tagged-union family"
            );
            j += 1;
        }
        i += 1;
    }
}

// `SpawnResult` result tags (SPEC §10.1).
const _: () = assert_unique_tags(&[SPAWN_RESULT_OK, SPAWN_RESULT_ERR]);

// `SpawnError` tags (SPEC §10.1).
const _: () = assert_unique_tags(&[
    SPAWN_ERROR_TAG_GROUP_NOT_FOUND,
    SPAWN_ERROR_TAG_SPAWN_FAILED,
    SPAWN_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE,
    SPAWN_ERROR_TAG_SATELLITE_UNREACHABLE,
    SPAWN_ERROR_TAG_UNSUPPORTED_KIND,
    SPAWN_ERROR_TAG_PARENT_NOT_FOUND,
    SPAWN_ERROR_TAG_PARENT_KIND_MISMATCH,
]);

// `MoveResult` result tags (ADR-0056).
const _: () = assert_unique_tags(&[MOVE_RESULT_OK, MOVE_RESULT_ERR]);

// `MoveError` tags (ADR-0056).
const _: () = assert_unique_tags(&[
    MOVE_ERROR_TAG_MOVE_FAILED,
    MOVE_ERROR_TAG_UNSUPPORTED_SATELLITE_ROUTE,
]);

// `Scope` tags (SPEC §7.4 / §11.L3).
const _: () = assert_unique_tags(&[SCOPE_TAG_RESOURCE, SCOPE_TAG_GROUP, SCOPE_TAG_GLOBAL]);

// `AgentEvent` event-kind tags (SPEC §7.5 / §10.3).
const _: () = assert_unique_tags(&[
    EVENT_TAG_COMMAND_STARTED,
    EVENT_TAG_COMMAND_FINISHED,
    EVENT_TAG_TITLE_CHANGED,
    EVENT_TAG_BELL,
    EVENT_TAG_RESOURCE_SPAWNED,
    EVENT_TAG_RESOURCE_CLOSED,
    EVENT_TAG_DIRTY,
    EVENT_TAG_IDLE,
    EVENT_TAG_TERMINAL_CONTROL,
    EVENT_TAG_ASKED,
    EVENT_TAG_CWD_CHANGED,
]);

// `InputEvent` tags (the `ROUTE_INPUT` argument, `docs/spec/input.md`).
const _: () = assert_unique_tags(&[
    INPUT_EVENT_TAG_KEY,
    INPUT_EVENT_TAG_MOUSE,
    INPUT_EVENT_TAG_FOCUS,
    INPUT_EVENT_TAG_PASTE,
]);

// `StateScope` tags (SPEC §5.1, `GET_STATE` argument).
const _: () = assert_unique_tags(&[STATE_SCOPE_TAG_SERVER]);

// `CommandResult` tags (SPEC §5).
const _: () = assert_unique_tags(&[
    COMMAND_RESULT_TAG_OK,
    COMMAND_RESULT_TAG_OK_WITH,
    COMMAND_RESULT_TAG_ERROR,
]);

// `CommandValue` tags (SPEC §5).
const _: () = assert_unique_tags(&[
    COMMAND_VALUE_TAG_RESOURCE_ID,
    COMMAND_VALUE_TAG_GROUP_ID,
    COMMAND_VALUE_TAG_STATE,
    COMMAND_VALUE_TAG_JSON,
    COMMAND_VALUE_TAG_BYTES,
    COMMAND_VALUE_TAG_FILE_UPLOAD,
]);

// `AttachTarget` tags (SPEC §13).
const _: () = assert_unique_tags(&[
    ATTACH_TARGET_LAST,
    ATTACH_TARGET_BY_NAME,
    ATTACH_TARGET_BY_ID,
    ATTACH_TARGET_CREATE_IF_MISSING,
]);

mod codec;
mod command;
mod command_codec;
mod directory;
mod kind;
mod payload;
mod status;
mod whoami;

pub use command::{
    AgentEvent, Command, CommandResult, CommandValue, ControlAction, FileUploadAck, InputMode,
    KillConditions, KillPrecondition, ReportedAgentState, ResourceEventType, ResourceLifecycle,
    StateScope, TerminalSignal,
};
pub use directory::{
    DirectoryEntry, DirectoryErrorCode, DirectoryListing, DirectoryListingError,
    DirectoryListingResult, MAX_DIRECTORY_ENTRIES,
};
pub use kind::FrameKind;
pub use payload::{
    AttachTarget, MoveError, MoveResult, Scope, SpawnError, SpawnResource, SpawnResult,
    ViewportInfo,
};
pub use status::{
    CloseReason, DetachReason, ErrorCode, ErrorScope, HistoryRejectionReason,
    HistoryTombstoneReason, TombstoneReason,
};
pub use whoami::{
    AUTH_ROUTE_BEARER_QUIC, AUTH_ROUTE_BEARER_WEBTRANSPORT, AUTH_ROUTE_BEARER_WSS,
    AUTH_ROUTE_LOOPBACK_QUIC, AUTH_ROUTE_LOOPBACK_WEBTRANSPORT, AUTH_ROUTE_LOOPBACK_WS,
    AUTH_ROUTE_SSH_STDIO, AUTH_ROUTE_UDS, ServingUser, SshClient, WHOAMI_KEY,
    WHOAMI_SCHEMA_VERSION, WhoamiRecord,
};

pub(in crate::wire) use codec::{
    decode_attach_target, decode_bootstrap_codec, decode_bootstrap_id, decode_bootstrap_profile,
    decode_bootstrap_stream_profile, decode_env, decode_focus_event, decode_key_event,
    decode_metadata_scope_key, decode_mouse_event, decode_move_result, decode_optional_u32,
    decode_paste_event, decode_scope, decode_server_instance, decode_spawn_result,
    decode_stream_id, decode_string_list, decode_terminal_id, decode_viewport_info,
    encode_attach_target, encode_bootstrap_codec, encode_bootstrap_profile, encode_env,
    encode_focus_event, encode_key_event, encode_mouse_event, encode_move_result,
    encode_paste_event, encode_scope, encode_server_instance, encode_spawn_result,
    encode_string_list, encode_terminal_id, encode_viewport_info,
};
pub(in crate::wire) use command_codec::{
    decode_agent_event, decode_command, decode_command_result, encode_agent_event, encode_command,
    encode_command_result,
};
pub(in crate::wire) use directory::{decode_directory_listing, decode_list_directory};

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    /// Every `u8` wire-tag const in this file is duplicate-proof at compile
    /// time. `TYPE_` and `COMMAND_TAG_` consts must take their value from
    /// the [`FrameType`] / [`CommandTag`] enums (a repeated enum
    /// discriminant is compile error E0081); every other tag const must
    /// keep a literal value only inside an [`assert_unique_tags`] family
    /// list. A tag added the old way, as a bare literal const, fails here
    /// instead of shipping a silent duplicate (phux-ke0c).
    #[test]
    fn wire_tag_consts_are_uniqueness_checked() {
        // Only the const-declaration half of the file is scanned; the test
        // module itself would otherwise match its own patterns.
        let src = include_str!("mod.rs");
        let src = src.split("#[cfg(test)]").next().unwrap_or(src);

        // First pass: collect the const names inside the assertion lists.
        // Lists may be single-line or multi-line after rustfmt, so scan
        // regions rather than lines.
        let mut asserted: BTreeSet<&str> = BTreeSet::new();
        let mut rest = src;
        while let Some(start) = rest.find("assert_unique_tags(&[") {
            let after = &rest[start + "assert_unique_tags(&[".len()..];
            let Some(end) = after.find("])") else {
                panic!("unterminated assert_unique_tags list");
            };
            for name in after[..end]
                .split(|c: char| !(c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'))
            {
                if name.chars().any(|c| c.is_ascii_uppercase()) {
                    asserted.insert(name);
                }
            }
            rest = &after[end + 2..];
        }
        assert!(
            !asserted.is_empty(),
            "no assert_unique_tags lists found - the compile-time checks are gone"
        );

        // Second pass: check every `u8` const declaration.
        for line in src.lines() {
            let line = line.trim();
            let decl = line
                .strip_prefix("pub const ")
                .or_else(|| line.strip_prefix("pub(crate) const "));
            let Some(decl) = decl else { continue };
            let Some((name, expr)) = decl.split_once(": u8 = ") else {
                continue;
            };
            let expr = expr.trim_end_matches(';').trim();
            if name.starts_with("TYPE_") {
                assert!(
                    expr.starts_with("FrameType::"),
                    "{name} must take its value from the FrameType enum so a duplicate \
                     discriminant is a compile error, found `{expr}`"
                );
            } else if name.starts_with("COMMAND_TAG_") {
                assert!(
                    expr.starts_with("CommandTag::"),
                    "{name} must take its value from the CommandTag enum so a duplicate \
                     discriminant is a compile error, found `{expr}`"
                );
            } else if expr.starts_with("0x") || expr.starts_with(|c: char| c.is_ascii_digit()) {
                assert!(
                    asserted.contains(name),
                    "{name} is a hand-allocated literal wire tag missing from an \
                     assert_unique_tags family list"
                );
            }
        }
    }
}
