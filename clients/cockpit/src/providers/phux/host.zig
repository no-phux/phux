//! Owning-thread adapter for the stable phux client C ABI.
//!
//! `PhuxClient` and every pointer borrowed from it remain on the UI thread.
//! The socket worker only stages complete frames in `phux_transport.Bridge`.
//! Borrowed C grids are translated directly into the reusable final
//! `canvas.TerminalGrid` buffers; there is no second emulator or projection.

const std = @import("std");
const transport = @import("phux_transport");
const provider = @import("provider_contract");
const presentation_module = @import("presentation.zig");
const c = @import("abi.zig").c;
const operations = @import("operations.zig");
const workspace_bridge = @import("workspace_bridge.zig");
const agent_sessions = @import("agent_sessions.zig");

test {
    // Zig runs the tests of an imported file only where it is referenced
    // from a test block. The derivation lives there; without this its
    // suite sits in the graph and never runs.
    _ = agent_sessions;
}
const workspace = provider.workspace;
pub const OperationResult = operations.types.Result;
pub const test_support = @import("operation_test_support.zig");
pub const AgentSession = agent_sessions.Session;
pub const AgentState = agent_sessions.State;
pub const AgentRecordsKind = agent_sessions.RecordsKind;

pub const enabled = true;
pub const max_terminals: usize = workspace.max_replicas;
// Matches the ABI roster bound; independent from live engine replica slots.
pub const max_catalog_terminals: usize = workspace.max_terminals;
pub const max_notices: usize = 64;
pub const max_search_results: usize = 4096;
pub const max_sessions: usize = 256;
pub const max_title_bytes: usize = 4096;
pub const max_session_name_bytes: usize = 4096;
pub const max_notice_bytes: usize = 64 * 1024;
/// One live ResourceOutput, bounded by phux-protocol::wire::frame::MAX_FRAME_LEN.
/// The server stamps the whole append before broadcasting it, so its delivered
/// bytes can exceed the 64 KiB producer-input limit.
pub const max_agent_records_bytes: usize = 16 * 1024 * 1024;
/// Retained history uses phux-config::MAX_AGENT_LOG_BYTES, not the append cap.
/// The default is 4 MiB; a configured server can retain up to 64 MiB.
pub const max_agent_retained_bytes: usize = 64 * 1024 * 1024;
pub const max_agent_sessions: usize = agent_sessions.max_sessions;
/// Admission is bounded independently from one frame's text budget. The
/// painter degrades rows atomically; the provider retains the complete valid
/// viewport instead of disconnecting on ordinary dense Unicode content.
pub const max_cell_utf8_bytes = presentation_module.max_cell_utf8_bytes;
pub const max_grid_utf8_bytes = presentation_module.max_grid_utf8_bytes;

pub const State = enum { new, hello_queued, negotiated, attached, detached, failed };
pub const SyncDelta = struct {
    workspace_changed: bool = false,
    metadata_changed: bool = false,
    /// A go-to-directory listing settled (listed, refused or unknown) in
    /// this drain; the picker reads it on the next snapshot invalidation.
    directory_changed: bool = false,
    /// A standby's session query settled with a different list than before.
    sessions_listed: bool = false,
    /// A session rename moved the session list, or this client's own rename
    /// settled (renamed, refused or unknown). Never a sign of listing.
    sessions_renamed: bool = false,
    ready_published: bool = false,
    generation_changed: bool = false,
    detached: bool = false,
    added_count: usize = 0,
    removed_count: usize = 0,
};
pub const DocumentSpace = provider.DocumentSpace;
pub const DocumentPoint = provider.DocumentPoint;
pub const Anchor = struct { opaque_id: u64 = 0 };
pub const SearchResult = struct { start: Anchor, end: Anchor };
pub const NoticeKind = enum { status, job };
pub const Notice = struct {
    kind: NoticeKind,
    detail: u32,
    status_code: u32,
    terminal_ref: provider.TerminalRef,
    generation: provider.Generation,
    bytes: []u8,

    pub fn isBell(notice: Notice) bool {
        return notice.kind == .status and notice.detail == c.PHUX_CLIENT_STATUS_BELL;
    }
};
pub const SessionSummary = struct {
    id: u32,
    name: []u8,
    created_at_unix_secs: i64,
    window_count: u16,
    attached_client_count: u16,
    focused: bool,
    /// ADR-0105: the session survives its last window. Only a server that
    /// advertises KEEP_EMPTY_SESSIONS reports it (phux_client_session_flags).
    keep_empty: bool = false,
    /// A keep-empty session that holds no windows: a real, empty session,
    /// never a broken one.
    empty: bool = false,

    fn deinit(session: *SessionSummary, gpa: std.mem.Allocator) void {
        gpa.free(session.name);
    }
};
pub const Error = error{
    InvalidState,
    InvalidIdentity,
    Protocol,
    Engine,
    OutOfMemory,
    Panic,
    NoValue,
};

const RemoteId = provider.RemoteResourceId;
const CanvasStore = presentation_module.CanvasStore;
pub const ColorPolicy = @import("grid_metadata.zig").Policy;
pub const FrozenPresentation = @import("frozen_presentation.zig").FrozenPresentation;

const Terminal = struct {
    measured_cell: ?provider.MeasuredCell = null,
    id: RemoteId,
    generation: provider.Generation = .{},
    phase: provider.Phase = .attaching,
    dirty: bool = false,
    seen_in_attach: bool = false,
    remove_at_barrier: bool = false,
    published: bool = false,
    canvas: CanvasStore = .{},
    title: std.ArrayListUnmanaged(u8) = .empty,
    pending_title: std.ArrayListUnmanaged(u8) = .empty,
    pending_title_set: bool = false,
    cols: u16 = 0,
    rows: u16 = 0,
    history_total_rows: u64 = 0,
    history_viewport_offset: u64 = 0,
    history_visible_rows: u64 = 0,
    history_loading: bool = false,
    history_has_more: bool = false,
    history_pages_loaded: u64 = 0,
    history_unread_rows: u64 = 0,
    bell_owner: ?provider.ReplicaOwner = null,
    viewport: ?provider.Viewport = null,
    /// The coordinator this replica belongs to (`Host.provider_id` when it
    /// was admitted): part of every ref and owner it publishes.
    provider_id: provider.ProviderId = .phux,

    fn deinit(terminal: *Terminal, gpa: std.mem.Allocator) void {
        terminal.canvas.deinit(gpa);
        terminal.title.deinit(gpa);
        terminal.pending_title.deinit(gpa);
    }

    fn terminalRef(terminal: *const Terminal) provider.TerminalRef {
        return .{ .provider_id = terminal.provider_id, .terminal_id = .{ .phux = terminal.id } };
    }

    fn owner(terminal: *const Terminal) provider.ReplicaOwner {
        return .{ .terminal_ref = terminal.terminalRef(), .generation = terminal.generation };
    }

    fn presentation(terminal: *const Terminal) ?provider.Presentation {
        if (!terminal.published) return null;
        return .{
            .measured_cell = terminal.measured_cell,
            .grid = terminal.canvas.grid(terminal.phase == .live),
            .owner = terminal.owner(),
            .phase = terminal.phase,
            .title = terminal.title.items,
            .cols = terminal.cols,
            .rows = terminal.rows,
            .history_total_rows = terminal.history_total_rows,
            .history_viewport_offset = terminal.history_viewport_offset,
            .history_visible_rows = terminal.history_visible_rows,
            .history_loading = terminal.history_loading,
            .history_has_more = terminal.history_has_more,
            .history_pages_loaded = terminal.history_pages_loaded,
            .history_unread_rows = terminal.history_unread_rows,
        };
    }
};

fn markGridDirty(terminal: *Terminal, attached: bool) void {
    terminal.dirty = true;
    terminal.seen_in_attach = true;
    terminal.remove_at_barrier = false;
    if (attached and terminal.published) terminal.phase = .frozen;
}

fn publishTitle(terminal: *Terminal) void {
    if (!terminal.pending_title_set) return;
    terminal.title.items.len = terminal.pending_title.items.len;
    @memcpy(terminal.title.items, terminal.pending_title.items);
    terminal.pending_title.items.len = 0;
    terminal.pending_title_set = false;
}

