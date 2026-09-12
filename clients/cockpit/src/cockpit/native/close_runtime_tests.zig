//! Real Model + Phux Provider/FFI; only server outcomes are scripted.
const std = @import("std");
const testing = std.testing;
const close = @import("close_runtime.zig");
const support = @import("../phux_support.zig");
const creation = @import("../durable_creation_tests.zig");
const fixture = if (support.phux_enabled) support.PhuxProvider.test_support else struct {};
const model_module = @import("../model.zig");
const layout = @import("../layout.zig");

/// Supplies the cutover's exact-tree attachment lookup over the real Model.
/// Only this mapping changes; both independent Providers use real FFI Clients.
const ViewModel = struct {
    base: *model_module.Model,
    window_epochs: []const u64,
    remote: *const support.PhuxProvider,

    pub fn windowOpen(self: *const ViewModel, window: usize) bool {
        return self.base.windowOpen(window);
    }

    pub fn wsAtConst(self: *const ViewModel, window: usize) ?*const model_module.Workspace {
        return self.base.wsAtConst(window);
    }

    pub fn phuxForTreeConst(self: *const ViewModel, tree: *const layout.Tree) ?*const support.PhuxProvider {
        if (tree != &self.base.ws().tabs[0]) return null;
        return self.remote;
    }
};

fn ref(id: u32) support.TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = id } } };
}

fn receipt(remote: anytype, request: u32, kind: anytype) support.OperationResult {
    return .{ .request_id = request, .connection_epoch = remote.connectionEpoch(), .kind = kind, .status = .success };
}

test "close runtime accepts only exact Client completed close receipt" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    const other = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture.sock" }, null, "other");
    defer other.destroy();
    try fixture.attachHost(other.host);
    var coordinator: close.Coordinator(2) = .{};
    const target = try close.Target.capturePane(model, remote, 0, 0, ref(7));
    try testing.expectError(error.InvalidIdentity, close.Target.capturePane(model, other, 0, 0, ref(7)));
    try testing.expectError(error.InvalidIdentity, coordinator.begin(model, other, target));
    var wrong_owner = target;
    wrong_owner.source = close.Source.capture(other);
    try testing.expectError(error.InvalidIdentity, coordinator.begin(model, other, wrong_owner));
    try fixture.expectOutgoingCount(other.bridge, 0);
    const request = try coordinator.begin(model, remote, target);
    // Same endpoint, epoch, resource and request ID are deliberately reused.
    try testing.expectEqual(request, try other.requestCloseResource(ref(7), other.connectionEpoch()));
    const result = receipt(remote, request, .close_resource);
    try testing.expect(!coordinator.completeFrom(other, result));
    try testing.expect(coordinator.takeOutcomeFrom(model, other) == null);
    var wrong = result;
    wrong.request_id += 1;
    try testing.expect(!coordinator.completeFrom(remote, wrong));
    wrong = result;
    wrong.kind = .detach;
    try testing.expect(!coordinator.completeFrom(remote, wrong));
    wrong = result;
    wrong.connection_epoch += 1;
    try testing.expect(!coordinator.completeFrom(remote, wrong));
    try fixture.stageFixture(remote.bridge, "detach-ok.bin");
    _ = try remote.host.drainReadiness();
    // Raw Ok is only cancellation. FFI retains the operation until closure.
    try testing.expect(remote.takeOperationResult() == null);
    try testing.expect(coordinator.takeOutcomeFrom(model, remote) == null);
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
    try testing.expect(remote.owner(ref(7)) != null);
    try fixture.stageFixture(remote.bridge, "initial-terminal-closed.bin");
    _ = try remote.host.drainReadiness();
    try testing.expect(remote.owner(ref(7)) == null);
    try testing.expect(coordinator.completeFrom(remote, remote.takeOperationResult().?));
    const outcome = coordinator.takeOutcomeFrom(model, remote).?;
    try testing.expectEqual(.completed, outcome.phase);
    try testing.expect(outcome.target.owners[0].terminal_ref.eql(ref(7)));
    try testing.expect(coordinator.takeOutcomeFrom(model, remote) == null);
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count); // Engine owns mutation.
}

