const std = @import("std");
const engine_module = @import("native/ts_engine.zig");
const protocol = @import("native/ts_protocol.zig");
const model_module = @import("model.zig");
const support = @import("phux_support.zig");
const fixture = if (support.phux_enabled)
    support.PhuxProvider.test_support
else
    struct {};
const testing = std.testing;
const native_sdk = @import("native_sdk");
const ChannelFx = struct {
    pub fn phuxChannelLive(_: *const @This()) bool {
        return false;
    }
    pub fn openChannel(_: *const @This(), _: anytype) native_sdk.ChannelHandle {
        return .{};
    }
    pub fn closeChannel(_: *const @This(), _: u64) void {}
};

fn start() !*engine_module.Engine {
    const engine = try engine_module.Engine.create(testing.allocator, testing.io);
    errdefer engine.destroy();
    const remote = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture.sock" }, null, "test");
    model_module.attachPhuxProvider(engine.model, remote);
    try fixture.attachHost(remote.host);
    remote.attach_queued = true;
    _ = engine.recovery.pump(engine.model);
    engine.model.phux_admit_on_ready = false;
    engine.model.reconcileRemoteTerminals();
    try testing.expect(engine.model.admitAndSelectCurrentRemoteTerminal());
    remote.bridge.outgoing.reset();
    return engine;
}

fn command(engine: *engine_module.Engine, kind: protocol.IntentKind, argument: u8) bool {
    const bytes = protocol.encodeIntent(.{ .kind = kind, .expected_revision = engine.revision, .argument = argument, .window = 255 });
    return engine.applyIntent(&bytes, &engine_module.NoShells{});
}

fn feed(engine: *engine_module.Engine, name: []const u8) !void {
    const remote = engine.model.phux().?;
    try fixture.stageFixture(remote.bridge, name);
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
    try testing.expect(!engine.model.phux_connection_unavailable);
}

pub fn tabPublication() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const local_count = model.provider.activeCount();
    const tab_count = model.ws().tab_count;
    try testing.expect(command(engine, .new_terminal, 0));
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try testing.expectEqual(local_count, model.provider.activeCount());
    try testing.expectEqual(tab_count, model.ws().tab_count);
    try fixture.expectOutgoing(model.phux().?.bridge, "spawn-owner-request.bin");
    try feed(engine, "spawn-local.bin");
    try testing.expectEqual(tab_count, model.ws().tab_count);
    _ = model.openWindow(1).?;
    model.active_window = 1;
    try feed(engine, "local-ready.bin");
    try testing.expectEqual(tab_count + 1, model.primary.tab_count);
    try testing.expectEqual(@as(usize, 0), model.ws().tab_count);
    try testing.expectEqual(@as(usize, 1), model.active_window);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
}

pub fn splitDestination() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const original = engine.model.ws().selected_tab;
    try testing.expect(command(engine, .native_command, @intFromEnum(protocol.NativeCommand.split_right)));
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try feed(engine, "spawn-local.bin");
    try testing.expect(engine.model.selectTab(0));
    try feed(engine, "local-ready.bin");
    try testing.expectEqual(@as(usize, 2), engine.model.primary.tabs[original].paneCount());
    try testing.expectEqual(@as(usize, 1), engine.model.primary.tabs[0].paneCount());
}

pub fn windowEpoch() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try testing.expect(command(engine, .new_window, 0));
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    const window = engine.model.active_window;
    try feed(engine, "spawn-local.bin");
    engine.model.closeWindow(window);
    _ = engine.model.openWindow(window).?;
    try feed(engine, "local-ready.bin");
    try testing.expectEqual(@as(usize, 0), engine.model.wsAt(window).?.tab_count);
    try testing.expect(engine.model.terminal_limit_refused);
}

pub fn unknownOutcome() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const local_count = engine.model.provider.activeCount();
    try testing.expect(command(engine, .new_terminal, 0));
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    const remote = engine.model.phux().?;
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .closed }, null);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expect(engine.model.terminal_limit_refused);
    try testing.expect(!command(engine, .new_terminal, 0));
    try testing.expectEqual(local_count, engine.model.provider.activeCount());
    try testing.expect(!remote.bridge.outgoing.hasPending());
}

pub fn incarnationRecovery() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const ref = model.focusedTerminalRef().?;
    engine.recovery.disconnect(model);
    try testing.expect(model.attachmentPending(ref));
    try testing.expect(engine.recovery.pump(model));
    try testing.expect(!model.attachmentPending(ref));
    const index = model.saved_attachments.find(ref).?;
    model.saved_attachments.entries[index].?.context.server_id = try .init("different-incarnation");
    engine.recovery.disconnect(model);
    const local_count = model.provider.activeCount();
    try testing.expect(!engine.recovery.pump(model));
    try testing.expect(model.attachmentPending(ref));
    try testing.expectEqual(local_count, model.provider.activeCount());
    try testing.expect(!model.phux().?.bridge.outgoing.hasPending());
}

