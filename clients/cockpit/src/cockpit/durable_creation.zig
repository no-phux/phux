//! Command acceptance is not a replica. Keep placement intent until the exact
//! returned terminal publishes, without following a later focus or retrying.
const model_module = @import("model.zig");
const support = @import("phux_support.zig");
const layout = @import("layout.zig");
const contract = @import("provider_contract");
const shared_mutations = @import("shared_mutations.zig");
const Model = model_module.Model;
const TerminalRef = contract.TerminalRef;

pub const Kind = enum { tab, window, split_right, split_down };
const Pending = struct {
    request: u32 = 0,
    epoch: u64 = 0,
    window: usize,
    window_epoch: u64,
    kind: Kind,
    origin: ?TerminalRef,
    tab_id: ?u32,
    terminal: ?TerminalRef = null,
    accepted: bool = false,
    session: u32 = 0,
    shared_window: ?[16]u8 = null,
    mutation_ticket: ?u64 = null,
    attach_only: bool = false,
    existing_member: bool = false,
    focus: ?TerminalRef = null,
    may_focus: bool = true,
};

pub const Creation = struct {
    pending: [16]?Pending = @splat(null),

    pub fn count(self: *const Creation) usize {
        var n: usize = 0;
        for (self.pending) |entry| if (entry != null) {
            n += 1;
        };
        return n;
    }

    fn vacant(self: *Creation) !*?Pending {
        for (&self.pending) |*slot| if (slot.* == null) return slot;
        return error.OperationCapacity;
    }

    pub fn hasPendingTerminal(self: *const Creation, ref: TerminalRef) bool {
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.terminal) |terminal| if (terminal.eql(ref)) return true;
        }
        return false;
    }

    pub fn request(self: *Creation, model: *Model, kind: Kind) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const remote = model.phux() orelse return error.NoProvider;
        if (remote.state() != .attached) return error.NotReady;
        if (!hasCapacity(model, self.count())) return error.TerminalCapacity;
        const slot = try self.vacant();
        const owner = try spawnOwner(model, model.focusedTerminalRef());
        var entry = try prepareDestination(model, kind, self.reservedAtDestination(model, kind), true);
        errdefer if (kind == .window) model.closeWindow(entry.window);
        entry.request = try remote.requestSpawn(owner, spawnViewport(remote, owner));
        entry.epoch = remote.connectionEpoch();
        slot.* = entry;
        if (kind == .window) model.active_window = entry.window;
    }

    fn reservedAtDestination(self: *const Creation, model: *const Model, kind: Kind) usize {
        var reserved: usize = 0;
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.window != model.active_window) continue;
            if (entry.window_epoch != model.window_epochs[entry.window]) continue;
            if (sharesCapacity(entry, model.wsConst(), kind)) reserved += 1;
        }
        return reserved;
    }

    /// Explicit navigation reuses the creation destination/epoch transaction;
    /// it never places a catalog-only identity before exact stream publication.
    pub fn requestAttach(self: *Creation, model: *Model, ref: TerminalRef) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const remote = model.phux() orelse return error.NoProvider;
        if (remote.state() != .attached) return error.NotReady;
        if (self.hasPendingTerminal(ref)) return;
        if (remote.presentation(ref)) |view| {
            if (view.phase == .live) return self.requestAdmit(model, ref);
        }
        const slot = try self.vacant();
        var entry = try self.prepareAttachment(model, ref);
        entry.request = try remote.requestAttach(ref);
        entry.epoch = remote.connectionEpoch();
        slot.* = entry;
    }

    /// A live catalog terminal needs shared admission, not another subscription.
    pub fn requestAdmit(self: *Creation, model: *Model, ref: TerminalRef) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const remote = model.phux() orelse return error.NoProvider;
        if (remote.state() != .attached) return error.NotReady;
        if (self.hasPendingTerminal(ref)) return;
        const view = remote.presentation(ref) orelse return error.NotReady;
        if (view.phase != .live) return error.NotReady;
        const slot = try self.vacant();
        var entry = try self.prepareAttachment(model, ref);
        entry.epoch = remote.connectionEpoch();
        entry.accepted = true;
        slot.* = entry;
        _ = self.pump(model);
    }

    fn prepareAttachment(self: *Creation, model: *Model, ref: TerminalRef) !Pending {
        const member = sharedMember(model, ref);
        if (!member and !hasCapacity(model, self.count())) return error.TerminalCapacity;
        var entry = try prepareDestination(model, .tab, self.reservedAtDestination(model, .tab), !member);
        entry.terminal = ref;
        entry.attach_only = true;
        entry.existing_member = member;
        return entry;
    }

    pub fn pump(self: *Creation, model: *Model) bool {
        const remote = model.phux() orelse return false;
        var changed = false;
        _ = remote;
        for (&self.pending) |*slot| {
            if (slot.* == null) continue;
            if (!finishPlacement(model, &slot.*.?)) continue;
            slot.* = null;
            changed = true;
        }
        return changed;
    }

    /// Called by the engine's focus synchronization, including local gestures.
    /// Once the user leaves, returning before completion does not revive focus
    /// authority for an earlier command.
    pub fn observeFocus(self: *Creation, model: anytype) void {
        for (&self.pending) |*slot| {
            if (slot.* == null) continue;
            if (!focusUnchanged(model, slot.*.?)) slot.*.?.may_focus = false;
        }
    }

    pub fn complete(self: *Creation, model: *Model, result: support.OperationResult) bool {
        const slot = self.find(result) orelse return false;
        acceptResult(model, &slot.*.?, result) catch {
            refusePlacement(model, slot.*.?);
            slot.* = null;
        };
        return true;
    }

    fn find(self: *Creation, result: support.OperationResult) ?*?Pending {
        for (&self.pending) |*slot| {
            const entry = slot.* orelse continue;
            if (entry.request != result.request_id or entry.epoch != result.connection_epoch) continue;
            return slot;
        }
        return null;
    }

    pub fn disconnect(self: *Creation, model: *Model) void {
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.mutation_ticket) |ticket| model.shared_mutations.forget(ticket);
            refusePlacement(model, entry);
        }
        @memset(&self.pending, null);
    }
};