pub const Host = struct {
    context_id: u64,
    gpa: std.mem.Allocator,
    client: *c.PhuxClient,
    bridge: *transport.Bridge,
    terminals: std.ArrayListUnmanaged(Terminal) = .empty,
    sessions: std.ArrayListUnmanaged(SessionSummary) = .empty,
    /// Provenance of the retained session catalog, not the connection currently
    /// handshaking. Reconnect must never requalify old rows with its new epoch.
    sessions_generation: u64 = 0,
    search_results: std.ArrayListUnmanaged(SearchResult) = .empty,
    search_owner: ?provider.ReplicaOwner = null,
    notices: std.ArrayListUnmanaged(Notice) = .empty,
    attach_barrier_seen: bool = false,
    client_generation: u64 = 1,
    operation_ledger: operations.Ledger(max_terminals) = .{},
    disconnected: bool = false,
    color_policy: ColorPolicy = .{},
    metadata_changed: bool = false,
    workspace_changed: bool = false,
    workspace_store: workspace_bridge.Store = .{},
    /// Agent sessions from the resource catalog, with the state their record
    /// streams declare. Never a replica: see agent_sessions.zig.
    agents: agent_sessions.Registry = .{},
    attached_session_id: ?u32 = null,
    /// The rename state last read from this connection's client: its
    /// session-list revision and status. A new client starts from zero.
    rename_revision_seen: u64 = 0,
    rename_status_seen: RenameStatus = .none,
    /// The coordinator this host is connected to (contract.phuxCoordinatorId).
    /// Every ref it mints carries it, and every ref it is handed must carry
    /// it: another coordinator's terminal 7 is not this one's, so a ref that
    /// names another coordinator is refused rather than routed here.
    provider_id: provider.ProviderId = .phux,

    /// A retarget clears every replica first, so no terminal keeps the
    /// identity of the coordinator it left. Any replica still held is
    /// restamped anyway: a host's terminals always carry its own id.
    pub fn setProviderId(host: *Host, id: provider.ProviderId) void {
        host.provider_id = id;
        host.agents.provider_id = id;
        host.workspace_store.provider_id = id;
        for (host.terminals.items) |*terminal| terminal.provider_id = id;
    }

    /// The replica identity a ref names on THIS coordinator, or null for a
    /// local PTY or another coordinator's terminal.
    fn remoteFromRef(host: *const Host, terminal_ref: provider.TerminalRef) ?RemoteId {
        if (terminal_ref.provider_id != host.provider_id) return null;
        return switch (terminal_ref.terminal_id) {
            .phux => |id| id,
            .local => null,
        };
    }

    fn refFor(host: *const Host, id: RemoteId) provider.TerminalRef {
        return .{ .provider_id = host.provider_id, .terminal_id = .{ .phux = id } };
    }

    /// Presentation-only update: no input, render query or replica mutation.
    pub fn setColorPolicy(host: *Host, policy: ColorPolicy) void {
        if (std.meta.eql(host.color_policy, policy)) return;
        host.color_policy = policy;
        for (host.terminals.items) |*terminal| terminal.canvas.setColorPolicy(policy);
    }

    pub fn create(gpa: std.mem.Allocator, bridge: *transport.Bridge) !*Host {
        const host = try gpa.create(Host);
        errdefer gpa.destroy(host);
        const context_id = try provider.context.allocate();
        host.* = .{ .gpa = gpa, .client = try newClient(), .bridge = bridge, .context_id = context_id };
        return host;
    }

    pub fn destroy(host: *Host) void {
        host.agents.deinit(host.gpa);
        host.workspace_store.deinit(host.gpa);
        host.clearSearchResults(null);
        for (host.terminals.items) |*terminal| terminal.deinit(host.gpa);
        host.terminals.deinit(host.gpa);
        host.clearSessions();
        host.sessions.deinit(host.gpa);
        for (host.notices.items) |notice| host.gpa.free(notice.bytes);
        host.notices.deinit(host.gpa);
        host.search_results.deinit(host.gpa);
        c.phux_client_free(host.client);
        host.gpa.destroy(host);
    }

    pub fn state(host: *const Host) State {
        return switch (c.phux_client_state(host.client)) {
            c.PHUX_CLIENT_STATE_NEW => .new,
            c.PHUX_CLIENT_STATE_HELLO_QUEUED => .hello_queued,
            c.PHUX_CLIENT_STATE_NEGOTIATED => .negotiated,
            c.PHUX_CLIENT_STATE_ATTACHED => .attached,
            c.PHUX_CLIENT_STATE_DETACHED => .detached,
            else => .failed,
        };
    }

    pub fn start(host: *Host, client_name: []const u8) !void {
        try outboundSize(client_name.len);
        try resultError(c.phux_client_queue_hello(host.client, bytes(client_name)));
        // Renames any client makes reach this connection's session list from
        // the start (the switcher rows and the header follow them), not only
        // after this client's own first rename. The client subscribes right
        // after HELLO_OK: a read-only SUBSCRIBE_METADATA, never an ATTACH, so
        // a listing connection follows renames without holding a viewport.
        try resultError(c.phux_client_follow_session_names(host.client));
        try host.stageOutgoing();
    }

    /// Borrowed until the next mutable host call; server IDs are opaque bytes.
    pub fn serverId(host: *const Host) ?[]const u8 {
        var raw: c.PhuxBytes = undefined;
        if (c.phux_client_server_id(host.client, &raw) != c.PHUX_CLIENT_OK) return null;
        return effectSlice(raw) catch null;
    }

    pub fn connectionEpoch(host: *const Host) u64 {
        return host.client_generation;
    }

    pub fn takeOperationResult(host: *Host) ?OperationResult {
        return host.operation_ledger.take();
    }

    pub fn workspaceSnapshot(host: *const Host) workspace.Snapshot {
        return host.workspace_store.snapshot();
    }

    pub fn catalogTerminals(host: *const Host) []const workspace.CatalogTerminal {
        return host.workspace_store.catalog;
    }

    pub fn terminalSession(host: *const Host, ref: provider.TerminalRef) ?u32 {
        return host.workspace_store.terminalSession(ref);
    }

    pub fn selectedSessionId(host: *const Host) ?u32 {
        return host.attached_session_id;
    }

    pub fn requestWorkspaceRefresh(host: *Host) !?u32 {
        try host.requireAttached();
        if (host.workspace_store.info.status == .pending) return null;
        const request_id = try host.operation_ledger.nextRequestId();
        try resultError(c.phux_client_workspace_refresh(host.client, request_id));
        host.workspaceAccepted(request_id);
        return request_id;
    }

    pub fn requestWorkspaceMutation(host: *Host, value: workspace.Mutation) !u32 {
        try host.requireAttached();
        const request_id = try host.operation_ledger.nextRequestId();
        // Copy the bounded name as callers may pass our current snapshot bytes.
        const name = try workspace.Text.init(value.name);
        var owned = value;
        owned.name = name.slice();
        const raw = try workspace_bridge.mutation(&owned, request_id, host.provider_id);
        try resultError(c.phux_client_workspace_mutate(host.client, &raw));
        host.workspaceAccepted(request_id);
        return request_id;
    }

    fn workspaceAccepted(host: *Host, request_id: u32) void {
        host.operation_ledger.last_id = request_id;
        host.captureWorkspace();
        // A local copy refusal must still correlate with the accepted request.
        host.workspace_store.info.request_id = request_id;
        host.stageOutgoing() catch host.disconnect();
    }

    fn captureWorkspace(host: *Host) void {
        const previous_revision = host.workspace_store.info.revision;
        const changed = host.copyWorkspace() catch |err| {
            host.workspace_store.refuse(err);
            host.workspace_changed = true;
            return;
        };
        if (!changed) return;
        host.workspace_changed = true;
        if (previous_revision == host.workspace_store.info.revision) return;
        host.refreshSessions() catch |err| host.workspace_store.refuse(err);
    }

    fn copyWorkspace(host: *Host) !bool {
        var raw = workspace_bridge.record(c.PhuxWorkspaceInfo);
        try resultError(c.phux_client_workspace_info(host.client, &raw));
        // Attached identity remains authoritative even if a roster conversion
        // is refused; global GET_STATE focus is never a session-switch signal.
        if (raw.session_id != 0) host.attached_session_id = raw.session_id;
        return host.workspace_store.captureInfo(host.gpa, host.client, raw);
    }

    /// Queue acceptance returns an ID even if transport staging then fails.
    /// That accepted operation becomes unknown, never an implicit spawn retry.
    /// Owners retain their exact terminal identity, including satellite host;
    /// satellite routing is explicit and matches that owner's host.
    pub fn requestSpawn(host: *Host, owner_ref: ?provider.TerminalRef, viewport: provider.Viewport) !u32 {
        return host.requestSpawnIn(owner_ref, viewport, &.{});
    }

    /// Go to Directory's listing, retained by the client: at most one
    /// request, and a reply to a superseded request never surfaces.
    pub const DirectoryStatus = enum(u32) { none = 0, pending = 1, listed = 2, refused = 3, unknown_outcome = 4, _ };
    pub const DirectoryInfo = struct {
        supported: bool = false,
        status: DirectoryStatus = .none,
        request_id: u32 = 0,
        error_code: u32 = 0,
        truncated: bool = false,
        entry_count: usize = 0,
        path: []const u8 = "",
        parent: ?[]const u8 = null,
        message: []const u8 = "",
    };
    pub const DirectoryEntry = struct { name: []const u8, symlink: bool };

    /// `path` must not borrow client storage: this is a mutable client call.
    pub fn requestDirectory(host: *Host, path: []const u8) !u32 {
        return host.requestDirectoryOn(path, "");
    }

    /// The listing on `satellite`, a satellite of the attached hub, or on
    /// the serving host when empty. The client refuses a satellite unless
    /// HELLO_OK advertised LIST_DIRECTORY_HOST, since an older hub would
    /// list itself; nothing is queued then. Neither span may borrow client
    /// storage.
    pub fn requestDirectoryOn(host: *Host, path: []const u8, satellite: []const u8) !u32 {
        try host.requireAttached();
        const request_id = try host.operation_ledger.nextRequestId();
        const request: c.PhuxDirectoryRequest = .{
            .size = @sizeOf(c.PhuxDirectoryRequest),
            .version = c.PHUX_CLIENT_ABI_VERSION,
            .request_id = request_id,
            .path = bytes(path),
            .host = bytes(satellite),
        };
        try resultError(c.phux_client_list_directory_on(host.client, &request));
        host.operation_ledger.last_id = request_id;
        host.stageOutgoing() catch host.disconnect();
        return request_id;
    }

    /// Whether the connected hub lists a named satellite's directories.
    pub fn directoryHostSupported(host: *const Host) bool {
        var supported = false;
        if (c.phux_client_directory_host_supported(host.client, &supported) != c.PHUX_CLIENT_OK) return false;
        return supported;
    }

    /// Borrowed until the next mutable host call.
    pub fn directoryInfo(host: *const Host) DirectoryInfo {
        var raw = std.mem.zeroes(c.PhuxDirectoryListingInfo);
        raw.size = @sizeOf(c.PhuxDirectoryListingInfo);
        raw.version = c.PHUX_CLIENT_ABI_VERSION;
        if (c.phux_client_directory_info(host.client, &raw) != c.PHUX_CLIENT_OK) return .{};
        return .{
            .supported = raw.supported,
            .status = @enumFromInt(raw.status),
            .request_id = raw.request_id,
            .error_code = raw.error_code,
            .truncated = raw.truncated,
            .entry_count = raw.entry_count,
            .path = effectSlice(raw.path) catch "",
            .parent = if (raw.has_parent) effectSlice(raw.parent) catch null else null,
            .message = effectSlice(raw.message) catch "",
        };
    }

    /// Borrowed until the next mutable host call.
    pub fn directoryEntry(host: *const Host, index: usize) ?DirectoryEntry {
        var raw = std.mem.zeroes(c.PhuxDirectoryEntry);
        raw.size = @sizeOf(c.PhuxDirectoryEntry);
        raw.version = c.PHUX_CLIENT_ABI_VERSION;
        if (c.phux_client_directory_entry_get(host.client, index, &raw) != c.PHUX_CLIENT_OK) return null;
        const name = effectSlice(raw.name) catch return null;
        return .{ .name = name, .symlink = raw.flags & c.PHUX_DIRECTORY_ENTRY_SYMLINK != 0 };
    }

    fn directoryStatusRaw(host: *const Host) DirectoryStatus {
        return host.directoryInfo().status;
    }

    /// `cwd` empty inherits the server's default; otherwise the shell starts
    /// there. Copied by the queue call.
    pub fn requestSpawnIn(host: *Host, owner_ref: ?provider.TerminalRef, viewport: provider.Viewport, cwd: []const u8) !u32 {
        return host.spawnWith(owner_ref, viewport, cwd, false);
    }

    /// A spawn bound to the server's instance token (ADR-0109), so a later
    /// conditional kill can name exactly this terminal. Refused by a server
    /// without CONDITIONAL_KILL; ask `conditionalKillSupported` first.
    pub fn requestSpawnBound(host: *Host, owner_ref: ?provider.TerminalRef, viewport: provider.Viewport, cwd: []const u8) !u32 {
        return host.spawnWith(owner_ref, viewport, cwd, true);
    }

    pub fn conditionalKillSupported(host: *const Host) bool {
        var supported = false;
        if (c.phux_client_conditional_kill_supported(host.client, &supported) != c.PHUX_CLIENT_OK) return false;
        return supported;
    }

    /// KILL_RESOURCE_IF for a terminal this coordinator spawned and bound:
    /// killed only if its instance token still matches and no connection
    /// but the spawning one has attached or used it. Never unconditional.
    /// A listing connection may send it; it attaches nothing.
    pub fn requestKillIf(host: *Host, terminal_ref: provider.TerminalRef, instance: [16]u8) !u32 {
        if (host.bridge.incoming.takeDisconnect() != null) host.disconnect();
        if (host.disconnected) return error.InvalidState;
        const now = host.state();
        if (now != .negotiated and now != .attached) return error.InvalidState;
        const remote = host.remoteFromRef(terminal_ref) orelse return error.InvalidIdentity;
        const request_id = try host.operation_ledger.nextId();
        const raw = cId(&remote);
        try resultError(c.phux_client_queue_kill_if(host.client, request_id, &raw, &instance));
        host.operation_ledger.accepted(request_id, host.client_generation, .kill_if, terminal_ref);
        host.stageOutgoing() catch host.disconnect();
        return request_id;
    }

    fn spawnWith(host: *Host, owner_ref: ?provider.TerminalRef, viewport: provider.Viewport, cwd: []const u8, bound: bool) !u32 {
        const request_id = try host.preflightOperation();
        try host.reserveTerminalSlot(null);
        const owner_id = try host.spawnOwner(owner_ref);
        const raw_owner = if (owner_id) |id| cId(id) else null;
        const satellite = if (owner_id) |id| id.host() else &.{};
        const options: c.PhuxSpawnOptions = .{
            .size = @sizeOf(c.PhuxSpawnOptions),
            .version = c.PHUX_CLIENT_ABI_VERSION,
            .request_id = request_id,
            .owner_terminal = if (raw_owner) |*id| id else null,
            .satellite = bytes(satellite),
            .argv = null,
            .argc = 0,
            .cwd = bytes(cwd),
            .cols = viewport.cols,
            .rows = viewport.rows,
        };
        if (bound)
            try resultError(c.phux_client_queue_spawn_bound(host.client, &options))
        else
            try resultError(c.phux_client_queue_spawn(host.client, &options));
        host.operation_ledger.accepted(request_id, host.client_generation, .spawn, null);
        host.stageOutgoing() catch host.disconnect();
        return request_id;
    }

    pub fn requestAttach(host: *Host, terminal_ref: provider.TerminalRef) !u32 {
        const request_id = try host.preflightOperation();
        const remote = host.remoteFromRef(terminal_ref) orelse return error.InvalidIdentity;
        const raw = cId(&remote);
        _ = try remoteFromC(raw);
        if (remote.id == 0) return error.InvalidIdentity;
        if (host.findTerminal(terminal_ref) == null) try host.reserveTerminalSlot(terminal_ref);
        const options: c.PhuxAttachResourceOptions = .{
            .size = @sizeOf(c.PhuxAttachResourceOptions),
            .version = c.PHUX_CLIENT_ABI_VERSION,
            .request_id = request_id,
            .terminal_id = raw,
        };
        try resultError(c.phux_client_queue_attach_resource(host.client, &options));
        host.operation_ledger.accepted(request_id, host.client_generation, .attach, terminal_ref);
        // Capacity was reserved before queueing. A placeholder counts against
        // the terminal limit but stays invisible until the stream is READY.
        host.admitOperationTerminal(remote);
        host.stageOutgoing() catch host.disconnect();
        return request_id;
    }

    fn preflightOperation(host: *Host) !u32 {
        try host.requireAttached();
        return host.operation_ledger.nextId();
    }

    fn requireAttached(host: *Host) !void {
        if (host.bridge.incoming.takeDisconnect() != null) host.disconnect();
        if (host.disconnected or host.state() != .attached) return error.InvalidState;
    }

    pub fn requestDetach(host: *Host, terminal_ref: provider.TerminalRef) !u32 {
        const request_id = try host.preflightOperation();
        const terminal = host.findTerminalConst(terminal_ref) orelse return error.InvalidIdentity;
        if (!terminal.published) return error.InvalidState;
        const options: c.PhuxDetachResourceOptions = .{
            .size = @sizeOf(c.PhuxDetachResourceOptions),
            .version = c.PHUX_CLIENT_ABI_VERSION,
            .request_id = request_id,
            .terminal_id = cId(&terminal.id),
        };
        try resultError(c.phux_client_queue_detach_resource(host.client, &options));
        host.operation_ledger.accepted(request_id, host.client_generation, .detach, terminal_ref);
        host.stageOutgoing() catch host.disconnect();
        return request_id;
    }

    pub fn catalogRefs(host: *const Host, out: []provider.TerminalRef) usize {
        const count = @min(out.len, host.workspace_store.catalog.len);
        for (out[0..count], host.workspace_store.catalog[0..count]) |*ref, entry| ref.* = entry.terminal_ref;
        return count;
    }

    fn reserveTerminalSlot(host: *Host, target: ?provider.TerminalRef) !void {
        if (host.terminals.items.len + host.operation_ledger.pendingSpawns() >= max_terminals)
            return error.TerminalCapacity;
        try host.reserveCatalogIdentity(target);
        try host.terminals.ensureTotalCapacity(host.gpa, max_terminals);
    }

    fn reserveCatalogIdentity(host: *const Host, target: ?provider.TerminalRef) !void {
        if (target) |ref| if (host.workspace_store.contains(ref)) return;
        if (host.catalogIdentityCount() >= max_catalog_terminals) return error.TerminalCapacity;
    }

    fn catalogIdentityCount(host: *const Host) usize {
        var count = host.workspace_store.catalog.len + host.operation_ledger.pendingSpawns();
        for (host.terminals.items) |*terminal| {
            if (!host.workspace_store.contains(terminal.terminalRef())) count += 1;
        }
        return count;
    }

    fn admitOperationTerminal(host: *Host, remote: RemoteId) void {
        for (host.terminals.items) |*terminal| if (terminal.id.eql(remote)) return;
        std.debug.assert(host.terminals.items.len < max_terminals);
        host.terminals.appendAssumeCapacity(.{ .id = remote, .provider_id = host.provider_id });
    }

    fn spawnOwner(host: *const Host, terminal_ref: ?provider.TerminalRef) !?*const RemoteId {
        const ref = terminal_ref orelse return null;
        if (host.operation_ledger.detaching(ref)) return error.InvalidState;
        const terminal = host.findTerminalConst(ref) orelse return error.InvalidIdentity;
        if (!terminal.published or terminal.phase != .live) return error.InvalidState;
        return &terminal.id;
    }

    pub fn disconnect(host: *Host) void {
        host.freezePublished();
        if (host.disconnected) return;
        host.disconnected = true;
        // Capture replies already observed before cancelling the remainder.
        host.captureOperations() catch {};
        _ = c.phux_client_disconnect(host.client);
        host.captureWorkspace();
        host.operation_ledger.disconnect(host.client_generation);
        _ = c.phux_client_operation_clear(host.client);
        host.bridge.outgoing.reset();
    }

    /// Attach an existing server session without creating anything. A null
    /// name follows the server's current-session rule; a name selects exactly
    /// that session. A server with no match must answer with a typed failure.
    pub fn attachExisting(host: *Host, session: ?[]const u8, viewport: provider.Viewport) !void {
        if (session) |name| try outboundSize(name.len);
        const target: u32 = @intCast(if (session == null) c.PHUX_ATTACH_LAST else c.PHUX_ATTACH_BY_NAME);
        try host.queueAttach(target, 0, if (session) |name| name else &.{}, viewport);
    }

    pub fn attachSessionId(host: *Host, session_id: u32, viewport: provider.Viewport) !void {
        if (session_id == 0) return error.InvalidIdentity;
        try host.queueAttach(c.PHUX_ATTACH_BY_ID, session_id, &.{}, viewport);
    }

    fn queueAttach(host: *Host, target_kind: u32, session_id: u32, name: []const u8, viewport: provider.Viewport) !void {
        const options: c.PhuxAttachOptions = .{
            .size = @sizeOf(c.PhuxAttachOptions),
            .version = c.PHUX_CLIENT_ABI_VERSION,
            .attach_id = 1,
            .target_kind = target_kind,
            .session_id = session_id,
            .name = bytes(name),
            .cols = viewport.cols,
            .rows = viewport.rows,
            .has_pixel_size = viewport.pixels != null,
            .pixel_width = if (viewport.pixels) |pixels| pixels.width else 0,
            .pixel_height = if (viewport.pixels) |pixels| pixels.height else 0,
            .request_scrollback = true,
            .scrollback_limit_lines = 5000,
        };
        try resultError(c.phux_client_queue_attach(host.client, &options));
        try host.stageOutgoing();
    }

    /// Replace only the owning-thread C client. Published canvases and terminal
    /// ordering remain frozen until the replacement reaches its ATTACHED barrier.
    pub fn reconnect(host: *Host, client_name: []const u8) !void {
        host.disconnect();
        errdefer host.freezePublished();
        const next_generation = std.math.add(u64, host.client_generation, 1) catch
            return error.GenerationExhausted;
        const replacement = try newClient();
        host.clearSearchResults(null);
        c.phux_client_free(host.client);
        host.client = replacement;
        host.workspace_store.deinit(host.gpa);
        host.client_generation = next_generation;
        host.rename_revision_seen = 0;
        host.rename_status_seen = .none;
        host.operation_ledger.last_id = 0;
        host.disconnected = false;
        host.attach_barrier_seen = false;
        for (host.terminals.items) |*terminal| {
            terminal.phase = if (terminal.published) .reconnecting else .attaching;
            terminal.seen_in_attach = false;
            terminal.remove_at_barrier = false;
            terminal.dirty = false;
            terminal.pending_title.items.len = 0;
            terminal.pending_title_set = false;
            terminal.viewport = null;
        }
        try host.start(client_name);
    }

    /// Only for an explicit different-session selection. Same-session reconnect
    /// must retain its last-good canvases until the replacement READY barrier.
    pub fn clearSessionReplicas(host: *Host) void {
        host.disconnect();
        host.clearSearchResults(null);
        for (host.terminals.items) |*terminal| terminal.deinit(host.gpa);
        host.terminals.items.len = 0;
        host.agents.clear(host.gpa);
        host.workspace_store.deinit(host.gpa);
        host.workspace_changed = true;
    }

    pub fn freezePublished(host: *Host) void {
        for (host.terminals.items) |*terminal| {
            if (terminal.published and terminal.phase != .ended and terminal.phase != .failed)
                terminal.phase = .frozen;
        }
    }

    /// UI-thread wake handler. The worker never calls the C client.
    pub fn drainReadiness(host: *Host) !SyncDelta {
        if (host.disconnected) return error.InvalidState;
        errdefer host.disconnect();
        var delta: SyncDelta = .{};
        const directory_before = host.directoryStatusRaw();
        const query_before = host.sessionQueryStatus();
        while (host.bridge.incoming.take()) |frame| {
            defer host.bridge.incoming.release(frame);
            try resultErrorWithContext(host.client, "feed frame", c.phux_client_feed_frame(host.client, frame.ptr, frame.len));
        }
        delta.directory_changed = host.directoryStatusRaw() != directory_before;
        if (query_before == session_query_pending and host.sessionQueryStatus() == session_query_ok)
            delta.sessions_listed = try host.adoptListedSessions();
        delta.sessions_renamed = try host.adoptRenamedSessions();
        host.captureWorkspace();
        delta.removed_count += try host.prepareAttachAdmission();
        // Catalog first, effects second, in one drain: the catalog is the
        // roster the attach snapshot published, and an AGENT_RECORDS CLOSED
        // arriving in the same batch must retire a row the catalog still lists.
        try host.refreshResources();
        try host.captureEffects();
        delta.detached = host.state() == .detached;
        if (host.state() == .attached and !host.attach_barrier_seen) {
            delta.removed_count += host.pruneRemoved(true);
            host.attach_barrier_seen = true;
            delta.ready_published = true;
            try host.publishDirty(&delta);
        } else if (host.attach_barrier_seen) {
            delta.removed_count += host.pruneRemoved(false);
            try host.publishDirty(&delta);
        }
        try host.stageOutgoing();
        delta.workspace_changed = host.workspace_changed;
        delta.metadata_changed = host.metadata_changed or host.workspace_changed;
        host.metadata_changed = false;
        host.workspace_changed = false;
        return delta;
    }

    pub fn terminalRefs(host: *const Host, out: []provider.TerminalRef) usize {
        var count: usize = 0;
        for (host.terminals.items) |*terminal| {
            if (!terminal.published) continue;
            if (count < out.len) out[count] = terminal.terminalRef();
            count += 1;
        }
        return @min(count, out.len);
    }

    /// Successful operations admit a record before their result is exposed.
    /// Absence after acceptance therefore distinguishes closure from waiting
    /// for a not-yet-published bootstrap.
    pub fn terminalKnown(host: *const Host, ref: provider.TerminalRef) bool {
        return host.findTerminalConst(ref) != null;
    }

    pub fn sessionCatalog(host: *const Host) []const SessionSummary {
        return host.sessions.items;
    }

    /// Every agent session the roster holds, in catalog order. Owned by the
    /// host and stable until the next drain.
    pub fn agentSessions(host: *const Host) []const AgentSession {
        return host.agents.all();
    }

    /// The agent sessions running under one terminal. `out` bounds the answer.
    pub fn agentSessionsUnder(host: *const Host, ref: provider.TerminalRef, out: []*const AgentSession) usize {
        const parent = host.remoteFromRef(ref) orelse return 0;
        return host.agents.childrenOf(parent, out);
    }

    /// Stream-derived attention for one terminal: an agent under it is waiting
    /// on a person. A signal SOURCE for the quiet attention path, beside the
    /// bell and the phase latches — never a second attention mechanism.
    pub fn agentAttention(host: *const Host, ref: provider.TerminalRef) bool {
        if (host.disconnected or host.state() != .attached) return false;
        const parent = host.remoteFromRef(ref) orelse return false;
        return host.agents.parentNeedsAttention(parent);
    }

    /// Whether this identity is an agent session rather than a terminal.
    /// Nothing that renders a surface may be reached through one.
    pub fn isAgentSession(host: *const Host, ref: provider.TerminalRef) bool {
        const id = host.remoteFromRef(ref) orelse return false;
        return host.agents.findConst(id) != null;
    }

    pub fn contains(host: *const Host, terminal_ref: provider.TerminalRef) bool {
        const terminal = host.findTerminalConst(terminal_ref) orelse return false;
        return terminal.published;
    }

    pub fn owner(host: *const Host, terminal_ref: provider.TerminalRef) ?provider.ReplicaOwner {
        const terminal = host.findTerminalConst(terminal_ref) orelse return null;
        if (!terminal.published) return null;
        return terminal.owner();
    }

    pub fn ownerIsCurrent(host: *const Host, owner_value: provider.ReplicaOwner) bool {
        if (host.operation_ledger.detaching(owner_value.terminal_ref)) return false;
        const terminal = host.findTerminalConst(owner_value.terminal_ref) orelse return false;
        return terminal.phase == .live and terminal.owner().eql(owner_value);
    }

    pub fn presentation(host: *const Host, terminal_ref: provider.TerminalRef) ?provider.Presentation {
        const terminal = host.findTerminalConst(terminal_ref) orelse return null;
        return terminal.presentation();
    }

    pub fn capturePresentation(host: *const Host, expected: provider.ReplicaOwner) !*FrozenPresentation {
        const terminal = host.findTerminalConst(expected.terminal_ref) orelse return error.InvalidState;
        if (!terminal.owner().eql(expected)) return error.InvalidState;
        const value = terminal.presentation() orelse return error.InvalidState;
        return FrozenPresentation.create(host.gpa, &terminal.canvas, value);
    }

    pub fn lastViewport(host: *const Host, terminal_ref: provider.TerminalRef) ?provider.Viewport {
        return (host.findTerminalConst(terminal_ref) orelse return null).viewport;
    }

    pub fn viewportResize(host: *Host, terminal_ref: provider.TerminalRef, viewport: provider.Viewport) !void {
        const terminal = host.findTerminal(terminal_ref) orelse return error.InvalidState;
        const id = try host.currentCId(terminal.owner());
        try resultError(c.phux_client_terminal_resize(
            host.client,
            &id,
            viewport.cols,
            viewport.rows,
        ));
        try host.stageOutgoing();
        try host.capturePublishStage();
        const current_terminal = host.findTerminal(terminal_ref) orelse return error.InvalidState;
        current_terminal.viewport = viewport;
    }

    pub fn sendKey(host: *Host, owner_value: provider.ReplicaOwner, input: *const provider.KeyInput) !void {
        const id = try host.currentCId(owner_value);
        try outboundSize(input.text.len);
        const event: c.PhuxKeyEvent = .{
            .size = @sizeOf(c.PhuxKeyEvent),
            .version = c.PHUX_CLIENT_ABI_VERSION,
            .action = switch (input.action) {
                .press => c.PHUX_KEY_PRESS,
                .repeat => c.PHUX_KEY_REPEAT,
                .release => c.PHUX_KEY_RELEASE,
            },
            .key = @intFromEnum(input.physical),
            .modifiers = @bitCast(input.modifiers),
            .consumed_modifiers = 0,
            .composing = input.composing,
            .has_text = input.text.len != 0,
            .text = bytes(input.text),
            .has_unshifted_codepoint = input.unshifted_codepoint != null,
            .unshifted_codepoint = if (input.unshifted_codepoint) |cp| cp else 0,
        };
        try resultError(c.phux_client_send_key(host.client, &id, &event));
        try host.stageOutgoing();
    }

    pub fn sendMouse(host: *Host, owner_value: provider.ReplicaOwner, input: *const provider.MouseInput) !void {
        const id = try host.currentCId(owner_value);
        const event: c.PhuxMouseEvent = .{
            .size = @sizeOf(c.PhuxMouseEvent),
            .version = c.PHUX_CLIENT_ABI_VERSION,
            .action = switch (input.action) {
                .press => c.PHUX_MOUSE_PRESS,
                .release => c.PHUX_MOUSE_RELEASE,
                .move => c.PHUX_MOUSE_MOTION,
            },
            .button = mouseButton(input.button),
            .modifiers = @bitCast(input.modifiers),
            .x = input.x,
            .y = input.y,
        };
        try resultError(c.phux_client_send_mouse(host.client, &id, &event));
        try host.stageOutgoing();
    }

    fn mouseButton(button: provider.MouseButton) u32 {
        return switch (button) {
            .none => c.PHUX_MOUSE_BUTTON_UNKNOWN,
            .left => c.PHUX_MOUSE_BUTTON_LEFT,
            .right => c.PHUX_MOUSE_BUTTON_RIGHT,
            .middle => c.PHUX_MOUSE_BUTTON_MIDDLE,
            .button_4 => c.PHUX_MOUSE_BUTTON_FOUR,
            .button_5 => c.PHUX_MOUSE_BUTTON_FIVE,
            .button_6 => c.PHUX_MOUSE_BUTTON_SIX,
            .button_7 => c.PHUX_MOUSE_BUTTON_SEVEN,
            else => c.PHUX_MOUSE_BUTTON_UNKNOWN,
        };
    }

    pub fn mouseTracking(host: *const Host, owner_value: provider.ReplicaOwner) !bool {
        const id = try host.currentCIdConst(owner_value);
        var tracking = false;
        try resultError(c.phux_client_terminal_mouse_tracking(host.client, &id, &tracking));
        return tracking;
    }

    pub fn sendFocus(host: *Host, owner_value: provider.ReplicaOwner, focused: bool) !void {
        const id = try host.currentCId(owner_value);
        try resultError(c.phux_client_send_focus(host.client, &id, focused));
        try host.stageOutgoing();
    }

    pub fn mouseMode(host: *const Host, owner_value: provider.ReplicaOwner) !provider.MouseMode {
        const id = try host.currentCIdConst(owner_value);
        var mode: u32 = 0;
        try resultError(c.phux_client_terminal_mouse_mode(host.client, &id, &mode));
        return std.enums.fromInt(provider.MouseMode, mode) orelse error.InvalidState;
    }

    pub fn recordMeasuredCell(host: *Host, owner_value: provider.ReplicaOwner, cell: provider.MeasuredCell) void {
        const terminal = host.findTerminal(owner_value.terminal_ref) orelse return;
        if (terminal.owner().eql(owner_value)) terminal.measured_cell = cell;
    }

    pub fn selectionGesture(host: *Host, owner_value: provider.ReplicaOwner, event: provider.SelectionGesture) !provider.SelectionGestureResult {
        const id = try host.currentCId(owner_value);
        // Ghostty's gesture geometry is integral. Fixed-point surface units
        // retain the canvas's fractional advances instead of rounding a cell.
        const scale = 1024;
        const raw: c.PhuxSelectionGestureEvent = .{
            .size = @sizeOf(c.PhuxSelectionGestureEvent),
            .version = c.PHUX_CLIENT_ABI_VERSION,
            .phase = @intFromEnum(event.phase),
            .clicks = event.clicks,
            .handle = event.handle,
            .column = event.cell.column,
            .row = event.cell.row,
            .rectangle = event.rectangle,
            .reserved = 0,
            .x = event.x * scale,
            .y = event.y * scale,
            .columns = event.columns,
            .cell_width = try gestureExtent(event.cell_width),
            .screen_height = try gestureExtent(event.screen_height),
            .padding_left = 0,
        };
        var result: c.PhuxSelectionGestureResult = undefined;
        try resultError(c.phux_client_selection_gesture(host.client, &id, &raw, &result));
        errdefer {
            _ = c.phux_client_selection_clear(host.client, &id);
            _ = c.phux_client_anchor_release(host.client, &id, result.start);
            _ = c.phux_client_anchor_release(host.client, &id, result.end);
        }
        if (host.findTerminal(owner_value.terminal_ref)) |terminal| terminal.dirty = true;
        try host.capturePublishStage();
        return .{ .handle = result.handle, .start = result.start.opaque_id, .end = result.end.opaque_id };
    }

    fn gestureExtent(value: f32) !u32 {
        const scale = 1024;
        const limit: f32 = @floatFromInt(std.math.maxInt(u32) / scale);
        if (!std.math.isFinite(value) or value < @as(f32, 1) / scale or value > limit) return error.InvalidState;
        return @intFromFloat(@round(value * scale));
    }

    pub fn sendPaste(host: *Host, owner_value: provider.ReplicaOwner, payload: []const u8, trusted: bool) !void {
        const id = try host.currentCId(owner_value);
        try outboundSize(payload.len);
        try resultError(c.phux_client_send_paste(host.client, &id, if (payload.len == 0) null else payload.ptr, payload.len, trusted));
        try host.stageOutgoing();
    }

    pub fn scrollViewport(host: *Host, owner_value: provider.ReplicaOwner, scroll: provider.Scroll) !void {
        const id = try host.currentCId(owner_value);
        const kind: u32 = switch (scroll.kind) {
            .top => c.PHUX_VIEWPORT_SCROLL_TOP,
            .bottom => c.PHUX_VIEWPORT_SCROLL_BOTTOM,
            .delta => c.PHUX_VIEWPORT_SCROLL_DELTA,
        };
        try resultError(c.phux_client_scroll_viewport(host.client, &id, kind, scroll.value));
        if (host.findTerminal(owner_value.terminal_ref)) |terminal| terminal.dirty = true;
        try host.capturePublishStage();
    }

    pub fn createAnchor(host: *Host, owner_value: provider.ReplicaOwner, point: DocumentPoint) !Anchor {
        const id = try host.currentCId(owner_value);
        var anchor: c.PhuxDocumentAnchor = undefined;
        const raw_point: c.PhuxDocumentPoint = .{
            .space = @intFromEnum(point.space),
            .row = point.row,
            .column = point.column,
            .reserved = 0,
        };
        try resultError(c.phux_client_anchor_create(host.client, &id, raw_point, &anchor));
        return .{ .opaque_id = anchor.opaque_id };
    }

    /// Clear only this replica's client presentation, never its durable process.
    pub fn clearPresentation(host: *Host, owner_value: provider.ReplicaOwner) !void {
        const id = try host.currentCId(owner_value);
        try resultError(c.phux_client_clear_presentation(host.client, &id, owner_value.generation.stream_id, owner_value.generation.bootstrap_id));
        // The FFI already retired these anchors. Forget only this owner's
        // cached result array instead of issuing stale releases/failure callbacks.
        if (host.search_owner) |search_owner| if (search_owner.eql(owner_value)) {
            host.search_results.items.len = 0;
            host.search_owner = null;
        };
        if (host.findTerminal(owner_value.terminal_ref)) |terminal| terminal.dirty = true;
        try host.capturePublishStage();
    }

    /// Reveal an engine-owned search position without sending terminal input.
    pub fn pinViewport(host: *Host, owner_value: provider.ReplicaOwner, anchor: Anchor) !void {
        const id = try host.currentCId(owner_value);
        try resultError(c.phux_client_history_viewport_pin(host.client, &id, toCAnchor(anchor)));
        if (host.findTerminal(owner_value.terminal_ref)) |terminal| terminal.dirty = true;
        try host.capturePublishStage();
    }

    pub fn releaseAnchor(host: *Host, owner_value: provider.ReplicaOwner, anchor: Anchor) void {
        const id = host.currentCId(owner_value) catch return;
        _ = c.phux_client_anchor_release(host.client, &id, toCAnchor(anchor));
    }

    pub fn setSelection(host: *Host, owner_value: provider.ReplicaOwner, start_anchor: Anchor, end_anchor: Anchor, rectangle: bool) !void {
        const id = try host.currentCId(owner_value);
        try resultError(c.phux_client_selection_set(host.client, &id, toCAnchor(start_anchor), toCAnchor(end_anchor), rectangle));
        if (host.findTerminal(owner_value.terminal_ref)) |terminal| terminal.dirty = true;
        try host.capturePublishStage();
    }

    pub fn clearSelection(host: *Host, owner_value: provider.ReplicaOwner) !void {
        const id = try host.currentCId(owner_value);
        try resultError(c.phux_client_selection_clear(host.client, &id));
        if (host.findTerminal(owner_value.terminal_ref)) |terminal| terminal.dirty = true;
        try host.capturePublishStage();
    }

    /// Case sensitivity is NOT a parameter. It comes from
    /// `provider.search_case_sensitive`, the one place the app's search rule
    /// lives, so this side cannot be asked to match by a rule the local side
    /// is incapable of honouring.
    pub fn search(host: *Host, owner_value: provider.ReplicaOwner, query: []const u8) ![]const SearchResult {
        const id = try host.currentCId(owner_value);
        try outboundSize(query.len);
        host.clearSearchResults(null);
        var borrowed: [*c]const c.PhuxSearchResult = null;
        var count: usize = 0;
        try resultError(c.phux_client_search(host.client, &id, bytes(query), provider.search_case_sensitive, &borrowed, &count));
        if (count > max_search_results or (count != 0 and borrowed == null)) {
            _ = c.phux_client_search_results_release(host.client);
            return error.OutOfMemory;
        }
        host.search_results.ensureTotalCapacity(host.gpa, count) catch {
            _ = c.phux_client_search_results_release(host.client);
            return error.OutOfMemory;
        };
        host.search_results.items.len = count;
        for (host.search_results.items, 0..) |*result, index| {
            result.* = .{
                .start = .{ .opaque_id = borrowed[index].start.opaque_id },
                .end = .{ .opaque_id = borrowed[index].end.opaque_id },
            };
        }
        host.search_owner = owner_value;
        try host.capturePublishStage();
        return host.search_results.items;
    }

    /// `expected_owner` fences UI cleanup so an obsolete result cannot clear a
    /// newer generation's search anchors.
    pub fn clearSearchResults(host: *Host, expected_owner: ?provider.ReplicaOwner) void {
        const stored_owner = host.search_owner orelse return;
        if (expected_owner) |expected| if (!stored_owner.eql(expected)) return;
        const remote = host.remoteFromRef(stored_owner.terminal_ref) orelse {
            host.search_results.items.len = 0;
            host.search_owner = null;
            return;
        };
        const id = cId(&remote);
        for (host.search_results.items) |result| {
            _ = c.phux_client_anchor_release(host.client, &id, toCAnchor(result.start));
            if (result.end.opaque_id != result.start.opaque_id)
                _ = c.phux_client_anchor_release(host.client, &id, toCAnchor(result.end));
        }
        host.search_results.items.len = 0;
        host.search_owner = null;
    }

    pub fn selectionText(host: *Host, owner_value: provider.ReplicaOwner, gpa: std.mem.Allocator) ![]u8 {
        const id = try host.currentCId(owner_value);
        var text: c.PhuxBytes = undefined;
        try resultError(c.phux_client_selection_text(host.client, &id, &text));
        if (text.len != 0 and text.data == null) return error.Protocol;
        return gpa.dupe(u8, if (text.len == 0) &.{} else text.data[0..text.len]);
    }

    pub fn takeNotice(host: *Host) ?Notice {
        if (host.notices.items.len == 0) return null;
        return host.notices.orderedRemove(0);
    }

    pub fn phase(host: *const Host, ref: provider.TerminalRef) ?provider.Phase {
        const terminal = host.findTerminalConst(ref) orelse return null;
        return terminal.phase;
    }

    pub fn bellRung(host: *const Host, ref: provider.TerminalRef) bool {
        const terminal = host.findTerminalConst(ref) orelse return false;
        const owner_value = terminal.bell_owner orelse return false;
        return host.ownerIsCurrent(owner_value);
    }

    /// One attention edge per current replica until the terminal is attended.
    pub fn ringBell(host: *Host, owner_value: provider.ReplicaOwner) bool {
        if (!host.ownerIsCurrent(owner_value)) return false;
        if (host.bellRung(owner_value.terminal_ref)) return false;
        const terminal = host.findTerminal(owner_value.terminal_ref) orelse return false;
        terminal.bell_owner = owner_value;
        return true;
    }

    pub fn acknowledgeBell(host: *Host, ref: provider.TerminalRef) void {
        const terminal = host.findTerminal(ref) orelse return;
        terminal.bell_owner = null;
    }

    pub fn releaseNotice(host: *Host, notice: Notice) void {
        host.gpa.free(notice.bytes);
    }

    fn capturePublishStage(host: *Host) !void {
        errdefer host.freezePublished();
        try host.captureEffects();
        if (host.attach_barrier_seen) {
            var ignored: SyncDelta = .{};
            _ = host.pruneRemoved(false);
            try host.publishDirty(&ignored);
        }
        try host.stageOutgoing();
    }

    fn refreshSessions(host: *Host) !void {
        const count = c.phux_client_session_count(host.client);
        if (count > max_sessions) return error.Protocol;

        var next: std.ArrayListUnmanaged(SessionSummary) = .empty;
        errdefer {
            for (next.items) |*session| session.deinit(host.gpa);
            next.deinit(host.gpa);
        }
        try next.ensureTotalCapacity(host.gpa, count);
        for (0..count) |index| {
            var raw: c.PhuxSessionInfo = undefined;
            try resultError(c.phux_client_session_get(host.client, index, &raw));
            var session = try copySessionSummary(host.gpa, raw);
            var flags: u32 = 0;
            if (c.phux_client_session_flags(host.client, index, &flags) == c.PHUX_CLIENT_OK) {
                session.keep_empty = flags & c.PHUX_SESSION_FLAG_KEEP_EMPTY != 0;
                session.empty = flags & c.PHUX_SESSION_FLAG_EMPTY != 0;
            }
            next.append(host.gpa, session) catch {
                var owned = session;
                owned.deinit(host.gpa);
                return error.OutOfMemory;
            };
        }
        host.clearSessions();
        host.sessions.deinit(host.gpa);
        host.sessions = next;
        host.sessions_generation = host.client_generation;
    }

    fn clearSessions(host: *Host) void {
        for (host.sessions.items) |*session| session.deinit(host.gpa);
        host.sessions.items.len = 0;
    }

    const session_query_pending: u32 = 1;
    const session_query_ok: u32 = 2;
    comptime {
        std.debug.assert(c.PHUX_SESSION_QUERY_PENDING == session_query_pending);
        std.debug.assert(c.PHUX_SESSION_QUERY_OK == session_query_ok);
    }

    /// Standby: ask for the session list without attaching (GET_STATE). Null
    /// while a query is already outstanding.
    pub fn querySessions(host: *Host) !?u32 {
        if (host.disconnected or host.state() != .negotiated) return error.InvalidState;
        if (host.sessionQueryStatus() == session_query_pending) return null;
        const request_id = try host.operation_ledger.nextRequestId();
        try resultError(c.phux_client_query_sessions(host.client, request_id));
        host.operation_ledger.last_id = request_id;
        host.stageOutgoing() catch host.disconnect();
        return request_id;
    }

    fn sessionQueryStatus(host: *const Host) u32 {
        var request_id: u32 = 0;
        var status: u32 = 0;
        if (c.phux_client_session_query_status(host.client, &request_id, &status) != c.PHUX_CLIENT_OK) return 0;
        return status;
    }

    /// Rename the session named `current` on this connection's server
    /// (`phux.session.name/v1`). The client judges it against its own list
    /// first, so a refusal may already be settled when this returns; read
    /// `renameInfo`. Nothing here attaches or sizes anything: a listing
    /// connection may rename as well as an attached one.
    pub fn requestRename(host: *Host, current: []const u8, new_name: []const u8) !u32 {
        if (host.disconnected) return error.InvalidState;
        const now = host.state();
        if (now != .negotiated and now != .attached) return error.InvalidState;
        try outboundSize(current.len);
        try outboundSize(new_name.len);
        const request_id = try host.operation_ledger.nextRequestId();
        try resultError(c.phux_client_rename_session(host.client, request_id, bytes(current), bytes(new_name)));
        host.operation_ledger.last_id = request_id;
        host.stageOutgoing() catch host.disconnect();
        return request_id;
    }

    pub const RenameStatus = enum(u32) { none = 0, pending = 1, renamed = 2, refused = 3, unknown_outcome = 4, _ };
    /// `message` borrows the client until the next mutable host call.
    pub const RenameInfo = struct {
        status: RenameStatus = .none,
        request_id: u32 = 0,
        session_id: u32 = 0,
        sessions_revision: u64 = 0,
        message: []const u8 = "",
    };
    comptime {
        std.debug.assert(c.PHUX_SESSION_RENAME_PENDING == @intFromEnum(RenameStatus.pending));
        std.debug.assert(c.PHUX_SESSION_RENAME_UNKNOWN_OUTCOME == @intFromEnum(RenameStatus.unknown_outcome));
    }

    pub fn renameInfo(host: *const Host) RenameInfo {
        var raw = std.mem.zeroes(c.PhuxSessionRenameInfo);
        raw.size = @sizeOf(c.PhuxSessionRenameInfo);
        raw.version = c.PHUX_CLIENT_ABI_VERSION;
        if (c.phux_client_session_rename_info(host.client, &raw) != c.PHUX_CLIENT_OK) return .{};
        return .{
            .status = @enumFromInt(raw.status),
            .request_id = raw.request_id,
            .session_id = raw.session_id,
            .sessions_revision = raw.sessions_revision,
            .message = effectSlice(raw.message) catch "",
        };
    }

    /// A rename the server broadcast moved the client's list in place: read
    /// it again. True when the list moved or this client's rename settled,
    /// so the chrome and the rename panel hear about it.
    fn adoptRenamedSessions(host: *Host) !bool {
        const info = host.renameInfo();
        const settled = host.rename_status_seen == .pending and info.status != .pending;
        host.rename_status_seen = info.status;
        if (info.sessions_revision == host.rename_revision_seen) return settled;
        host.rename_revision_seen = info.sessions_revision;
        try host.refreshSessions();
        return true;
    }

    /// Adopt a settled query's list as this connection's. True when it
    /// differs from the list it replaces, so an unchanged refresh announces
    /// nothing and a refresh loop cannot feed itself.
    fn adoptListedSessions(host: *Host) !bool {
        const before = sessionsDigest(host.sessions.items, host.sessions_generation);
        try host.refreshSessions();
        return sessionsDigest(host.sessions.items, host.sessions_generation) != before;
    }

    /// Everything a switcher row shows of a session: its id and name, and
    /// the window count and keep-empty flags its Empty session label is
    /// derived from. A listing peer's session that gains or loses its
    /// windows keeps its id and name, so without them its row would keep
    /// the old label until an unrelated repaint.
    fn sessionsDigest(sessions: []const SessionSummary, generation: u64) u64 {
        var hasher = std.hash.Wyhash.init(generation);
        for (sessions) |session| {
            hasher.update(std.mem.asBytes(&session.id));
            hasher.update(std.mem.asBytes(&session.window_count));
            hasher.update(&.{ @intFromBool(session.keep_empty), @intFromBool(session.empty) });
            hasher.update(session.name);
            hasher.update(&.{0});
        }
        return hasher.final();
    }

    /// A standby's list describes only the connection that listed it: on
    /// disconnect or retarget it goes, and the generation no longer matches
    /// any connection, so rows captured from it stop resolving.
    pub fn forgetSessions(host: *Host) void {
        host.clearSessions();
        host.sessions_generation = 0;
    }

    fn captureOperations(host: *Host) !void {
        const count = c.phux_client_operation_count(host.client);
        if (count > max_terminals) return error.Protocol;
        // Copy ALL borrowed results before any mutable FFI call (including
        // effect clearing, terminal publication, or disconnect).
        var copied: [max_terminals]OperationResult = undefined;
        for (copied[0..count], 0..) |*result, index| {
            var raw: c.PhuxOperationResult = std.mem.zeroes(c.PhuxOperationResult);
            raw.size = @sizeOf(c.PhuxOperationResult);
            raw.version = c.PHUX_CLIENT_ABI_VERSION;
            try resultError(c.phux_client_operation_get(host.client, index, &raw));
            result.* = try copyOperation(raw, host.client_generation, host.provider_id);
            var instance: [16]u8 = undefined;
            var bound = false;
            if (c.phux_client_operation_instance(host.client, index, &instance, &bound) == c.PHUX_CLIENT_OK and bound)
                result.instance = instance;
        }
        try resultError(c.phux_client_operation_clear(host.client));
        for (copied[0..count]) |result| {
            try host.operation_ledger.complete(result);
            try host.applyOperationIdentity(&result);
        }
    }

    fn applyOperationIdentity(host: *Host, result: *const OperationResult) !void {
        // A conditional kill names a terminal it never admits as a replica.
        if (result.kind == .kill_if) return;
        const terminal_ref = result.terminal_ref orelse return;
        if (result.kind == .detach) {
            if (result.status == .success) host.removeOperationReplica(terminal_ref);
            return;
        }
        if (result.status == .success) {
            try host.admitOperationReplica(terminal_ref);
            return;
        }
        if (result.kind == .attach and result.status == .refused) host.removeOperationReplica(terminal_ref);
    }

    fn removeOperationReplica(host: *Host, ref: provider.TerminalRef) void {
        const terminal = host.findTerminal(ref) orelse return;
        terminal.remove_at_barrier = true;
    }

    fn admitOperationReplica(host: *Host, ref: provider.TerminalRef) !void {
        const remote = host.remoteFromRef(ref) orelse return error.InvalidIdentity;
        _ = try host.ensureTerminal(cId(&remote));
    }

    fn prepareAttachAdmission(host: *Host) !usize {
        if (host.attach_barrier_seen) return 0;
        host.markObsoleteAttachReplicas();
        const removed = host.pruneRemoved(false);
        if (host.state() != .attached) return removed;
        // ATTACH_READY emits damage for every resolved participant. Mark the
        // retained IDs before freeing unseen slots, without admitting new IDs.
        try host.markReadyAttachReplicas();
        return removed + host.pruneRemoved(true);
    }

    fn markObsoleteAttachReplicas(host: *Host) void {
        // Reconnect clears this store. A nonzero publication is the copied
        // ATTACHED registry, even while metadata and initial READY are pending.
        if (host.workspace_store.info.revision == 0) return;
        for (host.terminals.items) |*terminal| {
            if (terminal.generation.epoch_id == host.client_generation) continue;
            if (host.retainedByAttachCatalog(terminal.terminalRef())) continue;
            terminal.remove_at_barrier = true;
        }
    }

    fn retainedByAttachCatalog(host: *const Host, ref: provider.TerminalRef) bool {
        for (host.workspace_store.catalog) |entry| {
            if (!entry.terminal_ref.eql(ref)) continue;
            // Unknown satellite ownership is not evidence of removal.
            return entry.session_id == 0 or entry.session_id == host.workspace_store.info.session_id;
        }
        return false;
    }

    fn markReadyAttachReplicas(host: *Host) !void {
        const count = c.phux_client_effect_count(host.client);
        for (0..count) |index| {
            var effect: c.PhuxClientEffect = undefined;
            try resultError(c.phux_client_effect_get(host.client, index, &effect));
            if (effect.kind != c.PHUX_CLIENT_EFFECT_DAMAGE) continue;
            const terminal = (try host.findTerminalRaw(effect.terminal_id)) orelse continue;
            if (effect.detail == c.PHUX_CLIENT_DAMAGE_REMOVED) {
                terminal.remove_at_barrier = true;
            } else {
                terminal.seen_in_attach = true;
            }
        }
    }

    fn captureEffects(host: *Host) !void {
        const count = c.phux_client_effect_count(host.client);
        var index: usize = 0;
        while (index < count) : (index += 1) {
            var effect: c.PhuxClientEffect = undefined;
            try resultError(c.phux_client_effect_get(host.client, index, &effect));
            const generation: provider.Generation = .{
                .epoch_id = host.client_generation,
                .stream_id = effect.stream_id,
                .bootstrap_id = effect.bootstrap_id,
                .last_seq = effect.seq,
            };
            switch (effect.kind) {
                c.PHUX_CLIENT_EFFECT_DAMAGE => try host.captureDamage(&effect),
                c.PHUX_CLIENT_EFFECT_STATUS => {
                    try host.captureStatus(&effect);
                    try host.appendNotice(.status, &effect, generation);
                },
                c.PHUX_CLIENT_EFFECT_JOB => try host.appendNotice(.job, &effect, generation),
                c.PHUX_CLIENT_EFFECT_AGENT_RECORDS => try host.captureAgentRecords(&effect),
                else => return error.Protocol,
            }
        }
        try resultError(c.phux_client_effect_clear(host.client));
        try host.captureOperations();
    }

    /// Fold one AgentSession record batch into the roster.
    ///
    /// An unrecognized records kind is DROPPED, not refused: the header
    /// declares new kinds additive, and a host that disconnected on one would
    /// make an additive change breaking. An unrecognized effect kind is still
    /// a protocol error, because the effect union is not additive that way.
    fn captureAgentRecords(host: *Host, effect: *const c.PhuxClientEffect) !void {
        const reintroduced = effect.detail == c.PHUX_CLIENT_AGENT_RECORDS_CLOSED and
            try host.catalogContainsAgent(try remoteFromC(effect.terminal_id));
        try host.captureAgentRecordDelivery(effect, reintroduced);
    }

    fn catalogContainsAgent(host: *const Host, id: RemoteId) !bool {
        const count = c.phux_client_resource_count(host.client);
        for (0..count) |index| {
            var raw = workspace_bridge.record(c.PhuxResourceInfo);
            try resultError(c.phux_client_resource_get(host.client, index, &raw));
            if (raw.kind != c.PHUX_RESOURCE_AGENT_SESSION) continue;
            if ((try remoteFromC(raw.terminal_id)).eql(id)) return true;
        }
        return false;
    }

    /// CLOSED removes the ABI catalog entry before feed_frame returns. If the
    /// final catalog lists it again, a later inventory reintroduced the ID.
    /// Keep that membership but withdraw the closed stream's evidence now:
    /// its replacement RETAINED may be delayed or refused.
    fn captureAgentRecordDelivery(host: *Host, effect: *const c.PhuxClientEffect, reintroduced: bool) !void {
        const kind: agent_sessions.RecordsKind = switch (effect.detail) {
            c.PHUX_CLIENT_AGENT_RECORDS_RETAINED => .retained,
            c.PHUX_CLIENT_AGENT_RECORDS_LIVE => .live,
            c.PHUX_CLIENT_AGENT_RECORDS_CLOSED => .closed,
            else => return,
        };
        const id = try remoteFromC(effect.terminal_id);
        const payload = try effectSlice(effect.bytes);
        const limit = if (kind == .retained) max_agent_retained_bytes else max_agent_records_bytes;
        if (payload.len > limit) return error.Protocol;
        const generation: provider.Generation = .{
            .epoch_id = host.client_generation,
            .stream_id = effect.stream_id,
            .bootstrap_id = effect.bootstrap_id,
            .last_seq = effect.seq,
        };
        const changed = if (reintroduced)
            host.agents.withdrawRecordsGeneration(id, generation)
        else
            try host.agents.applyRecordsGeneration(host.gpa, id, generation, kind, payload);
        if (changed) host.metadata_changed = true;
    }

    /// Adopt the resource catalog the latest ATTACHED snapshot published.
    ///
    /// Only while attached. Before the first attach the catalog is empty by
    /// definition, and during a reconnect an empty read is the absence of an
    /// answer rather than the answer "no agents" — adopting it would blank
    /// every row for the length of the outage. The last good roster stands
    /// until a replacement snapshot exists, exactly as the canvases do.
    ///
    /// Spans are borrowed until the next mutable client call; `adopt` copies
    /// everything it keeps before this function returns.
    fn refreshResources(host: *Host) !void {
        if (host.state() != .attached) return;
        const count = c.phux_client_resource_count(host.client);
        var entries: std.ArrayListUnmanaged(agent_sessions.Entry) = .empty;
        defer entries.deinit(host.gpa);
        for (0..count) |index| {
            var raw = workspace_bridge.record(c.PhuxResourceInfo);
            try resultError(c.phux_client_resource_get(host.client, index, &raw));
            try host.appendAgentEntry(&entries, raw);
        }
        try host.adoptAgentEntries(entries.items);
    }

    fn appendAgentEntry(host: *Host, entries: *std.ArrayListUnmanaged(agent_sessions.Entry), raw: c.PhuxResourceInfo) !void {
        if (raw.kind != c.PHUX_RESOURCE_AGENT_SESSION) return;
        if (entries.items.len == max_agent_sessions) return error.Protocol;
        var entry = try agentEntryFromC(raw);
        entry.epoch_id = host.client_generation;
        try entries.append(host.gpa, entry);
    }

    fn adoptAgentEntries(host: *Host, entries: []const agent_sessions.Entry) !void {
        const changed = !host.agents.catalogMatches(entries);
        try host.agents.adopt(host.gpa, entries);
        if (changed) host.metadata_changed = true;
    }

    fn captureDamage(host: *Host, effect: *const c.PhuxClientEffect) !void {
        if (effect.detail == c.PHUX_CLIENT_DAMAGE_REMOVED) {
            // A removed participant may already have released its slot before
            // admission. Never recreate a replica merely to remove it again.
            const terminal = (try host.findTerminalRaw(effect.terminal_id)) orelse return;
            terminal.phase = .tombstoned;
            terminal.remove_at_barrier = true;
            return;
        }
        const terminal = try host.ensureTerminal(effect.terminal_id);
        markGridDirty(terminal, host.attach_barrier_seen);
    }

    fn captureStatus(host: *Host, effect: *const c.PhuxClientEffect) !void {
        switch (effect.detail) {
            c.PHUX_CLIENT_STATUS_TITLE => try host.captureTitle(effect),
            c.PHUX_CLIENT_STATUS_RESYNC_REQUIRED => {
                host.metadata_changed = true;
                try host.markResync(effect.terminal_id);
            },
            c.PHUX_CLIENT_STATUS_DETACHED => {
                host.metadata_changed = true;
                host.markDetached();
            },
            c.PHUX_CLIENT_STATUS_SERVER_ERROR => {
                host.metadata_changed = true;
                host.markServerFailure();
            },
            c.PHUX_CLIENT_STATUS_HISTORY, c.PHUX_CLIENT_STATUS_HISTORY_UNAVAILABLE => {
                host.metadata_changed = true;
                if (try host.findTerminalRaw(effect.terminal_id)) |terminal| terminal.dirty = true;
            },
            else => {},
        }
    }

    fn captureTitle(host: *Host, effect: *const c.PhuxClientEffect) !void {
        const terminal = try host.ensureTerminal(effect.terminal_id);
        const payload = try effectSlice(effect.bytes);
        if (payload.len > max_title_bytes) return error.Protocol;
        const destination = if (host.attach_barrier_seen and terminal.published) &terminal.title else &terminal.pending_title;
        const title_known = destination == &terminal.title or terminal.pending_title_set;
        if (title_known and std.mem.eql(u8, destination.items, payload)) return;
        try destination.ensureTotalCapacity(host.gpa, payload.len);
        destination.items.len = payload.len;
        @memcpy(destination.items, payload);
        host.metadata_changed = true;
        if (destination == &terminal.pending_title) terminal.pending_title_set = true;
    }

    fn markResync(host: *Host, raw: c.PhuxResourceId) !void {
        if (try host.findTerminalRaw(raw)) |terminal| {
            terminal.phase = .tombstoned;
            return;
        }
        for (host.terminals.items) |*terminal| terminal.phase = .tombstoned;
    }

    fn markDetached(host: *Host) void {
        host.attach_barrier_seen = false;
        for (host.terminals.items) |*terminal| {
            terminal.phase = if (terminal.published) .reconnecting else .attaching;
            terminal.seen_in_attach = false;
            terminal.remove_at_barrier = false;
            terminal.pending_title.items.len = 0;
            terminal.pending_title_set = false;
        }
    }

    fn markServerFailure(host: *Host) void {
        // Operation refusals are correlated outcomes, not terminal failures.
        if (host.state() != .failed) return;
        for (host.terminals.items) |*terminal| terminal.phase = .failed;
    }

    fn publishDirty(host: *Host, delta: *SyncDelta) !void {
        for (host.terminals.items) |*terminal| {
            if (!terminal.dirty or terminal.remove_at_barrier or !terminal.seen_in_attach) continue;
            try host.publishTerminal(terminal, delta);
        }
    }

    fn publishTerminal(host: *Host, terminal: *Terminal, delta: *SyncDelta) !void {
        // Reserve non-grid presentation storage before borrowing a view,
        // so no allocation failure can strand its top anchor.
        try terminal.title.ensureTotalCapacity(host.gpa, max_title_bytes);
        const id = cId(&terminal.id);
        var view: c.PhuxTerminalGridView = undefined;
        const result = c.phux_client_terminal_grid(host.client, &id, &view);
        if (result == c.PHUX_CLIENT_NO_VALUE) {
            if (terminal.published) terminal.phase = .frozen;
            return;
        }
        try resultErrorWithContext(host.client, "read terminal grid", result);
        try host.copyTerminalCanvas(terminal, &id, &view);
        const next_generation: provider.Generation = .{
            .epoch_id = host.client_generation,
            .stream_id = view.stream_id,
            .bootstrap_id = view.bootstrap_id,
            .last_seq = view.last_seq,
        };
        const was_published = terminal.published;
        const changed = was_published and !terminal.generation.sameReplica(next_generation);
        publishTitle(terminal);
        terminal.generation = next_generation;
        terminal.cols = view.cols;
        terminal.rows = view.rows;
        terminal.history_total_rows = view.history_total_rows;
        terminal.history_viewport_offset = view.history_viewport_offset;
        terminal.history_visible_rows = view.history_visible_rows;
        terminal.history_loading = view.history_loading;
        terminal.history_has_more = view.history_has_more;
        terminal.history_pages_loaded = view.history_pages_loaded;
        terminal.history_unread_rows = view.history_unread_rows;
        terminal.phase = .live;
        terminal.published = true;
        terminal.dirty = false;
        if (!was_published) delta.added_count += 1;
        delta.generation_changed = delta.generation_changed or changed;
    }

    fn copyTerminalCanvas(host: *Host, terminal: *Terminal, id: *const c.PhuxResourceId, view: *const c.PhuxTerminalGridView) !void {
        const returned_id = remoteFromC(view.terminal_id) catch |err| {
            releaseTopAnchor(host.client, id, view.top_anchor);
            return err;
        };
        if (!returned_id.eql(terminal.id)) {
            releaseTopAnchor(host.client, id, view.top_anchor);
            return error.InvalidIdentity;
        }
        terminal.canvas.setColorPolicy(host.color_policy);
        terminal.canvas.copyClient(host.gpa, host.client, view) catch |err| {
            releaseTopAnchor(host.client, id, view.top_anchor);
            return err;
        };
        if (view.top_anchor.opaque_id != 0)
            try resultErrorWithContext(host.client, "release terminal top anchor", c.phux_client_anchor_release(host.client, id, view.top_anchor));
    }

    fn appendNotice(host: *Host, kind: NoticeKind, effect: *const c.PhuxClientEffect, generation: provider.Generation) !void {
        const payload = try effectSlice(effect.bytes);
        if (payload.len > max_notice_bytes) return error.Protocol;
        const remote = try remoteFromC(effect.terminal_id);
        const owned = host.gpa.dupe(u8, payload) catch return error.OutOfMemory;
        errdefer host.gpa.free(owned);
        if (host.notices.items.len == max_notices) {
            const dropped = host.notices.orderedRemove(0);
            host.gpa.free(dropped.bytes);
        }
        try host.notices.append(host.gpa, .{
            .kind = kind,
            .detail = effect.detail,
            .status_code = effect.status_code,
            .terminal_ref = host.refFor(remote),
            .generation = generation,
            .bytes = owned,
        });
    }

    fn ensureTerminal(host: *Host, raw: c.PhuxResourceId) !*Terminal {
        const id = try remoteFromC(raw);
        for (host.terminals.items) |*terminal| if (terminal.id.eql(id)) return terminal;
        // An AgentSession publishes no replica and refuses every terminal
        // facet call. Minting a slot for one would put an empty surface in a
        // pane and route keystrokes at a resource that cannot take them.
        if (host.agents.findConst(id) != null) return error.Protocol;
        if (host.terminals.items.len == max_terminals) return error.OutOfMemory;
        try host.terminals.append(host.gpa, .{ .id = id, .provider_id = host.provider_id });
        return &host.terminals.items[host.terminals.items.len - 1];
    }

    fn findTerminalRaw(host: *Host, raw: c.PhuxResourceId) !?*Terminal {
        const id = try remoteFromC(raw);
        for (host.terminals.items) |*terminal| if (terminal.id.eql(id)) return terminal;
        return null;
    }

    fn findTerminal(host: *Host, terminal_ref: provider.TerminalRef) ?*Terminal {
        const id = host.remoteFromRef(terminal_ref) orelse return null;
        for (host.terminals.items) |*terminal| if (terminal.id.eql(id)) return terminal;
        return null;
    }

    fn findTerminalConst(host: *const Host, terminal_ref: provider.TerminalRef) ?*const Terminal {
        const id = host.remoteFromRef(terminal_ref) orelse return null;
        for (host.terminals.items) |*terminal| if (terminal.id.eql(id)) return terminal;
        return null;
    }

    fn pruneRemoved(host: *Host, include_unseen: bool) usize {
        var removed: usize = 0;
        var index = host.terminals.items.len;
        while (index > 0) {
            index -= 1;
            const terminal = &host.terminals.items[index];
            if (!terminal.remove_at_barrier and (!include_unseen or terminal.seen_in_attach)) continue;
            if (host.search_owner) |owner_value| {
                if (owner_value.terminal_ref.eql(terminal.terminalRef())) host.clearSearchResults(owner_value);
            }
            terminal.deinit(host.gpa);
            _ = host.terminals.orderedRemove(index);
            removed += 1;
        }
        return removed;
    }

    fn currentCId(host: *Host, owner_value: provider.ReplicaOwner) !c.PhuxResourceId {
        if (host.operation_ledger.detaching(owner_value.terminal_ref)) return error.InvalidState;
        const terminal = host.findTerminal(owner_value.terminal_ref) orelse return error.InvalidState;
        if (terminal.phase != .live or !terminal.owner().eql(owner_value)) return error.InvalidState;
        return cId(&terminal.id);
    }

    fn currentCIdConst(host: *const Host, owner_value: provider.ReplicaOwner) !c.PhuxResourceId {
        if (host.operation_ledger.detaching(owner_value.terminal_ref)) return error.InvalidState;
        const terminal = host.findTerminalConst(owner_value.terminal_ref) orelse return error.InvalidState;
        if (terminal.phase != .live or !terminal.owner().eql(owner_value)) return error.InvalidState;
        return cId(&terminal.id);
    }

    fn stageOutgoing(host: *Host) !void {
        // A worker may already have taken an earlier copy when a later one
        // fails. Retiring this connection clears both queues and prevents any
        // caller from replaying the retained FFI prefix after allocator recovery.
        errdefer host.disconnect();
        if (host.bridge.incoming.takeDisconnect() != null) {
            host.disconnect();
            return error.Protocol;
        }
        if (host.disconnected) return error.InvalidState;
        const count = c.phux_client_outgoing_count(host.client);
        var index: usize = 0;
        while (index < count) : (index += 1) {
            var frame: c.PhuxBytes = undefined;
            try resultError(c.phux_client_outgoing_get(host.client, index, &frame));
            const payload = try effectSlice(frame);
            if (!host.bridge.outgoing.stage(payload)) return error.OutOfMemory;
        }
        try resultError(c.phux_client_outgoing_clear(host.client));
    }
};

