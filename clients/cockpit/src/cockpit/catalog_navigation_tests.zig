//! Catalog admission contracts using the same Rust-produced provider fixtures
//! as durable creation. Admission receipts precede shared topology publication.
const std = @import("std");
const support = @import("phux_support.zig");
const engine_module = @import("native/ts_engine.zig");
const commands = @import("native/tab_commands.zig");
const fixture = if (support.phux_enabled) support.PhuxProvider.test_support else struct {};
const testing = std.testing;

const ChannelFx = struct {
    pub fn phuxChannelLive(_: *const @This()) bool {
        return false;
    }
    pub fn openChannel(_: *const @This(), _: anytype) @import("native_sdk").ChannelHandle {
        return .{};
    }
    pub fn closeChannel(_: *const @This(), _: u64) void {}
    pub fn showNotification(_: *const @This(), _: anytype) void {}
};

fn drain(engine: *engine_module.Engine) !void {
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
    try testing.expect(!engine.model.phux_connection_unavailable);
}

fn request(engine: *engine_module.Engine, destination: @import("model.zig").PaletteDestination, id: u64, out: []u8) []const u8 {
    const target = commands.catalog.capture(engine.model, destination).?;
    var buffer: [commands.catalog.max_len]u8 = undefined;
    const bytes = target.encode(&buffer);
    out[0] = 1;
    out[1] = 2;
    std.mem.writeInt(u64, out[2..10], id, .little);
    @memcpy(out[10..][0..bytes.len], bytes);
    return out[0 .. 10 + bytes.len];
}

fn workspaceReply(engine: *engine_module.Engine, name: []const u8, expected: u32, id: u32) !void {
    const path = try std.fmt.allocPrint(testing.allocator, "src/providers/phux/fixtures/{s}", .{name});
    defer testing.allocator.free(path);
    const bytes = try std.Io.Dir.cwd().readFileAlloc(testing.io, path, testing.allocator, .limited(64 * 1024));
    defer testing.allocator.free(bytes);
    try testing.expectEqual(0x8000_0000 + expected, std.mem.readInt(u32, bytes[8..12], .big));
    std.mem.writeInt(u32, bytes[8..12], 0x8000_0000 + id, .big);
    try testing.expect(engine.model.phux().?.bridge.incoming.stage(bytes));
}

pub fn prepareAvailable() !*engine_module.Engine {
    const engine = try @import("durable_creation_tests.zig").start();
    errdefer engine.destroy();
    const remote = engine.model.phux().?;
    _ = try remote.requestSpawn(engine.model.focusedTerminalRef(), remote.attach_viewport);
    try fixture.stageFixture(remote.bridge, "spawn-local.bin");
    _ = try remote.drainReadiness();
    try fixture.stageFixture(remote.bridge, "local-ready.bin");
    _ = try remote.drainReadiness();
    _ = try remote.requestWorkspaceRefresh();
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 3);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 2);
    _ = try remote.drainReadiness();
    _ = try engine.model.shared_workspace.apply(engine.model, remote.workspaceSnapshot(), remote.connectionEpoch());
    engine.model.reconcileRemoteTerminals();
    return engine;
}

pub fn availableRequest(engine: *engine_module.Engine, out: []u8) []const u8 {
    const ref: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 8 } } };
    return request(engine, .{ .available_terminal = ref }, 0xfedc_ba98_7654_3210, out);
}

