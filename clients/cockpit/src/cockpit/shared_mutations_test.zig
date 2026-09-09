const std = @import("std");
const mutations = @import("shared_mutations.zig");
const contract = @import("provider_contract");
const workspace = contract.workspace;
const testing = std.testing;
const first_id: workspace.WindowId = @splat(1);
const second_id: workspace.WindowId = @splat(2);

fn terminal(id: u32) contract.TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = contract.RemoteTerminalId.fromPhux(0, id, "") catch unreachable } };
}

const nodes = [_]workspace.Node{
    .{ .kind = .horizontal, .first = 1, .second = 2 },
    .{ .kind = .leaf, .terminal_ref = terminal(1) },
    .{ .kind = .vertical, .first = 3, .second = 4 },
    .{ .kind = .leaf, .terminal_ref = terminal(2) },
    .{ .kind = .leaf, .terminal_ref = terminal(3) },
    .{ .kind = .leaf, .terminal_ref = terminal(4) },
};
const windows = [_]workspace.Window{
    .{ .id = first_id, .root = 0 },
    .{ .id = second_id, .root = 5 },
};

// No process-destruction API exists on this fake. The real coordinator has
// only the typed workspace capability, so presentation close cannot call kill.
const Remote = struct {
    snapshot: workspace.Snapshot = .{ .revision = 10, .session_id = 7, .state = .authoritative, .windows = &windows, .nodes = &nodes },
    epoch: u64 = 3,
    next_id: u32 = 40,
    refreshes: usize = 0,
    writes: usize = 0,
    sent: ?workspace.Mutation = null,

    pub fn workspaceSnapshot(self: *Remote) workspace.Snapshot {
        return self.snapshot;
    }
    pub fn connectionEpoch(self: *Remote) u64 {
        return self.epoch;
    }
    pub fn state(_: *Remote) enum { attached } {
        return .attached;
    }
    pub fn requestWorkspaceRefresh(self: *Remote) !?u32 {
        if (self.snapshot.status == .pending) return null;
        self.refreshes += 1;
        return self.begin();
    }
    pub fn requestWorkspaceMutation(self: *Remote, mutation: workspace.Mutation) !u32 {
        try testing.expect(self.snapshot.status != .pending);
        self.writes += 1;
        self.sent = mutation;
        return self.begin();
    }
    fn begin(self: *Remote) u32 {
        self.next_id += 1;
        self.snapshot.request_id = self.next_id;
        self.snapshot.status = .pending;
        return self.next_id;
    }
    fn finish(self: *Remote, status: workspace.Status) void {
        self.snapshot.status = status;
        self.snapshot.revision += 1;
    }
};

const Model = struct {
    remote: Remote = .{},
    shared_workspace: struct { session: u32 = 7, revision: u64 = 10, refused: bool = false } = .{},
    terminal_limit_refused: bool = false,
    pub fn phux(self: *Model) ?*Remote {
        return &self.remote;
    }
};

fn split() workspace.Mutation {
    return .{ .expected_revision = 10, .session_id = 7, .kind = .split, .window_id = first_id, .terminal_ref = terminal(2), .new_terminal_ref = terminal(9), .direction = .vertical };
}

fn refreshCreation(queue: *mutations.Coordinator, model: *Model) void {
    _ = queue.pump(model);
    model.remote.finish(.confirmed);
    _ = queue.pump(model);
}

test "creation waits behind refresh then submits captured split once and waits for confirmation" {
    var queue: mutations.Coordinator = .{};
    var model: Model = .{};
    _ = try model.remote.requestWorkspaceRefresh();
    const ticket = try queue.requestCreation(&model, split(), 3);
    try testing.expect(!queue.pump(&model));
    try testing.expectEqual(@as(usize, 1), model.remote.refreshes);
    try testing.expectEqual(@as(usize, 0), model.remote.writes);
    model.remote.finish(.confirmed);
    refreshCreation(&queue, &model);
    try testing.expectEqual(@as(usize, 2), model.remote.refreshes);
    try testing.expect(!queue.pump(&model));
    const sent = model.remote.sent.?;
    try testing.expectEqual(@as(u64, 12), sent.expected_revision);
    try testing.expectEqual(@as(u32, 7), sent.session_id);
    try testing.expectEqual(first_id, sent.window_id);
    try testing.expect(sent.terminal_ref.?.eql(terminal(2)));
    try testing.expect(sent.new_terminal_ref.?.eql(terminal(9)));
    try testing.expectEqual(.vertical, sent.direction);
    try testing.expect(queue.takeCompletion(ticket) == null);
    try testing.expect(!queue.pump(&model));
    try testing.expectEqual(@as(usize, 1), model.remote.writes);
    model.remote.finish(.confirmed);
    try testing.expect(queue.pump(&model));
    try testing.expectEqual(.confirmed, queue.takeCompletion(ticket).?);
    try testing.expect(queue.takeCompletion(ticket) == null);
}