fn newClient() !*c.PhuxClient {
    var raw: ?*c.PhuxClient = null;
    const options: c.PhuxClientOptions = .{
        .size = @sizeOf(c.PhuxClientOptions),
        .version = c.PHUX_CLIENT_ABI_VERSION,
        .max_bootstrap_chunk_bytes = 256 * 1024,
        .max_history_page_bytes = 1024 * 1024,
        .max_history_page_rows = 1024,
        .max_history_cache_bytes = 8 * 1024 * 1024,
        .max_history_materialized_rows = 8192,
        .history_prefetch_rows = 256,
    };
    try resultError(c.phux_client_new(&options, &raw));
    return raw orelse error.InvalidState;
}

fn remoteFromC(raw: c.PhuxResourceId) !RemoteId {
    if (raw.host.len != 0 and raw.host.data == null) return error.InvalidIdentity;
    const host_name: []const u8 = if (raw.host.len == 0) &.{} else raw.host.data[0..raw.host.len];
    if (raw.kind == c.PHUX_RESOURCE_ID_LOCAL) {
        if (host_name.len != 0) return error.InvalidIdentity;
    } else if (raw.kind == c.PHUX_RESOURCE_ID_SATELLITE) {
        if (host_name.len == 0) return error.InvalidIdentity;
        _ = std.unicode.Utf8View.init(host_name) catch return error.InvalidIdentity;
    } else return error.InvalidIdentity;
    return RemoteId.fromPhux(raw.kind, raw.id, host_name) catch return error.InvalidIdentity;
}

