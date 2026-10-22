//! The L3 metadata, L1 resource lifecycle, and command/event
//! [`FrameKind`] variants.

#[cfg(doc)]
use super::FrameKind;

/// Appends the L3 metadata, L1 resource, and command [`FrameKind`] variants to the list being
/// built and hands it to `$next`; see `frame_kind_enum!` in `kind.rs`.
macro_rules! resource_frames {
    ($next:ident!($($rest:tt)*) $($done:tt)*) => {
        $next!($($rest)* $($done)*
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

    /// Host-side one-level browse or recursive fuzzy path search (L3 §5).
    PathQuery {
        /// Correlates the reply.
        request_id: u32,
        /// Absolute directory, `~`, or `~/rest` on the selected host.
        root: String,
        /// Browse filter, or fuzzy search term when recursive.
        query: String,
        /// False for one-level browse, true for cross-directory search.
        recursive: bool,
        /// Satellite host, or serving host when absent.
        host: Option<SatelliteHost>,
    },

    /// Correlated path rows, status, or typed refusal (L3 §5).
    PathResults {
        /// Correlates a `PATH_QUERY`.
        request_id: u32,
        /// Successful search or refusal.
        result: PathQueryResult,
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
        /// Cell size in pixels `(width, height)`, fields 4/5 (ADR-0145).
        /// `None` keeps the Terminal's last cell size; a server that
        /// predates the fields skips them by length.
        cell_px: Option<(u16, u16)>,
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
        );
    };
}
