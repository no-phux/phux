//! Actual Model/FFI ownership tests for the Engine's captured Machines hooks.
const std = @import("std");
const support = @import("../phux_support.zig");
const engine_module = @import("ts_engine.zig");
const machine = @import("machine_runtime.zig");
const remote_api = if (support.phux_enabled) @import("phux_provider").remote_api else struct {};

const Fx = struct {
    restarts: usize = 0,
    pub fn restartPeer(self: *@This(), _: *engine_module.Engine, _: usize) bool {
        self.restarts += 1;
        return true;
    }
    pub fn restartPhux(self: *@This(), _: *engine_module.Engine) bool {
        self.restarts += 1;
        return true;
    }
    pub fn closeChannel(_: *@This(), _: u64) void {}
    pub fn cancelTimer(_: *@This(), _: u64) void {}
};

fn target(engine: *engine_module.Engine, remote: *support.PhuxProvider) machine.Target {
    return .{
        .attachment_id = remote.context_id,
        .source_context = remote.host.context_id,
        .connection_epoch = remote.connectionEpoch(),
        .identity = .{ .role = .remote, .name = "mini", .endpoint = "ws://127.0.0.1:1", .session = "work" },
        .origin = .{ .window = 0, .epoch = engine.model.window_epochs[0] },
    };
}

test "Engine captured Machines retry and group disconnect fence every exact attachment before mutation" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var registry = try remote_api.TestRegistry.init("mini", "ws://127.0.0.1:1");
    defer registry.deinit();
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const identity: machine.RegistryIdentity = .{ .role = 1, .name = "mini", .endpoint = "ws://127.0.0.1:1", .session = "work" };
    var fx: Fx = .{};
    const first = try support.PhuxProvider.createCaptured(std.testing.allocator, std.testing.io, try remote_api.Tunnel.resolve("mini", registry.path), identity, "machine-engine");
    try engine.adoptCapturedPeer(first, &fx);
    const second = try first.createSiblingAttachment(std.testing.allocator, std.testing.io, "machine-engine");
    try engine.adoptCapturedPeer(second, &fx);
    try std.testing.expectEqual(@as(usize, 2), fx.restarts);
    const one = target(engine, first);
    const two = target(engine, second);
    var stale = two;
    stale.connection_epoch += 1;
    try std.testing.expectError(error.StaleTarget, engine.disconnectCapturedPeers(&.{ one, stale }, &fx));
    try std.testing.expect(engine.model.phuxForAttachment(one.attachment_id) == first);
    try std.testing.expect(engine.model.phuxForAttachment(two.attachment_id) == second);
    try std.testing.expectError(error.StaleTarget, engine.retryCapturedPeer(stale, try remote_api.Tunnel.resolve("mini", registry.path), identity, &fx));
    try std.testing.expectEqual(@as(usize, 2), fx.restarts);
    try engine.retryCapturedPeer(one, try remote_api.Tunnel.resolve("mini", registry.path), identity, &fx);
    try std.testing.expectEqual(@as(usize, 3), fx.restarts);
    try std.testing.expectEqual(one.attachment_id, first.context_id);
    try std.testing.expect(first.worker == null and second.worker == null);
    try engine.disconnectCapturedPeers(&.{ one, two }, &fx);
    try std.testing.expect(engine.model.phuxForAttachment(one.attachment_id) == null);
    try std.testing.expect(engine.model.phuxForAttachment(two.attachment_id) == null);
}
