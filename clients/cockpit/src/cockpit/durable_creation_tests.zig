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
    pub fn showNotification(_: *const @This(), _: anytype) void {}
};

pub fn start() !*engine_module.Engine {
    const engine = try engine_module.Engine.create(testing.allocator, testing.io);
    errdefer engine.destroy();
    const remote = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture.sock" }, null, "test");
    model_module.attachPhuxProvider(engine.model, remote);
    try fixture.attachHost(remote.host);
    remote.attach_queued = true;
    try drain(engine);
    remote.bridge.outgoing.reset();
    return engine;
}

pub fn frozenPaintRecovery() !void {
    try @import("frozen_paint_tests.zig").recovery();
}

fn command(engine: *engine_module.Engine, kind: protocol.IntentKind, argument: u8) bool {
    const bytes = protocol.encodeIntent(.{ .kind = kind, .expected_revision = engine.revision, .argument = argument, .window = 255 });
    return engine.applyIntent(&bytes, &engine_module.NoShells{});
}

fn feed(engine: *engine_module.Engine, name: []const u8) !void {
    const remote = engine.model.phux().?;
    try fixture.stageFixture(remote.bridge, name);
    try drain(engine);
}

fn drain(engine: *engine_module.Engine) !void {
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
    try testing.expect(!engine.model.phux_connection_unavailable);
}

/// Canonical Rust bodies with only the scenario's request correlation changed.
/// Field 1 is the request ID: the fixture's TLV descriptor must still match.
fn stageReply(engine: *engine_module.Engine, directory: []const u8, name: []const u8, expected: u32, request: u32) !void {
    const path = try std.fmt.allocPrint(testing.allocator, "src/{s}/fixtures/{s}", .{ directory, name });
    defer testing.allocator.free(path);
    const bytes = try std.Io.Dir.cwd().readFileAlloc(testing.io, path, testing.allocator, .limited(64 * 1024));
    defer testing.allocator.free(bytes);
    try testing.expectEqualSlices(u8, &.{ 1, 4, 4 }, bytes[5..8]);
    try testing.expectEqual(expected, std.mem.readInt(u32, bytes[8..12], .big));
    std.mem.writeInt(u32, bytes[8..12], request, .big);
    try testing.expect(engine.model.phux().?.bridge.incoming.stage(bytes));
}

fn workspaceReply(engine: *engine_module.Engine, name: []const u8, expected: u32, request: u32) !void {
    try stageReply(engine, "providers/phux", name, 0x8000_0000 + expected, 0x8000_0000 + request);
}

fn confirmCreation(engine: *engine_module.Engine, kind: enum { add, split }) !void {
    const metadata = if (kind == .split) "workspace_rename_metadata.bin" else "workspace_refresh_metadata.bin";
    try workspaceReply(engine, metadata, if (kind == .split) 5 else 3, 3);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 2);
    try drain(engine);
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try testing.expectEqual(.pending, engine.model.phux().?.workspaceSnapshot().status);
    if (kind == .split) {
        try workspaceReply(engine, "workspace_split_metadata.bin", 8, 5);
        try workspaceReply(engine, "workspace_split_state.bin", 7, 4);
    } else {
        try workspaceReply(engine, "workspace_add_metadata.bin", 14, 5);
        try workspaceReply(engine, "workspace_add_state.bin", 13, 4);
    }
    try drain(engine);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
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
    try testing.expectEqual(tab_count, model.primary.tab_count);
    try confirmCreation(engine, .add);
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
    _ = engine.model.openWindow(1).?;
    engine.model.active_window = 1;
    try feed(engine, "local-ready.bin");
    try testing.expectEqual(@as(usize, 1), engine.model.primary.tabs[original].paneCount());
    try confirmCreation(engine, .split);
    try testing.expectEqual(@as(usize, 2), engine.model.primary.tabs[original].paneCount());
    try testing.expectEqual(@as(usize, 0), engine.model.ws().tab_count);
    try testing.expectEqual(@as(usize, 1), engine.model.active_window);
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

