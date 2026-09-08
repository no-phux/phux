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
pub const OperationResult = operations.types.Result;
pub const test_support = @import("operation_test_support.zig");

pub const enabled = true;
pub const max_terminals: usize = 16;
// Matches Cockpit's bounded discovery inventory; contains no engine replicas.
pub const max_catalog_terminals: usize = 64;
pub const max_notices: usize = 64;
pub const max_search_results: usize = 4096;
pub const max_sessions: usize = 256;
pub const max_title_bytes: usize = 4096;
pub const max_session_name_bytes: usize = 4096;
pub const max_notice_bytes: usize = 64 * 1024;
/// Admission is bounded independently from one frame's text budget. The
/// painter degrades rows atomically; the provider retains the complete valid
/// viewport instead of disconnecting on ordinary dense Unicode content.
pub const max_cell_utf8_bytes = presentation_module.max_cell_utf8_bytes;
pub const max_grid_utf8_bytes = presentation_module.max_grid_utf8_bytes;

pub const State = enum { new, hello_queued, negotiated, attached, detached, failed };
pub const SyncDelta = struct {
    metadata_changed: bool = false,
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
};
pub const SessionSummary = struct {
    id: u32,
    name: []u8,
    created_at_unix_secs: i64,
    window_count: u16,
    attached_client_count: u16,
    focused: bool,

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

const RemoteId = provider.RemoteTerminalId;
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
    viewport: ?provider.Viewport = null,

    fn deinit(terminal: *Terminal, gpa: std.mem.Allocator) void {
        terminal.canvas.deinit(gpa);
        terminal.title.deinit(gpa);
        terminal.pending_title.deinit(gpa);
    }

    fn terminalRef(terminal: *const Terminal) provider.TerminalRef {
        return phuxRef(terminal.id);
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
    gpa: std.mem.Allocator,
    client: *c.PhuxClient,
    bridge: *transport.Bridge,
    terminals: std.ArrayListUnmanaged(Terminal) = .empty,
    sessions: std.ArrayListUnmanaged(SessionSummary) = .empty,
    search_results: std.ArrayListUnmanaged(SearchResult) = .empty,
    search_owner: ?provider.ReplicaOwner = null,
    notices: std.ArrayListUnmanaged(Notice) = .empty,
    attach_barrier_seen: bool = false,
    client_generation: u64 = 1,
    operation_ledger: operations.Ledger(max_terminals) = .{},
    detached_catalog: [max_catalog_terminals]provider.TerminalRef = undefined,
    detached_catalog_count: usize = 0,
    disconnected: bool = false,
    color_policy: ColorPolicy = .{},
    metadata_changed: bool = false,

    /// Presentation-only update: no input, render query or replica mutation.
    pub fn setColorPolicy(host: *Host, policy: ColorPolicy) void {
        if (std.meta.eql(host.color_policy, policy)) return;
        host.color_policy = policy;
        for (host.terminals.items) |*terminal| terminal.canvas.setColorPolicy(policy);
    }

    pub fn create(gpa: std.mem.Allocator, bridge: *transport.Bridge) !*Host {
        const host = try gpa.create(Host);
        errdefer gpa.destroy(host);
        host.* = .{ .gpa = gpa, .client = try newClient(), .bridge = bridge };
        return host;
    }

    pub fn destroy(host: *Host) void {
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

    /// Queue acceptance returns an ID even if transport staging then fails.
    /// That accepted operation becomes unknown, never an implicit spawn retry.
    /// Owners retain their exact terminal identity, including satellite host;
    /// satellite routing is explicit and matches that owner's host.
    pub fn requestSpawn(host: *Host, owner_ref: ?provider.TerminalRef, viewport: provider.Viewport) !u32 {
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
            .cwd = bytes(&.{}),
            .cols = viewport.cols,
            .rows = viewport.rows,
        };
        try resultError(c.phux_client_queue_spawn(host.client, &options));
        host.operation_ledger.accepted(request_id, host.client_generation, .spawn, null);
        host.stageOutgoing() catch host.disconnect();
        return request_id;
    }

    pub fn requestAttach(host: *Host, terminal_ref: provider.TerminalRef) !u32 {
        const request_id = try host.preflightOperation();
        const remote = remoteFromRef(terminal_ref) orelse return error.InvalidIdentity;
        const raw = cId(&remote);
        _ = try remoteFromC(raw);
        if (remote.id == 0) return error.InvalidIdentity;
        if (host.findTerminal(terminal_ref) == null) try host.reserveTerminalSlot(terminal_ref);
        const options: c.PhuxAttachTerminalOptions = .{
            .size = @sizeOf(c.PhuxAttachTerminalOptions),
            .version = c.PHUX_CLIENT_ABI_VERSION,
            .request_id = request_id,
            .terminal_id = raw,
        };
        try resultError(c.phux_client_queue_attach_terminal(host.client, &options));
        host.operation_ledger.accepted(request_id, host.client_generation, .attach, terminal_ref);
        // Capacity was reserved before queueing. A placeholder counts against
        // the terminal limit but stays invisible until the stream is READY.
        host.admitOperationTerminal(remote);
        host.stageOutgoing() catch host.disconnect();
        return request_id;
    }

    fn preflightOperation(host: *Host) !u32 {
        if (host.bridge.incoming.takeDisconnect() != null) host.disconnect();
        if (host.disconnected or host.state() != .attached) return error.InvalidState;
        return host.operation_ledger.nextId();
    }

    pub fn requestDetach(host: *Host, terminal_ref: provider.TerminalRef) !u32 {
        const request_id = try host.preflightOperation();
        const terminal = host.findTerminalConst(terminal_ref) orelse return error.InvalidIdentity;
        if (!terminal.published) return error.InvalidState;
        if (!host.catalogContains(terminal_ref) and host.detached_catalog_count == max_catalog_terminals)
            return error.TerminalCapacity;
        const options: c.PhuxDetachTerminalOptions = .{
            .size = @sizeOf(c.PhuxDetachTerminalOptions),
            .version = c.PHUX_CLIENT_ABI_VERSION,
            .request_id = request_id,
            .terminal_id = cId(&terminal.id),
        };
        try resultError(c.phux_client_queue_detach_terminal(host.client, &options));
        host.operation_ledger.accepted(request_id, host.client_generation, .detach, terminal_ref);
        if (!host.catalogContains(terminal_ref)) {
            host.detached_catalog[host.detached_catalog_count] = terminal_ref;
            host.detached_catalog_count += 1;
        }
        host.stageOutgoing() catch host.disconnect();
        return request_id;
    }

    fn catalogContains(host: *const Host, ref: provider.TerminalRef) bool {
        for (host.detached_catalog[0..host.detached_catalog_count]) |entry| if (entry.eql(ref)) return true;
        return false;
    }

    pub fn catalogRefs(host: *const Host, out: []provider.TerminalRef) usize {
        var count = host.terminalRefs(out);
        for (host.detached_catalog[0..host.detached_catalog_count]) |ref| {
            if (host.contains(ref)) continue;
            if (count == out.len) break;
            out[count] = ref;
            count += 1;
        }
        return count;
    }

    fn reserveTerminalSlot(host: *Host, target: ?provider.TerminalRef) !void {
        if (host.terminals.items.len + host.operation_ledger.pendingSpawns() >= max_terminals)
            return error.TerminalCapacity;
        try host.reserveCatalogIdentity(target);
        try host.terminals.ensureTotalCapacity(host.gpa, max_terminals);
    }

    fn reserveCatalogIdentity(host: *const Host, target: ?provider.TerminalRef) !void {
        if (target) |ref| if (host.catalogContains(ref)) return;
        var count = host.detached_catalog_count + host.operation_ledger.pendingSpawns();
        for (host.terminals.items) |*terminal| {
            if (!host.catalogContains(terminal.terminalRef())) count += 1;
        }
        if (count >= max_catalog_terminals) return error.TerminalCapacity;
    }

    fn admitOperationTerminal(host: *Host, remote: RemoteId) void {
        for (host.terminals.items) |*terminal| if (terminal.id.eql(remote)) return;
        std.debug.assert(host.terminals.items.len < max_terminals);
        host.terminals.appendAssumeCapacity(.{ .id = remote });
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
        host.client_generation = next_generation;
        host.operation_ledger.last_id = 0;
        host.detached_catalog_count = 0;
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
        while (host.bridge.incoming.take()) |frame| {
            defer host.bridge.incoming.release(frame);
            try resultErrorWithContext(host.client, "feed frame", c.phux_client_feed_frame(host.client, frame.ptr, frame.len));
        }
        if (host.state() == .attached and !host.attach_barrier_seen) try host.refreshSessions();
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
        delta.metadata_changed = host.metadata_changed;
        host.metadata_changed = false;
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
            .version = 1,
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
        const remote = remoteFromRef(stored_owner.terminal_ref) orelse {
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
            const session = try copySessionSummary(host.gpa, raw);
            next.append(host.gpa, session) catch {
                var owned = session;
                owned.deinit(host.gpa);
                return error.OutOfMemory;
            };
        }
        host.clearSessions();
        host.sessions.deinit(host.gpa);
        host.sessions = next;
    }

    fn clearSessions(host: *Host) void {
        for (host.sessions.items) |*session| session.deinit(host.gpa);
        host.sessions.items.len = 0;
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
            result.* = try copyOperation(raw, host.client_generation);
        }
        try resultError(c.phux_client_operation_clear(host.client));
        for (copied[0..count]) |result| {
            try host.operation_ledger.complete(result);
            try host.applyOperationIdentity(&result);
        }
    }

    fn applyOperationIdentity(host: *Host, result: *const OperationResult) !void {
        const terminal_ref = result.terminal_ref orelse return;
        if (result.kind == .detach) {
            if (result.status == .success) {
                const terminal = host.findTerminal(terminal_ref) orelse return;
                terminal.remove_at_barrier = true;
            }
            return;
        }
        if (result.status == .success) {
            const remote = remoteFromRef(terminal_ref) orelse return error.InvalidIdentity;
            _ = try host.ensureTerminal(cId(&remote));
        } else if (result.kind == .attach and result.status == .refused) {
            const terminal = host.findTerminal(terminal_ref) orelse return;
            terminal.remove_at_barrier = true;
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
                c.PHUX_CLIENT_EFFECT_DAMAGE => {
                    const terminal = try host.ensureTerminal(effect.terminal_id);
                    if (effect.detail == c.PHUX_CLIENT_DAMAGE_REMOVED) {
                        terminal.phase = .tombstoned;
                        terminal.remove_at_barrier = true;
                    } else {
                        markGridDirty(terminal, host.attach_barrier_seen);
                    }
                },
                c.PHUX_CLIENT_EFFECT_STATUS => {
                    try host.captureStatus(&effect);
                    try host.appendNotice(.status, &effect, generation);
                },
                c.PHUX_CLIENT_EFFECT_JOB => try host.appendNotice(.job, &effect, generation),
                else => return error.Protocol,
            }
        }
        try resultError(c.phux_client_effect_clear(host.client));
        try host.captureOperations();
    }

    fn captureStatus(host: *Host, effect: *const c.PhuxClientEffect) !void {
        switch (effect.detail) {
            c.PHUX_CLIENT_STATUS_TITLE => try host.captureTitle(effect),
            c.PHUX_CLIENT_STATUS_RESYNC_REQUIRED => try host.markResync(effect.terminal_id),
            c.PHUX_CLIENT_STATUS_DETACHED => host.markDetached(),
            c.PHUX_CLIENT_STATUS_SERVER_ERROR => host.markServerFailure(),
            c.PHUX_CLIENT_STATUS_HISTORY, c.PHUX_CLIENT_STATUS_HISTORY_UNAVAILABLE => {
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

    fn markResync(host: *Host, raw: c.PhuxTerminalId) !void {
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

    fn copyTerminalCanvas(host: *Host, terminal: *Terminal, id: *const c.PhuxTerminalId, view: *const c.PhuxTerminalGridView) !void {
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
            .terminal_ref = phuxRef(remote),
            .generation = generation,
            .bytes = owned,
        });
    }

    fn ensureTerminal(host: *Host, raw: c.PhuxTerminalId) !*Terminal {
        const id = try remoteFromC(raw);
        for (host.terminals.items) |*terminal| if (terminal.id.eql(id)) return terminal;
        if (host.terminals.items.len == max_terminals) return error.OutOfMemory;
        try host.terminals.append(host.gpa, .{ .id = id });
        return &host.terminals.items[host.terminals.items.len - 1];
    }

    fn findTerminalRaw(host: *Host, raw: c.PhuxTerminalId) !?*Terminal {
        const id = try remoteFromC(raw);
        for (host.terminals.items) |*terminal| if (terminal.id.eql(id)) return terminal;
        return null;
    }

    fn findTerminal(host: *Host, terminal_ref: provider.TerminalRef) ?*Terminal {
        const id = remoteFromRef(terminal_ref) orelse return null;
        for (host.terminals.items) |*terminal| if (terminal.id.eql(id)) return terminal;
        return null;
    }

    fn findTerminalConst(host: *const Host, terminal_ref: provider.TerminalRef) ?*const Terminal {
        const id = remoteFromRef(terminal_ref) orelse return null;
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

    fn currentCId(host: *Host, owner_value: provider.ReplicaOwner) !c.PhuxTerminalId {
        if (host.operation_ledger.detaching(owner_value.terminal_ref)) return error.InvalidState;
        const terminal = host.findTerminal(owner_value.terminal_ref) orelse return error.InvalidState;
        if (terminal.phase != .live or !terminal.owner().eql(owner_value)) return error.InvalidState;
        return cId(&terminal.id);
    }

    fn currentCIdConst(host: *const Host, owner_value: provider.ReplicaOwner) !c.PhuxTerminalId {
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

fn remoteFromC(raw: c.PhuxTerminalId) !RemoteId {
    if (raw.host.len != 0 and raw.host.data == null) return error.InvalidIdentity;
    const host_name: []const u8 = if (raw.host.len == 0) &.{} else raw.host.data[0..raw.host.len];
    if (raw.kind == c.PHUX_TERMINAL_LOCAL) {
        if (host_name.len != 0) return error.InvalidIdentity;
    } else if (raw.kind == c.PHUX_TERMINAL_SATELLITE) {
        if (host_name.len == 0) return error.InvalidIdentity;
        _ = std.unicode.Utf8View.init(host_name) catch return error.InvalidIdentity;
    } else return error.InvalidIdentity;
    return RemoteId.fromPhux(raw.kind, raw.id, host_name) catch return error.InvalidIdentity;
}

fn remoteFromRef(terminal_ref: provider.TerminalRef) ?RemoteId {
    if (terminal_ref.provider_id != .phux) return null;
    return switch (terminal_ref.terminal_id) {
        .phux => |id| id,
        .local => null,
    };
}

fn phuxRef(id: RemoteId) provider.TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = id } };
}

fn cId(id: *const RemoteId) c.PhuxTerminalId {
    const host_name = id.host();
    return .{
        .kind = id.kind,
        .id = id.id,
        .host = .{ .data = if (host_name.len == 0) null else host_name.ptr, .len = host_name.len },
    };
}

fn copyOperation(raw: c.PhuxOperationResult, epoch: u64) !OperationResult {
    var result: OperationResult = .{
        .request_id = raw.request_id,
        .connection_epoch = epoch,
        .kind = std.enums.fromInt(operations.types.Kind, raw.kind) orelse return error.Protocol,
        .status = std.enums.fromInt(operations.types.Status, raw.status) orelse return error.Protocol,
        .error_domain = std.enums.fromInt(operations.types.ErrorDomain, raw.error_domain) orelse return error.Protocol,
        .error_code = raw.error_code,
    };
    if (raw.terminal_id.id != 0) result.terminal_ref = phuxRef(try remoteFromC(raw.terminal_id));
    const message = try effectSlice(raw.message);
    if (message.len > result.message_storage.len) return error.Protocol;
    @memcpy(result.message_storage[0..message.len], message);
    result.message_len = message.len;
    return result;
}
fn bytes(slice: []const u8) c.PhuxBytes {
    return .{ .data = if (slice.len == 0) null else slice.ptr, .len = slice.len };
}

fn effectSlice(raw: c.PhuxBytes) ![]const u8 {
    if (raw.len != 0 and raw.data == null) return error.Protocol;
    return if (raw.len == 0) &.{} else raw.data[0..raw.len];
}

fn toCAnchor(anchor: Anchor) c.PhuxDocumentAnchor {
    return .{ .opaque_id = anchor.opaque_id };
}

fn releaseTopAnchor(client: *c.PhuxClient, terminal_id: *const c.PhuxTerminalId, anchor: c.PhuxDocumentAnchor) void {
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

test "contains hides terminals until their canvas is published" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const id = try RemoteId.fromPhux(c.PHUX_TERMINAL_LOCAL, 7, "");
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

test "grid damage freezes a published canvas until replacement copy" {
    var published: Terminal = .{
        .id = try RemoteId.fromPhux(c.PHUX_TERMINAL_LOCAL, 8, ""),
        .phase = .live,
        .published = true,
    };
    markGridDirty(&published, true);
    try std.testing.expectEqual(provider.Phase.frozen, published.phase);
    try std.testing.expect(published.dirty);
    try std.testing.expect(published.seen_in_attach);

    var reconnecting: Terminal = .{
        .id = try RemoteId.fromPhux(c.PHUX_TERMINAL_LOCAL, 9, ""),
        .phase = .reconnecting,
        .published = true,
    };
    markGridDirty(&reconnecting, false);
    try std.testing.expectEqual(provider.Phase.reconnecting, reconnecting.phase);

    var unpublished: Terminal = .{
        .id = try RemoteId.fromPhux(c.PHUX_TERMINAL_LOCAL, 10, ""),
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

    const id = try RemoteId.fromPhux(c.PHUX_TERMINAL_LOCAL, 10, "");
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

    const id = try RemoteId.fromPhux(c.PHUX_TERMINAL_LOCAL, 21, "");
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

    const id = try RemoteId.fromPhux(c.PHUX_TERMINAL_LOCAL, 22, "");
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

    const first_id = try RemoteId.fromPhux(c.PHUX_TERMINAL_LOCAL, 31, "");
    const second_id = try RemoteId.fromPhux(c.PHUX_TERMINAL_SATELLITE, 8, "build-host");
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

test "reordered remote enumeration retains stable refs and lookup" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();

    const first_id = try RemoteId.fromPhux(c.PHUX_TERMINAL_LOCAL, 41, "");
    const second_id = try RemoteId.fromPhux(c.PHUX_TERMINAL_SATELLITE, 41, "satellite");
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

// GUARD: satellite-cid-borrow
test "satellite C identity borrows the exact owning host storage" {
    const remote = try RemoteId.fromPhux(c.PHUX_TERMINAL_SATELLITE, 41, "satellite-with-exact-host");
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

test "detach churn beyond replica capacity retains durable catalog without engine slots" {
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
    try std.testing.expectEqual(initial + 20, host.catalogRefs(&refs));
}

test "satellite spawn requires explicit attach and permits READY before command acknowledgment" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try test_support.attachHost(host);
    const satellite = try RemoteId.fromPhux(c.PHUX_TERMINAL_SATELLITE, 6, "build-host");
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
    const remote = try RemoteId.fromPhux(c.PHUX_TERMINAL_SATELLITE, 9, "build-host");
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
    const remote = try RemoteId.fromPhux(c.PHUX_TERMINAL_SATELLITE, 9, "build-host");
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

// GUARD: operation-resize-identity
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

// GUARD: partial-outgoing-no-replay
test "key partial outgoing staging cannot replay after allocator recovery" {
    try expectPartialStagingCannotReplay(.key);
}

test "paste partial outgoing staging cannot replay after allocator recovery" {
    try expectPartialStagingCannotReplay(.paste);
}

test "publication partial outgoing staging cannot replay after allocator recovery" {
    try expectPartialStagingCannotReplay(.publication);
}
