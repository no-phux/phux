//! Actual Model/Engine and FFI contracts for the generic New Session adapter.
const std = @import("std");
const Engine = @import("cockpit/native/ts_engine.zig").Engine;
const Model = @import("cockpit/model.zig").Model;
const support = @import("cockpit/phux_support.zig");
const Remote = support.PhuxProvider;
const runtime = @import("cockpit/native/new_session_runtime.zig");
const sessions = @import("cockpit/native/new_session.zig");
const empty_session = @import("cockpit/native/empty_session.zig");

fn peer(engine: *Engine) !*Remote {
    const slot = try engine.model.freePeerSlot();
    const remote = try Remote.create(std.testing.allocator, std.testing.io, .{ .unix = "/unused" }, null, "new-session-runtime-test");
    engine.model.peers.items[slot].provider = remote;
    remote.standBy();
    try remote.host.start("new-session-runtime-test");
    try negotiate(remote);
    return remote;
}

fn negotiate(remote: *Remote) !void {
    try Remote.test_support.stageFixture(remote.bridge, "hello_keep_empty.bin");
    _ = try remote.host.drainReadiness();
    remote.bridge.outgoing.reset();
}

test "new session runtime keeps simultaneous same-host captures on independent Clients" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    const first = try peer(engine);
    const second = try peer(engine);
    try std.testing.expectEqual(first.providerId(), second.providerId());
    try std.testing.expect(runtime.capture(engine, null, 7) == null);
    const a = runtime.capture(engine, first, 7).?;
    engine.model.active_window = 1;
    const b = runtime.capture(engine, second, 7).?;
    try std.testing.expect(a.provider_context != b.provider_context);
    try std.testing.expect(a.host_context != b.host_context);
    try std.testing.expect(runtime.current(engine, a));
    const aid = try runtime.send(engine, a, "first", true);
    const bid = try runtime.send(engine, b, "second", true);
    try std.testing.expectEqual(aid, bid); // Identical per-Client request IDs.
    try std.testing.expect(runtime.poll(engine, a, aid) == .pending);
    try std.testing.expect(runtime.poll(engine, b, bid) == .pending);
    try Remote.test_support.expectOutgoingCount(first.bridge, 3);
    try Remote.test_support.expectOutgoingCount(second.bridge, 3);
    var wrong_host = a;
    wrong_host.host_context = b.host_context;
    try std.testing.expect(!runtime.current(engine, wrong_host));
    runtime.release(engine, wrong_host, aid);
    first.host.disconnect();
    try std.testing.expectEqual(.unknown_outcome, first.sessionCreateInfo(aid).status);
    try std.testing.expect(runtime.poll(engine, a, aid) == .unknown);
    runtime.release(engine, a, aid);
    try std.testing.expectEqual(.none, first.sessionCreateInfo(aid).status);
    try std.testing.expect(runtime.poll(engine, b, bid) == .pending);
    try std.testing.expect(first.selectedSessionId() == null);
    try std.testing.expect(second.selectedSessionId() == null);
}

test "new session runtime never releases a reused request ID into a reconnected Client" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const remote = try peer(engine);
    const old = runtime.capture(engine, remote, 1).?;
    const old_id = try runtime.send(engine, old, "old", true);
    try remote.host.reconnect("new-session-runtime-test");
    try negotiate(remote);
    const replacement = runtime.capture(engine, remote, 1).?;
    try std.testing.expect(old.connection_epoch != replacement.connection_epoch);
    const new_id = try runtime.send(engine, replacement, "replacement", true);
    try std.testing.expectEqual(old_id, new_id);
    try std.testing.expect(!runtime.current(engine, old));
    try std.testing.expect(runtime.poll(engine, old, old_id) == .unknown);
    try std.testing.expectError(error.InvalidState, runtime.send(engine, old, "stale", true));
    runtime.release(engine, old, old_id);
    try std.testing.expect(runtime.poll(engine, replacement, new_id) == .pending);
    remote.host.disconnect();
    // If the old release touched the new request, disconnect would drop its
    // released tombstone. A retained UNKNOWN proves it stayed independently owned.
    try std.testing.expectEqual(.unknown_outcome, remote.sessionCreateInfo(new_id).status);
    runtime.release(engine, replacement, new_id);
    try std.testing.expectEqual(.none, remote.sessionCreateInfo(new_id).status);
}