test "cutover new native window receives only its confirmed shared singleton" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const original = model.focusedTerminalRef().?;
    try testing.expect(command(engine, .new_window, 0));
    const window = model.active_window;
    try testing.expect(window != 0);
    try testing.expectEqual(@as(usize, 0), model.ws().tab_count);
    try feed(engine, "spawn-local.bin");
    try feed(engine, "local-ready.bin");
    try testing.expectEqual(@as(usize, 0), model.ws().tab_count);
    try confirmCreation(engine, .add);
    try testing.expectEqual(window, model.locateTerminal(refFor(8)).?.window);
    try testing.expectEqual(@as(usize, 0), model.locateTerminal(original).?.window);
    try testing.expect(model.focusedTerminalRef().?.eql(refFor(8)));
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
    try testing.expect(!engine.model.terminal_limit_refused);
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
    const remote = model.phux().?;
    const original_owner = model.terminalOwner(ref).?;
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .closed }, null);
    try testing.expect(model.attachmentPending(ref));
    try testing.expect(model.terminalOwner(ref) == null);
    try reconnectBootstrap(engine, false);
    try testing.expect(model.attachmentPending(ref));
    try testing.expect(model.terminalOwner(ref) == null);
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_metadata.bin");
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_state.bin");
    try drain(engine);
    try testing.expect(!model.attachmentPending(ref));
    try testing.expect(model.terminalOwner(ref) != null);
    try testing.expect(!remote.ownerIsCurrent(original_owner));
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .closed }, null);
    const local_count = model.provider.activeCount();
    try reconnectBootstrap(engine, true);
    // A reused numeric identity from a replacement server cannot authorize the
    // old pane before that server publishes its own authoritative workspace.
    try testing.expect(model.attachmentPending(ref));
    try testing.expect(model.terminalOwner(ref) == null);
    try testing.expectEqual(local_count, model.provider.activeCount());
    try testing.expectEqual(@as(u32, 0), remote.host.operation_ledger.last_id);
}

fn reconnectBootstrap(engine: *engine_module.Engine, replacement: bool) !void {
    const remote = engine.model.phux().?;
    try remote.host.reconnect("shared-context-test");
    remote.attach_queued = false;
    const hello = try fixture.readFixture("hello.bin");
    defer testing.allocator.free(hello);
    if (replacement) {
        const offset = std.mem.indexOf(u8, hello, "cockpit-fixture") orelse return error.MissingServerIdentity;
        @memcpy(hello[offset..][0.."foreign-server!".len], "foreign-server!");
    }
    try testing.expect(remote.bridge.incoming.stage(hello));
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
    try feed(engine, "attached.bin");
}

pub fn restoredSubscription() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const ref: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, 8, "") } };
    const remote = model.phux().?;
    _ = try remote.requestWorkspaceRefresh();
    try workspaceReply(engine, "workspace_split_metadata.bin", 8, 3);
    try workspaceReply(engine, "workspace_split_state.bin", 7, 2);
    try drain(engine);
    try testing.expect(model.locateTerminal(ref) != null);
    try testing.expect(remote.presentation(ref) == null);
    try stageReply(engine, "tests", "restore-accepted.bin", 1, 2);
    try drain(engine);
    try testing.expect(remote.presentation(ref) == null);
    try feed(engine, "local-ready.bin");
    try testing.expect(model.remotePresentation(ref) != null);
    try testing.expectEqual(@as(u32, 2), remote.host.operation_ledger.last_id);
}

