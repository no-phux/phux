//! PHA-284 on the REMOTE provider: a real Model and Engine over a real Phux
//! Provider and FFI Client; only server frames are scripted, as canonical
//! Rust-encoded fixtures. The local provider's coverage cannot stand in for
//! these: the projected path is exactly where a fault once shipped unseen.
const std = @import("std");
const testing = std.testing;
const native_sdk = @import("native_sdk");
const support = @import("../phux_support.zig");
const creation = @import("../durable_creation_tests.zig");
const model_module = @import("../model.zig");
const engine_module = @import("ts_engine.zig");
const projection = @import("workspace_projection.zig");
const empty_session = @import("empty_session.zig");
const fixture = if (support.phux_enabled) support.PhuxProvider.test_support else struct {};

const NoticeFx = struct {
    notifications: usize = 0,
    window_closes: usize = 0,
    quits: usize = 0,
    pub fn phuxChannelLive(_: *const @This()) bool {
        return false;
    }
    pub fn openChannel(_: *@This(), _: anytype) native_sdk.ChannelHandle {
        return .{};
    }
    pub fn closeChannel(_: *@This(), _: u64) void {}
    pub fn showNotification(self: *@This(), _: anytype) void {
        self.notifications += 1;
    }
    pub fn closeWindow(self: *@This(), _: []const u8) void {
        self.window_closes += 1;
    }
    pub fn quitApp(self: *@This()) void {
        self.quits += 1;
    }
};

fn ref(id: u32) support.TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = id } } };
}

fn drain(engine: *engine_module.Engine, fx: *NoticeFx) bool {
    return engine.onPhuxChannel(fx, .{ .key = support.phux_channel_key, .kind = .data }, null);
}

fn stage(engine: *engine_module.Engine, name: []const u8) !void {
    try fixture.stageFixture(engine.model.phux().?.bridge, name);
}

fn feed(engine: *engine_module.Engine, fx: *NoticeFx, name: []const u8) !bool {
    try stage(engine, name);
    return drain(engine, fx);
}

fn title(engine: *engine_module.Engine, id: u32, storage: []u8) []const u8 {
    return projection.terminalTitleInto(engine.model, ref(id), storage);
}

/// A canonical workspace reply re-correlated to internal request `request`.
/// Field 1 is the request ID: the fixture's TLV descriptor must still match.
fn stageWorkspaceReply(engine: *engine_module.Engine, name: []const u8, request: u32) !void {
    const bytes = try fixture.readFixture(name);
    defer testing.allocator.free(bytes);
    try testing.expectEqualSlices(u8, &.{ 1, 4, 4 }, bytes[5..8]);
    std.mem.writeInt(u32, bytes[8..12], 0x8000_0000 + request, .big);
    try testing.expect(engine.model.phux().?.bridge.incoming.stage(bytes));
}

/// Stage a workspace refresh whose registry holds `state`, with no layout
/// metadata; `first` is the refresh's first internal request.
fn stageRefresh(engine: *engine_module.Engine, state: []const u8, first: u32) !void {
    try testing.expect((try engine.model.phux().?.requestWorkspaceRefresh()) != null);
    try stageWorkspaceReply(engine, state, first);
    try stageWorkspaceReply(engine, "status-no-layout-metadata.bin", first + 1);
}

/// Attached to a keep-empty session 1 holding terminal 7 (ADR-0114).
fn startKeepEmpty() !*engine_module.Engine {
    const engine = try engine_module.Engine.create(testing.allocator, testing.io);
    errdefer engine.destroy();
    const remote = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture.sock" }, null, "keep-empty");
    model_module.attachPhuxProvider(engine.model, remote);
    try fixture.attachHostWith(remote.host, "hello_keep_empty.bin");
    remote.attach_queued = true;
    var fx: NoticeFx = .{};
    _ = drain(engine, &fx);
    remote.bridge.outgoing.reset();
    try stageRefresh(engine, "status-keep-empty-state.bin", 2);
    _ = drain(engine, &fx);
    try testing.expect(engine.model.locateTerminal(ref(7)) != null);
    return engine;
}

const Ordering = enum { same_drain, snapshot_first, snapshot_later };

/// Terminal 7's shell ends and the snapshot without it lands in `ordering`.
fn endWithSnapshot(engine: *engine_module.Engine, fx: *NoticeFx, ordering: Ordering, state: []const u8, first: u32) !void {
    switch (ordering) {
        .same_drain => {
            try stageRefresh(engine, state, first);
            _ = try feed(engine, fx, "remote-exited.bin");
        },
        .snapshot_first => {
            try stageRefresh(engine, state, first);
            _ = drain(engine, fx);
            _ = try feed(engine, fx, "remote-exited.bin");
        },
        .snapshot_later => {
            _ = try feed(engine, fx, "remote-exited.bin");
            try stageRefresh(engine, state, first);
            _ = drain(engine, fx);
        },
    }
}