const Hooks = struct {
    engine: *Engine,
    model: *Model,
    allocator: std.mem.Allocator = std.testing.allocator,
    remote: *Remote,
    selection_epoch: u64 = 1,
    releases: usize = 0,
    focused: usize = 0,

    pub fn captureNewSessionDestination(self: *@This()) ?sessions.Destination {
        return runtime.capture(self.engine, self.remote, self.selection_epoch);
    }
    pub fn newSessionDestinationCurrent(self: *@This(), destination: sessions.Destination) bool {
        return runtime.current(self.engine, destination);
    }
    pub fn sendNewSession(self: *@This(), destination: sessions.Destination, name: []const u8, keep_empty: bool) !u32 {
        return runtime.send(self.engine, destination, name, keep_empty);
    }
    pub fn pollNewSession(self: *@This(), destination: sessions.Destination, id: u32) sessions.Outcome {
        return runtime.poll(self.engine, destination, id);
    }
    pub fn releaseNewSession(self: *@This(), destination: sessions.Destination, id: u32) void {
        self.releases += 1;
        runtime.release(self.engine, destination, id);
    }
    pub fn didCreateSession(self: *@This(), _: sessions.Destination, _: u32, _: anytype) void {
        self.focused += 1;
    }
};

fn request(kind: sessions.Kind, token: u64, name: []const u8, out: []u8) []const u8 {
    out[0] = 1;
    out[1] = @intFromEnum(kind);
    std.mem.writeInt(u64, out[2..10], token, .little);
    out[10] = @intCast(name.len);
    @memcpy(out[11..][0..name.len], name);
    return out[0 .. 11 + name.len];
}

test "new session runtime cancellation and window closure retire a pending same-Client receipt exactly once" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    engine.model.active_window = 1;
    const remote = try peer(engine);
    var hooks: Hooks = .{ .engine = engine, .model = engine.model, .remote = remote };
    var controller: sessions.Controller = .{};
    defer controller.deinit(std.testing.allocator);
    var input: [256]u8 = undefined;
    var output: [sessions.max_bytes]u8 = undefined;
    _ = try controller.handle(&hooks, .{}, request(.describe, 0, "", &input), &output);
    _ = try controller.handle(&hooks, .{}, request(.create, 1, "pending", &input), &output);
    _ = try controller.handle(&hooks, .{}, request(.cancel, 1, "", &input), &output);
    try std.testing.expectEqual(@as(usize, 0), hooks.releases);
    engine.model.closeWindow(1);
    controller.poll(&hooks, .{});
    controller.poll(&hooks, .{});
    try std.testing.expectEqual(@as(usize, 1), hooks.releases);
    try std.testing.expectEqual(@as(usize, 0), hooks.focused);
    try std.testing.expectEqual(@as(usize, 0), controller.entries.items.len);
    // Release happened while pending; disconnect now drops that tombstone.
    remote.host.disconnect();
    try std.testing.expectEqual(.none, remote.sessionCreateInfo(1).status);
}