pub fn destinationReservations() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const available = model_module.max_tabs - model.primary.tab_count;
    for (0..available) |_| try testing.expect(command(engine, .new_terminal, 0));
    try testing.expect(!command(engine, .new_terminal, 0));
    try testing.expectEqual(available, engine.creation.count());
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
    const available = @import("layout.zig").max_panes - model.selectedTree().?.paneCount();
    for (0..available) |_| try testing.expect(command(engine, .native_command, @intFromEnum(protocol.NativeCommand.split_right)));
    try testing.expect(!command(engine, .native_command, @intFromEnum(protocol.NativeCommand.split_right)));
    try testing.expectEqual(available, engine.creation.count());
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
    const initialized = initialized: {
        const value = try startup.initializeResolvedModel(testing.allocator, testing.io, .{}, null, path, null);
        errdefer std.heap.page_allocator.destroy(value.model);
        errdefer model_module.deinitModel(value.model);
        try testing.expectEqual(.restored, value.provenance);
        const remote = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture.sock" }, null, "test");
        model_module.attachPhuxProvider(value.model, remote);
        break :initialized value;
    };
    const engine = try engine_module.Engine.createFromInitialized(initialized);
    defer engine.destroy();
    try testing.expectEqual(@as(usize, 0), engine.model.provider.activeCount());
    const remote = engine.model.phux().?;
    try fixture.attachHost(remote.host);
    _ = try engine.model.shared_workspace.apply(engine.model, remote.workspaceSnapshot(), remote.connectionEpoch());
    try testing.expectEqual(@as(u32, 7), engine.model.focusedTerminalRef().?.terminal_id.phux.id);
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

pub fn emptyTitleReconnect() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const remote = engine.model.phux().?;
    const ref = engine.model.focusedTerminalRef().?;
    try feed(engine, "remote-title.bin");
    try testing.expectEqualStrings("remote-title-review", remote.presentation(ref).?.title);
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .closed }, null);
    try remote.host.reconnect("title-reconnect");
    try fixture.stageFixture(remote.bridge, "hello.bin");
    _ = try remote.host.drainReadiness();
    try remote.host.attachSessionId(1, .{ .cols = 80, .rows = 24 });
    try feed(engine, "attached.bin");
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_metadata.bin");
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_state.bin");
    try drain(engine);
    try testing.expectEqualStrings("", engine.model.remotePresentation(ref).?.title);
}

test "Phux creation refuses unavailable workspace before spawning or subscribing" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const ref = model.focusedTerminalRef().?;
    const local_count = model.provider.activeCount();
    model.shared_workspace.session = 0;
    try testing.expectError(error.WorkspaceUnavailable, engine.creation.request(model, .tab));
    try testing.expectError(error.WorkspaceUnavailable, engine.creation.requestAttach(model, ref));
    try testing.expectError(error.WorkspaceUnavailable, engine.creation.requestAdmit(model, ref));
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expectEqual(@as(u32, 0), model.phux().?.host.operation_ledger.last_id);
    try testing.expectEqual(local_count, model.provider.activeCount());
}

test "session change retires accepted creation without redirecting its queued mutation" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    try engine.creation.request(model, .tab);
    try feed(engine, "spawn-local.bin");
    try feed(engine, "local-ready.bin");
    try testing.expectEqual(@as(u32, 2), remote.host.operation_ledger.last_id);
    model.shared_workspace.session = 2;
    try testing.expect(engine.creation.pump(model));
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 3);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 2);
    try drain(engine);
    try testing.expectEqual(@as(u32, 3), remote.host.operation_ledger.last_id);
    try testing.expect(remote.host.operation_ledger.detaching(refFor(8)));
    try testing.expect(!model.terminal_limit_refused);
    try expectCatalogTerminal(engine, 8);
    try testing.expectEqual(@as(usize, 1), model.primary.tab_count);
}

test "refused shared creation retains the accepted durable terminal in the catalog" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    try engine.creation.request(model, .tab);
    try feed(engine, "spawn-local.bin");
    try feed(engine, "local-ready.bin");
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 3);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 2);
    try drain(engine);
    try testing.expectEqual(@as(u32, 3), remote.host.operation_ledger.last_id);
    // Another writer's unchanged singleton wins the confirmation read.
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 5);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 4);
    try drain(engine);
    try testing.expectEqual(.refused, remote.workspaceSnapshot().status);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expectEqual(@as(usize, 1), model.primary.tab_count);
    try testing.expect(model.terminal_limit_refused);
    try expectCatalogTerminal(engine, 8);
}

