//! Tests load canonical Rust-generated frames through the production queues.
const std = @import("std");

pub fn readFixture(name: []const u8) ![]u8 {
    const path = try std.fmt.allocPrint(std.testing.allocator, "src/tests/fixtures/{s}", .{name});
    defer std.testing.allocator.free(path);
    return std.Io.Dir.cwd().readFileAlloc(std.testing.io, path, std.testing.allocator, .limited(64 * 1024));
}

pub fn expectOutgoing(bridge: anytype, name: []const u8) !void {
    const expected = try readFixture(name);
    defer std.testing.allocator.free(expected);
    const actual = bridge.outgoing.take() orelse return error.MissingFrame;
    defer bridge.outgoing.release(actual);
    try std.testing.expectEqualSlices(u8, expected, actual);
}

pub fn stageFixture(bridge: anytype, name: []const u8) !void {
    const encoded = try readFixture(name);
    defer std.testing.allocator.free(encoded);
    var offset: usize = 0;
    while (offset < encoded.len) {
        const len = 4 + std.mem.readInt(u32, encoded[offset..][0..4], .big);
        try std.testing.expect(bridge.incoming.stage(encoded[offset..][0..len]));
        offset += len;
    }
}

pub fn attachHost(host: anytype) !void {
    try host.start("operations-test");
    try stageFixture(host.bridge, "hello.bin");
    _ = try host.drainReadiness();
    try host.attachSessionId(1, .{ .cols = 80, .rows = 24 });
    try stageFixture(host.bridge, "attached.bin");
    const delta = try host.drainReadiness();
    try std.testing.expect(delta.ready_published);
    host.bridge.outgoing.reset();
}