/// This Mac's coordinator's ref; tests and the default provider id only.
fn phuxRef(id: RemoteId) provider.TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = id } };
}

fn cId(id: *const RemoteId) c.PhuxResourceId {
    const host_name = id.host();
    return .{
        .kind = id.kind,
        .id = id.id,
        .host = .{ .data = if (host_name.len == 0) null else host_name.ptr, .len = host_name.len },
    };
}

fn copyOperation(raw: c.PhuxOperationResult, epoch: u64, provider_id: provider.ProviderId) !OperationResult {
    var result: OperationResult = .{
        .request_id = raw.request_id,
        .connection_epoch = epoch,
        .kind = std.enums.fromInt(operations.types.Kind, raw.kind) orelse return error.Protocol,
        .status = std.enums.fromInt(operations.types.Status, raw.status) orelse return error.Protocol,
        .error_domain = std.enums.fromInt(operations.types.ErrorDomain, raw.error_domain) orelse return error.Protocol,
        .error_code = raw.error_code,
    };
    if (raw.terminal_id.id != 0) result.terminal_ref = .{ .provider_id = provider_id, .terminal_id = .{ .phux = try remoteFromC(raw.terminal_id) } };
    const message = try effectSlice(raw.message);
    if (message.len > result.message_storage.len) return error.Protocol;
    @memcpy(result.message_storage[0..message.len], message);
    result.message_len = message.len;
    return result;
}
fn bytes(slice: []const u8) c.PhuxBytes {
    return .{ .data = if (slice.len == 0) null else slice.ptr, .len = slice.len };
}

