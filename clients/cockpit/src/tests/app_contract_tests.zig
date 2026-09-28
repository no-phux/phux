const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const support = @import("support.zig");

const canvas = native_sdk.canvas;
const testing = std.testing;

const createSession = support.createSession;

test "Phux Cockpit owns its dark graphite and lime visual register" {
    const session = try createSession(80, 24);
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    const tokens = app.cockpitTokens(&model);
    try testing.expectEqual(canvas.Color.rgb8(9, 11, 15), tokens.colors.background);
    try testing.expectEqual(canvas.Color.rgb8(17, 20, 27), tokens.colors.surface);
    try testing.expectEqual(canvas.Color.rgb8(244, 247, 251), tokens.colors.text);
    try testing.expectEqual(canvas.Color.rgb8(190, 242, 100), tokens.colors.accent);
}

test "retained response capacity matches the outbound ring" {
    try testing.expectEqual(app.outbound_buffer_bytes, grid.Session.response_capacity_max);
}
