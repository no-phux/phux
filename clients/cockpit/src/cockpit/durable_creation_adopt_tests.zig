//! Bound-spawn adoption against the real Model, provider and FFI wire fixtures.
const std = @import("std");
const testing = std.testing;
const support = @import("phux_support.zig");
const model_module = @import("model.zig");
const Engine = @import("native/ts_engine.zig").Engine;
const fixture = if (support.phux_enabled) support.PhuxProvider.test_support else struct {};

fn start() !*Engine {
    const engine = try Engine.create(testing.allocator, testing.io);
    errdefer engine.destroy();
    const remote = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture-never-dialed" }, null, "adopt");
    model_module.attachPhuxProvider(engine.model, remote);
    try fixture.attachHostWith(remote.host, "hello_conditional_kill.bin");
    remote.attach_queued = true;
    _ = try remote.drainReadiness();
    try project(engine);
    remote.bridge.outgoing.reset();
    return engine;
}

fn project(engine: *Engine) !void {
    const model = engine.model;
    const remote = model.phux().?;
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    _ = model.shared_workspace.selectDesired(model);
    _ = engine.creation.observeProjection(model);
}

fn spawned(engine: *Engine) !support.OperationResult {
    const remote = engine.model.phux().?;
    try testing.expectEqual(@as(u32, 1), try remote.requestSpawnBound(null, remote.attach_viewport, ""));
    try fixture.stageFixture(remote.bridge, "spawn-bound.bin");
    _ = try remote.drainReadiness();
    remote.bridge.outgoing.reset();
    const result = remote.takeOperationResult().?;
    try testing.expectEqual(.success, result.status);
    try testing.expectEqual([_]u8{0xa5} ** 16, result.instance.?);
    return result;
}

fn adopt(engine: *Engine, result: support.OperationResult, window: usize, epoch: u64, focus: bool) !void {
    try engine.creation.adoptSpawnIn(engine.model, engine.model.phux().?, result, window, epoch, 91, focus);
}

fn ready(engine: *Engine) !void {
    const remote = engine.model.phux().?;
    try fixture.stageFixture(remote.bridge, "local-ready.bin");
    _ = try remote.drainReadiness();
    _ = engine.creation.pump(engine.model);
}

fn workspaceReply(engine: *Engine, name: []const u8, expected: u32, request: u32) !void {
    const path = try std.fmt.allocPrint(testing.allocator, "src/providers/phux/fixtures/{s}", .{name});
    defer testing.allocator.free(path);
    const bytes = try std.Io.Dir.cwd().readFileAlloc(testing.io, path, testing.allocator, .limited(64 * 1024));
    defer testing.allocator.free(bytes);
    try testing.expectEqualSlices(u8, &.{ 1, 4, 4 }, bytes[5..8]);
    try testing.expectEqual(0x8000_0000 + expected, std.mem.readInt(u32, bytes[8..12], .big));
    std.mem.writeInt(u32, bytes[8..12], 0x8000_0000 + request, .big);
    try testing.expect(engine.model.phux().?.bridge.incoming.stage(bytes));
}

fn sendMutation(engine: *Engine) !void {
    try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 3);
    try workspaceReply(engine, "workspace_refresh_state.bin", 2, 2);
    _ = try engine.model.phux().?.drainReadiness();
    _ = engine.model.shared_mutations.pump(engine.model);
    _ = engine.creation.pump(engine.model);
    try testing.expectEqual(.pending, engine.model.phux().?.workspaceSnapshot().status);
}

fn confirm(engine: *Engine, accepted: bool) !void {
    if (accepted) {
        try workspaceReply(engine, "workspace_add_metadata.bin", 14, 5);
        try workspaceReply(engine, "workspace_add_state.bin", 13, 4);
    } else {
        try workspaceReply(engine, "workspace_refresh_metadata.bin", 3, 5);
        try workspaceReply(engine, "workspace_refresh_state.bin", 2, 4);
    }
    _ = try engine.model.phux().?.drainReadiness();
    _ = engine.model.shared_mutations.pump(engine.model);
    _ = engine.creation.pump(engine.model);
}

