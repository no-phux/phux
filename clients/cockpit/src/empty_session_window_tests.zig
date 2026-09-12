//! Exact-window empty-session contracts on the real Model and independent FFI Clients.
const std = @import("std");
const testing = std.testing;
const Engine = @import("cockpit/native/ts_engine.zig").Engine;
const Model = @import("cockpit/model.zig").Model;
const support = @import("cockpit/phux_support.zig");
const Remote = support.PhuxProvider;
const empty = @import("cockpit/native/empty_session.zig");

test "empty window selecting another window preserves legacy unqualified empty view" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    engine.model.empty_pick = .{ .coordinator = .local, .session = 3, .window = 0 };
    engine.model.active_window = 1;
    empty.dismiss(engine.model);
    try testing.expect(engine.model.empty_pick != null);
    engine.model.active_window = 0;
    empty.dismiss(engine.model);
    try testing.expect(engine.model.empty_pick == null);
}

fn peer(engine: *Engine) !*Remote {
    const slot = try engine.model.freePeerSlot();
    const remote = try Remote.create(testing.allocator, testing.io, .{ .unix = "/empty-window-unused" }, null, "empty-window");
    engine.model.peers.items[slot].provider = remote;
    try remote.show(3);
    try remote.host.start("empty-window");
    try stage(remote, "hello_keep_empty.bin");
    try remote.host.attachSessionId(3, .{ .cols = 80, .rows = 24 });
    try stage(remote, "attached_empty.bin");
    try stage(remote, "workspace_empty.bin");
    try testing.expectEqual(.attached, remote.state());
    try testing.expectEqual(@as(?u32, 3), remote.selectedSessionId());
    try testing.expectEqual(@as(usize, 2), remote.sessionCatalog().len);
    try testing.expect(remote.sessionCatalog()[1].empty);
    remote.bridge.outgoing.reset();
    return remote;
}

fn stage(remote: *Remote, name: []const u8) !void {
    try Remote.test_support.stageFixture(remote.bridge, name);
    _ = try remote.host.drainReadiness();
}

fn bind(model: *Model, remote: *Remote, window: usize) void {
    // Engine.create supplies an unstarted local placeholder tab. Selecting an
    // empty attachment removes that projection, as runtime session routing does.
    model.wsAt(window).?.tab_count = 0;
    model.bindSharedAttachment(remote);
    model.bindWindowAttachment(window, remote.context_id);
    const state = model.sharedWorkspaceForAttachment(remote.context_id).?;
    state.session = 3;
    state.epoch = remote.connectionEpoch();
    model.empty_picks[window] = .{ .attachment_id = remote.context_id, .coordinator = remote.providerId(), .session = 3, .window = window, .window_epoch = model.window_epochs[window] };
    model.empty_picks[window].?.setName("scratch");
}

const Hooks = struct {
    model: *Model,
    calls: usize = 0,
    context: u64 = 0,
    window: usize = 99,
    epoch: u64 = 0,
    focused: bool = false,
    refuse: bool = false,

    pub fn openPeerTabFromInWindow(self: *@This(), remote: *Remote, window: usize, epoch: u64, cwd: []const u8, may_focus: bool) !void {
        try testing.expectEqualStrings("", cwd);
        self.calls += 1;
        self.context = remote.context_id;
        self.window = window;
        self.epoch = epoch;
        self.focused = may_focus;
        if (self.refuse) return error.Refused;
    }
    pub fn openTabAt(_: *@This(), _: []const u8, _: ?usize) bool {
        return false;
    }
    pub fn openPeerTabAt(_: *@This(), _: support.ProviderId, _: []const u8, _: ?usize) bool {
        return false;
    }
};

