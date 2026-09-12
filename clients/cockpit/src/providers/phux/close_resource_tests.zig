//! Actual FFI queues and receipts prove explicit close ownership and fencing.
const std = @import("std");
const host_module = @import("host.zig");
const transport = @import("phux_transport");
const provider = @import("provider_contract");
const support = @import("operation_test_support.zig");

fn ref(id: u32) !provider.TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = try provider.RemoteResourceId.fromPhux(0, id, "") } };
}

test "batch close rejects invalid suffix and emits one correlated command" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try host_module.Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try support.attachHost(host);
    const epoch = host.connectionEpoch();
    const terminal = try ref(7);
    try std.testing.expectError(error.InvalidIdentity, host.requestCloseResources(&.{terminal}, epoch + 1));
    try std.testing.expectError(error.InvalidIdentity, host.requestCloseResources(&.{ terminal, try ref(999) }, epoch));
    // The Host maps both FFI argument and state refusals to InvalidState.
    try std.testing.expectError(error.InvalidState, host.requestCloseResources(&.{ terminal, terminal }, epoch));
    var reason_buffer: [128]u8 = undefined;
    const reason = host.copyLastError(&reason_buffer);
    try std.testing.expectEqualStrings("close batch repeats a terminal ID", reason);
    var short: [3]u8 = undefined;
    try std.testing.expectEqualStrings("clo", host.copyLastError(&short));
    var empty: [0]u8 = .{};
    try std.testing.expectEqual(@as(usize, 0), host.copyLastError(&empty).len);
    try support.expectOutgoingCount(&bridge, 0);
    try std.testing.expectEqual(@as(u32, 1), try host.requestCloseResources(&.{terminal}, epoch));
    try std.testing.expectEqualStrings("close batch repeats a terminal ID", reason);
    try std.testing.expectEqual(@as(usize, 0), host.copyLastError(&short).len);
    try support.expectOutgoingCount(&bridge, 1);
    try support.stageFixture(&bridge, "detach-ok.bin");
    _ = try host.drainReadiness();
    try std.testing.expect(host.takeOperationResult() == null);
    try std.testing.expectEqual(provider.Phase.live, host.presentation(terminal).?.phase);
    try support.stageFixture(&bridge, "initial-terminal-closed.bin");
    _ = try host.drainReadiness();
    const result = host.takeOperationResult().?;
    try std.testing.expectEqual(.close_resources, result.kind);
    try std.testing.expectEqual(.success, result.status);
    try std.testing.expect(result.terminal_ref == null);
    try std.testing.expect(!host.terminalKnown(terminal));
}

test "explicit close requires captured connection and exact published owner" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try host_module.Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try support.attachHost(host);
    const terminal = try ref(7);
    const epoch = host.connectionEpoch();
    try std.testing.expectError(error.InvalidIdentity, host.requestCloseResource(terminal, epoch + 1));
    try std.testing.expectError(error.InvalidIdentity, host.requestCloseResource(try ref(999), epoch));
    var other_provider = terminal;
    other_provider.provider_id = .local;
    try std.testing.expectError(error.InvalidIdentity, host.requestCloseResource(other_provider, epoch));
    try support.expectOutgoingCount(&bridge, 0);
    try std.testing.expectEqual(@as(u32, 1), try host.requestCloseResource(terminal, epoch));
    try std.testing.expectEqual(provider.Phase.live, host.presentation(terminal).?.phase);
    try std.testing.expectError(error.InvalidState, host.requestCloseResource(terminal, epoch));
    try support.expectOutgoingCount(&bridge, 1);
    // This canonical fixture carries COMMAND_RESULT(request=1, Ok), regardless
    // of which command produced it. A close receipt must never act like detach.
    try support.stageFixture(&bridge, "detach-ok.bin");
    _ = try host.drainReadiness();
    try std.testing.expect(host.takeOperationResult() == null);
    try std.testing.expectEqual(provider.Phase.live, host.presentation(terminal).?.phase);
    try support.stageFixture(&bridge, "initial-terminal-closed.bin");
    _ = try host.drainReadiness();
    const result = host.takeOperationResult().?;
    try std.testing.expectEqual(.close_resource, result.kind);
    try std.testing.expectEqual(.success, result.status);
    try std.testing.expectEqual(epoch, result.connection_epoch);
    try std.testing.expect(result.terminal_ref.?.eql(terminal));
    try std.testing.expect(!host.terminalKnown(terminal));
    try std.testing.expectError(error.InvalidIdentity, host.requestCloseResource(terminal, epoch));
}

test "close consumes pending disconnect and cannot replay against recycled identity" {
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    const host = try host_module.Host.create(std.testing.allocator, &bridge);
    defer host.destroy();
    try support.attachHost(host);
    const terminal = try ref(7);
    const captured_epoch = host.connectionEpoch();
    bridge.incoming.markDisconnected(.socket_lost);
    try std.testing.expectError(error.InvalidState, host.requestCloseResource(terminal, captured_epoch));
    try support.expectOutgoingCount(&bridge, 0);
    try host.reconnect("close-replacement");
    try support.stageFixture(&bridge, "hello.bin");
    _ = try host.drainReadiness();
    try host.attachSessionId(1, .{ .cols = 80, .rows = 24 });
    try support.stageFixture(&bridge, "attached.bin");
    _ = try host.drainReadiness();
    bridge.outgoing.reset();
    try std.testing.expect(captured_epoch != host.connectionEpoch());
    try std.testing.expectError(error.InvalidIdentity, host.requestCloseResource(terminal, captured_epoch));
    try support.expectOutgoingCount(&bridge, 0);
    _ = try host.requestCloseResource(terminal, host.connectionEpoch());
    host.disconnect(); // Staged output is thrown away before any worker sends it.
    try support.expectOutgoingCount(&bridge, 0);
    const result = host.takeOperationResult().?;
    try std.testing.expectEqual(.close_resource, result.kind);
    try std.testing.expectEqual(.unknown_outcome, result.status);
}