fn expectCleanup(remote: *support.PhuxProvider) !void {
    const frame = remote.bridge.outgoing.take() orelse return error.MissingConditionalCleanup;
    defer remote.bridge.outgoing.release(frame);
    try testing.expectEqual(@as(u8, 0x31), frame[4]);
    const instance = [_]u8{0xa5} ** 16;
    try testing.expect(std.mem.indexOf(u8, frame, &instance) != null);
    try testing.expect(!remote.bridge.outgoing.hasPending());
}

test "adopted bound spawn converges into the exact captured shared tab with retained ticket" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const result = try spawned(engine);
    _ = model.openWindow(1).?;
    const epoch = model.window_epochs[1];
    try testing.expect(model.locateTerminal(result.terminal_ref.?) == null);
    try adopt(engine, result, 1, epoch, false);
    try testing.expectEqual(@as(usize, 0), model.active_window);
    try ready(engine);
    try sendMutation(engine);
    try confirm(engine, true);
    try testing.expect(engine.creation.peekCompletion() == null);
    try project(engine);
    const location = model.locateTerminal(result.terminal_ref.?).?;
    try testing.expectEqual(@as(usize, 1), location.window);
    try testing.expect(model.terminalOwner(result.terminal_ref.?).?.source_context == model.phux().?.host.context_id);
    const completion = engine.creation.peekCompletion().?;
    try testing.expectEqual(@as(u64, 91), completion.command_id);
    try testing.expectEqual(result.request_id, completion.request_id);
    try testing.expectEqual(.success, completion.operation);
    try testing.expectEqual(.placed, completion.placement);
    try testing.expectEqual(.superseded, completion.focus);
    try testing.expectEqual(epoch, completion.destination_window_epoch);
    try testing.expect(engine.creation.ackCompletion(91));
    try testing.expect(engine.creation.peekCompletion() == null);
}

test "adopted spawn rejects closed reused and out-of-range destinations before effects" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const result = try spawned(engine);
    const model = engine.model;
    const remote = model.phux().?;
    _ = model.openWindow(1).?;
    const epoch = model.window_epochs[1];
    model.closeWindow(1);
    try testing.expectError(error.StaleDestination, adopt(engine, result, 1, epoch, true));
    _ = model.openWindow(1).?;
    try testing.expectError(error.StaleDestination, adopt(engine, result, 1, epoch, true));
    try testing.expectError(error.StaleDestination, adopt(engine, result, model_module.max_windows, epoch, true));
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expect(!remote.bridge.outgoing.hasPending());
    try testing.expectEqual(@as(u32, 1), remote.host.operation_ledger.last_id);
}

test "adopted spawn full result capacity leaves the naked receipt owned by its caller" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const result = try spawned(engine);
    const model = engine.model;
    for (0..16) |index| {
        const id: u64 = @intCast(index + 10);
        try engine.creation.reserveSessionCorrelated(model, 2, null, id);
        engine.creation.failSessionAttempt(model, id);
    }
    try testing.expectError(error.OperationCapacity, adopt(engine, result, 0, model.window_epochs[0], true));
    try testing.expect(!model.phux().?.bridge.outgoing.hasPending());
    // The caller can still dispose of exactly the receipt it retained.
    _ = try model.phux().?.requestKillIf(result.terminal_ref.?, result.instance.?);
    try expectCleanup(model.phux().?);
}

test "adopted spawn source fences refuse foreign providers and stale epochs without writes" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    var result = try spawned(engine);
    const model = engine.model;
    const foreign = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/other-fixture" }, null, "adopt");
    defer foreign.destroy();
    try testing.expectError(error.StaleContext, engine.creation.adoptSpawnIn(model, foreign, result, 0, model.window_epochs[0], 91, true));
    result.connection_epoch += 1;
    try testing.expectError(error.StaleContext, adopt(engine, result, 0, model.window_epochs[0], true));
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expect(!model.phux().?.bridge.outgoing.hasPending());
    try testing.expect(!foreign.bridge.outgoing.hasPending());
}