/// One catalog row, still borrowing the ABI's spans. `parent` is NULL when the
/// resource has none; it is not an error, and it is not a self-parent.
fn agentEntryFromC(raw: c.PhuxResourceInfo) !agent_sessions.Entry {
    const parent: ?RemoteId = if (raw.parent == null) null else try remoteFromC(raw.parent.*);
    return .{
        .id = try remoteFromC(raw.terminal_id),
        .parent = parent,
        .provider_name = try effectSlice(raw.provider),
        .native_id = try effectSlice(raw.native_id),
        .state = try effectSlice(raw.state),
    };
}

fn effectSlice(raw: c.PhuxBytes) ![]const u8 {
    if (raw.len != 0 and raw.data == null) return error.Protocol;
    return if (raw.len == 0) &.{} else raw.data[0..raw.len];
}

fn toCAnchor(anchor: Anchor) c.PhuxDocumentAnchor {
    return .{ .opaque_id = anchor.opaque_id };
}

fn releaseTopAnchor(client: *c.PhuxClient, terminal_id: *const c.PhuxResourceId, anchor: c.PhuxDocumentAnchor) void {
    if (anchor.opaque_id != 0) _ = c.phux_client_anchor_release(client, terminal_id, anchor);
}

fn resultError(result: c.PhuxClientResult) Error!void {
    return switch (result) {
        c.PHUX_CLIENT_OK => {},
        c.PHUX_CLIENT_NO_VALUE => error.NoValue,
        c.PHUX_CLIENT_INVALID_ARGUMENT, c.PHUX_CLIENT_INVALID_STATE => error.InvalidState,
        c.PHUX_CLIENT_PROTOCOL_ERROR => error.Protocol,
        c.PHUX_CLIENT_ENGINE_ERROR => error.Engine,
        c.PHUX_CLIENT_OUT_OF_MEMORY => error.OutOfMemory,
        else => error.Panic,
    };
}

fn resultErrorWithContext(client: *c.PhuxClient, operation: []const u8, result: c.PhuxClientResult) Error!void {
    resultError(result) catch |err| {
        var raw: c.PhuxBytes = undefined;
        if (c.phux_client_last_error(client, &raw) == c.PHUX_CLIENT_OK) {
            const message = effectSlice(raw) catch &.{};
            if (message.len != 0) std.log.err("Phux client {s}: {s}", .{ operation, message });
        }
        return err;
    };
}

fn outboundSize(len: usize) !void {
    if (len > c.PHUX_CLIENT_MAX_OUTBOUND_BYTES) return error.InvalidState;
}

fn copySessionSummary(gpa: std.mem.Allocator, raw: c.PhuxSessionInfo) !SessionSummary {
    const name = try effectSlice(raw.name);
    if (raw.session_id == 0 or name.len > max_session_name_bytes) return error.Protocol;
    _ = std.unicode.Utf8View.init(name) catch return error.Protocol;
    return .{
        .id = raw.session_id,
        .name = gpa.dupe(u8, name) catch return error.OutOfMemory,
        .created_at_unix_secs = raw.created_at_unix_secs,
        .window_count = raw.window_count,
        .attached_client_count = raw.attached_client_count,
        .focused = raw.focused,
    };
}

/// One resource-catalog row, built by hand. The shape the ABI publishes, with
/// the size/version stamp `phux_client_resource_get` requires, so the decode
/// under test is the same one a live catalog read goes through.
fn resourceInfoFixture(
    id: u32,
    kind: u32,
    parent: [*c]const c.PhuxResourceId,
    provider_name: []const u8,
    native_id: []const u8,
    state: []const u8,
) c.PhuxResourceInfo {
    var raw = workspace_bridge.record(c.PhuxResourceInfo);
    raw.terminal_id = .{ .kind = c.PHUX_RESOURCE_ID_LOCAL, .id = id, .host = bytes("") };
    raw.kind = kind;
    raw.parent = parent;
    raw.provider = bytes(provider_name);
    raw.native_id = bytes(native_id);
    raw.state = bytes(state);
    return raw;
}

fn agentRecordsEffect(id: u32, detail: u32, payload: []const u8) c.PhuxClientEffect {
    return .{
        .kind = c.PHUX_CLIENT_EFFECT_AGENT_RECORDS,
        .detail = detail,
        .status_code = 0,
        .terminal_id = .{ .kind = c.PHUX_RESOURCE_ID_LOCAL, .id = id, .host = bytes("") },
        .stream_id = 4,
        .bootstrap_id = 5,
        .seq = 21,
        .first_row = 0,
        .last_row = 0,
        .bytes = bytes(payload),
    };
}

fn adoptCatalogFixture(host: *Host, catalog: []const c.PhuxResourceInfo) !void {
    var entries: std.ArrayListUnmanaged(agent_sessions.Entry) = .empty;
    defer entries.deinit(host.gpa);
    for (catalog) |raw| {
        try host.appendAgentEntry(&entries, raw);
    }
    try host.adoptAgentEntries(entries.items);
}

test "the resource catalog projects agent rows under a parent and never a replica" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const parent_raw: c.PhuxResourceId = .{ .kind = c.PHUX_RESOURCE_ID_LOCAL, .id = 7, .host = bytes("") };
    const catalog = [_]c.PhuxResourceInfo{
        resourceInfoFixture(7, c.PHUX_RESOURCE_TERMINAL, null, "", "", ""),
        resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, &parent_raw, "claude", "sess-1", "working"),
        resourceInfoFixture(11, c.PHUX_RESOURCE_AGENT_SESSION, &parent_raw, "codex", "sess-2", "done"),
        // A kind this header does not name is opaque and is never a terminal.
        resourceInfoFixture(13, 99, &parent_raw, "", "", ""),
    };
    try adoptCatalogFixture(host, &catalog);

    const parent_ref = phuxRef(try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 7, ""));
    var rows: [max_agent_sessions]*const AgentSession = undefined;
    try std.testing.expectEqual(@as(usize, 2), host.agentSessionsUnder(parent_ref, &rows));
    try std.testing.expectEqualStrings("claude", rows[0].provider_name);
    try std.testing.expectEqualStrings("sess-1", rows[0].native_id);
    try std.testing.expectEqual(AgentState.working, rows[0].state());
    try std.testing.expect(rows[0].parentRef().?.eql(parent_ref));
    try std.testing.expectEqual(AgentState.done, rows[1].state());
    try std.testing.expect(!host.agentAttention(parent_ref));

    // A row is not a surface: no replica was minted, the terminal roster is
    // untouched, and the identity is refused if anything tries to mint one.
    try std.testing.expectEqual(@as(usize, 0), host.terminals.items.len);
    try std.testing.expect(host.isAgentSession(rows[0].ref()));
    try std.testing.expect(!host.contains(rows[0].ref()));
    try std.testing.expectEqual(@as(?provider.ReplicaOwner, null), host.owner(rows[0].ref()));
    try std.testing.expectError(error.Protocol, host.ensureTerminal(catalog[1].terminal_id));
    // The parent terminal is admitted the ordinary way, side by side with it.
    _ = try host.ensureTerminal(parent_raw);
    try std.testing.expectEqual(@as(usize, 1), host.terminals.items.len);
}

test "hand-built AGENT_RECORDS batches move one row and close it" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);

    const parent_raw: c.PhuxResourceId = .{ .kind = c.PHUX_RESOURCE_ID_LOCAL, .id = 7, .host = bytes("") };
    try adoptCatalogFixture(host, &[_]c.PhuxResourceInfo{
        resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, &parent_raw, "claude", "sess-1", "unknown"),
    });
    const parent_ref = phuxRef(try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 7, ""));
    const agent_id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 9, "");

    const retained =
        "{\"seq\":1,\"ts_ms\":10,\"type\":\"session_start\",\"data\":{\"provider\":\"claude\"}}\n" ++
        "{\"seq\":2,\"ts_ms\":20,\"type\":\"prompt\",\"data\":{\"chars\":4}}\n";
    var effect = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_RETAINED, retained);
    try host.captureAgentRecords(&effect);
    try std.testing.expectEqual(AgentState.working, host.agents.findConst(agent_id).?.state());
    try std.testing.expect(host.metadata_changed);
    try std.testing.expect(!host.agentAttention(parent_ref));

    host.metadata_changed = false;
    effect = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_LIVE, "{\"seq\":3,\"ts_ms\":30,\"type\":\"notification\",\"data\":{\"kind\":\"permission\"}}\n");
    try host.captureAgentRecords(&effect);
    try std.testing.expectEqual(AgentState.blocked, host.agents.findConst(agent_id).?.state());
    try std.testing.expect(host.metadata_changed);
    try std.testing.expect(host.agentAttention(parent_ref));

    // Narration is quiet: no state edge, so nothing is republished.
    host.metadata_changed = false;
    effect = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_LIVE, "{\"seq\":4,\"ts_ms\":40,\"type\":\"tool_end\",\"data\":{}}\n");
    try host.captureAgentRecords(&effect);
    try std.testing.expect(!host.metadata_changed);

    // An additive records kind is dropped rather than guessed at or refused.
    effect = agentRecordsEffect(9, 99, "{\"type\":\"stop\",\"data\":{}}\n");
    try host.captureAgentRecords(&effect);
    try std.testing.expectEqual(AgentState.blocked, host.agents.findConst(agent_id).?.state());

    // CLOSED retires the row, and with it the attention it was raising.
    effect = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_CLOSED, "");
    try host.captureAgentRecords(&effect);
    try std.testing.expectEqual(@as(usize, 0), host.agentSessions().len);
    try std.testing.expect(!host.agentAttention(parent_ref));
    try std.testing.expect(host.metadata_changed);
}

test "a live records payload past the frame ceiling is a protocol failure" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const oversized = try std.testing.allocator.alloc(u8, max_agent_records_bytes + 1);
    defer std.testing.allocator.free(oversized);
    @memset(oversized, 'x');
    var effect = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_LIVE, oversized);
    try std.testing.expectError(error.Protocol, host.captureAgentRecords(&effect));
}

