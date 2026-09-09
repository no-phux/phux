//! Shipping native painter policy hookup; structural grid evidence only.
const std = @import("std");
const testing = std.testing;
const app = @import("../native_test_root.zig");
const support = @import("support.zig");
const canvas = @import("native_sdk").canvas;
const geometry = @import("native_sdk").geometry;
const painter = @import("../cockpit/native/terminal_painter.zig");

fn frame(remote: *app.PhuxProvider, comptime name: []const u8) !void {
    try testing.expect(remote.bridge.incoming.stage(@embedFile("../providers/phux/style_fixture/" ++ name ++ ".bin")));
    _ = try remote.drainReadiness();
}

test "shipping painter applies configured remote theme on every paint without frames" {
    if (comptime !app.phux_enabled) return error.SkipZigTest;
    const remote = try app.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/unused-theme" }, null, "theme");
    const session = try support.createSession(80, 24);
    var model = app.initialModelWithPhux(session, remote);
    defer app.deinitModel(&model);
    try remote.host.start("theme");
    inline for (.{ "hello", "attached", "begin", "chunk", "ready", "attach-ready", "tail", "reset-colors" }) |name| try frame(remote, name);
    try app.PhuxProvider.test_support.stageWorkspaceFixture(remote.bridge, "workspace_initial_metadata.bin");
    try app.PhuxProvider.test_support.stageWorkspaceFixture(remote.bridge, "workspace_initial_state.bin");
    _ = try remote.drainReadiness();
    model.reconcileRemoteTerminals();
    try testing.expect(try model.shared_workspace.apply(&model, remote.workspaceSnapshot(), remote.connectionEpoch()));
    var refs: [1]app.TerminalRef = undefined;
    try testing.expectEqual(@as(usize, 1), remote.terminalRefs(&refs));

    model.config.foreground = .{ .r = 201, .g = 211, .b = 221 };
    model.config.background = .{ .r = 21, .g = 31, .b = 41 };
    model.config.cursor_color = .{ .r = 61, .g = 81, .b = 101 };
    model.config.selection_background = .{ .r = 51, .g = 71, .b = 91 };
    try paint(&model);
    var grid = remote.presentation(refs[0]).?.grid;
    try testing.expectEqual(canvas.Color.rgb8(201, 211, 221), grid.rows[1].cells[0].fg);
    try testing.expectEqual(canvas.Color.rgb8(21, 31, 41), grid.background);
    try testing.expectEqual(canvas.Color.rgb8(61, 81, 101), grid.cursor_color);
    try testing.expectEqual(canvas.Color.rgb8(51, 71, 91), grid.selection_color);

    // The second paint is the ONLY action after changing the config. Clearing
    // cursor-color must reveal the current accent, not the previous fallback.
    model.config.foreground = .{ .r = 101, .g = 121, .b = 141 };
    model.config.background = .{ .r = 41, .g = 51, .b = 61 };
    model.config.cursor_color = null;
    model.config.selection_background = .{ .r = 81, .g = 91, .b = 111 };
    try paint(&model);
    grid = remote.presentation(refs[0]).?.grid;
    try testing.expectEqual(canvas.Color.rgb8(101, 121, 141), grid.rows[1].cells[0].fg);
    try testing.expectEqual(canvas.Color.rgb8(41, 51, 61), grid.rows[1].cells[0].bg.?);
    try testing.expectEqual(canvas.Color.rgb8(81, 91, 111), grid.cursor_color);
    try testing.expectEqual(grid.cursor_color, grid.selection_color);
    try testing.expectEqual(canvas.Color.rgb8(180, 120, 60), grid.rows[0].cells[0].fg);
}

fn paint(model: *const app.Model) !void {
    const commands = try testing.allocator.alloc(canvas.CanvasCommand, canvas.max_display_list_commands);
    defer testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try painter.paintWindowIndex(model, &builder, 0, geometry.SizeF.init(980, 640), .{}, 0);
    try testing.expect(builder.displayList().commands.len > 0);
}
