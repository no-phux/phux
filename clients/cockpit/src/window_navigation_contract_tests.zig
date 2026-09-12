//! Actual Model/projection contract, imported by the composition root's tests.
const std = @import("std");
const engine_module = @import("cockpit/native/ts_engine.zig");
const labels = @import("cockpit/native/window_labels.zig");
const windows = @import("cockpit/native/ts_window_navigation.zig");

fn search(engine: anytype, query: []const u8, out: []u8) ![]const u8 {
    var request: [79]u8 = @splat(0);
    request[0] = 1;
    request[1] = 4;
    std.mem.writeInt(u64, request[2..10], engine.revision, .little);
    request[12] = @intCast(query.len);
    @memcpy(request[13..][0..query.len], query);
    request[13 + query.len] = 4;
    return labels.encode(engine.model, engine.revision, request[0 .. 15 + query.len], out);
}

test "window navigator projects real model tabs and empty windows" {
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    var request = [_]u8{0} ** 15;
    request[0] = 1;
    request[1] = 4;
    std.mem.writeInt(u64, request[2..10], engine.revision, .little);
    request[13] = 4;
    var out: [windows.max_bytes]u8 = undefined;
    const reply = try labels.encode(engine.model, engine.revision, &request, &out);
    try std.testing.expectEqual(@as(u16, 3), std.mem.readInt(u16, reply[15..17], .little));
    try std.testing.expect(std.mem.indexOf(u8, reply, "Local PTY") != null);
    try std.testing.expect(std.mem.indexOf(u8, reply, "Empty window") != null);
}

test "window navigator finds full terminal title and directory beyond presentation limits" {
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const ref = engine.model.primary.tabTerminal(0).?;
    const pane = engine.model.provider.terminal(ref).?;
    pane.session.feed("\x1b]2;" ++ "T" ** 180 ++ "title-suffix\x07");
    pane.session.feed("\x1b]7;file://host/" ++ "d" ** 300 ++ "/directory-suffix\x1b\\");
    var out: [windows.max_bytes]u8 = undefined;
    for ([_][]const u8{ "title-suffix", "directory-suffix" }) |query| {
        const reply = try search(engine, query, &out);
        const at = 15 + query.len;
        try std.testing.expectEqual(@as(u16, 2), std.mem.readInt(u16, reply[at..][0..2], .little));
    }
}

test "window navigator searches full named empty session and host independently of display bounds" {
    if (comptime !@import("cockpit/phux_support.zig").phux_enabled) return error.SkipZigTest;
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(1) orelse return error.OutOfMemory;
    const remote = try @import("cockpit/phux_support.zig").PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .remote = .{ .target = "fixture-host" } }, null, "test");
    engine.model.phux_peers[0] = remote;
    remote.standBy();
    try remote.host.start("test");
    try std.testing.expect(remote.bridge.incoming.stage(@embedFile("tests/fixtures/hello.bin")));
    _ = try remote.host.drainReadiness();
    remote.host.sessions_generation = remote.host.connectionEpoch();
    const name = "N" ** 180 ++ "empty-suffix";
    try remote.host.sessions.append(std.testing.allocator, .{
        .id = 42,
        .name = try std.testing.allocator.dupe(u8, name),
        .created_at_unix_secs = 0,
        .window_count = 0,
        .attached_client_count = 0,
        .focused = false,
        .keep_empty = true,
        .empty = true,
    });
    engine.model.empty_pick = .{ .coordinator = remote.providerId(), .session = 42, .window = 1 };
    engine.model.empty_pick.?.setName(name);
    const view = @import("cockpit/native/empty_session.zig").view(engine.model, 1) orelse return error.MissingEmptyView;
    try std.testing.expectEqualStrings(name, view.name);
    try std.testing.expectEqualStrings("fixture-host", view.host);
    var out: [windows.max_bytes]u8 = undefined;
    for ([_][]const u8{ "empty-suffix", "fixture-host" }) |query| {
        const reply = try search(engine, query, &out);
        const at = 15 + query.len;
        try std.testing.expectEqual(@as(u16, 1), std.mem.readInt(u16, reply[at..][0..2], .little));
        try std.testing.expect(std.mem.indexOf(u8, reply, "Empty session") != null);
    }
}