pub fn admission(fx: anytype) !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try prepareAvailable();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    const original = model.focusedTerminalRef().?;
    const ref: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 8 } } };
    var buffer: [commands.catalog.max_len + 10]u8 = undefined;
    const available = availableRequest(engine, &buffer);
    try testing.expect(model.locateTerminal(ref) == null);
    const admitted = engine.applySelectionCommand(available, fx);
    try testing.expectEqual(commands.Status.accepted_pending, admitted.status);
    try testing.expectEqual(commands.Reason.none, admitted.reason);
    try testing.expectEqual(@as(u8, 3), admitted.encode()[1]);
    try testing.expectEqual(@as(u64, 0xfedc_ba98_7654_3210), admitted.id);
    try testing.expect(model.focusedTerminalRef().?.eql(original));
    try testing.expect(model.locateTerminal(ref) == null);
    try testing.expect(engine.creation.hasPendingTerminal(ref));
    try testing.expect(model.shared_workspace.desired_terminal == null);
    const duplicate = engine.applySelectionCommand(available, fx);
    try testing.expectEqual(commands.Reason.unavailable, duplicate.reason);
    try testing.expectEqual(commands.Status.rejected, duplicate.status);
    try testing.expect(model.shared_workspace.desired_terminal == null);
    for (engine.creation.pending) |slot| {
        const entry = slot orelse continue;
        try testing.expect(entry.may_focus);
    }

    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 5);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 4);
    try drain(engine);
    try workspaceReply(engine, "workspace_add_metadata.bin", 14, 7);
    try workspaceReply(engine, "workspace_add_state.bin", 13, 6);
    try drain(engine);
    try testing.expect(model.focusedTerminalRef().?.eql(ref));
    try testing.expect(model.locateTerminal(ref) != null);
    const completion = engine.creation.peekCompletion().?;
    try testing.expectEqual(admitted.id, completion.command_id);
    try testing.expectEqual(.success, completion.operation);
    try testing.expectEqual(.placed, completion.placement);
    try testing.expectEqual(.focused, completion.focus);
    try testing.expect(!engine.creation.ackCompletion(44));
    try testing.expect(engine.creation.ackCompletion(admitted.id));
    // A fresh action with the same captured target resolves today's placement,
    // rather than trying to re-attach its old "available" catalog position.
    std.mem.writeInt(u64, buffer[2..10], 3, .little);
    const placed = engine.applySelectionCommand(available, fx);
    try testing.expectEqual(commands.Status.applied, placed.status);
    try testing.expectEqual(commands.Reason.none, placed.reason);

    // Metadata/revision movement does not invalidate the captured authority.
    engine.revision += 1;
    const session = request(engine, .{ .session = 2 }, 4, &buffer);
    const pending = engine.applySelectionCommand(session, fx);
    try testing.expectEqual(commands.Status.accepted_pending, pending.status);
    try testing.expectEqual(commands.Reason.none, pending.reason);
    try testing.expectEqual(@as(?u32, 2), remote.session_id);
    try testing.expectEqual(@as(?u32, 1), remote.selectedSessionId());
    try testing.expectEqual(@as(usize, 1), fx.navigation_restarts);
}

pub fn currentSession(fx: anytype) !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try @import("durable_creation_tests.zig").start();
    defer engine.destroy();
    var buffer: [commands.catalog.max_len + 10]u8 = undefined;
    const bytes = request(engine, .{ .session = 1 }, 1, &buffer);
    const applied = engine.applySelectionCommand(bytes, fx);
    try testing.expectEqual(commands.Status.applied, applied.status);
    try testing.expectEqual(commands.Reason.none, applied.reason);
    try testing.expectEqual(@as(u8, 1), applied.encode()[1]);
    try testing.expectEqual(@as(usize, 0), fx.navigation_restarts);
}

pub fn reconnectProvenance() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try @import("durable_creation_tests.zig").start();
    defer engine.destroy();
    const remote = engine.model.phux().?;
    const ref = engine.model.focusedTerminalRef().?;
    const session = commands.catalog.capture(engine.model, .{ .session = 1 }).?;
    try remote.host.reconnect("test");
    const during_session = commands.catalog.capture(engine.model, .{ .session = 1 }).?;
    const during_terminal = commands.catalog.capture(engine.model, .{ .placed_terminal = .{ .window = 0, .tab = 0, .terminal_ref = ref } }).?;
    try testing.expect(session.resolve(engine.model) == null);
    try testing.expect(during_session.resolve(engine.model) == null);
    try testing.expect(during_terminal.resolve(engine.model) == null);

    // A new handshake/catalog reuses the same numeric IDs. Old rows painted
    // DURING reconnect must still carry the provenance of their old inventory.
    try fixture.stageFixture(remote.bridge, "hello.bin");
    _ = try remote.host.drainReadiness();
    try remote.host.attachSessionId(1, .{ .cols = 80, .rows = 24 });
    try fixture.stageFixture(remote.bridge, "attached.bin");
    _ = try remote.host.drainReadiness();
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_metadata.bin");
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_state.bin");
    _ = try remote.host.drainReadiness();
    try testing.expect(during_session.resolve(engine.model) == null);
    try testing.expect(during_terminal.resolve(engine.model) == null);
    const current = commands.catalog.capture(engine.model, .{ .session = 1 }).?;
    try testing.expect(current.resolve(engine.model) != null);
}