test "empty window independent same-endpoint Clients retain both views and selection dismisses only its window" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    const first = try peer(engine);
    const second = try peer(engine);
    try testing.expectEqual(first.providerId(), second.providerId());
    try testing.expect(first.context_id != second.context_id);
    try testing.expect(empty.view(engine.model, 0) == null);
    bind(engine.model, first, 0);
    bind(engine.model, second, 1);
    try testing.expect(engine.model.windowOpen(0));
    try testing.expectEqual(@as(usize, 0), engine.model.primary.tab_count);
    try testing.expect(engine.model.phuxForAttachmentConst(first.context_id) == first);
    try testing.expectEqual(first.context_id, engine.model.window_attachments[0].?.id);
    try testing.expectEqual(engine.model.window_epochs[0], engine.model.empty_picks[0].?.window_epoch);
    try testing.expectEqual(first.context_id, empty.view(engine.model, 0).?.attachment_id);
    try testing.expectEqual(second.context_id, empty.view(engine.model, 1).?.attachment_id);
    try testing.expect(empty.holds(engine.model, 0, 0, false));
    try testing.expect(empty.holds(engine.model, 1, 0, false));
    engine.model.active_window = 1;
    empty.dismiss(engine.model);
    try testing.expect(engine.model.empty_picks[1] == null);
    try testing.expect(engine.model.empty_picks[0] != null);
    try second.show(1);
    try testing.expectEqualStrings("scratch", empty.view(engine.model, 0).?.name);
    try Remote.test_support.expectOutgoingCount(first.bridge, 0);
}

test "empty window New Tab callback captures exact provider window epoch and refuses duplicate" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    const first = try peer(engine);
    const second = try peer(engine);
    bind(engine.model, first, 0);
    bind(engine.model, second, 1);
    engine.model.active_window = 1;
    var hooks: Hooks = .{ .model = engine.model };
    try testing.expect(empty.newTab(&hooks, .{}) == .opened);
    try testing.expectEqual(second.context_id, hooks.context);
    try testing.expectEqual(@as(usize, 1), hooks.window);
    try testing.expectEqual(engine.model.window_epochs[1], hooks.epoch);
    try testing.expect(hooks.focused);
    try testing.expect(empty.newTab(&hooks, .{}) == .refused);
    try testing.expectEqual(@as(usize, 1), hooks.calls);
    try testing.expect(!engine.model.empty_picks[0].?.tab_requested);
    try Remote.test_support.expectOutgoingCount(first.bridge, 0);
    try Remote.test_support.expectOutgoingCount(second.bridge, 0);
}

test "empty window recycled slot and stale attachment never attach or fall back" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    const first = try peer(engine);
    const second = try peer(engine);
    bind(engine.model, first, 0);
    bind(engine.model, second, 1);
    const stale = engine.model.empty_picks[1].?;
    engine.model.closeWindow(1);
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    engine.model.active_window = 1;
    engine.model.empty_picks[1] = stale;
    var hooks: Hooks = .{ .model = engine.model };
    try testing.expect(empty.view(engine.model, 1) == null);
    try testing.expect(empty.newTab(&hooks, .{}) == .refused);
    try testing.expect(empty.pump(&hooks, 1));
    try testing.expect(engine.model.empty_picks[1] == null);
    bind(engine.model, second, 1);
    engine.model.empty_picks[1].?.attachment_id = std.math.maxInt(u64);
    try testing.expect(empty.view(engine.model, 1) == null);
    try testing.expect(empty.newTab(&hooks, .{}) == .refused);
    try testing.expectEqual(@as(usize, 0), hooks.calls);
    try Remote.test_support.expectOutgoingCount(first.bridge, 0);
    try Remote.test_support.expectOutgoingCount(second.bridge, 0);
}

test "empty window disconnect reconnect failure and retirement stay on their exact attachment" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    const first = try peer(engine);
    const second = try peer(engine);
    bind(engine.model, first, 0);
    bind(engine.model, second, 1);
    var hooks: Hooks = .{ .model = engine.model, .refuse = true };
    try testing.expect(empty.newTab(&hooks, .{}) == .refused);
    try testing.expect(!engine.model.empty_picks[0].?.tab_requested);
    engine.model.empty_picks[1].?.tab_requested = true;
    first.host.disconnect();
    empty.forgetAttachment(engine.model, first.context_id, false);
    try testing.expect(empty.view(engine.model, 0).?.unavailable);
    try testing.expect(!empty.view(engine.model, 1).?.unavailable);
    try testing.expect(engine.model.empty_picks[1].?.tab_requested);
    try testing.expect(empty.newTab(&hooks, .{}) == .refused);
    const old_epoch = first.connectionEpoch();
    try first.host.reconnect("empty-window");
    try testing.expect(first.connectionEpoch() != old_epoch);
    try testing.expect(empty.view(engine.model, 0).?.unavailable);
    try stage(first, "hello_keep_empty.bin");
    try first.host.attachSessionId(3, .{ .cols = 80, .rows = 24 });
    try stage(first, "attached_empty.bin");
    try stage(first, "workspace_empty.bin");
    try testing.expect(!empty.view(engine.model, 0).?.unavailable);
    try testing.expect(!engine.model.empty_picks[0].?.tab_requested);
    try testing.expectEqual(@as(usize, 1), hooks.calls);
    empty.forgetAttachment(engine.model, first.context_id, true);
    try testing.expect(engine.model.empty_picks[0] == null);
    try testing.expect(engine.model.empty_picks[1] != null);
    try Remote.test_support.expectOutgoingCount(second.bridge, 0);
}