fn expectCatalogTerminal(engine: *engine_module.Engine, id: u32) !void {
    const ref: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, id, "") } };
    const remote = engine.model.phux().?;
    try testing.expect(remote.terminalKnown(ref));
    for (remote.catalogTerminals()) |entry| {
        if (entry.terminal_ref.eql(ref)) return;
    }
    return error.TerminalMissingFromCatalog;
}

fn refFor(id: u32) support.TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = id } } };
}

fn navigationBytes(revision: u64, index: u16) [12]u8 {
    var bytes = [_]u8{0} ** 12;
    bytes[0] = 1;
    bytes[1] = 13;
    std.mem.writeInt(u64, bytes[2..10], revision, .little);
    std.mem.writeInt(u16, bytes[10..12], index, .little);
    return bytes;
}

fn navigationIndex(engine: *engine_module.Engine, destination: model_module.PaletteDestination) !u16 {
    const projection = @import("native/workspace_projection.zig");
    var entries: [16]projection.PaletteEntry = undefined;
    const count = projection.paletteEntriesWindowIn(engine.model, engine.model.wsConst(), .{ .first = 0, .count = entries.len }, &entries);
    for (entries[0..count], 0..) |entry, index| {
        if (destinationMatches(entry, destination)) return @intCast(index);
    }
    return error.MissingNavigationDestination;
}

fn destinationMatches(actual: model_module.PaletteDestination, expected: model_module.PaletteDestination) bool {
    if (std.meta.activeTag(actual) != std.meta.activeTag(expected)) return false;
    return switch (expected) {
        .available_terminal => |ref| actual.available_terminal.eql(ref),
        .session => |id| actual.session == id,
        .placed_terminal => false,
    };
}

pub fn navigationSharedAdmission(fx: anytype) !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    // A terminal spawned by another command is live but remains catalog-only
    // until an explicit navigation intent confirms its shared admission.
    _ = try remote.requestSpawn(model.focusedTerminalRef(), remote.attach_viewport);
    try feed(engine, "spawn-local.bin");
    try fixture.stageFixture(remote.bridge, "local-ready.bin");
    _ = try remote.drainReadiness();
    _ = try remote.requestWorkspaceRefresh();
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 3);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 2);
    _ = try remote.drainReadiness();
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    model.reconcileRemoteTerminals();
    const ref = refFor(8);
    try testing.expect(model.locateTerminal(ref) == null);
    const available = navigationBytes(engine.revision, try navigationIndex(engine, .{ .available_terminal = ref }));
    try testing.expect(engine.applyIntent(&available, fx));
    try testing.expect(model.locateTerminal(ref) == null);
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 5);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 4);
    try drain(engine);
    try workspaceReply(engine, "workspace_add_metadata.bin", 14, 7);
    try workspaceReply(engine, "workspace_add_state.bin", 13, 6);
    try drain(engine);
    try testing.expect(model.focusedTerminalRef().?.eql(ref));
    try testing.expectEqual(@as(usize, 2), model.ws().tab_count);
    try testing.expectEqual(@as(u32, 4), remote.host.operation_ledger.last_id);
    const session = navigationBytes(engine.revision, try navigationIndex(engine, .{ .session = 2 }));
    try testing.expect(engine.applyIntent(&session, fx));
    try testing.expectEqual(@as(?u32, 2), remote.session_id);
    try testing.expectEqual(@as(?u32, 1), remote.selectedSessionId());
    try testing.expectEqual(@as(usize, 1), fx.navigation_restarts);
    try testing.expect(!engine.applyIntent(&session, fx));
    remote.host.disconnect();
    model.phux_connection_unavailable = true;
    var reconnect = navigationBytes(engine.revision, 0);
    reconnect[1] = 12;
    try testing.expect(engine.applyIntent(&reconnect, fx));
    try testing.expectEqual(@as(usize, 2), fx.navigation_restarts);
}