fn retireEmptyDestination(model: anytype, entry: Pending) void {
    if (entry.kind != .window) return;
    if (model.window_epochs[entry.window] != entry.window_epoch) return;
    const workspace = model.wsAt(entry.window) orelse return;
    if (workspace.tab_count != 0) return;
    model.closeWindow(entry.window);
}

fn prepareDestination(model: *Model, kind: Kind, reserved: usize, requires_capacity: bool) !Pending {
    const window = if (kind == .window) model.freeWindowIndex() orelse return error.WindowCapacity else model.active_window;
    const workspace = model.wsAt(model.active_window) orelse return error.InvalidDestination;
    try validateDestinationCapacity(workspace, kind, reserved, requires_capacity);
    const entry: Pending = .{
        .window = window,
        .window_epoch = model.window_epochs[window],
        .kind = kind,
        .origin = model.focusedTerminalRef(),
        .tab_id = workspace.tabId(workspace.selected_tab),
        .session = model.shared_workspace.session,
        .shared_window = workspace.shared_ids[workspace.selected_tab],
        .focus = if (kind == .window) null else model.focusedTerminalRef(),
    };
    // Allocate before enqueue: completion needs no fallible window allocation.
    if (kind == .window) _ = model.openWindow(window) orelse return error.WindowCapacity;
    return entry;
}

fn validateDestinationCapacity(workspace: *const model_module.Workspace, kind: Kind, reserved: usize, requires_capacity: bool) !void {
    if (isSplit(kind)) try validateSplit(workspace, reserved);
    if (!requires_capacity or kind != .tab) return;
    if (workspace.tab_count + reserved >= model_module.max_tabs) return error.TabCapacity;
}

fn spawnViewport(remote: anytype, owner: ?TerminalRef) contract.Viewport {
    if (owner) |ref| return remote.lastViewport(ref) orelse remote.attach_viewport;
    return remote.attach_viewport;
}

fn acceptResult(model: *Model, entry: *Pending, result: support.OperationResult) !void {
    const remote = model.phux() orelse return error.NoProvider;
    if (!contextCurrent(model, entry.*, remote.connectionEpoch())) return error.StaleContext;
    if (result.status != .success) return error.OperationFailed;
    const ref = result.terminal_ref orelse return error.MissingIdentity;
    entry.terminal = ref;
    if (result.kind == .spawn and ref.terminal_id.phux.kind == 1) {
        entry.request = try remote.requestAttach(ref);
    } else entry.accepted = true;
}