/// A keep-empty session's window stays, showing Empty session.
fn expectEmptySession(engine: *engine_module.Engine, fx: *const NoticeFx) !void {
    const model = engine.model;
    try testing.expect(model.locateTerminal(ref(7)) == null);
    try testing.expect(model.windowOpen(0));
    try testing.expectEqual(@as(usize, 0), model.ws().tab_count);
    try testing.expect(!model.ws().web_selected);
    try testing.expectEqual(@as(usize, 0), fx.quits);
    try testing.expect(empty_session.view(model, 0) != null);
}

/// Any other emptied window closes, and the last window closing quits.
fn expectWindowRetired(engine: *engine_module.Engine, fx: *const NoticeFx) !void {
    try testing.expect(engine.model.locateTerminal(ref(7)) == null);
    try testing.expect(!engine.model.windowOpen(0));
    try testing.expectEqual(@as(usize, 1), fx.window_closes);
    try testing.expectEqual(@as(usize, 1), fx.quits);
}

test "remote cwd status renames an untitled tab to the basename" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    var fx: NoticeFx = .{};
    var storage: [projection.max_terminal_title_bytes]u8 = undefined;
    try testing.expect(!std.mem.eql(u8, "cockpit-fixture", title(engine, 7, &storage)));
    try testing.expect(try feed(engine, &fx, "remote-cwd.bin"));
    try testing.expectEqualStrings("cockpit-fixture", title(engine, 7, &storage));
    // The shell's own title still outranks the directory it sits in.
    _ = try feed(engine, &fx, "remote-title.bin");
    try testing.expectEqualStrings("remote-title-review", title(engine, 7, &storage));
    try testing.expectEqual(@as(usize, 0), fx.notifications);
}

test "a live cwd of / names the tab / and an overlong one names nothing" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    var fx: NoticeFx = .{};
    var storage: [projection.max_terminal_title_bytes]u8 = undefined;
    const before = try testing.allocator.dupe(u8, title(engine, 7, &storage));
    defer testing.allocator.free(before);
    _ = try feed(engine, &fx, "remote-cwd-root.bin");
    try testing.expectEqualStrings("/", title(engine, 7, &storage));
    _ = try feed(engine, &fx, "remote-cwd.bin");
    _ = try feed(engine, &fx, "remote-cwd-overlong.bin");
    try testing.expectEqualStrings(before, title(engine, 7, &storage));
}

test "remote exited with reason exited closes the pane and killed too" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    for ([_][]const u8{ "remote-exited.bin", "remote-killed.bin" }) |name| {
        const engine = try creation.start();
        defer engine.destroy();
        const tree = &engine.model.ws().tabs[0];
        _ = try tree.split(tree.root, .horizontal, ref(8));
        var fx: NoticeFx = .{};
        try testing.expect(try feed(engine, &fx, name));
        try testing.expect(engine.model.locateTerminal(ref(7)) == null);
        try testing.expect(engine.model.locateTerminal(ref(8)) != null);
        try testing.expectEqual(@as(usize, 1), tree.paneCount());
        try testing.expectEqual(@as(usize, 0), fx.quits);
    }
}

test "remote exited in the last pane closes the emptied window, and the last window quits" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    var fx: NoticeFx = .{};
    try testing.expect(try feed(engine, &fx, "remote-exited.bin"));
    try expectWindowRetired(engine, &fx);
}

test "remote exited in a keep-empty session's last pane shows Empty session" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try startKeepEmpty();
    defer engine.destroy();
    var fx: NoticeFx = .{};
    _ = try feed(engine, &fx, "remote-exited.bin");
    try testing.expect(engine.model.locateTerminal(ref(7)) == null);
    try testing.expect(engine.model.windowOpen(0));
    try testing.expectEqual(@as(usize, 0), engine.model.ws().tab_count);
    try testing.expect(!engine.model.ws().web_selected);
    try testing.expectEqual(@as(usize, 0), fx.quits);
}

test "a keep-empty session settles on Empty session whichever of the close and the snapshot lands first" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    for ([_]Ordering{ .same_drain, .snapshot_first, .snapshot_later }) |ordering| {
        const engine = try startKeepEmpty();
        defer engine.destroy();
        var fx: NoticeFx = .{};
        try endWithSnapshot(engine, &fx, ordering, "status-keep-empty-ended-state.bin", 4);
        try expectEmptySession(engine, &fx);
    }
}