pub fn sharedCloseAndCatalogAdmission() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    try engine.creation.request(model, .tab);
    try feed(engine, "spawn-local.bin");
    try feed(engine, "local-ready.bin");
    try confirmCreation(engine, .add);
    const ref = refFor(8);
    const owner = remote.owner(ref).?;
    try testing.expect(command(engine, .native_command, @intFromEnum(protocol.NativeCommand.close_focused_pane)));
    try testing.expect(model.locateTerminal(ref) != null);
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 8);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 7);
    try drain(engine);
    try testing.expect(model.locateTerminal(ref) == null);
    try testing.expect(!remote.ownerIsCurrent(owner));
    try expectCatalogTerminal(engine, 8);
    try stageReply(engine, "tests", "detach-ok.bin", 1, 5);
    try drain(engine);
    try testing.expect(remote.presentation(ref) == null);
    const select = navigationBytes(engine.revision, try navigationIndex(engine, .{ .available_terminal = ref }));
    try testing.expect(engine.applyIntent(&select, &engine_module.NoShells{}));
    try testing.expect(model.locateTerminal(ref) == null);
    try stageReply(engine, "tests", "restore-accepted.bin", 1, 6);
    try drain(engine);
    try feed(engine, "local-ready.bin");
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 11);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 10);
    try drain(engine);
    try workspaceReply(engine, "workspace_add_metadata.bin", 14, 13);
    try workspaceReply(engine, "workspace_add_state.bin", 13, 12);
    try drain(engine);
    try testing.expect(model.locateTerminal(ref) != null);
    try testing.expect(remote.owner(ref) != null);
    try testing.expectEqual(@as(u32, 8), remote.host.operation_ledger.last_id);
}

fn moveSharedWindowToSecondary(engine: *engine_module.Engine) !void {
    const model = engine.model;
    const remote = model.phux().?;
    const id = model.primary.shared_ids[0].?;
    _ = model.openWindow(1).?;
    model.shared_workspace.placement_hint = .{ .shared_id = id, .window = 1, .window_epoch = model.window_epochs[1] };
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    model.active_window = 1;
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
}

pub fn nativeCloseRehomesWhileBusy() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    try moveSharedWindowToSecondary(engine);
    const ref = model.focusedTerminalRef().?;
    const owner = remote.owner(ref).?;
    _ = try remote.requestWorkspaceRefresh();
    try testing.expect(!command(engine, .close_tab, 0));
    const close = protocol.encodeIntent(.{ .kind = .close_window, .expected_revision = engine.revision, .window = 1, .argument = 0 });
    try testing.expect(engine.applyIntent(&close, &engine_module.NoShells{}));
    try testing.expect(!model.windowOpen(1));
    try testing.expectEqual(@as(usize, 0), model.locateTerminal(ref).?.window);
    try testing.expect(remote.ownerIsCurrent(owner));
    try testing.expectEqual(@as(u32, 1), remote.host.operation_ledger.last_id);
}

pub fn offlineSharedCloseRefuses() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    for ([_]protocol.IntentKind{ .native_command, .close_tab, .close_window }) |kind| {
        const engine = try start();
        defer engine.destroy();
        const model = engine.model;
        const remote = model.phux().?;
        try moveSharedWindowToSecondary(engine);
        const ref = model.focusedTerminalRef().?;
        _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .closed }, null);
        const close = protocol.encodeIntent(.{ .kind = kind, .expected_revision = engine.revision, .window = 1, .argument = if (kind == .native_command) @intFromEnum(protocol.NativeCommand.close_focused_pane) else 0 });
        try testing.expectEqual(kind == .close_window, engine.applyIntent(&close, &engine_module.NoShells{}));
        const native_window: usize = if (kind == .close_window) 0 else 1;
        try testing.expectEqual(native_window, model.locateTerminal(ref).?.window);
        try testing.expectEqual(@as(u32, 0), remote.host.operation_ledger.last_id);
        try testing.expect(!remote.bridge.outgoing.hasPending());
        try remote.host.reconnect("operations-test");
        remote.attach_queued = false;
        try fixture.stageFixture(remote.bridge, "hello.bin");
        _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
        try feed(engine, "attached.bin");
        try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_metadata.bin");
        try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_state.bin");
        try drain(engine);
        try testing.expect(remote.owner(ref) != null);
        try testing.expectEqual(native_window, model.locateTerminal(ref).?.window);
    }
}