test "retained agent backlog above one append remains valid" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try adoptCatalogFixture(host, &.{resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, null, "claude", "session", "working")});
    const line = "{\"type\":\"provider_raw\",\"data\":{}}\n";
    const repeats = (64 * 1024) / line.len + 1;
    const ask = "{\"type\":\"ask\",\"data\":{\"question\":\"still here?\"}}\n";
    const payload = try std.testing.allocator.alloc(u8, repeats * line.len + ask.len);
    defer std.testing.allocator.free(payload);
    for (0..repeats) |index| @memcpy(payload[index * line.len ..][0..line.len], line);
    @memcpy(payload[repeats * line.len ..], ask);
    var effect = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_RETAINED, payload);
    try host.captureAgentRecords(&effect);
    try std.testing.expectEqual(AgentState.blocked, host.agentSessions()[0].state());
    // A stamped live batch is bounded by the frame, not the producer input.
    effect.detail = c.PHUX_CLIENT_AGENT_RECORDS_LIVE;
    try host.captureAgentRecords(&effect);
    try std.testing.expectEqual(AgentState.blocked, host.agentSessions()[0].state());
}

test "ABI generations fence evidence and same-state updates invalidate metadata" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try adoptCatalogFixture(host, &.{resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, null, "claude", "session", "working")});
    var effect = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_RETAINED, "{\"seq\":1,\"type\":\"ask\",\"data\":{\"question\":\"first?\"}}");
    try host.captureAgentRecords(&effect);
    host.metadata_changed = false;
    effect.detail = c.PHUX_CLIENT_AGENT_RECORDS_LIVE;
    effect.bytes = bytes("{\"seq\":2,\"type\":\"ask\",\"data\":{\"question\":\"second?\"}}");
    try host.captureAgentRecords(&effect);
    try std.testing.expect(host.metadata_changed);
    try std.testing.expectEqualStrings("second?", host.agentSessions()[0].latest_evidence.?.reason.slice());
    const old = effect;
    effect.detail = c.PHUX_CLIENT_AGENT_RECORDS_RETAINED;
    effect.bootstrap_id += 1;
    effect.bytes = bytes("{\"seq\":3,\"type\":\"stop\"}");
    try host.captureAgentRecords(&effect);
    try std.testing.expectEqual(AgentState.done, host.agentSessions()[0].state());
    host.metadata_changed = false;
    effect = old;
    try host.captureAgentRecords(&effect);
    effect.detail = c.PHUX_CLIENT_AGENT_RECORDS_CLOSED;
    effect.bytes = bytes("");
    try host.captureAgentRecords(&effect);
    try std.testing.expect(!host.metadata_changed);
    try std.testing.expectEqual(@as(usize, 1), host.agentSessions().len);
    try std.testing.expectEqual(AgentState.done, host.agentSessions()[0].state());
}

test "old ABI generation cannot change or retire a replacement agent" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try adoptCatalogFixture(host, &.{resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, null, "claude", "session", "working")});
    var old = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_RETAINED, "{\"seq\":1,\"type\":\"ask\"}");
    try host.captureAgentRecords(&old);
    var current = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_RETAINED, "{\"seq\":2,\"type\":\"stop\"}");
    current.bootstrap_id += 1;
    try host.captureAgentRecords(&current);
    try std.testing.expectEqual(AgentState.done, host.agentSessions()[0].state());
    old.detail = c.PHUX_CLIENT_AGENT_RECORDS_LIVE;
    old.bytes = bytes("{\"seq\":3,\"type\":\"prompt\"}");
    try host.captureAgentRecords(&old);
    try std.testing.expectEqual(AgentState.done, host.agentSessions()[0].state());
    old.detail = c.PHUX_CLIENT_AGENT_RECORDS_CLOSED;
    old.bytes = bytes("");
    try host.captureAgentRecords(&old);
    try std.testing.expectEqual(@as(usize, 1), host.agentSessions().len);
}

test "old ABI closure cannot retire a replacement agent" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try adoptCatalogFixture(host, &.{resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, null, "claude", "session", "working")});
    var effect = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_RETAINED, "{\"seq\":1,\"type\":\"ask\"}");
    try host.captureAgentRecords(&effect);
    effect.bootstrap_id += 1;
    effect.bytes = bytes("{\"seq\":2,\"type\":\"stop\"}");
    try host.captureAgentRecords(&effect);
    effect.bootstrap_id -= 1;
    effect.detail = c.PHUX_CLIENT_AGENT_RECORDS_CLOSED;
    effect.bytes = bytes("");
    try host.captureAgentRecords(&effect);
    try std.testing.expectEqual(@as(usize, 1), host.agentSessions().len);
}

test "same-drain reintroduced catalog membership survives queued older CLOSED" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const parent_raw: c.PhuxResourceId = .{ .kind = c.PHUX_RESOURCE_ID_LOCAL, .id = 7, .host = bytes("") };
    const parent_ref = phuxRef(try remoteFromC(parent_raw));
    const catalog = [_]c.PhuxResourceInfo{resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, &parent_raw, "claude", "session", "working")};
    try adoptCatalogFixture(host, &catalog);
    var effect = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_RETAINED, "{\"seq\":1,\"type\":\"ask\"}");
    try host.captureAgentRecords(&effect);
    try std.testing.expect(host.agentAttention(parent_ref));
    // The host reads the final catalog before any accumulated effects. Feed
    // the membership result of omission followed by reappearance in one drain.
    try adoptCatalogFixture(host, &catalog);
    host.metadata_changed = false;
    effect.detail = c.PHUX_CLIENT_AGENT_RECORDS_CLOSED;
    effect.bytes = bytes("");
    try host.captureAgentRecordDelivery(&effect, true);
    try std.testing.expectEqual(@as(usize, 1), host.agentSessions().len);
    // Reattachment is asynchronous: the next retained batch may be delayed or
    // refused. The ended generation must stop claiming attention immediately.
    try std.testing.expectEqual(AgentState.working, host.agentSessions()[0].state());
    try std.testing.expect(host.agentSessions()[0].latest_evidence == null);
    try std.testing.expect(host.agentSessions()[0].generation == null);
    try std.testing.expect(host.agentSessions()[0].last_record_seq == null);
    try std.testing.expect(!host.agentAttention(parent_ref));
    try std.testing.expect(host.metadata_changed);
    host.metadata_changed = false;
    try adoptCatalogFixture(host, &catalog);
    effect.detail = c.PHUX_CLIENT_AGENT_RECORDS_RETAINED;
    effect.bytes = bytes("{\"seq\":1,\"type\":\"ask\"}");
    try host.captureAgentRecordDelivery(&effect, false);
    try std.testing.expect(host.agentSessions()[0].generation == null);
    try std.testing.expect(!host.metadata_changed);
    effect.detail = c.PHUX_CLIENT_AGENT_RECORDS_RETAINED;
    effect.bootstrap_id += 1;
    effect.bytes = bytes("{\"seq\":2,\"type\":\"provider_raw\"}");
    try host.captureAgentRecordDelivery(&effect, false);
    try std.testing.expectEqual(AgentState.working, host.agentSessions()[0].state());
    try std.testing.expect(host.agentSessions()[0].latest_evidence == null);
    host.metadata_changed = false;
    var stale_close = effect;
    stale_close.bootstrap_id -= 1;
    stale_close.detail = c.PHUX_CLIENT_AGENT_RECORDS_CLOSED;
    stale_close.bytes = bytes("");
    try host.captureAgentRecordDelivery(&stale_close, true);
    try std.testing.expectEqual(effect.bootstrap_id, host.agentSessions()[0].generation.?.bootstrap_id);
    try std.testing.expect(!host.metadata_changed);
    effect.detail = c.PHUX_CLIENT_AGENT_RECORDS_CLOSED;
    effect.bytes = bytes("");
    try host.captureAgentRecordDelivery(&effect, false);
    try std.testing.expectEqual(@as(usize, 0), host.agentSessions().len);
}

test "offline and reconnecting agents retain evidence without actionable attention" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const parent_raw: c.PhuxResourceId = .{ .kind = c.PHUX_RESOURCE_ID_LOCAL, .id = 7, .host = bytes("") };
    try adoptCatalogFixture(host, &.{resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, &parent_raw, "claude", "session", "working")});
    var effect = agentRecordsEffect(9, c.PHUX_CLIENT_AGENT_RECORDS_RETAINED, "{\"seq\":1,\"type\":\"ask\",\"data\":{\"question\":\"wait?\"}}");
    try host.captureAgentRecords(&effect);
    const parent_ref = phuxRef(try remoteFromC(parent_raw));
    try std.testing.expect(host.agentAttention(parent_ref));
    host.disconnect();
    try std.testing.expect(!host.agentAttention(parent_ref));
    try std.testing.expectEqualStrings("wait?", host.agentSessions()[0].latest_evidence.?.reason.slice());
    try host.reconnect("offline-test");
    try std.testing.expect(!host.agentAttention(parent_ref));
    // A new connection's catalog is authoritative even if it reuses IDs.
    try adoptCatalogFixture(host, &.{resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, &parent_raw, "claude", "session", "working")});
    try std.testing.expect(host.agentSessions()[0].latest_evidence == null);
}

test "catalog filtering budgets agents independently and publishes same-size changes" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    var entries: std.ArrayListUnmanaged(agent_sessions.Entry) = .empty;
    defer entries.deinit(std.testing.allocator);
    // Exercise the same bounded filter that the production catalog read uses.
    for (0..max_catalog_terminals + 1) |index| {
        try host.appendAgentEntry(&entries, resourceInfoFixture(@intCast(index + 1), c.PHUX_RESOURCE_TERMINAL, null, "", "", ""));
    }
    try host.appendAgentEntry(&entries, resourceInfoFixture(9000, c.PHUX_RESOURCE_AGENT_SESSION, null, "claude", "old", "working"));
    try host.adoptAgentEntries(entries.items);
    try std.testing.expectEqual(@as(usize, 1), host.agentSessions().len);
    host.metadata_changed = false;
    entries.items[0].native_id = "new";
    entries.items[0].state = "blocked";
    try host.adoptAgentEntries(entries.items);
    try std.testing.expect(host.metadata_changed);
    try std.testing.expectEqualStrings("new", host.agentSessions()[0].native_id);
    try std.testing.expectEqual(AgentState.blocked, host.agentSessions()[0].state());
    host.metadata_changed = false;
    try host.adoptAgentEntries(entries.items);
    try std.testing.expect(!host.metadata_changed);
    for (1..max_agent_sessions) |index| {
        try host.appendAgentEntry(&entries, resourceInfoFixture(@intCast(9000 + index), c.PHUX_RESOURCE_AGENT_SESSION, null, "claude", "", "working"));
    }
    try std.testing.expectError(error.Protocol, host.appendAgentEntry(&entries, resourceInfoFixture(9999, c.PHUX_RESOURCE_AGENT_SESSION, null, "claude", "", "working")));
    try std.testing.expectEqual(max_agent_sessions, entries.items.len);
}

test "an attach refresh drops the rows the new snapshot omits" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const parent_raw: c.PhuxResourceId = .{ .kind = c.PHUX_RESOURCE_ID_LOCAL, .id = 7, .host = bytes("") };
    try adoptCatalogFixture(host, &[_]c.PhuxResourceInfo{
        resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, &parent_raw, "claude", "sess-1", "working"),
        resourceInfoFixture(11, c.PHUX_RESOURCE_AGENT_SESSION, &parent_raw, "codex", "sess-2", "working"),
    });
    var effect = agentRecordsEffect(11, c.PHUX_CLIENT_AGENT_RECORDS_LIVE, "{\"type\":\"ask\",\"data\":{\"question\":\"go?\"}}\n");
    try host.captureAgentRecords(&effect);

    // ParentClosed reaches this projection as an ordinary absence of the child.
    try adoptCatalogFixture(host, &[_]c.PhuxResourceInfo{
        resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, &parent_raw, "claude", "sess-1", "working"),
    });
    const parent_ref = phuxRef(try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 7, ""));
    try std.testing.expectEqual(@as(usize, 1), host.agentSessions().len);
    try std.testing.expect(!host.agentAttention(parent_ref));
}

test "a parentless catalog row is carried but projected under nothing" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    try adoptCatalogFixture(host, &[_]c.PhuxResourceInfo{
        resourceInfoFixture(9, c.PHUX_RESOURCE_AGENT_SESSION, null, "claude", "sess-1", "blocked"),
    });
    try std.testing.expectEqual(@as(usize, 1), host.agentSessions().len);
    try std.testing.expectEqual(@as(?provider.TerminalRef, null), host.agentSessions()[0].parentRef());
    const parent_ref = phuxRef(try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 7, ""));
    var rows: [max_agent_sessions]*const AgentSession = undefined;
    try std.testing.expectEqual(@as(usize, 0), host.agentSessionsUnder(parent_ref, &rows));
    try std.testing.expect(!host.agentAttention(parent_ref));
}

test "contains hides terminals until their canvas is published" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 7, "");
    try host.terminals.append(host.gpa, .{ .id = id });
    const terminal_ref = phuxRef(id);

    try std.testing.expect(!host.contains(terminal_ref));
    host.terminals.items[0].published = true;
    try std.testing.expect(host.contains(terminal_ref));
}

test "session summaries copy every server field and own their names" {
    var source = [_]u8{ 'b', 'u', 'i', 'l', 'd' };
    var summary = try copySessionSummary(std.testing.allocator, .{
        .session_id = 17,
        .name = bytes(&source),
        .created_at_unix_secs = 1234,
        .window_count = 3,
        .attached_client_count = 2,
        .focused = true,
    });
    defer summary.deinit(std.testing.allocator);

    source[0] = 'x';
    try std.testing.expectEqual(@as(u32, 17), summary.id);
    try std.testing.expectEqualStrings("build", summary.name);
    try std.testing.expectEqual(@as(i64, 1234), summary.created_at_unix_secs);
    try std.testing.expectEqual(@as(u16, 3), summary.window_count);
    try std.testing.expectEqual(@as(u16, 2), summary.attached_client_count);
    try std.testing.expect(summary.focused);

    var invalid_utf8 = [_]u8{0xff};
    try std.testing.expectError(error.Protocol, copySessionSummary(std.testing.allocator, .{
        .session_id = 18,
        .name = bytes(&invalid_utf8),
        .created_at_unix_secs = 0,
        .window_count = 0,
        .attached_client_count = 0,
        .focused = false,
    }));
}

test "a listing's digest moves when a session gains or loses its windows, not only its name" {
    // phux-c2td.31: a listing peer's keep-empty session that gains a window
    // keeps its id and name. The digest hashed only those, so the refresh
    // reported no change and its row kept the Empty session label.
    var name = [_]u8{ 's', 'c', 'r', 'a', 't', 'c', 'h' };
    const empty: SessionSummary = .{
        .id = 3,
        .name = &name,
        .created_at_unix_secs = 0,
        .window_count = 0,
        .attached_client_count = 0,
        .focused = false,
        .keep_empty = true,
        .empty = true,
    };
    const baseline = Host.sessionsDigest(&.{empty}, 1);
    try std.testing.expectEqual(baseline, Host.sessionsDigest(&.{empty}, 1));

    var windowed = empty;
    windowed.window_count = 1;
    windowed.empty = false;
    try std.testing.expect(Host.sessionsDigest(&.{windowed}, 1) != baseline);
    var counted = empty;
    counted.window_count = 2;
    try std.testing.expect(Host.sessionsDigest(&.{counted}, 1) != baseline);
    var unflagged = empty;
    unflagged.keep_empty = false;
    try std.testing.expect(Host.sessionsDigest(&.{unflagged}, 1) != baseline);
    var not_empty = empty;
    not_empty.empty = false;
    try std.testing.expect(Host.sessionsDigest(&.{not_empty}, 1) != baseline);
}

test "grid damage freezes a published canvas until replacement copy" {
    var published: Terminal = .{
        .id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 8, ""),
        .phase = .live,
        .published = true,
    };
    markGridDirty(&published, true);
    try std.testing.expectEqual(provider.Phase.frozen, published.phase);
    try std.testing.expect(published.dirty);
    try std.testing.expect(published.seen_in_attach);

    var reconnecting: Terminal = .{
        .id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 9, ""),
        .phase = .reconnecting,
        .published = true,
    };
    markGridDirty(&reconnecting, false);
    try std.testing.expectEqual(provider.Phase.reconnecting, reconnecting.phase);

    var unpublished: Terminal = .{
        .id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 10, ""),
    };
    markGridDirty(&unpublished, true);
    try std.testing.expectEqual(provider.Phase.attaching, unpublished.phase);
    try std.testing.expect(!unpublished.published);
}

test "publish failure after attach freezes an existing canvas" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 10, "");
    try host.terminals.append(host.gpa, .{
        .id = id,
        .phase = .live,
        .dirty = true,
        .seen_in_attach = true,
        .published = true,
    });
    host.attach_barrier_seen = true;
    const original_allocator = host.gpa;
    host.gpa = std.testing.failing_allocator;
    defer host.gpa = original_allocator;

    try std.testing.expectError(error.OutOfMemory, host.capturePublishStage());
    try std.testing.expectEqual(provider.Phase.frozen, host.terminals.items[0].phase);
}

test "ATTACHED inventory remains pixel-invisible until READY publication" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 21, "");
    const terminal = try host.ensureTerminal(cId(&id));
    terminal.seen_in_attach = true;
    terminal.dirty = true;
    try terminal.canvas.screen_text.appendSlice(host.gpa, "staged pixels");
    const terminal_ref = terminal.terminalRef();

    const before_ready = try host.drainReadiness();
    try std.testing.expect(!before_ready.ready_published);
    try std.testing.expect(!host.contains(terminal_ref));
    try std.testing.expect(host.presentation(terminal_ref) == null);
    var refs: [1]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 0), host.terminalRefs(&refs));

    // Publication is the host-side effect of the C client's dual READY fence.
    terminal.generation = .{ .stream_id = 7, .bootstrap_id = 9, .last_seq = 4 };
    terminal.phase = .live;
    terminal.published = true;
    terminal.dirty = false;
    const ready = host.presentation(terminal_ref).?;
    try std.testing.expectEqualStrings("staged pixels", ready.grid.screen_text);
    try std.testing.expectEqual(provider.Phase.live, ready.phase);
    try std.testing.expect(ready.grid.running);
}

test "generation fences stale completion while sequence progress remains current" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 22, "");
    try host.terminals.append(host.gpa, .{
        .id = id,
        .generation = .{ .stream_id = 30, .bootstrap_id = 40, .last_seq = 1 },
        .phase = .live,
        .published = true,
    });
    const terminal = &host.terminals.items[0];
    const old_owner = terminal.owner();

    terminal.generation.last_seq = 200;
    try std.testing.expect(host.ownerIsCurrent(old_owner));

    terminal.generation.bootstrap_id = 41;
    const current_owner = terminal.owner();
    host.search_owner = current_owner;
    try host.search_results.append(host.gpa, .{
        .start = .{ .opaque_id = 0 },
        .end = .{ .opaque_id = 0 },
    });

    host.clearSearchResults(old_owner);
    try std.testing.expectEqual(@as(usize, 1), host.search_results.items.len);
    try std.testing.expect(host.search_owner.?.eql(current_owner));
    try std.testing.expect(!host.ownerIsCurrent(old_owner));
    try std.testing.expectError(error.InvalidState, host.sendFocus(old_owner, true));

    terminal.generation.last_seq += 1;
    try std.testing.expect(host.ownerIsCurrent(current_owner));
    host.clearSearchResults(current_owner);
    try std.testing.expectEqual(@as(usize, 0), host.search_results.items.len);
    try std.testing.expect(host.search_owner == null);
}