pub fn restoredSubscription() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const ref: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteTerminalId.fromPhux(0, 8, "") } };
    try testing.expect(model.admitTab(ref));
    engine.recovery.disconnect(model);
    try testing.expect(model.attachmentPending(ref));
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
    try fixture.expectOutgoing(model.phux().?.bridge, "attach-local-request.bin");
    try testing.expect(model.attachmentPending(ref));
    try feed(engine, "local-ready.bin");
    try testing.expect(model.attachmentPending(ref));
    try feed(engine, "restore-accepted.bin");
    try testing.expect(!model.attachmentPending(ref));
    try testing.expect(model.remotePresentation(ref) != null);
}

pub fn destinationReservations() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    while (model.primary.tab_count < model_module.max_tabs - 1) {
        const pane = try model.provider.createTerminal();
        try testing.expect(model.admitTab(pane.id));
    }
    try testing.expect(command(engine, .new_terminal, 0));
    try testing.expect(!command(engine, .new_terminal, 0));
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
}

pub fn windowRefusal() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try testing.expect(command(engine, .new_window, 0));
    const window = engine.model.active_window;
    try testing.expect(engine.model.windowOpen(window));
    try feed(engine, "spawn-refused.bin");
    try testing.expect(!engine.model.windowOpen(window));
    try testing.expect(engine.model.terminal_limit_refused);
}

pub fn splitReservations() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const tree = model.selectedTree().?;
    const origin = tree.focusedTerminal().?;
    while (tree.paneCount() < @import("layout.zig").max_panes - 1) {
        const pane = try model.provider.createTerminal();
        _ = try tree.split(tree.find(origin).?, .horizontal, pane.id);
        _ = tree.focusTerminal(origin);
    }
    try testing.expect(command(engine, .native_command, @intFromEnum(protocol.NativeCommand.split_right)));
    try testing.expect(!command(engine, .native_command, @intFromEnum(protocol.NativeCommand.split_right)));
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
}

pub fn restoredEmptyWorkspace() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const startup = @import("startup.zig");
    const path = ".zig-cache/durable-empty-workspace.state";
    defer std.Io.Dir.cwd().deleteFile(testing.io, path) catch {};
    const seed = try engine_module.Engine.create(testing.allocator, testing.io);
    seed.model.state.setPath(path);
    _ = seed.model.provider.destroyTerminal(seed.model.focusedTerminalRef().?);
    seed.model.dropTab(0);
    seed.model.writeWorkspaceState(testing.io);
    seed.destroy();
    var initialized = try startup.initializeResolvedModel(testing.allocator, testing.io, .{}, null, path, null);
    try testing.expectEqual(.restored, initialized.provenance);
    const remote = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture.sock" }, null, "test");
    model_module.attachPhuxProvider(&initialized.model, remote);
    const engine = try engine_module.Engine.createFromInitialized(initialized);
    defer engine.destroy();
    try testing.expectEqual(@as(usize, 0), engine.model.provider.activeCount());
    try testing.expect(engine.model.focusedTerminalRef() == null);
}

pub fn reconnectClosePublishes() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try testing.expect(command(engine, .new_window, 0));
    const window = engine.model.active_window;
    const revision = engine.revision;
    engine.model.phux_reconnect_after_close = true;
    const changed = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .closed }, null);
    try testing.expect(!engine.model.windowOpen(window));
    try testing.expect(engine.model.phux_connection_unavailable);
    try testing.expect(changed);
    try testing.expect(engine.revision > revision);
}

pub fn directReconnectFences() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const ref = model.focusedTerminalRef().?;
    try testing.expect(!model.attachmentPending(ref));
    try testing.expect(command(engine, .new_window, 0));
    const window = model.active_window;
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try testing.expect(engine.restartNavigationConnection(&ChannelFx{}, null));
    try testing.expect(model.phux_connection_unavailable);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expect(!model.windowOpen(window));
    try testing.expect(model.attachmentPending(ref));
}

pub fn earlyTerminalDeath() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const original = model.focusedTerminalRef().?;
    try testing.expect(command(engine, .new_window, 0));
    const window = model.active_window;
    const remote = model.phux().?;
    remote.bridge.outgoing.reset();
    try fixture.stageFixture(remote.bridge, "spawn-local.bin");
    try fixture.stageFixture(remote.bridge, "local-ready.bin");
    try fixture.stageFixture(remote.bridge, "spawned-terminal-closed.bin");
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expect(!model.windowOpen(window));
    try testing.expect(model.locateTerminal(original) != null);
    // READY may enqueue protocol acknowledgments, but never another spawn.
    try testing.expectEqual(@as(u32, 1), remote.host.operation_ledger.last_id);
}

pub fn titleAnnouncement() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const ref = model.focusedTerminalRef().?;
    const revision = engine.revision;
    try testing.expect(!std.mem.eql(u8, model.remotePresentation(ref).?.title, "remote-title-review"));
    try fixture.stageFixture(model.phux().?.bridge, "remote-title.bin");
    const changed = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
    try testing.expectEqualStrings("remote-title-review", model.remotePresentation(ref).?.title);
    try testing.expect(changed);
    try testing.expect(engine.revision > revision);
}
