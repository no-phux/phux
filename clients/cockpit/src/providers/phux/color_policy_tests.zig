//! Canonical frames through the shipping provider -> Host publication path.
const std = @import("std");
const testing = std.testing;
const canvas = @import("native_sdk").canvas;
const provider = @import("provider.zig");
const contract = @import("provider_contract");

fn feed(remote: *provider.PhuxProvider, comptime name: []const u8) !void {
    try testing.expect(remote.bridge.incoming.stage(@embedFile("style_fixture/" ++ name ++ ".bin")));
    _ = try remote.drainReadiness();
}

fn attached() !*provider.PhuxProvider {
    const remote = try provider.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/unused-style-test" }, null, "colors");
    errdefer remote.destroy();
    remote.setColorPolicy(first);
    try remote.host.start("colors");
    try feed(remote, "hello");
    // drainReadiness queues the shipping attach after HELLO_OK.
    inline for (.{ "attached", "begin", "chunk", "ready", "attach-ready" }) |name| try feed(remote, name);
    return remote;
}

fn ref(remote: *provider.PhuxProvider) !contract.TerminalRef {
    var refs: [1]contract.TerminalRef = undefined;
    try testing.expectEqual(@as(usize, 1), remote.terminalRefs(&refs));
    return refs[0];
}

const first: provider.ColorPolicy = .{
    .foreground = canvas.Color.rgb8(221, 201, 181),
    .background = canvas.Color.rgb8(31, 41, 51),
    .cursor_fallback = canvas.Color.rgb8(71, 91, 111),
    .selection_color = canvas.Color.rgb8(121, 81, 41),
};
const second: provider.ColorPolicy = .{
    .foreground = canvas.Color.rgb8(181, 211, 241),
    .background = canvas.Color.rgb8(61, 21, 41),
    .cursor_fallback = canvas.Color.rgb8(161, 121, 81),
    .selection_color = canvas.Color.rgb8(51, 151, 101),
};

test "shipping Host preserves configured colors across OSC reset and idle theme repaint" {
    const remote = try attached();
    defer remote.destroy();
    const terminal = try ref(remote);
    const overridden = remote.presentation(terminal).?.grid;
    try testing.expectEqual(canvas.Color.rgb8(192, 176, 160), overridden.foreground);
    try testing.expectEqual(canvas.Color.rgb8(16, 40, 64), overridden.background);
    try testing.expectEqual(canvas.Color.rgb8(128, 160, 192), overridden.cursor_color);
    try testing.expectEqual(first.selection_color, overridden.selection_color);
    try feed(remote, "tail");
    try feed(remote, "reset-colors");
    try expectDefaults(remote.presentation(terminal).?.grid, first);
    const owner = remote.owner(terminal).?;
    const text = try testing.allocator.dupe(u8, remote.presentation(terminal).?.grid.screen_text);
    defer testing.allocator.free(text);
    remote.setColorPolicy(second); // No frame, timer, allocation or engine query.
    try expectDefaults(remote.presentation(terminal).?.grid, second);
    try testing.expect(remote.owner(terminal).?.eql(owner));
    try testing.expectEqualStrings(text, remote.presentation(terminal).?.grid.screen_text);
    remote.setColorPolicy(first);
    try expectDefaults(remote.presentation(terminal).?.grid, first);
    // DECSCNM swaps configured defaults once; explicit RGB is still explicit.
    try feed(remote, "reverse");
    var reversed = first;
    reversed.foreground = first.background;
    reversed.background = first.foreground;
    try expectDefaults(remote.presentation(terminal).?.grid, reversed);
    // Reconnect destroys the source engine. Recolor the frozen owned grid.
    try remote.host.reconnect("colors");
    remote.setColorPolicy(second);
    reversed = second;
    reversed.foreground = second.background;
    reversed.background = second.foreground;
    try expectDefaults(remote.presentation(terminal).?.grid, reversed);
}

fn expectDefaults(grid: canvas.TerminalGrid, policy: provider.ColorPolicy) !void {
    try testing.expectEqual(policy.foreground, grid.foreground);
    try testing.expectEqual(policy.background, grid.background);
    try testing.expectEqual(policy.cursor_fallback, grid.cursor_color);
    try testing.expectEqual(policy.selection_color, grid.selection_color);
    try testing.expectEqual(policy.foreground, grid.rows[1].cells[0].fg);
    try testing.expectEqual(policy.background, grid.rows[1].cells[0].bg.?);
    try testing.expectEqual(canvas.Color.rgb8(180, 120, 60), grid.rows[0].cells[0].fg);
    try testing.expectEqual(canvas.Color.rgb8(20, 40, 80), grid.rows[0].cells[0].bg.?);
    try testing.expectEqual(canvas.Color.rgb8(60, 90, 150), grid.rows[0].cells[0].underline_color.?);
    try testing.expectEqual(canvas.Color.rgb8(20, 60, 100), grid.rows[4].cells[7].bg.?);
    try testing.expectEqual(policy.background, grid.rows[5].cells[1].fg); // inverse, once
    try testing.expectEqual(policy.foreground, grid.rows[5].cells[1].bg.?);
    try testing.expect(grid.rows[5].cells[1].underline_color == null);
}