test "reconnect freezes complete canvases and preserves terminal identities" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const first_id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 31, "");
    const second_id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_SATELLITE, 8, "build-host");
    try host.terminals.append(host.gpa, .{
        .id = first_id,
        .generation = .{ .stream_id = 1, .bootstrap_id = 2, .last_seq = 3 },
        .phase = .live,
        .published = true,
    });
    try host.terminals.append(host.gpa, .{
        .id = second_id,
        .generation = .{ .stream_id = 4, .bootstrap_id = 5, .last_seq = 6 },
        .phase = .live,
        .published = true,
    });
    try host.terminals.items[0].canvas.screen_text.appendSlice(host.gpa, "first complete grid");
    try host.terminals.items[1].canvas.screen_text.appendSlice(host.gpa, "second complete grid");

    var before: [2]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 2), host.terminalRefs(&before));
    const old_owner = host.owner(before[0]).?;

    try host.reconnect("cockpit");
    try std.testing.expectEqual(State.hello_queued, host.state());
    var after: [2]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 2), host.terminalRefs(&after));
    try std.testing.expect(before[0].eql(after[0]));
    try std.testing.expect(before[1].eql(after[1]));
    try std.testing.expect(!host.ownerIsCurrent(old_owner));

    const first = host.presentation(before[0]).?;
    const second = host.presentation(before[1]).?;
    try std.testing.expectEqual(provider.Phase.reconnecting, first.phase);
    try std.testing.expectEqual(provider.Phase.reconnecting, second.phase);
    try std.testing.expect(!first.grid.running);
    try std.testing.expect(!second.grid.running);
    try std.testing.expectEqualStrings("first complete grid", first.grid.screen_text);
    try std.testing.expectEqualStrings("second complete grid", second.grid.screen_text);
}

test "replacement session admits one terminal after sixteen old replicas" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    for (0..max_terminals) |index| {
        const id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, @intCast(index + 100), "");
        const terminal = try host.ensureTerminal(cId(&id));
        terminal.published = true;
        terminal.phase = .live;
    }
    host.clearSessionReplicas();
    try host.reconnect("replacement-session");
    try test_support.stageFixture(&bridge, "hello.bin");
    _ = try host.drainReadiness();
    try host.attachSessionId(1, .{ .cols = 80, .rows = 24 });
    // Real captureEffects admits the new terminal before the READY prune.
    try test_support.stageFixture(&bridge, "attached.bin");
    const delta = try host.drainReadiness();
    const replacement = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 7, "");
    try std.testing.expect(delta.ready_published);
    try std.testing.expectEqual(@as(usize, 1), host.terminals.items.len);
    try std.testing.expect(host.terminalKnown(phuxRef(replacement)));
}

test "same-session reconnect replaces one of sixteen replicas before admitting new effects" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try host.start("full-inventory");
    try test_support.stageFixture(&bridge, "hello.bin");
    _ = try host.drainReadiness();
    try host.attachSessionId(1, .{ .cols = 80, .rows = 24 });
    var offset: usize = 0;
    try test_support.stageFrames(&bridge, @embedFile("fixtures/reconnect_initial.bin"), &offset, 50);
    try std.testing.expect((try host.drainReadiness()).ready_published);
    try std.testing.expectEqual(max_terminals, host.terminals.items.len);
    try std.testing.expectEqual(@as(?u32, 1), host.selectedSessionId());
    const survivor = phuxRef(try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 7, ""));
    const removed = phuxRef(try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 22, ""));
    const replacement = phuxRef(try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 23, ""));
    const old_grid = host.presentation(survivor).?.grid.screen_text;
    const frozen = try host.capturePresentation(host.owner(survivor).?);
    defer frozen.destroy();
    const removed_frozen = try host.capturePresentation(host.owner(removed).?);
    defer removed_frozen.destroy();

    try host.reconnect("full-inventory");
    try test_support.stageFixture(&bridge, "hello.bin");
    _ = try host.drainReadiness();
    try host.attachSessionId(1, .{ .cols = 80, .rows = 24 });
    // The complete ATTACHED roster proves 22 obsolete before any new TITLE or
    // DAMAGE effect needs its slot. Same-ID borrowed grids survive until READY.
    offset = 0;
    const encoded = @embedFile("fixtures/reconnect_replacement.bin");
    try test_support.stageFrames(&bridge, encoded, &offset, 1);
    const roster_delta = try host.drainReadiness();
    try std.testing.expect(!roster_delta.ready_published);
    try std.testing.expectEqual(@as(?u32, 1), host.selectedSessionId());
    try test_support.stageFrames(&bridge, encoded, &offset, 48);
    const bootstrap_delta = try host.drainReadiness();
    try std.testing.expect(!bootstrap_delta.ready_published);
    const retained = host.presentation(survivor).?;
    try std.testing.expectEqual(provider.Phase.reconnecting, retained.phase);
    try std.testing.expectEqual(@intFromPtr(old_grid.ptr), @intFromPtr(retained.grid.screen_text.ptr));
    try std.testing.expectEqualStrings(frozen.value.grid.screen_text, retained.grid.screen_text);
    try test_support.stageFrames(&bridge, encoded, &offset, 1);
    const ready_delta = try host.drainReadiness();
    try std.testing.expect(ready_delta.ready_published);
    try std.testing.expectEqual(@as(usize, 1), roster_delta.removed_count + bootstrap_delta.removed_count + ready_delta.removed_count);
    try std.testing.expectEqual(max_terminals, host.terminals.items.len);
    try std.testing.expect(!host.terminalKnown(removed));
    try std.testing.expectEqual(provider.Phase.live, host.presentation(replacement).?.phase);
    try std.testing.expectEqualStrings("replacement", host.presentation(replacement).?.title);
    try std.testing.expect(std.mem.startsWith(u8, host.presentation(replacement).?.grid.screen_text, "NEW REPLICA"));
    try std.testing.expectEqualStrings(frozen.value.grid.screen_text, removed_frozen.value.grid.screen_text);
}

test "workspace C publication owns borrowed bytes and rejects invalid capacity atomically" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const before = host.workspaceSnapshot();
    try std.testing.expect(before.windows.len != 0);
    const saved_name = try host.gpa.dupe(u8, before.windows[0].name.slice());
    defer host.gpa.free(saved_name);
    const saved_id = before.windows[0].id;
    var borrowed = workspace_bridge.record(c.PhuxWorkspaceWindow);
    try resultError(c.phux_client_workspace_window_get(host.client, 0, &borrowed));
    try std.testing.expect(borrowed.name.len != 0);
    try std.testing.expect(@intFromPtr(borrowed.name.data) != @intFromPtr(before.windows[0].name.slice().ptr));
    // The FFI invalidates all borrows; the host snapshot remains independent.
    try resultError(c.phux_client_effect_clear(host.client));
    try std.testing.expectEqualStrings(saved_name, host.workspaceSnapshot().windows[0].name.slice());
    var raw = workspace_bridge.record(c.PhuxWorkspaceInfo);
    try resultError(c.phux_client_workspace_info(host.client, &raw));
    raw.window_count = workspace.max_windows + 1;
    try std.testing.expectError(error.WorkspaceCapacity, host.workspace_store.captureInfo(host.gpa, host.client, raw));
    raw.window_count = 0;
    raw.node_count = workspace.max_nodes + 1;
    try std.testing.expectError(error.WorkspaceCapacity, host.workspace_store.captureInfo(host.gpa, host.client, raw));
    raw.node_count = 0;
    raw.terminal_count = workspace.max_terminals + 1;
    try std.testing.expectError(error.WorkspaceCapacity, host.workspace_store.captureInfo(host.gpa, host.client, raw));
    try std.testing.expectEqual(before.revision, host.workspaceSnapshot().revision);
    try std.testing.expectEqualSlices(u8, &saved_id, &host.workspaceSnapshot().windows[0].id);
    try std.testing.expectEqualStrings(saved_name, host.workspaceSnapshot().windows[0].name.slice());
}

test "workspace refresh shares spawn IDs and discovers other sessions without replicas" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    try std.testing.expectEqual(workspace.Status.confirmed, host.workspaceSnapshot().status);
    try std.testing.expectEqual(@as(u32, 1), try host.requestSpawn(null, .{ .cols = 80, .rows = 24 }));
    try test_support.stageFixture(&bridge, "spawn-local.bin");
    _ = try host.drainReadiness();
    try std.testing.expectEqual(@as(u32, 1), host.takeOperationResult().?.request_id);
    var refs: [max_catalog_terminals]provider.TerminalRef = undefined;
    // A successful spawn is an admission record, not an invented roster row.
    try std.testing.expectEqual(@as(usize, 1), host.catalogRefs(&refs));
    bridge.outgoing.reset();
    const replicas = host.terminals.items.len;
    const old_revision = host.workspaceSnapshot().revision;
    try std.testing.expectEqual(@as(?u32, 2), try host.requestWorkspaceRefresh());
    try std.testing.expectEqual(@as(?u32, null), try host.requestWorkspaceRefresh());
    try std.testing.expectEqual(@as(u32, 2), host.operation_ledger.last_id);
    try std.testing.expectEqual(@as(usize, 0), host.operation_ledger.len);
    try std.testing.expectEqual(workspace.Status.pending, host.workspaceSnapshot().status);
    try test_support.expectOutgoingCount(&bridge, 2);
    try test_support.stageWorkspaceFixture(&bridge, "workspace_refresh_metadata.bin");
    _ = try host.drainReadiness();
    try std.testing.expectEqual(old_revision, host.workspaceSnapshot().revision);
    try test_support.stageWorkspaceFixture(&bridge, "workspace_refresh_state.bin");
    const changed = try host.drainReadiness();
    try std.testing.expect(changed.workspace_changed and changed.metadata_changed);
    try std.testing.expect(host.workspaceSnapshot().revision > old_revision);
    try std.testing.expectEqual(@as(usize, 3), host.catalogTerminals().len);
    try std.testing.expectEqual(replicas, host.terminals.items.len);
    const external = phuxRef(try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 9, ""));
    try std.testing.expectEqual(@as(?u32, 2), host.terminalSession(external));
    try std.testing.expect(!host.terminalKnown(external));
    try std.testing.expect(!host.contains(external));
    try std.testing.expectEqual(@as(usize, 3), host.catalogRefs(&refs));
    try std.testing.expectEqual(@as(?u32, 1), host.selectedSessionId());
    try std.testing.expectEqual(@as(usize, 2), host.sessionCatalog().len);
    try std.testing.expectEqualStrings("external", host.sessionCatalog()[1].name);
    try std.testing.expectEqualStrings("unplaced terminal", host.catalogTerminals()[1].title.slice());
}

test "workspace rename split resize queue mapped requests and adopt only confirmation" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    _ = try host.requestWorkspaceRefresh();
    try completeWorkspaceFixture(host, "workspace_refresh");
    bridge.outgoing.reset();
    const window_id = host.workspaceSnapshot().windows[0].id;
    const revision = host.workspaceSnapshot().revision;
    try std.testing.expectEqual(@as(u32, 2), try host.requestWorkspaceMutation(.{
        .expected_revision = revision,
        .session_id = 1,
        .kind = .rename,
        .window_id = window_id,
        .name = "renamed",
    }));
    try std.testing.expectEqual(revision, host.workspaceSnapshot().revision);
    try std.testing.expect(!std.mem.eql(u8, "renamed", host.workspaceSnapshot().windows[0].name.slice()));
    try test_support.expectOutgoingCount(&bridge, 3);
    try completeWorkspaceFixture(host, "workspace_rename");
    try std.testing.expectEqualStrings("renamed", host.workspaceSnapshot().windows[0].name.slice());
    try std.testing.expectEqual(workspace.Status.confirmed, host.workspaceSnapshot().status);
    try std.testing.expectEqual(@as(u32, 3), try host.requestWorkspaceMutation(.{
        .expected_revision = host.workspaceSnapshot().revision,
        .session_id = 1,
        .kind = .split,
        .window_id = window_id,
        .terminal_ref = phuxRef(try RemoteId.fromPhux(0, 7, "")),
        .new_terminal_ref = phuxRef(try RemoteId.fromPhux(0, 8, "")),
        .direction = .horizontal,
        .ratio = 0.5,
    }));
    try test_support.expectOutgoingCount(&bridge, 3);
    try completeWorkspaceFixture(host, "workspace_split");
    try std.testing.expectEqual(@as(usize, 3), host.workspaceSnapshot().nodes.len);
    try std.testing.expectEqual(.horizontal, host.workspaceSnapshot().nodes[0].kind);
    try std.testing.expectEqual(@as(f32, 0.5), host.workspaceSnapshot().nodes[0].ratio);
    try std.testing.expectEqual(@as(u32, 4), try host.requestWorkspaceMutation(.{
        .expected_revision = host.workspaceSnapshot().revision,
        .session_id = 1,
        .kind = .resize,
        .window_id = window_id,
        .path_bits = 0,
        .path_len = 0,
        .ratio = 0.7,
    }));
    try test_support.expectOutgoingCount(&bridge, 3);
    try completeWorkspaceFixture(host, "workspace_resize");
    try std.testing.expectEqual(@as(f32, 0.7), host.workspaceSnapshot().nodes[0].ratio);
    try std.testing.expectEqual(@as(usize, 1), host.terminals.items.len);
    try std.testing.expectEqual(@as(usize, 0), host.operation_ledger.len);
}

fn completeWorkspaceFixture(host: *Host, comptime name: []const u8) !void {
    // Reverse delivery order also preserves the two-reply publication barrier.
    try test_support.stageWorkspaceFixture(host.bridge, name ++ "_state.bin");
    _ = try host.drainReadiness();
    try test_support.stageWorkspaceFixture(host.bridge, name ++ "_metadata.bin");
    _ = try host.drainReadiness();
}

test "old shared schema is refused without replacing last-good topology or writing metadata" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const before = host.workspaceSnapshot();
    const window_id = before.windows[0].id;
    try std.testing.expectEqual(@as(?u32, 1), try host.requestWorkspaceRefresh());
    try completeWorkspaceFixture(host, "workspace_old_schema");
    const refused = host.workspaceSnapshot();
    try std.testing.expectEqual(workspace.State.last_good_error, refused.state);
    try std.testing.expectEqual(workspace.Status.refused, refused.status);
    try std.testing.expectEqual(@as(u32, 1), refused.request_id);
    try std.testing.expect(refused.message.len != 0);
    try std.testing.expectEqual(before.revision, refused.revision);
    try std.testing.expectEqualSlices(u8, &window_id, &refused.windows[0].id);
    try std.testing.expectEqual(@as(usize, 1), host.catalogTerminals().len);
    bridge.outgoing.reset();
    try std.testing.expectError(error.InvalidState, host.requestWorkspaceMutation(.{
        .expected_revision = refused.revision,
        .session_id = 1,
        .kind = .rename,
        .window_id = window_id,
        .name = "must-not-overwrite",
    }));
    try std.testing.expect(!bridge.outgoing.hasPending());
}

test "workspace accepted request retains ID and reports unknown outcome on staging failure" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const original = bridge.outgoing.gpa;
    bridge.outgoing.gpa = std.testing.failing_allocator;
    defer bridge.outgoing.gpa = original;
    try std.testing.expectEqual(@as(?u32, 1), try host.requestWorkspaceRefresh());
    try std.testing.expect(host.disconnected);
    try std.testing.expectEqual(@as(u32, 1), host.workspaceSnapshot().request_id);
    try std.testing.expectEqual(workspace.Status.unknown_outcome, host.workspaceSnapshot().status);
    try std.testing.expectEqual(@as(usize, 0), host.operation_ledger.len);
    try std.testing.expect(!bridge.outgoing.hasPending());
}

test "workspace request status advances without allocating or replacing last-good topology" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const revision = host.workspaceSnapshot().revision;
    const state_before = host.workspaceSnapshot().state;
    const windows = host.workspaceSnapshot().windows.ptr;
    const original = host.gpa;
    host.gpa = std.testing.failing_allocator;
    defer host.gpa = original;
    try std.testing.expectEqual(@as(?u32, 1), try host.requestWorkspaceRefresh());
    try std.testing.expectEqual(@as(u32, 1), host.workspaceSnapshot().request_id);
    try std.testing.expectEqual(workspace.Status.pending, host.workspaceSnapshot().status);
    try std.testing.expectEqual(state_before, host.workspaceSnapshot().state);
    try std.testing.expectEqual(revision, host.workspaceSnapshot().revision);
    try std.testing.expectEqual(@intFromPtr(windows), @intFromPtr(host.workspaceSnapshot().windows.ptr));
    try test_support.expectOutgoingCount(&bridge, 2);
    host.disconnect();
    try std.testing.expectEqual(workspace.Status.unknown_outcome, host.workspaceSnapshot().status);
    try std.testing.expectEqual(@as(u32, 1), host.workspaceSnapshot().request_id);
}

test "reordered remote enumeration retains stable refs and lookup" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const first_id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 41, "");
    const second_id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_SATELLITE, 41, "satellite");
    const first = try host.ensureTerminal(cId(&first_id));
    first.published = true;
    first.phase = .live;
    const second = try host.ensureTerminal(cId(&second_id));
    second.published = true;
    second.phase = .live;

    var initial: [2]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 2), host.terminalRefs(&initial));
    _ = try host.ensureTerminal(cId(&second_id));
    _ = try host.ensureTerminal(cId(&first_id));

    var reordered: [2]provider.TerminalRef = undefined;
    try std.testing.expectEqual(@as(usize, 2), host.terminalRefs(&reordered));
    try std.testing.expect(initial[0].eql(reordered[0]));
    try std.testing.expect(initial[1].eql(reordered[1]));
    try std.testing.expect(host.contains(initial[0]));
    try std.testing.expect(host.contains(initial[1]));
    try std.testing.expect(!initial[0].eql(initial[1]));
}