test "new session runtime teardown retires pending and cancelled requests before provider stop" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const remote = try peer(engine);
    var hooks: Hooks = .{ .engine = engine, .model = engine.model, .remote = remote };
    var controller: sessions.Controller = .{};
    defer controller.deinit(std.testing.allocator);
    var input: [256]u8 = undefined;
    var output: [sessions.max_bytes]u8 = undefined;
    for ([_]u64{ 1, 2, 3 }) |token| {
        _ = try controller.handle(&hooks, .{}, request(.describe, 0, "", &input), &output);
        if (token != 3) _ = try controller.handle(&hooks, .{}, request(.create, token, "pending", &input), &output);
    }
    _ = try controller.handle(&hooks, .{}, request(.cancel, 2, "", &input), &output);
    // The old bridge teardown only freed controller memory, leaking pending
    // result ownership until the whole Client was eventually destroyed.
    const capacity = controller.entries.capacity;
    controller.retire(&hooks);
    controller.retire(&hooks);
    controller.poll(&hooks, .{});
    try std.testing.expectEqual(capacity, controller.entries.capacity);
    try std.testing.expectEqual(@as(usize, 0), controller.entries.items.len);
    try std.testing.expectEqual(@as(u64, 4), controller.next_token);
    _ = try controller.handle(&hooks, .{}, request(.create, 1, "late", &input), &output);
    try std.testing.expectEqual(@intFromEnum(sessions.Phase.unavailable), output[1]);
    try Remote.test_support.expectOutgoingCount(remote.bridge, 6);
    remote.host.disconnect();
    controller.deinit(std.testing.allocator);
    try std.testing.expectEqual(@as(usize, 2), hooks.releases);
    try std.testing.expectEqual(@as(usize, 0), hooks.focused);
    try std.testing.expectEqual(.none, remote.sessionCreateInfo(1).status);
    try std.testing.expectEqual(.none, remote.sessionCreateInfo(2).status);
}

const Effects = struct {
    calls: usize = 0,
    context: u64 = 0,
    pub fn restartPeer(_: *@This(), _: *Engine, _: usize) bool {
        return true;
    }
};

fn selectEmpty(engine: *Engine, remote: *Remote, id: u32, window: usize, epoch: u64, fx: *Effects) !void {
    try std.testing.expectEqual(engine.model.window_epochs[window], epoch);
    // Before the runtime's exact-context API lands, this test has one peer and
    // invokes the existing shipping empty-session navigation through Engine.
    engine.model.active_window = window;
    if (!engine.showPeerSession(remote.providerId(), id, fx)) return error.Refused;
    fx.calls += 1;
    fx.context = remote.context_id;
}

test "new session runtime focuses through Engine empty-session navigation only with captured selection authority" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    engine.model.active_window = 1;
    const remote = try peer(engine);
    remote.host.sessions_generation = remote.connectionEpoch();
    try remote.host.sessions.append(std.testing.allocator, .{
        .id = 42,
        .name = try std.testing.allocator.dupe(u8, "created"),
        .created_at_unix_secs = 0,
        .window_count = 0,
        .attached_client_count = 0,
        .focused = false,
        .keep_empty = true,
        .empty = true,
    });
    const destination = runtime.capture(engine, remote, 9).?;
    var fx: Effects = .{};
    runtime.didCreate(engine, destination, 42, 10, &fx, selectEmpty);
    try std.testing.expectEqual(@as(usize, 0), fx.calls);
    try std.testing.expect(empty_session.view(engine.model, 1) == null);
    runtime.didCreate(engine, destination, 42, 9, &fx, selectEmpty);
    try std.testing.expectEqual(@as(usize, 1), fx.calls);
    try std.testing.expectEqual(remote.context_id, fx.context);
    try std.testing.expectEqual(@as(u32, 42), empty_session.view(engine.model, 1).?.session);
    try std.testing.expect(empty_session.view(engine.model, 0) == null);
    try std.testing.expectEqual(@as(usize, 0), engine.model.wsAtConst(1).?.tab_count);
    try std.testing.expect(remote.selectedSessionId() == null);
    engine.model.closeWindow(1);
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    runtime.didCreate(engine, destination, 42, 9, &fx, selectEmpty);
    try std.testing.expectEqual(@as(usize, 1), fx.calls);
}