test "close runtime ResourceClosed before acknowledgement completes without a side channel" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    var coordinator: close.Coordinator(1) = .{};
    const target = try close.Target.capturePane(model, remote, 0, 0, ref(7));
    _ = try coordinator.begin(model, remote, target);
    try fixture.stageFixture(remote.bridge, "initial-terminal-closed.bin");
    _ = try remote.host.drainReadiness();
    try testing.expect(remote.owner(ref(7)) == null);
    try testing.expect(remote.takeOperationResult() == null);
    try testing.expect(coordinator.takeOutcomeFrom(model, remote) == null);
    try fixture.stageFixture(remote.bridge, "detach-ok.bin");
    _ = try remote.host.drainReadiness();
    try testing.expect(coordinator.completeFrom(remote, remote.takeOperationResult().?));
    try testing.expectEqual(.completed, coordinator.takeOutcomeFrom(model, remote).?.phase);
}

test "close runtime captured pane survives focus movement and window reuse refuses metadata" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    var coordinator: close.Coordinator(2) = .{};
    const target = try close.Target.capturePane(model, remote, 0, 0, ref(7));
    const tree = &model.ws().tabs[0];
    _ = try tree.split(tree.root, .horizontal, ref(8));
    try testing.expect(tree.focusedTerminal().?.eql(ref(8)));
    const request = try coordinator.begin(model, remote, target);
    // A scripted successful close receipt represents both FFI-owned proofs.
    try testing.expect(coordinator.takeOutcomeFrom(model, remote) == null);
    try testing.expect(coordinator.completeFrom(remote, receipt(remote, request, .close_resource)));
    model.window_epochs[0] += 1;
    const outcome = coordinator.takeOutcomeFrom(model, remote).?;
    try testing.expectEqual(.stale, outcome.phase);
    try testing.expect(outcome.target.owners[0].terminal_ref.eql(ref(7)));
    try testing.expectEqual(@as(usize, 2), tree.paneCount());
    try testing.expectError(error.InvalidTarget, coordinator.begin(model, remote, target));
}

test "close runtime unchanged tab and window cannot authorize replacement attachment" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    const remote = engine.model.phux().?;
    const other = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture.sock" }, null, "replacement-view");
    defer other.destroy();
    try fixture.attachHost(other.host);
    var model: ViewModel = .{ .base = engine.model, .window_epochs = &engine.model.window_epochs, .remote = remote };
    const target = try close.Target.capturePane(&model, remote, 0, 0, ref(7));
    var coordinator: close.Coordinator(1) = .{};
    model.remote = other;
    try testing.expectError(error.InvalidIdentity, coordinator.begin(&model, remote, target));
    try fixture.expectOutgoingCount(remote.bridge, 0);
    model.remote = remote;
    const request = try coordinator.begin(&model, remote, target);
    model.remote = other;
    try testing.expect(coordinator.completeFrom(remote, receipt(remote, request, .close_resource)));
    try testing.expectEqual(.stale, coordinator.takeOutcomeFrom(&model, remote).?.phase);
    try testing.expectEqual(@as(usize, 1), engine.model.ws().tab_count);
    try fixture.expectOutgoingCount(other.bridge, 0);
}

test "close runtime shared ID and tab generation fence reused presentation slots" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    var coordinator: close.Coordinator(1) = .{};
    const target = try close.Target.captureTab(model, remote, 0, 0);
    model.ws().tab_generation += 1;
    try testing.expectError(error.InvalidTarget, coordinator.begin(model, remote, target));
    model.ws().tab_generation -= 1;
    const request = try coordinator.begin(model, remote, target);
    model.ws().shared_ids[0] = @splat(99);
    try testing.expect(coordinator.completeFrom(remote, receipt(remote, request, .close_resources)));
    try testing.expectEqual(.stale, coordinator.takeOutcomeFrom(model, remote).?.phase);
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
}

test "close runtime completed batch cannot remove newly added work" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    _ = try remote.requestSpawn(null, .{ .cols = 80, .rows = 24 });
    try fixture.stageFixture(remote.bridge, "spawn-local.bin");
    try fixture.stageFixture(remote.bridge, "local-ready.bin");
    _ = try remote.host.drainReadiness();
    _ = remote.takeOperationResult();
    remote.bridge.outgoing.reset();
    const tree = &model.ws().tabs[0];
    _ = try tree.split(tree.root, .horizontal, ref(8));
    var coordinator: close.Coordinator(1) = .{};
    const target = try close.Target.captureTab(model, remote, 0, 0);
    try testing.expectEqual(@as(usize, 2), target.count);
    const request = try coordinator.begin(model, remote, target);
    try fixture.expectOutgoingCount(remote.bridge, 1);
    // Until FFI publishes the completed batch there is no metadata authority.
    try testing.expect(coordinator.takeOutcomeFrom(model, remote) == null);
    _ = try tree.split(tree.focus, .vertical, ref(9));
    try testing.expect(coordinator.completeFrom(remote, receipt(remote, request, .close_resources)));
    try testing.expectEqual(.stale, coordinator.takeOutcomeFrom(model, remote).?.phase);
    try testing.expectEqual(@as(usize, 3), tree.paneCount());
}