test "every declaration in this module is compiled, not merely reachable" {
    // Zig analyzes only what is referenced, so a module can sit in the build
    // graph with its signatures never checked. Nothing calls Host.search.
    // See ref.zig.
    @import("phux_ref").refAllDeclsRecursive(@This());
}

test "satellite C identity borrows the exact owning host storage" {
    const remote = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_SATELLITE, 41, "satellite-with-exact-host");
    const raw = cId(&remote);
    try std.testing.expectEqual(@intFromPtr(remote.host().ptr), @intFromPtr(raw.host.data));
    try std.testing.expectEqualStrings("satellite-with-exact-host", raw.host.data[0..raw.host.len]);

    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    const terminal = try host.ensureTerminal(raw);
    terminal.phase = .live;
    terminal.published = true;
    const from_owner = try host.currentCId(terminal.owner());
    try std.testing.expectEqual(@intFromPtr(terminal.id.host().ptr), @intFromPtr(from_owner.host.data));
    try std.testing.expectEqualStrings("satellite-with-exact-host", from_owner.host.data[0..from_owner.host.len]);
}

test "spawn result acceptance is separate from canonical READY publication" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    try std.testing.expectEqualStrings("cockpit-fixture", host.serverId().?);
    const id = try host.requestSpawn(null, .{ .cols = 80, .rows = 24 });
    try std.testing.expectEqual(@as(u32, 1), id);
    try std.testing.expect(host.takeOperationResult() == null);
    try test_support.stageFixture(&bridge, "spawn-local.bin");
    const accepted_delta = try host.drainReadiness();
    const result = host.takeOperationResult().?;
    try std.testing.expectEqual(id, result.request_id);
    try std.testing.expectEqual(host.connectionEpoch(), result.connection_epoch);
    try std.testing.expectEqual(operations.types.Status.success, result.status);
    try std.testing.expect(!host.contains(result.terminal_ref.?));
    try std.testing.expectEqual(@as(usize, 0), accepted_delta.added_count);
    try std.testing.expectEqual(@as(usize, 0), c.phux_client_operation_count(host.client));

    try test_support.stageFixture(&bridge, "local-ready.bin");
    const ready = try host.drainReadiness();
    try std.testing.expectEqual(@as(usize, 1), ready.added_count);
    try std.testing.expect(host.contains(result.terminal_ref.?));
    try std.testing.expect(std.mem.startsWith(u8, host.presentation(result.terminal_ref.?).?.grid.screen_text, "OPERATION READY"));
}

test "detach churn leaves discovery to authoritative registry refresh" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const initial = host.terminals.items.len;
    const encoded = try test_support.readFixture("detach-churn.bin");
    defer std.testing.allocator.free(encoded);
    var offset: usize = 0;
    for (0..20) |_| {
        _ = try host.requestSpawn(null, .{ .cols = 80, .rows = 24 });
        try test_support.stageFrames(&bridge, encoded, &offset, 4);
        _ = try host.drainReadiness();
        const result = host.takeOperationResult().?;
        const ref = result.terminal_ref.?;
        const old = host.owner(ref).?;
        try std.testing.expect(host.ownerIsCurrent(old));
        _ = try host.requestDetach(ref);
        try std.testing.expect(!host.ownerIsCurrent(old));
        try std.testing.expectError(error.InvalidState, host.currentCId(old));
        try test_support.stageFrames(&bridge, encoded, &offset, 1);
        _ = try host.drainReadiness();
        try std.testing.expectEqual(operations.types.Kind.detach, host.takeOperationResult().?.kind);
        try std.testing.expectEqual(initial, host.terminals.items.len);
        try std.testing.expect(!host.contains(ref));
        bridge.outgoing.reset();
    }
    try std.testing.expectEqual(encoded.len, offset);
    var refs: [max_catalog_terminals]provider.TerminalRef = undefined;
    try std.testing.expectEqual(initial, host.catalogRefs(&refs));
    _ = try host.requestWorkspaceRefresh();
    try completeWorkspaceFixture(host, "workspace_refresh");
    try std.testing.expectEqual(@as(usize, 3), host.catalogRefs(&refs));
    try std.testing.expectEqual(initial, host.terminals.items.len);
}

test "a host mints its coordinator's refs and refuses another coordinator's terminal with the same id" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    // As a provider does: the coordinator's id before its first connection.
    const mini = provider.phuxCoordinatorId("mini");
    host.setProviderId(mini);
    try test_support.attachHost(host);
    const id = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_LOCAL, 7, "");
    const terminal = try host.ensureTerminal(cId(&id));
    terminal.published = true;
    terminal.phase = .live;
    const mine = host.refFor(id);
    try std.testing.expectEqual(mini, mine.provider_id);
    try std.testing.expect(host.contains(mine));
    try std.testing.expect(host.owner(mine).?.terminal_ref.eql(mine));
    // This Mac's terminal 7 is not this coordinator's terminal 7.
    const here = phuxRef(id);
    try std.testing.expect(!host.contains(here));
    try std.testing.expect(host.owner(here) == null);
    try std.testing.expect(host.presentation(here) == null);
    try std.testing.expectError(error.InvalidIdentity, host.requestAttach(here));
    const foreign_owner: provider.ReplicaOwner = .{ .terminal_ref = here, .generation = terminal.generation };
    try std.testing.expectError(error.InvalidState, host.sendKey(foreign_owner, &.{ .action = .press, .physical = @enumFromInt(0), .text = "x" }));
    try std.testing.expectError(error.InvalidState, host.viewportResize(here, .{ .cols = 80, .rows = 24 }));
    try test_support.expectOutgoingCount(&bridge, 0);
}

test "satellite spawn requires explicit attach and permits READY before command acknowledgment" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const satellite = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_SATELLITE, 6, "build-host");
    const owner_terminal = try host.ensureTerminal(cId(&satellite));
    owner_terminal.published = true;
    owner_terminal.phase = .live;
    _ = try host.requestSpawn(owner_terminal.terminalRef(), .{ .cols = 80, .rows = 24 });
    try test_support.expectOutgoing(&bridge, "spawn-satellite-request.bin");
    try test_support.stageFixture(&bridge, "spawn-satellite.bin");
    _ = try host.drainReadiness();
    const spawned = host.takeOperationResult().?;
    try std.testing.expectEqualStrings("build-host", spawned.terminal_ref.?.terminal_id.phux.host());
    try std.testing.expect(!host.contains(spawned.terminal_ref.?));
    // Taking the result cannot release the not-yet-ready terminal's capacity.
    try std.testing.expectEqual(@as(usize, 3), host.terminals.items.len);
    const attach_id = try host.requestAttach(spawned.terminal_ref.?);
    try std.testing.expectEqual(@as(u32, 2), attach_id);
    try test_support.expectOutgoing(&bridge, "attach-satellite-request.bin");
    try test_support.stageFixture(&bridge, "satellite-ready.bin");
    _ = try host.drainReadiness();
    try std.testing.expect(host.contains(spawned.terminal_ref.?));
    try std.testing.expect(host.takeOperationResult() == null);
    try test_support.stageFixture(&bridge, "attach-accepted.bin");
    _ = try host.drainReadiness();
    const attached = host.takeOperationResult().?;
    try std.testing.expectEqual(attach_id, attached.request_id);
    try std.testing.expectEqual(operations.types.Kind.attach, attached.kind);
    try std.testing.expect(attached.terminal_ref.?.eql(spawned.terminal_ref.?));
}

test "operation refusal owns its message and preserves the live canvas" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const owner_value = host.terminals.items[0].owner();
    _ = try host.requestSpawn(owner_value.terminal_ref, .{ .cols = 80, .rows = 24 });
    try test_support.expectOutgoing(&bridge, "spawn-owner-request.bin");
    try test_support.stageFixture(&bridge, "spawn-refused.bin");
    _ = try host.drainReadiness();
    const refused = host.takeOperationResult().?;
    try std.testing.expectEqual(operations.types.Status.refused, refused.status);
    try std.testing.expectEqual(operations.types.ErrorDomain.spawn, refused.error_domain);
    try std.testing.expectEqual(@as(u32, 1), refused.error_code);
    try std.testing.expectEqualStrings("fixture refusal", refused.message());
    try std.testing.expect(host.ownerIsCurrent(owner_value));
    try std.testing.expectEqual(@as(usize, 1), host.terminals.items.len);
    try std.testing.expectEqual(@as(u32, 2), try host.requestSpawn(null, .{ .cols = 80, .rows = 24 }));
    host.disconnect();
    try std.testing.expectEqualStrings("fixture refusal", refused.message());
}

test "attach refusal reclaims unpublished capacity and can be retried" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const remote = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_SATELLITE, 9, "build-host");
    _ = try host.requestAttach(phuxRef(remote));
    try test_support.stageFixture(&bridge, "attach-refused.bin");
    _ = try host.drainReadiness();
    const result = host.takeOperationResult().?;
    try std.testing.expectEqual(operations.types.Status.refused, result.status);
    try std.testing.expect(result.terminal_ref.?.eql(phuxRef(remote)));
    try std.testing.expectEqual(@as(usize, 1), host.terminals.items.len);
    try std.testing.expectEqual(@as(u32, 2), try host.requestAttach(phuxRef(remote)));
}

test "attach refusal after READY removes only the refused terminal" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const original_owner = host.terminals.items[0].owner();
    const remote = try RemoteId.fromPhux(c.PHUX_RESOURCE_ID_SATELLITE, 9, "build-host");
    const terminal_ref = phuxRef(remote);
    _ = try host.requestAttach(terminal_ref);
    try test_support.stageFixture(&bridge, "satellite-ready.bin");
    _ = try host.drainReadiness();
    try std.testing.expect(host.contains(terminal_ref));
    try test_support.stageFixture(&bridge, "attach-refused.bin");
    const refused_delta = try host.drainReadiness();
    try std.testing.expect(!host.contains(terminal_ref));
    try std.testing.expectEqual(@as(usize, 1), refused_delta.removed_count);
    try std.testing.expect(host.ownerIsCurrent(original_owner));
    try std.testing.expectEqual(operations.types.Status.refused, host.takeOperationResult().?.status);
}

test "queued spawn keeps its request id when transport staging fails" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const allocator = bridge.outgoing.gpa;
    bridge.outgoing.gpa = std.testing.failing_allocator;
    defer bridge.outgoing.gpa = allocator;
    const id = try host.requestSpawn(null, .{ .cols = 80, .rows = 24 });
    const result = host.takeOperationResult().?;
    try std.testing.expectEqual(id, result.request_id);
    try std.testing.expectEqual(operations.types.Status.unknown_outcome, result.status);
    try std.testing.expectEqual(State.detached, host.state());
    try std.testing.expectEqual(@as(usize, 0), c.phux_client_outgoing_count(host.client));
    try std.testing.expect(!bridge.outgoing.hasPending());
    try std.testing.expectError(error.InvalidState, host.drainReadiness());
}

test "resize viewport follows identity when effects remove an earlier terminal" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    _ = try host.requestSpawn(null, .{ .cols = 80, .rows = 24 });
    try test_support.stageFixture(&bridge, "spawn-local.bin");
    try test_support.stageFixture(&bridge, "local-ready.bin");
    _ = try host.drainReadiness();
    const terminal_ref = host.takeOperationResult().?.terminal_ref.?;
    try std.testing.expect(host.lastViewport(terminal_ref) == null);
    const closed = try test_support.readFixture("initial-terminal-closed.bin");
    defer std.testing.allocator.free(closed);
    try resultError(c.phux_client_feed_frame(host.client, closed.ptr, closed.len));
    // The C client has staged removal; host array compaction happens inside resize.
    try std.testing.expectEqual(@as(usize, 2), host.terminals.items.len);
    const viewport: provider.Viewport = .{ .cols = 80, .rows = 24 };
    try host.viewportResize(terminal_ref, viewport);
    try std.testing.expectEqual(@as(usize, 1), host.terminals.items.len);
    try std.testing.expect(host.lastViewport(terminal_ref) != null);
    try std.testing.expectEqualDeep(viewport, host.lastViewport(terminal_ref).?);
}

test "disconnect finalizes queued operations once with their old connection epoch" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const first_epoch = host.connectionEpoch();
    const first = try host.requestSpawn(null, .{ .cols = 80, .rows = 24 });
    bridge.incoming.markDisconnected(.socket_lost);
    try std.testing.expectError(error.Protocol, host.drainReadiness());
    try std.testing.expectError(error.InvalidState, host.requestSpawn(null, .{ .cols = 80, .rows = 24 }));
    bridge.incoming.reset();
    bridge.outgoing.reset();
    try host.reconnect("operations-test");
    const unknown = host.takeOperationResult().?;
    try std.testing.expectEqual(first, unknown.request_id);
    try std.testing.expectEqual(first_epoch, unknown.connection_epoch);
    try std.testing.expectEqual(operations.types.Status.unknown_outcome, unknown.status);
    try std.testing.expectEqual(first_epoch + 1, host.connectionEpoch());
    try std.testing.expect(host.takeOperationResult() == null);
    // Reconnect queues only HELLO, never the former spawn.
    const hello = bridge.outgoing.take().?;
    bridge.outgoing.release(hello);
    try std.testing.expect(bridge.outgoing.take() == null);
}

test "terminal admission preflight includes pending spawns and validates before consuming ids" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    try std.testing.expectError(error.InvalidState, host.requestSpawn(null, .{ .cols = 0, .rows = 24 }));
    for (1..max_terminals) |expected_id| {
        try std.testing.expectEqual(@as(u32, @intCast(expected_id)), try host.requestSpawn(null, .{ .cols = 80, .rows = 24 }));
    }
    const queued = bridge.outgoing.pending_bytes;
    try std.testing.expectError(error.TerminalCapacity, host.requestSpawn(null, .{ .cols = 80, .rows = 24 }));
    try std.testing.expectEqual(queued, bridge.outgoing.pending_bytes);
    host.disconnect();
    var count: usize = 0;
    while (host.takeOperationResult()) |result| {
        try std.testing.expectEqual(operations.types.Status.unknown_outcome, result.status);
        count += 1;
    }
    try std.testing.expectEqual(max_terminals - 1, count);
}

const PartialStagingCaller = enum { key, paste, publication };

fn invokePartialStagingCaller(host: *Host, owner_value: provider.ReplicaOwner, caller: PartialStagingCaller) !void {
    switch (caller) {
        .key => try host.sendKey(owner_value, &.{ .action = .press, .physical = @enumFromInt(c.PHUX_KEY_A), .text = "a" }),
        .paste => try host.sendPaste(owner_value, "partial staging paste", true),
        .publication => {
            const id = try host.currentCId(owner_value);
            try resultError(c.phux_client_send_focus(host.client, &id, false));
            try host.capturePublishStage();
        },
    }
}

fn expectPartialStagingCannotReplay(caller: PartialStagingCaller) !void {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const owner_value = host.terminals.items[0].owner();
    const epoch = host.connectionEpoch();

    // Emulate the worker handing two creation requests to the socket. One
    // reply is already in the C client; the other request has no known outcome.
    const accepted_request = try host.requestSpawn(null, .{ .cols = 80, .rows = 24 });
    const accepted_frame = bridge.outgoing.take().?;
    bridge.outgoing.release(accepted_frame);
    const reply = try test_support.readFixture("spawn-local.bin");
    defer std.testing.allocator.free(reply);
    try resultError(c.phux_client_feed_frame(host.client, reply.ptr, reply.len));
    const pending_request = try host.requestSpawn(null, .{ .cols = 80, .rows = 24 });
    const pending_frame = bridge.outgoing.take().?;
    bridge.outgoing.release(pending_frame);

    // Queue one real input frame before the caller queues the second. Reserve
    // queue metadata so allocation 1 succeeds for the prefix payload and
    // allocation 2 fails for the next payload, independent of array growth.
    const id = try host.currentCId(owner_value);
    try resultError(c.phux_client_send_focus(host.client, &id, true));
    var prefix: c.PhuxBytes = undefined;
    try resultError(c.phux_client_outgoing_get(host.client, 0, &prefix));
    const prefix_len = prefix.len;
    try bridge.outgoing.frames.ensureTotalCapacity(std.testing.allocator, transport.max_queued_frames);
    var failing = std.testing.FailingAllocator.init(std.testing.allocator, .{ .fail_index = 1 });
    bridge.outgoing.gpa = failing.allocator();
    defer bridge.outgoing.gpa = std.testing.allocator;
    try std.testing.expectError(error.OutOfMemory, invokePartialStagingCaller(host, owner_value, caller));
    try std.testing.expect(failing.has_induced_failure);
    try std.testing.expectEqual(@as(usize, 1), failing.allocations);
    try std.testing.expectEqual(prefix_len, failing.allocated_bytes);

    // A worker can consume the staged prefix and the queue-overflow signal.
    // Recovering the allocator must not allow that same prefix to be staged
    // again from the C client. This retry reproduces the old replay path.
    if (bridge.outgoing.take()) |frame| bridge.outgoing.release(frame);
    _ = bridge.outgoing.takeDisconnect();
    bridge.outgoing.gpa = std.testing.allocator;
    host.stageOutgoing() catch {};
    try std.testing.expect(!bridge.outgoing.hasPending());
    try std.testing.expectEqual(@as(usize, 0), c.phux_client_outgoing_count(host.client));
    try std.testing.expectError(error.InvalidState, host.stageOutgoing());
    try std.testing.expectError(error.InvalidState, invokePartialStagingCaller(host, owner_value, caller));
    try std.testing.expectEqual(State.detached, host.state());
    try std.testing.expectEqual(provider.Phase.frozen, host.presentation(owner_value.terminal_ref).?.phase);

    const accepted = host.takeOperationResult().?;
    try std.testing.expectEqual(accepted_request, accepted.request_id);
    try std.testing.expectEqual(epoch, accepted.connection_epoch);
    try std.testing.expectEqual(operations.types.Status.success, accepted.status);
    const unknown = host.takeOperationResult().?;
    try std.testing.expectEqual(pending_request, unknown.request_id);
    try std.testing.expectEqual(epoch, unknown.connection_epoch);
    try std.testing.expectEqual(operations.types.Status.unknown_outcome, unknown.status);
    try std.testing.expect(host.takeOperationResult() == null);
    try std.testing.expectEqual(@as(usize, 0), c.phux_client_operation_count(host.client));
}

test "key partial outgoing staging cannot replay after allocator recovery" {
    try expectPartialStagingCannotReplay(.key);
}

test "paste partial outgoing staging cannot replay after allocator recovery" {
    try expectPartialStagingCannotReplay(.paste);
}

test "publication partial outgoing staging cannot replay after allocator recovery" {
    try expectPartialStagingCannotReplay(.publication);
}