const result_command: u64 = 0xfedc_ba98_7654_3210;

fn confirmCorrelatedSplit(engine: *engine_module.Engine) !void {
    try workspaceReply(engine, "workspace_rename_metadata.bin", 5, 3);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 2);
    try drain(engine);
    try workspaceReply(engine, "workspace_split_metadata.bin", 8, 5);
    try workspaceReply(engine, "workspace_split_state.bin", 7, 4);
    const remote = engine.model.phux().?;
    _ = try remote.drainReadiness();
    _ = engine.model.shared_mutations.pump(engine.model);
    _ = engine.creation.pump(engine.model);
}

test "correlated spawn result waits for exact winning projection and retains until ack" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    const epoch = remote.connectionEpoch();
    try engine.creation.requestCorrelated(model, .split_right, result_command);
    try testing.expect(!engine.creation.ackCompletion(result_command));
    try feed(engine, "spawn-local.bin");
    try feed(engine, "local-ready.bin");
    try confirmCorrelatedSplit(engine);
    try testing.expect(engine.creation.peekCompletion() == null);
    try testing.expectEqual(@as(usize, 1), model.primary.tabs[0].paneCount());
    // Emitting desired_terminal does not produce a result or manufacture a pane.
    try testing.expect(model.shared_workspace.desired_terminal.?.eql(refFor(8)));
    try testing.expect(!engine.creation.observeProjection(model));
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), epoch);
    try testing.expect(engine.creation.observeProjection(model));
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(result_command, result.command_id);
    try testing.expectEqual(@as(u32, 1), result.request_id);
    try testing.expectEqual(@as(u32, 3), result.placement_request_id);
    try testing.expectEqual(epoch, result.connection_epoch);
    try testing.expect(result.terminal_ref.?.eql(refFor(8)));
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.placed, result.placement);
    try testing.expectEqual(.focused, result.focus);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    engine.creation.disconnect(model);
    try testing.expectEqualDeep(result, engine.creation.peekCompletion().?);
    try testing.expect(!engine.creation.ackCompletion(result_command + 1));
    try testing.expect(engine.creation.ackCompletion(result_command));
    try testing.expect(engine.creation.peekCompletion() == null);
    try testing.expect(!engine.creation.ackCompletion(result_command));
}

test "superseded correlated focus preserves operation success and placement" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const original = model.focusedTerminalRef().?;
    try engine.creation.requestCorrelated(model, .split_right, result_command);
    engine.creation.supersedeFocus();
    try feed(engine, "spawn-local.bin");
    try feed(engine, "local-ready.bin");
    try confirmCorrelatedSplit(engine);
    const remote = model.phux().?;
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    try testing.expect(engine.creation.observeProjection(model));
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.placed, result.placement);
    try testing.expectEqual(.superseded, result.focus);
    try testing.expect(model.focusedTerminalRef().?.eql(original));
}