test "close runtime exact refusal and unknown retain views and owned reasons without retry" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    var coordinator: close.Coordinator(1) = .{};
    const target = try close.Target.captureTab(model, remote, 0, 0);
    const request = try coordinator.begin(model, remote, target);
    var refused = receipt(remote, request, .close_resources);
    refused.status = .refused;
    const reason = "server refused atomic close";
    @memcpy(refused.message_storage[0..reason.len], reason);
    refused.message_len = reason.len;
    try testing.expect(coordinator.completeFrom(remote, refused));
    @memset(refused.message_storage[0..reason.len], 'x');
    const outcome = coordinator.takeOutcomeFrom(model, remote).?;
    try testing.expectEqual(.refused, outcome.phase);
    try testing.expectEqualStrings(reason, outcome.reason());
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
    try testing.expect(remote.owner(ref(7)) != null);
    try testing.expectEqual(request, remote.host.operation_ledger.last_id);
    try fixture.expectOutgoingCount(remote.bridge, 1);
}

test "close runtime reconnect cannot replay old intent or consume replacement request" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    var coordinator: close.Coordinator(1) = .{};
    const target = try close.Target.capturePane(model, remote, 0, 0, ref(7));
    const request = try coordinator.begin(model, remote, target);
    const result = receipt(remote, request, .close_resource);
    remote.host.disconnect();
    try remote.host.reconnect("replacement");
    try testing.expect(!coordinator.completeFrom(remote, result));
    try testing.expectEqual(.unknown, coordinator.takeOutcomeFrom(model, remote).?.phase);
    try testing.expectError(error.InvalidIdentity, coordinator.begin(model, remote, target));
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
}

test "close runtime refuses satellite-containing tab atomically before request consumption" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try creation.start();
    defer engine.destroy();
    const model = engine.model;
    const remote = model.phux().?;
    _ = try remote.requestSpawn(null, .{ .cols = 80, .rows = 24 });
    try fixture.stageFixture(remote.bridge, "spawn-local.bin");
    _ = try remote.host.drainReadiness();
    _ = remote.takeOperationResult();
    const spawned: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(1, 9, "build-host") } };
    _ = try remote.requestAttach(spawned);
    try fixture.stageFixture(remote.bridge, "satellite-ready.bin");
    try fixture.stageFixture(remote.bridge, "attach-accepted.bin");
    _ = try remote.host.drainReadiness();
    _ = remote.takeOperationResult();
    remote.bridge.outgoing.reset();
    const tree = &model.ws().tabs[0];
    _ = try tree.split(tree.root, .horizontal, spawned);
    var coordinator: close.Coordinator(1) = .{};
    const target = try close.Target.captureTab(model, remote, 0, 0);
    const before = remote.host.operation_ledger.last_id;
    try testing.expectError(error.InvalidState, coordinator.begin(model, remote, target));
    var reason_buffer: [256]u8 = undefined;
    const reason = remote.copyLastError(&reason_buffer);
    try testing.expectEqualStrings("atomic close is unavailable for satellite resources; batch was not queued", reason);
    try testing.expectEqual(before, remote.host.operation_ledger.last_id);
    try fixture.expectOutgoingCount(remote.bridge, 0);
    try testing.expect(coordinator.takeOutcomeFrom(model, remote) == null);
    try testing.expectEqual(@as(usize, 2), tree.paneCount());
    const pane = try close.Target.capturePane(model, remote, 0, 0, spawned);
    try testing.expectError(error.InvalidState, coordinator.begin(model, remote, pane));
    try testing.expectEqualStrings("atomic close is unavailable for satellite resources; batch was not queued", reason);
    try testing.expectEqualStrings("satellite close requires an instance-bound resource; no safe incarnation fence", remote.copyLastError(&reason_buffer));
    try testing.expectEqual(before, remote.host.operation_ledger.last_id);
    try fixture.expectOutgoingCount(remote.bridge, 0);
}
