//! Actual Model/projection contract, imported by the composition root's tests.
const std = @import("std");
const engine_module = @import("cockpit/native/ts_engine.zig");
const labels = @import("cockpit/native/window_labels.zig");
const windows = @import("cockpit/native/ts_window_navigation.zig");

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