fn finishPlacement(model: *Model, entry: *Pending) bool {
    const remote = model.phux() orelse return false;
    if (!contextCurrent(model, entry.*, remote.connectionEpoch())) {
        if (entry.mutation_ticket) |ticket| model.shared_mutations.forget(ticket);
        refusePlacement(model, entry.*);
        return true;
    }
    if (!focusUnchanged(model, entry.*)) entry.may_focus = false;
    if (entry.mutation_ticket) |ticket| return finishMutation(model, entry.*, ticket);
    return finishPublication(model, entry);
}

fn finishPublication(model: *Model, entry: *Pending) bool {
    const remote = model.phux().?;
    if (!entry.accepted) return false;
    const ref = entry.terminal orelse return false;
    if (!remote.terminalKnown(ref)) {
        retireEmptyDestination(model, entry.*);
        return true;
    }
    const view = remote.presentation(ref) orelse return false;
    if (view.phase != .live) return false;
    return placeLive(model, entry, ref);
}

fn placeLive(model: *Model, entry: *Pending, ref: TerminalRef) bool {
    if (entry.session != 0) {
        return submitShared(model, entry, ref) catch {
            refusePlacement(model, entry.*);
            return true;
        };
    }
    place(model, entry.*, ref) catch refusePlacement(model, entry.*);
    return true;
}

fn contextCurrent(model: anytype, entry: Pending, epoch: u64) bool {
    if (entry.epoch != epoch) return false;
    return entry.session == model.shared_workspace.session;
}

fn destinationCurrent(model: anytype, entry: Pending) bool {
    if (!model.windowOpen(entry.window)) return false;
    return model.window_epochs[entry.window] == entry.window_epoch;
}

fn focusUnchanged(model: anytype, entry: Pending) bool {
    if (model.active_window != entry.window) return false;
    const current = model.focusedTerminalRef();
    const expected = entry.focus orelse return current == null;
    return if (current) |ref| expected.eql(ref) else false;
}

fn submitShared(model: *Model, entry: *Pending, ref: TerminalRef) !bool {
    if (!destinationCurrent(model, entry.*)) return error.StaleDestination;
    const snapshot = model.phux().?.workspaceSnapshot();
    if (snapshot.session_id != entry.session) return error.StaleContext;
    if (snapshot.state == .unavailable or snapshot.state == .last_good_error) return error.WorkspaceUnavailable;
    if (entry.attach_only) {
        if (shared_mutations.terminalWindow(snapshot, ref)) |id| {
            entry.existing_member = true;
            publishSelection(model, entry.*, ref, id);
            return true;
        }
    }
    const mutation = try creationMutation(entry.*, ref, snapshot.revision);
    entry.mutation_ticket = try model.shared_mutations.requestCreation(model, mutation, entry.epoch);
    _ = model.shared_mutations.pump(model);
    return false;
}

fn creationMutation(entry: Pending, ref: TerminalRef, revision: u64) !contract.workspace.Mutation {
    var mutation: contract.workspace.Mutation = .{
        .expected_revision = revision,
        .session_id = entry.session,
        .kind = .add,
        .terminal_ref = ref,
    };
    if (isSplit(entry.kind)) {
        mutation.kind = .split;
        mutation.window_id = entry.shared_window orelse return error.StaleDestination;
        mutation.terminal_ref = entry.origin orelse return error.StaleDestination;
        mutation.new_terminal_ref = ref;
        mutation.direction = if (entry.kind == .split_right) .horizontal else .vertical;
    }
    return mutation;
}

fn sharedMember(model: *Model, ref: TerminalRef) bool {
    if (model.shared_workspace.session == 0) return false;
    return shared_mutations.terminalWindow(model.phux().?.workspaceSnapshot(), ref) != null;
}

fn finishMutation(model: anytype, entry: Pending, ticket: u64) bool {
    const outcome = model.shared_mutations.takeCompletion(ticket) orelse return false;
    if (outcome != .confirmed or !destinationCurrent(model, entry)) {
        refusePlacement(model, entry);
        return true;
    }
    const ref = entry.terminal.?;
    const id = confirmedDestination(model.phux().?.workspaceSnapshot(), entry, ref) orelse {
        // SET succeeded but another writer's value won. Keep the process in
        // discovery and reconcile that value instead of manufacturing a pane.
        refusePlacement(model, entry);
        return true;
    };
    publishSelection(model, entry, ref, id);
    return true;
}

