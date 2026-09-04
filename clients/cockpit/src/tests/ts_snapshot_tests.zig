const std = @import("std");
const app = @import("../main.zig");
const support = @import("support.zig");

const testing = std.testing;

test "TypeScript snapshot contains stable tab identities and bounded titles" {
    const session = try support.createDefaultSession();
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    try testing.expect(model.admitTab(app.initialTerminalRef(1)));
    model.primary.selected_tab = 1;
    model.primary.palette.open = true;

    var bytes: [app.ts_snapshot_max_bytes]u8 = undefined;
    const snapshot = try app.encodeTsSnapshot(&model, 7, 11, [_]app.TsTabRun{ .{ .first = 0, .count = 2, .extent = 168 }, .{}, .{}, .{}, .{} }, .{}, &bytes);
    try testing.expectEqual(@as(u8, 1), snapshot[0]);
    try testing.expectEqual(@as(u8, 2), snapshot[1]);
    try testing.expectEqual(@as(u64, 7), std.mem.readInt(u64, snapshot[2..10], .little));
    try testing.expectEqual(@as(u64, 11), std.mem.readInt(u64, snapshot[10..18], .little));
    try testing.expectEqual(@as(u8, 2), snapshot[20]);
    try testing.expectEqual(@as(u8, 1), snapshot[21]);
    try testing.expect(snapshot[22] & (1 << 5) != 0);
    try testing.expectEqual(@as(u8, 0), snapshot[24]);
    try testing.expectEqual(@as(u8, 2), snapshot[25]);
    try testing.expectEqual(@as(u16, 168), std.mem.readInt(u16, snapshot[26..28], .little));

    const first_id = std.mem.readInt(u32, snapshot[28..32], .little);
    const first_title_len: usize = snapshot[33];
    const first_cwd_len: usize = snapshot[34];
    const second_at = 35 + first_title_len + first_cwd_len;
    const second_id = std.mem.readInt(u32, snapshot[second_at..][0..4], .little);
    try testing.expect(first_id != 0);
    try testing.expect(second_id != 0);
    try testing.expect(first_id != second_id);
}

test "TypeScript snapshot refuses truncation instead of emitting a partial record" {
    const session = try support.createDefaultSession();
    var model = app.initialModel(session);
    defer app.deinitModel(&model);

    var bytes: [27]u8 = undefined;
    try testing.expectError(error.BufferTooSmall, app.encodeTsSnapshot(&model, 0, 0, [_]app.TsTabRun{ .{}, .{}, .{}, .{}, .{} }, .{}, &bytes));
}
