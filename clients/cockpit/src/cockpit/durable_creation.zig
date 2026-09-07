//! Command acceptance is not a replica. Keep placement intent until the exact
//! returned terminal publishes, without following a later focus or retrying.
const model_module = @import("model.zig");
const support = @import("phux_support.zig");
const layout = @import("layout.zig");
const contract = @import("provider_contract");
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

    pub fn request(self: *Creation, model: *Model, kind: Kind) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const remote = model.phux() orelse return error.NoProvider;
        if (remote.state() != .attached) return error.NotReady;
        if (!hasCapacity(model, self.count())) return error.TerminalCapacity;
        const slot = try self.vacant();
        const owner = try spawnOwner(model, model.focusedTerminalRef());
        var entry = try prepareDestination(model, kind, self.reservedAtDestination(model, kind));
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

    pub fn pump(self: *Creation, model: *Model) bool {
        const remote = model.phux() orelse return false;
        var changed = false;
        _ = remote;
        for (&self.pending) |*slot| {
            const entry = slot.* orelse continue;
            if (!finishPlacement(model, entry)) continue;
            slot.* = null;
            changed = true;
        }
        return changed;
    }

    pub fn complete(self: *Creation, model: *Model, result: support.OperationResult) bool {
        const slot = self.find(result) orelse return false;
        acceptResult(model, &slot.*.?, result) catch {
            retireEmptyDestination(model, slot.*.?);
            model.terminal_limit_refused = true;
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
        if (self.count() != 0) model.terminal_limit_refused = true;
        for (self.pending) |entry| if (entry) |value| retireEmptyDestination(model, value);
        @memset(&self.pending, null);
    }
};

fn retireEmptyDestination(model: *Model, entry: Pending) void {
    if (entry.kind != .window) return;
    if (model.window_epochs[entry.window] != entry.window_epoch) return;
    const workspace = model.wsAt(entry.window) orelse return;
    if (workspace.tab_count != 0) return;
    model.closeWindow(entry.window);
}

fn prepareDestination(model: *Model, kind: Kind, reserved: usize) !Pending {
    const window = if (kind == .window) model.freeWindowIndex() orelse return error.WindowCapacity else model.active_window;
    const workspace = model.wsAt(model.active_window) orelse return error.InvalidDestination;
    if (isSplit(kind)) try validateSplit(workspace, reserved);
    if (kind == .tab and workspace.tab_count + reserved >= model_module.max_tabs) return error.TabCapacity;
    const entry: Pending = .{ .window = window, .window_epoch = model.window_epochs[window], .kind = kind, .origin = model.focusedTerminalRef(), .tab_id = workspace.tabId(workspace.selected_tab) };
    // Allocate before enqueue: completion needs no fallible window allocation.
    if (kind == .window) _ = model.openWindow(window) orelse return error.WindowCapacity;
    return entry;
}

fn spawnViewport(remote: anytype, owner: ?TerminalRef) contract.Viewport {
    if (owner) |ref| return remote.lastViewport(ref) orelse remote.attach_viewport;
    return remote.attach_viewport;
}

fn acceptResult(model: *Model, entry: *Pending, result: support.OperationResult) !void {
    const remote = model.phux() orelse return error.NoProvider;
    if (result.status != .success) return error.OperationFailed;
    const ref = result.terminal_ref orelse return error.MissingIdentity;
    entry.terminal = ref;
    if (result.kind == .spawn and ref.terminal_id.phux.kind == 1) {
        entry.request = try remote.requestAttach(ref);
    } else entry.accepted = true;
}

fn finishPlacement(model: *Model, entry: Pending) bool {
    const remote = model.phux() orelse return false;
    if (entry.epoch != remote.connectionEpoch()) {
        model.terminal_limit_refused = true;
        return true;
    }
    if (!entry.accepted) return false;
    const ref = entry.terminal orelse return false;
    const view = remote.presentation(ref) orelse return false;
    if (view.phase != .live) return false;
    place(model, entry, ref) catch {
        model.terminal_limit_refused = true;
    };
    return true;
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