fn confirmedDestination(snapshot: contract.workspace.Snapshot, entry: Pending, ref: TerminalRef) ?[16]u8 {
    const id = shared_mutations.terminalWindow(snapshot, ref) orelse return null;
    if (isSplit(entry.kind)) {
        const expected = entry.shared_window orelse return null;
        return if (@import("std").mem.eql(u8, &expected, &id)) id else null;
    }
    const window = shared_mutations.findWindow(snapshot, id) orelse return null;
    if (window.root >= snapshot.nodes.len) return null;
    // An add creates one shared window. A winning concurrent arrangement that
    // moved this terminal into a group must not move that entire group locally.
    if (snapshot.nodes[window.root].kind != .leaf) return null;
    return id;
}

fn publishSelection(model: anytype, entry: Pending, ref: TerminalRef, id: [16]u8) void {
    if (!destinationCurrent(model, entry)) return;
    if (!entry.existing_member and !isSplit(entry.kind)) {
        model.shared_workspace.placement_hint = .{ .shared_id = id, .window = entry.window, .window_epoch = entry.window_epoch };
    }
    if (entry.may_focus and focusUnchanged(model, entry)) model.shared_workspace.desired_terminal = ref;
}

fn refusePlacement(model: anytype, entry: Pending) void {
    retireEmptyDestination(model, entry);
    model.terminal_limit_refused = true;
    if (entry.session != 0) model.shared_workspace.refused = true;
}

fn isSplit(kind: Kind) bool {
    return kind == .split_right or kind == .split_down;
}

fn sharesCapacity(entry: Pending, workspace: *const model_module.Workspace, kind: Kind) bool {
    if (!isSplit(kind)) return !isSplit(entry.kind);
    if (!isSplit(entry.kind)) return false;
    return entry.tab_id == workspace.tabId(workspace.selected_tab);
}

fn hasCapacity(model: *const Model, reserved: usize) bool {
    var count = reserved;
    for (0..model_module.max_windows) |window| {
        if (!model.windowOpen(window)) continue;
        const workspace = model.wsAtConst(window) orelse continue;
        for (workspace.tabs[0..workspace.tab_count]) |tree| count += tree.paneCount();
    }
    return count < @import("topology.zig").max_terminals;
}

fn spawnOwner(model: *const Model, origin: ?TerminalRef) !?TerminalRef {
    const ref = origin orelse return null;
    if (support.providerKind(ref) != .phux) return null;
    _ = model.terminalOwner(ref) orelse return error.NotReady;
    return ref;
}

fn validateSplit(workspace: *const model_module.Workspace, reserved: usize) !void {
    const tree = workspace.selectedTreeConst() orelse return error.InvalidDestination;
    if (tree.focusedTerminal() == null) return error.InvalidDestination;
    if (tree.paneCount() + reserved >= layout.max_panes) return error.PaneCapacity;
}

fn place(model: *Model, entry: Pending, ref: TerminalRef) !void {
    if (!model.windowOpen(entry.window)) return error.StaleDestination;
    if (model.window_epochs[entry.window] != entry.window_epoch) return error.StaleDestination;
    if (!model.canAddPane()) return error.TerminalCapacity;
    if (model.locateTerminal(ref) != null) return error.AlreadyPlaced;
    const workspace = model.wsAt(entry.window) orelse return error.StaleDestination;
    if (isSplit(entry.kind)) {
        try placeSplit(workspace, entry, ref);
    } else {
        if (!workspace.admitTab(ref)) return error.TabCapacity;
        _ = workspace.selectTerminal(ref);
    }
    model.terminal_limit_refused = false;
}

fn placeSplit(workspace: *model_module.Workspace, entry: Pending, ref: TerminalRef) !void {
    const origin = entry.origin orelse return error.StaleDestination;
    for (workspace.tabs[0..workspace.tab_count], 0..) |*tree, index| {
        if (workspace.tabId(index) != entry.tab_id) continue;
        const node = tree.find(origin) orelse return error.StaleDestination;
        _ = try tree.split(node, if (entry.kind == .split_right) .horizontal else .vertical, ref);
        return;
    }
    return error.StaleDestination;
}

