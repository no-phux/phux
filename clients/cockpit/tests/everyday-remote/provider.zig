//! Driven only by everyday-remote-live.py's private enrolled-server fixture.
const std = @import("std");
const contract = @import("provider_contract");
const PhuxProvider = @import("phux_provider").PhuxProvider;
const testing = std.testing;

fn tick(remote: *PhuxProvider) !void {
    _ = try remote.drainReadiness();
    try std.Io.sleep(testing.io, .fromMilliseconds(2), .awake);
}

fn ready(remote: *PhuxProvider) !contract.TerminalRef {
    var refs: [8]contract.TerminalRef = undefined;
    for (0..5000) |_| {
        try tick(remote);
        const count = remote.terminalRefs(&refs);
        if (count == 0) continue;
        if (remote.phase(refs[0]) == .live) return refs[0];
    }
    return error.AttachDeadline;
}

fn recorded(value: []const u8) !bool {
    const bytes = try std.Io.Dir.cwd().readFileAlloc(testing.io, "executed", testing.allocator, .limited(4096));
    defer testing.allocator.free(bytes);
    return std.mem.endsWith(u8, bytes, value);
}

fn input(remote: *PhuxProvider, ref: contract.TerminalRef, value: []const u8) !void {
    try testing.expect(!try recorded(value));
    try remote.sendPaste(remote.owner(ref).?, value, true);
    for (0..5000) |_| {
        try tick(remote);
        if (try recorded(value)) return;
    }
    return error.InputDeadline;
}

test "enrolled remote provider preserves canvas and fences old input across reconnect" {
    const raw = std.c.getenv("EVERYDAY_REMOTE_CONFIG") orelse return error.FixtureRequired;
    const remote = try PhuxProvider.create(
        testing.allocator,
        testing.io,
        .{ .remote = .{ .target = "loop", .config_path = std.mem.span(raw) } },
        "everyday",
        "everyday-remote-provider",
    );
    defer remote.destroy();
    try remote.open(.{});
    const ref = try ready(remote);
    try input(remote, ref, "provider-before-reconnect\n");
    // Drain the output that follows the independently observed execution file.
    for (0..100) |_| try tick(remote);
    const old = remote.owner(ref).?;
    const canvas = try testing.allocator.dupe(u8, remote.host.terminals.items[0].canvas.screen_text.items);
    defer testing.allocator.free(canvas);
    try testing.expect(canvas.len > 0);
    try remote.reconnect(.{});
    try testing.expectEqual(contract.Phase.reconnecting, remote.phase(ref).?);
    try testing.expectEqualStrings(canvas, remote.host.terminals.items[0].canvas.screen_text.items);
    try testing.expectError(error.InvalidState, remote.sendPaste(old, "must-not-run\n", true));
    const restored = try ready(remote);
    try testing.expect(ref.eql(restored));
    try testing.expect(!remote.ownerIsCurrent(old));
    try testing.expectError(error.InvalidState, remote.sendPaste(old, "must-not-run\n", true));
    try input(remote, restored, "provider-after-reconnect\n");
}