test "queued creation cannot follow a later session or connection epoch" {
    for ([_]bool{ false, true }) |new_epoch| {
        var queue: mutations.Coordinator = .{};
        var model: Model = .{};
        const ticket = try queue.requestCreation(&model, split(), 3);
        if (new_epoch) model.remote.epoch += 1 else model.shared_workspace.session = 8;
        try testing.expect(queue.pump(&model));
        try testing.expectEqual(.unknown_outcome, queue.takeCompletion(ticket).?);
        try testing.expectEqual(@as(usize, 0), model.remote.refreshes);
        try testing.expectEqual(@as(usize, 0), model.remote.writes);
        try testing.expect(model.shared_workspace.refused);
    }
}

test "refresh cannot redirect a captured split to a terminal in another shared window" {
    var queue: mutations.Coordinator = .{};
    var model: Model = .{};
    const ticket = try queue.requestCreation(&model, split(), 3);
    _ = queue.pump(&model);
    const replacement = [_]workspace.Window{ .{ .id = first_id, .root = 5 }, .{ .id = second_id, .root = 0 } };
    model.remote.snapshot.windows = &replacement;
    model.remote.finish(.confirmed);
    try testing.expect(queue.pump(&model));
    try testing.expectEqual(.refused, queue.takeCompletion(ticket).?);
    try testing.expectEqual(@as(usize, 0), model.remote.writes);
}

test "mutation refusal and unknown outcomes never retry or erase terminal discovery" {
    for ([_]workspace.Status{ .refused, .unknown_outcome }) |status| {
        var queue: mutations.Coordinator = .{};
        var model: Model = .{};
        const ticket = try queue.requestCreation(&model, split(), 3);
        refreshCreation(&queue, &model);
        _ = queue.pump(&model);
        model.remote.finish(status);
        try testing.expect(queue.pump(&model));
        const expected: mutations.Outcome = if (status == .refused) .refused else .unknown_outcome;
        try testing.expectEqual(expected, queue.takeCompletion(ticket).?);
        try testing.expect(!queue.pump(&model));
        try testing.expectEqual(@as(usize, 1), model.remote.writes);
        try testing.expectEqual(@as(usize, 6), model.remote.snapshot.nodes.len);
        try testing.expect(model.shared_workspace.refused);
    }
}

test "presentation removal is typed and whole-window close is one atomic request" {
    var queue: mutations.Coordinator = .{};
    var model: Model = .{};
    try queue.requestRemove(&model, terminal(2));
    try testing.expectEqual(.remove, model.remote.sent.?.kind);
    try testing.expectEqual(first_id, model.remote.sent.?.window_id);
    try testing.expect(model.remote.sent.?.terminal_ref.?.eql(terminal(2)));
    try testing.expectEqual(@as(usize, 6), model.remote.snapshot.nodes.len);
    model.remote.finish(.confirmed);
    try testing.expect(queue.pump(&model));
    model.shared_workspace.revision = model.remote.snapshot.revision;
    try queue.requestRemoveWindow(&model, first_id);
    try testing.expectEqual(.remove_window, model.remote.sent.?.kind);
    try testing.expectEqual(@as(usize, 2), model.remote.writes);
}

test "reorder preserves identity and busy direct edits are refused" {
    var queue: mutations.Coordinator = .{};
    var model: Model = .{};
    try queue.requestReorder(&model, second_id, 0);
    try testing.expectEqual(.reorder, model.remote.sent.?.kind);
    try testing.expectEqual(second_id, model.remote.sent.?.window_id);
    try testing.expectEqual(@as(u32, 0), model.remote.sent.?.index);
    try testing.expectError(error.WorkspaceBusy, queue.requestRemoveWindow(&model, first_id));
    try testing.expect(model.shared_workspace.refused);
    try testing.expectEqual(first_id, model.remote.snapshot.windows[0].id);
}

test "resize path is root-to-branch low-bit-first and stale revisions cannot rebase it" {
    var queue: mutations.Coordinator = .{};
    var model: Model = .{};
    try testing.expectError(error.InvalidPath, queue.requestResize(&model, first_id, 0, 1, 0.3));
    try testing.expectError(error.InvalidRatio, queue.requestResize(&model, first_id, 1, 1, std.math.nan(f32)));
    try queue.requestResize(&model, first_id, 1, 1, 0.3);
    try testing.expectEqual(.resize, model.remote.sent.?.kind);
    try testing.expectEqual(@as(u64, 1), model.remote.sent.?.path_bits);
    try testing.expectEqual(@as(u32, 1), model.remote.sent.?.path_len);
    try testing.expectEqual(@as(f32, 0.3), model.remote.sent.?.ratio);
    try testing.expectEqual(@as(f32, 0.5), model.remote.snapshot.nodes[2].ratio);
    model.remote.finish(.confirmed);
    _ = queue.pump(&model);
    try testing.expectError(error.StaleTarget, queue.requestResize(&model, first_id, 1, 1, 0.7));
    try testing.expectEqual(@as(usize, 1), model.remote.writes);
}

test "creation queue is bounded and disconnect clears every intent without sending" {
    var queue: mutations.Coordinator = .{};
    var model: Model = .{};
    for (0..16) |_| _ = try queue.requestCreation(&model, split(), 3);
    try testing.expectError(error.OperationCapacity, queue.requestCreation(&model, split(), 3));
    queue.disconnect(&model);
    try testing.expect(!queue.pump(&model));
    try testing.expect(model.shared_workspace.refused);
    try testing.expectEqual(@as(usize, 0), model.remote.writes);
}