test "empty window singleton compatibility rejects qualified identity and ambiguous machine" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const first = try peer(engine);
    engine.model.empty_pick = .{ .coordinator = first.providerId(), .session = 3, .window = 0, .attachment_id = first.context_id, .tab_requested = true };
    engine.model.empty_pick.?.setName("must not fall back");
    try testing.expect(empty.view(engine.model, 0) == null);
    engine.model.empty_pick.?.attachment_id = 0;
    try testing.expect(empty.view(engine.model, 0) != null);
    _ = try peer(engine);
    try testing.expect(empty.view(engine.model, 0) == null);
    try testing.expect(!empty.peerSessionEmpty(engine.model, first.providerId(), 3));
}

test "empty window delayed first tab waits for exact projection and never focuses another window" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    const first = try peer(engine);
    const second = try peer(engine);
    bind(engine.model, first, 0);
    bind(engine.model, second, 1);
    const state = engine.model.sharedWorkspaceForAttachment(first.context_id).?;
    state.attachment_id = second.context_id;
    var hooks: Hooks = .{ .model = engine.model };
    try testing.expect(empty.newTab(&hooks, .{}) == .opened);
    try testing.expect(engine.model.empty_picks[0].?.tab_requested);
    engine.model.active_window = 1;
    try testing.expect(!empty.pump(&hooks, 0));
    try testing.expectEqual(@as(usize, 0), hooks.calls);
    state.attachment_id = first.context_id;
    state.epoch +|= 1;
    try testing.expect(!empty.pump(&hooks, 0));
    state.epoch = first.connectionEpoch();
    try testing.expect(empty.pump(&hooks, 0));
    try testing.expectEqual(first.context_id, hooks.context);
    try testing.expectEqual(@as(usize, 0), hooks.window);
    try testing.expect(!hooks.focused);
    try testing.expect(!empty.pump(&hooks, 0));
    try testing.expectEqual(@as(usize, 1), hooks.calls);
    try testing.expect(empty.holds(engine.model, 0, 0, false));
    try testing.expect(!engine.model.empty_picks[0].?.tab_requested);
    try Remote.test_support.expectOutgoingCount(second.bridge, 0);
}

test "empty window qualified singleton never falls back to an ambient attached empty session" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const remote = try peer(engine);
    engine.model.phux_provider = remote;
    engine.model.peers.items[0].provider = null;
    engine.model.primary.tab_count = 0;
    try testing.expect(empty.view(engine.model, 0) != null);
    engine.model.empty_pick = .{ .coordinator = remote.providerId(), .session = 3, .window = 0, .attachment_id = std.math.maxInt(u64) };
    try testing.expect(empty.view(engine.model, 0) == null);
    var hooks: Hooks = .{ .model = engine.model };
    try testing.expect(empty.newTab(&hooks, .{}) == .refused);
    try Remote.test_support.expectOutgoingCount(remote.bridge, 0);
}

test "empty window primary attachment New Tab waits for its projection and pumps without a peer slot" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const remote = try peer(engine);
    engine.model.phux_provider = remote;
    engine.model.peers.items[0].provider = null;
    bind(engine.model, remote, 0);
    engine.model.shared_workspace.epoch = 0;
    var hooks: Hooks = .{ .model = engine.model };
    try testing.expect(empty.newTab(&hooks, .{}) == .opened);
    try testing.expectEqual(@as(usize, 0), hooks.calls);
    try testing.expect(!empty.pumpAttachment(&hooks, remote));
    engine.model.shared_workspace.epoch = remote.connectionEpoch();
    try testing.expect(empty.pumpAttachment(&hooks, remote));
    try testing.expectEqual(remote.context_id, hooks.context);
    try testing.expectEqual(@as(usize, 1), hooks.calls);
    try testing.expect(!empty.pumpAttachment(&hooks, remote));
    try Remote.test_support.expectOutgoingCount(remote.bridge, 0);
}