const ConfirmationFixture = struct {
    const ref: TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 9 } } };
    const id: [16]u8 = @splat(4);
    const windows = [_]contract.workspace.Window{.{ .id = id, .root = 0 }};
    const nodes = [_]contract.workspace.Node{.{ .kind = .leaf, .terminal_ref = ref }};
    const Queue = struct {
        outcome: ?shared_mutations.Outcome = null,
        pub fn takeCompletion(self: *@This(), _: u64) ?shared_mutations.Outcome {
            const result = self.outcome;
            self.outcome = null;
            return result;
        }
    };
    shared_mutations: Queue = .{},
    shared_workspace: struct {
        session: u32 = 7,
        refused: bool = false,
        desired_terminal: ?TerminalRef = null,
        placement_hint: ?struct { shared_id: [16]u8, window: usize, window_epoch: u64 } = null,
    } = .{},
    terminal_limit_refused: bool = false,
    window_epochs: [2]u64 = .{ 0, 5 },
    active_window: usize = 1,
    focus: ?TerminalRef = null,
    opened: bool = true,
    snapshot: contract.workspace.Snapshot = .{ .session_id = 7, .state = .authoritative, .windows = &windows, .nodes = &nodes },
    workspace: struct { tab_count: usize = 0 } = .{},

    fn entry() Pending {
        return .{ .epoch = 3, .session = 7, .window = 1, .window_epoch = 5, .kind = .window, .origin = null, .tab_id = null, .terminal = ref, .accepted = true };
    }
    pub fn phux(self: *@This()) ?*@This() {
        return self;
    }
    pub fn workspaceSnapshot(self: *@This()) contract.workspace.Snapshot {
        return self.snapshot;
    }
    pub fn windowOpen(self: *@This(), _: usize) bool {
        return self.opened;
    }
    pub fn focusedTerminalRef(self: *@This()) ?TerminalRef {
        return self.focus;
    }
    pub fn wsAt(self: *@This(), _: usize) ?@TypeOf(&self.workspace) {
        return &self.workspace;
    }
    pub fn closeWindow(self: *@This(), _: usize) void {
        self.opened = false;
    }
};

test "shared creation publishes destination only after winning confirmation" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    const entry = ConfirmationFixture.entry();
    try testing.expect(!finishMutation(&model, entry, 1));
    try testing.expect(model.shared_workspace.desired_terminal == null);
    try testing.expect(model.shared_workspace.placement_hint == null);
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(finishMutation(&model, entry, 1));
    try testing.expect(model.shared_workspace.desired_terminal.?.eql(ConfirmationFixture.ref));
    try testing.expectEqual(@as(usize, 1), model.shared_workspace.placement_hint.?.window);
    try testing.expectEqual(ConfirmationFixture.id, model.shared_workspace.placement_hint.?.shared_id);
}

test "confirmed creation cannot steal changed focus or acquire a reused native destination" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    const entry = ConfirmationFixture.entry();
    model.active_window = 0;
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(finishMutation(&model, entry, 1));
    try testing.expect(model.shared_workspace.desired_terminal == null);
    try testing.expectEqual(@as(usize, 1), model.shared_workspace.placement_hint.?.window);
    model.shared_workspace.placement_hint = null;
    model.window_epochs[1] += 1;
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(finishMutation(&model, entry, 2));
    try testing.expect(model.shared_workspace.placement_hint == null);
    try testing.expect(model.opened);
    try testing.expect(model.shared_workspace.refused);
    try testing.expect(!contextCurrent(&model, entry, 4));
    model.shared_workspace.session = 8;
    try testing.expect(!contextCurrent(&model, entry, 3));
}

test "creation focus suppression survives returning to the original focus" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    var creation: Creation = .{};
    creation.pending[0] = ConfirmationFixture.entry();
    model.active_window = 0;
    creation.observeFocus(&model);
    model.active_window = 1;
    creation.observeFocus(&model);
    const entry = creation.pending[0].?;
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(focusUnchanged(&model, entry));
    try testing.expect(finishMutation(&model, entry, 1));
    try testing.expect(model.shared_workspace.desired_terminal == null);
    try testing.expect(model.shared_workspace.placement_hint != null);
}