test "adopted spawn known destination refusal conditionally cleans exactly once" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const result = try spawned(engine);
    const model = engine.model;
    _ = model.openWindow(1).?;
    try adopt(engine, result, 1, model.window_epochs[1], true);
    model.closeWindow(1);
    try fixture.stageFixture(model.phux().?.bridge, "local-ready.bin");
    _ = try model.phux().?.drainReadiness();
    model.phux().?.bridge.outgoing.reset();
    try testing.expect(engine.creation.pump(model));
    try testing.expectEqual(.success, engine.creation.peekCompletion().?.operation);
    try testing.expectEqual(.destination_lost, engine.creation.peekCompletion().?.placement);
    try expectCleanup(model.phux().?);
    try testing.expect(!engine.creation.pump(model));
    try testing.expect(!engine.creation.complete(model, result));
    try testing.expect(!model.phux().?.bridge.outgoing.hasPending());
}

test "adopted spawn immediate pump failure retains transferred ownership and cleans once" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const result = try spawned(engine);
    const model = engine.model;
    const remote = model.phux().?;
    try ready(engine);
    remote.bridge.outgoing.reset();
    for (0..16) |_| {
        _ = try model.shared_mutations.requestCreation(model, .{
            .kind = .add,
            .expected_revision = model.shared_workspace.revision,
            .session_id = model.shared_workspace.session,
            .terminal_ref = result.terminal_ref,
        }, remote.connectionEpoch());
    }
    // Success transfers ownership even though its first pump refuses locally.
    try adopt(engine, result, 0, model.window_epochs[0], true);
    const completion = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, completion.operation);
    try testing.expectEqual(.refused, completion.placement);
    try testing.expectEqual(.operation_capacity, completion.reason);
    try expectCleanup(remote);
    try testing.expect(!engine.creation.pump(model));
    try testing.expect(engine.creation.ackCompletion(91));
    try testing.expect(!engine.creation.complete(model, result));
    try testing.expect(!remote.bridge.outgoing.hasPending());
}

test "adopted spawn sent mutation waits for authoritative refusal before conditional cleanup" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const result = try spawned(engine);
    const model = engine.model;
    _ = model.openWindow(1).?;
    try adopt(engine, result, 1, model.window_epochs[1], true);
    try ready(engine);
    try sendMutation(engine);
    model.phux().?.bridge.outgoing.reset();
    model.closeWindow(1);
    try testing.expect(!engine.creation.pump(model));
    try testing.expect(!model.phux().?.bridge.outgoing.hasPending());
    try confirm(engine, false);
    try testing.expectEqual(.success, engine.creation.peekCompletion().?.operation);
    try testing.expectEqual(.refused, engine.creation.peekCompletion().?.placement);
    try expectCleanup(model.phux().?);
}

test "adopted spawn confirmed write survives destination loss without destructive cleanup" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const result = try spawned(engine);
    const model = engine.model;
    _ = model.openWindow(1).?;
    try adopt(engine, result, 1, model.window_epochs[1], true);
    try ready(engine);
    try sendMutation(engine);
    model.phux().?.bridge.outgoing.reset();
    model.closeWindow(1);
    try confirm(engine, true);
    try testing.expectEqual(.destination_lost, engine.creation.peekCompletion().?.placement);
    try testing.expect(!model.phux().?.bridge.outgoing.hasPending());
}

test "adopted spawn disconnect and changed source retire as unknown with no replay or kill" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    for ([_]bool{ false, true }) |change_source| {
        const engine = try start();
        defer engine.destroy();
        const result = try spawned(engine);
        const model = engine.model;
        try adopt(engine, result, 0, model.window_epochs[0], true);
        try ready(engine);
        try sendMutation(engine);
        model.phux().?.bridge.outgoing.reset();
        if (change_source) {
            model.phux().?.host.context_id += 1;
            _ = engine.creation.pump(model);
        } else engine.creation.disconnect(model);
        try testing.expectEqual(.success, engine.creation.peekCompletion().?.operation);
        try testing.expectEqual(.unknown, engine.creation.peekCompletion().?.placement);
        try testing.expect(!engine.creation.complete(model, result));
        try testing.expect(!engine.creation.pump(model));
        try testing.expect(!model.phux().?.bridge.outgoing.hasPending());
    }
}