test "correlated refusal and disconnect preserve independent operation evidence" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    for ([_]enum { refused, pending_disconnect, accepted_disconnect }{ .refused, .pending_disconnect, .accepted_disconnect }) |scenario| {
        const engine = try start();
        defer engine.destroy();
        const model = engine.model;
        try engine.creation.requestCorrelated(model, .window, result_command);
        if (scenario == .refused) {
            try feed(engine, "spawn-refused.bin");
        } else {
            if (scenario == .accepted_disconnect) try feed(engine, "spawn-local.bin");
            engine.creation.disconnect(model);
        }
        const result = engine.creation.peekCompletion().?;
        try testing.expectEqual(result_command, result.command_id);
        try testing.expectEqual(@as(u32, 1), result.request_id);
        const expected: @import("command_results.zig").Operation = switch (scenario) {
            .refused => .refused,
            .pending_disconnect => .unknown,
            .accepted_disconnect => .success,
        };
        try testing.expectEqual(expected, result.operation);
        if (scenario != .refused) try testing.expect(!model.shared_workspace.refused);
        try testing.expectEqual(scenario == .refused, model.terminal_limit_refused);
        try testing.expectEqual(@as(usize, 0), engine.creation.count());
        if (scenario != .refused) try testing.expectEqual(.unknown, result.placement);
        if (scenario == .accepted_disconnect) {
            try testing.expect(result.terminal_ref.?.eql(refFor(8)));
            try testing.expect(model.phux().?.terminalKnown(refFor(8)));
        }
    }
}

test "successful satellite spawn survives follow-up attach refusal with original request identity" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try engine.creation.requestCorrelated(engine.model, .tab, result_command);
    // Inject the coordinator's typed spawn callback: the fixture's original
    // request owner is local, so its satellite spawn wire reply is inapplicable.
    // The resulting satellite attach still runs through the real provider/FFI.
    const satellite: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(1, 9, "fixture-host") } };
    try testing.expect(engine.creation.complete(engine.model, .{
        .request_id = 1,
        .connection_epoch = engine.model.phux().?.connectionEpoch(),
        .kind = .spawn,
        .status = .success,
        .terminal_ref = satellite,
    }));
    try stageReply(engine, "tests", "attach-refused.bin", 1, 2);
    try drain(engine);
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(@as(u32, 1), result.request_id);
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.refused, result.placement);
    try testing.expectEqual(.attach_refused, result.reason);
    try testing.expectEqual(@as(u32, 1), result.terminal_ref.?.terminal_id.phux.kind);
}

test "correlated destination reuse preserves success without acquiring a new window lifetime" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    try engine.creation.requestCorrelated(model, .window, result_command);
    const window = model.active_window;
    try feed(engine, "spawn-local.bin");
    model.closeWindow(window);
    _ = model.openWindow(window).?;
    try feed(engine, "local-ready.bin");
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.destination_lost, result.placement);
    try testing.expectEqual(.destination_lost, result.reason);
    try testing.expectEqual(@as(usize, 0), model.wsAt(window).?.tab_count);
    try testing.expect(model.windowOpen(window));
    try testing.expect(model.phux().?.terminalKnown(refFor(8)));
}

test "correlated competing shared topology cannot turn successful execution into refusal" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    try engine.creation.requestCorrelated(model, .tab, result_command);
    try feed(engine, "spawn-local.bin");
    try feed(engine, "local-ready.bin");
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 3);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 2);
    try drain(engine);
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 5);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 4);
    try drain(engine);
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.refused, result.placement);
    try testing.expect(result.terminal_ref.?.eql(refFor(8)));
    try testing.expectEqual(@as(usize, 1), model.primary.tab_count);
    try expectCatalogTerminal(engine, 8);
}

test "completed creation storage backpressures before spawn and duplicate commands reject" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    for (0..16) |index| {
        try engine.creation.requestCorrelated(model, .tab, @intCast(index + 1));
        try stageReply(engine, "tests", "spawn-refused.bin", 1, @intCast(index + 1));
        try drain(engine);
    }
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    const before = model.phux().?.host.operation_ledger.last_id;
    try testing.expectError(error.CommandBusy, engine.creation.requestCorrelated(model, .tab, 1));
    try testing.expectError(error.OperationCapacity, engine.creation.requestCorrelated(model, .tab, 17));
    try testing.expectEqual(before, model.phux().?.host.operation_ledger.last_id);
    try testing.expect(engine.creation.ackCompletion(1));
    try engine.creation.requestCorrelated(model, .tab, 17);
    try testing.expectEqual(before + 1, model.phux().?.host.operation_ledger.last_id);
}