test "losing topology confirmation leaves terminal available without speculative placement" {
    const testing = @import("std").testing;
    var model: ConfirmationFixture = .{};
    model.snapshot.windows = &.{};
    model.shared_mutations.outcome = .confirmed;
    try testing.expect(finishMutation(&model, ConfirmationFixture.entry(), 1));
    try testing.expect(model.shared_workspace.desired_terminal == null);
    try testing.expect(model.shared_workspace.placement_hint == null);
    try testing.expect(model.shared_workspace.refused);
    try testing.expectEqual(@as(usize, 1), model.snapshot.nodes.len);
}

test "shared spawn waits for acceptance live publication refresh and winning split" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const testing = @import("std").testing;
    const fixture = support.PhuxProvider.test_support;
    const engine = try @import("durable_creation_tests.zig").start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    const id = model.ws().shared_ids[0].?;
    try engine.creation.request(model, .split_right);
    try testing.expectEqual(@as(usize, 1), model.ws().tabs[0].paneCount());
    try fixture.stageFixture(remote.bridge, "spawn-local.bin");
    _ = try remote.drainReadiness();
    while (remote.takeOperationResult()) |result| _ = engine.creation.complete(model, result);
    _ = engine.creation.pump(model);
    try testing.expectEqual(@as(usize, 1), model.ws().tabs[0].paneCount());
    try fixture.stageFixture(remote.bridge, "local-ready.bin");
    _ = try remote.drainReadiness();
    _ = engine.creation.pump(model);
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try testing.expectEqual(.pending, remote.workspaceSnapshot().status);
    try testing.expectEqual(@as(u32, 2), remote.workspaceSnapshot().request_id);
    // The canonical split snapshot starts from the renamed window. Refresh
    // that same authoritative name so this test confirms the exact mutation.
    try stageCreationConfirmation(remote.bridge, "workspace_rename_metadata.bin", 5, 3);
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_refresh_state.bin");
    _ = try remote.drainReadiness();
    _ = model.shared_mutations.pump(model);
    _ = engine.creation.pump(model);
    try testing.expectEqual(@as(u32, 3), remote.workspaceSnapshot().request_id);
    try testing.expectEqual(@as(usize, 1), model.ws().tabs[0].paneCount());
    try stageCreationConfirmation(remote.bridge, "workspace_split_metadata.bin", 8, 5);
    try stageCreationConfirmation(remote.bridge, "workspace_split_state.bin", 7, 4);
    _ = try remote.drainReadiness();
    _ = model.shared_mutations.pump(model);
    try testing.expect(engine.creation.pump(model));
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    // Even confirmation publishes only intent. The provider projection is the
    // sole writer of the pane tree.
    try testing.expectEqual(@as(usize, 1), model.ws().tabs[0].paneCount());
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    try testing.expectEqual(id, model.ws().shared_ids[0].?);
    try testing.expectEqual(@as(usize, 2), model.ws().tabs[0].paneCount());
    try testing.expectEqual(@as(u32, 8), model.focusedTerminalRef().?.terminal_id.phux.id);
    try testing.expectEqual(@as(u32, 3), remote.host.operation_ledger.last_id);
}

fn stageCreationConfirmation(bridge: anytype, name: []const u8, old_id: u32, new_id: u32) !void {
    const std = @import("std");
    const path = try std.fmt.allocPrint(std.testing.allocator, "src/providers/phux/fixtures/{s}", .{name});
    defer std.testing.allocator.free(path);
    const bytes = try std.Io.Dir.cwd().readFileAlloc(std.testing.io, path, std.testing.allocator, .limited(64 * 1024));
    defer std.testing.allocator.free(bytes);
    var encoded: [4]u8 = undefined;
    std.mem.writeInt(u32, &encoded, 0x8000_0000 + old_id, .big);
    // Reuse the canonical Rust topology reply, changing only correlation: this
    // scenario skips the fixture generator's preceding rename operation.
    const index = std.mem.indexOf(u8, bytes, &encoded) orelse return error.MissingCorrelation;
    try std.testing.expectEqual(index, std.mem.lastIndexOf(u8, bytes, &encoded).?);
    std.mem.writeInt(u32, bytes[index..][0..4], 0x8000_0000 + new_id, .big);
    try std.testing.expect(bridge.incoming.stage(bytes));
}