test "any other session retires its emptied window whichever of the close and the snapshot lands first" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    for ([_]Ordering{ .same_drain, .snapshot_first, .snapshot_later }) |ordering| {
        const engine = try creation.start();
        defer engine.destroy();
        var fx: NoticeFx = .{};
        try endWithSnapshot(engine, &fx, ordering, "status-ended-state.bin", 2);
        try expectWindowRetired(engine, &fx);
    }
}

test "a hundred title updates and one exit in one drain still retire the pane" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    const tree = &engine.model.ws().tabs[0];
    _ = try tree.split(tree.root, .horizontal, ref(8));
    var fx: NoticeFx = .{};
    try stage(engine, "remote-title-burst.bin");
    try stage(engine, "remote-exited.bin");
    _ = drain(engine, &fx);
    try testing.expect(engine.model.locateTerminal(ref(7)) == null);
    try testing.expect(engine.model.locateTerminal(ref(8)) != null);
}

test "remote never spawned keeps restart" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    var fx: NoticeFx = .{};
    // A close that does not say the shell ended (this fixture's reason is
    // unstated) is not Cockpit's "ended": the pane stays standing, with its
    // attention marker, for Restart to target.
    _ = try feed(engine, &fx, "initial-terminal-closed.bin");
    try testing.expect(engine.model.locateTerminal(ref(7)) != null);
    try testing.expect(projection.terminalNeedsAttention(engine.model, ref(7)));
}

test "at prompt flips on command started and finished" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    var fx: NoticeFx = .{};
    try testing.expect(!projection.terminalAtPrompt(engine.model, ref(7)));
    _ = try feed(engine, &fx, "remote-command-finished.bin");
    try testing.expect(projection.terminalAtPrompt(engine.model, ref(7)));
    _ = try feed(engine, &fx, "remote-command-started.bin");
    try testing.expect(!projection.terminalAtPrompt(engine.model, ref(7)));
    _ = try feed(engine, &fx, "remote-command-finished.bin");
    try testing.expect(projection.terminalAtPrompt(engine.model, ref(7)));
}

var command_test_now_ns: u64 = 0;

fn commandTestClock() u64 {
    return command_test_now_ns;
}

/// One command on terminal 7 that ran `duration_ns`.
fn runCommand(engine: *engine_module.Engine, fx: *NoticeFx, duration_ns: u64) !void {
    command_test_now_ns += std.time.ns_per_s;
    _ = try feed(engine, fx, "remote-command-started.bin");
    command_test_now_ns += duration_ns;
    _ = try feed(engine, fx, "remote-command-finished.bin");
}

test "command finished on an unfocused pane raises a notice like a bell" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    engine.model.phux().?.host.now_ns = &commandTestClock;
    // Terminal 8 takes the focus, so terminal 7 is the unattended pane.
    const tree = &engine.model.ws().tabs[0];
    _ = try tree.split(tree.root, .horizontal, ref(8));
    engine.model.focused = false;
    var fx: NoticeFx = .{};
    // host.zig's min_command_notice_ns: a command this long is announced.
    const long: u64 = 10 * std.time.ns_per_s;
    // A finish with no start seen, and rapid short commands: nothing posted.
    _ = try feed(engine, &fx, "remote-command-finished.bin");
    for (0..5) |_| try runCommand(engine, &fx, std.time.ns_per_s);
    try testing.expectEqual(@as(usize, 0), fx.notifications);
    // One long command: exactly one, latched until acknowledged.
    try runCommand(engine, &fx, long);
    try testing.expectEqual(@as(usize, 1), fx.notifications);
    var storage: [projection.max_terminal_title_bytes]u8 = undefined;
    try testing.expectEqualStrings(title(engine, 7, &storage), engine.model.notifiedTitle());
    try runCommand(engine, &fx, long);
    try testing.expectEqual(@as(usize, 1), fx.notifications);
    // Regaining focus attends it; the next long command posts again.
    engine.setFocused(&engine_module.NoShells{}, true);
    engine.setFocused(&engine_module.NoShells{}, false);
    try runCommand(engine, &fx, long);
    try testing.expectEqual(@as(usize, 2), fx.notifications);
    // The bell's gate: nothing is posted while the app has the user's eyes.
    engine.setFocused(&engine_module.NoShells{}, true);
    try runCommand(engine, &fx, long);
    try testing.expectEqual(@as(usize, 2), fx.notifications);
}