test "duplicate pending correlated terminal is busy while legacy terminal requests remain idempotent" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    try engine.creation.requestCorrelated(model, .tab, result_command);
    try feed(engine, "spawn-local.bin");
    try testing.expectError(error.OperationBusy, engine.creation.requestAttachCorrelated(model, refFor(8), result_command + 1));
    try testing.expectError(error.OperationBusy, engine.creation.requestAdmitCorrelated(model, refFor(8), result_command + 1));
    try engine.creation.requestAttach(model, refFor(8));
    try testing.expectEqual(@as(u32, 1), model.phux().?.host.operation_ledger.last_id);
}

test "completion-only disconnect records successful spawn without enqueuing satellite attach" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    try engine.creation.requestCorrelated(model, .tab, result_command);
    const remote = model.phux().?;
    const satellite: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(1, 9, "fixture-host") } };
    try testing.expect(engine.creation.completeDisconnected(model, .{
        .request_id = 1,
        .connection_epoch = remote.connectionEpoch(),
        .kind = .spawn,
        .status = .success,
        .terminal_ref = satellite,
    }));
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.unknown, result.placement);
    try testing.expectEqual(.disconnected, result.reason);
    try testing.expectEqual(@as(u32, 0), result.attach_request_id);
    try testing.expectEqual(@as(u32, 1), remote.host.operation_ledger.last_id);
    try testing.expect(!model.terminal_limit_refused);
}

test "disconnect retains dispatched placement request identity and accepted execution" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    try engine.creation.requestCorrelated(model, .split_right, result_command);
    try feed(engine, "spawn-local.bin");
    try feed(engine, "local-ready.bin");
    try workspaceReply(engine, "workspace_rename_metadata.bin", 5, 3);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 2);
    try drain(engine);
    engine.creation.disconnect(model);
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.unknown, result.placement);
    try testing.expectEqual(@as(u64, 1), result.mutation_ticket);
    try testing.expectEqual(@as(u32, 3), result.placement_request_id);
    try testing.expectEqual(result.connection_epoch, result.placement_connection_epoch);
    try testing.expectEqual(@as(u32, 3), model.phux().?.host.operation_ledger.last_id);
}

test "saturated creation destination lifetime rejects before provider effects" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    model.window_epochs[model.active_window] = std.math.maxInt(u64);
    try testing.expectError(error.StaleDestination, engine.creation.requestCorrelated(model, .tab, result_command));
    try testing.expectEqual(@as(u32, 0), model.phux().?.host.operation_ledger.last_id);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
}

test "creation disconnect preserves already observed shared refusal or confirmation before pump" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    for ([_]bool{ false, true }) |confirmed| {
        const engine = try start();
        defer engine.destroy();
        const model = engine.model;
        const remote = model.phux().?;
        try engine.creation.requestCorrelated(model, .tab, result_command);
        try feed(engine, "spawn-local.bin");
        try feed(engine, "local-ready.bin");
        try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 3);
        try workspaceReply(engine, "workspace_refresh_state.bin", 2, 2);
        try drain(engine);
        if (confirmed) {
            try workspaceReply(engine, "workspace_add_metadata.bin", 14, 5);
            try workspaceReply(engine, "workspace_add_state.bin", 13, 4);
        } else {
            try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 5);
            try workspaceReply(engine, "workspace_refresh_state.bin", 2, 4);
        }
        _ = try remote.drainReadiness();
        engine.creation.disconnect(model);
        const result = engine.creation.peekCompletion().?;
        try testing.expectEqual(.success, result.operation);
        const expected: @import("command_results.zig").Placement = if (confirmed) .unknown else .refused;
        try testing.expectEqual(expected, result.placement);
        try testing.expectEqual(confirmed, result.mutation_outcome.? == .success);
        try testing.expectEqual(@as(u32, 3), result.placement_request_id);
        try testing.expectEqual(@as(u32, 3), remote.host.operation_ledger.last_id);
        try testing.expectEqual(@as(usize, 1), model.primary.tab_count);
    }
}
