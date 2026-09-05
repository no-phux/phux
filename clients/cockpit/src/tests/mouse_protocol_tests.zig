const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const support = @import("support.zig");

const geometry = native_sdk.geometry;
const testing = std.testing;

const destroyModelSessions = app.deinitModel;
const terminalInteractionFrame = support.terminalInteractionFrame;

const pointer_support = @import("pointer_support.zig");
const startPointerHost = pointer_support.startPointerHost;
const pointerInput = pointer_support.pointerInput;
const pointerInputAdvanced = pointer_support.pointerInputAdvanced;
const terminalCellPoint = pointer_support.terminalCellPoint;
const activePointerCaptureCount = pointer_support.activePointerCaptureCount;
const activePointerCapture = pointer_support.activePointerCapture;

test "TUI mouse reports exact SGR press motion release and wheel cells" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const app_iface = host.app();
    const pane = host.inner.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    try host.inner.effects.feedPtyOutput(pane.pty_key, "mouse\r\n\x1b[?1003h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    const frame = terminalInteractionFrame(harness, "mouse") orelse return error.TestExpectedTerminalInteractionSurface;
    const cell = pane.session.measuredCell() orelse return error.TestExpectedMeasuredCell;
    const down = geometry.PointF.init(frame.x + cell.width * 3.25, frame.y + cell.height * 2.25);
    const moved = geometry.PointF.init(frame.x + cell.width * 4.25, down.y);
    try pointerInput(harness, app_iface, .pointer_down, down, 0, .{}, 0);
    try pointerInput(harness, app_iface, .pointer_drag, moved, 0, .{}, 0);
    try pointerInput(harness, app_iface, .pointer_up, moved, 0, .{}, 0);
    try pointerInput(harness, app_iface, .scroll, moved, 0, .{}, cell.height);

    try testing.expectEqualStrings(
        "\x1b[<0;4;3M\x1b[<32;5;3M\x1b[<0;5;3m\x1b[<64;5;3M",
        host.inner.effects.ptyWrittenBytes(pane.pty_key),
    );
    try testing.expect(!pane.session.selectionActive());
}

test "mouse protocols cover X10 UTF-8 SGR URXVT modes modifiers and both wheel axes once" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const iface = host.app();
    const pane = host.inner.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    for (2..6) |frame_index| try harness.runtime.dispatchPlatformEvent(iface, .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = size,
        .scale_factor = 2,
        .frame_index = @intCast(frame_index),
        .timestamp_ns = @as(u64, frame_index) * 1_000_000,
    } });
    const frame = app.paneFrames(&host.inner.model, size)[0];
    var point = terminalCellPoint(pane, frame, 2, 3);

    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?9h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    var before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    try pointerInput(harness, iface, .pointer_up, point, 0, .{}, 0);
    try testing.expectEqualSlices(u8, &.{ 0x1b, '[', 'M', 32, 35, 36 }, host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);

    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?9l\x1b[?1000h\x1b[?1005h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    point = terminalCellPoint(pane, frame, 100, 2);
    before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    try pointerInput(harness, iface, .pointer_up, point, 0, .{}, 0);
    try testing.expectEqualStrings("\x1b[M \xc2\x85#\x1b[M#\xc2\x85#", host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);

    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1005l\x1b[?1015h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    point = terminalCellPoint(pane, frame, 2, 3);
    before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    try pointerInput(harness, iface, .pointer_up, point, 0, .{}, 0);
    try testing.expectEqualStrings("\x1b[32;3;4M\x1b[35;3;4M", host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);

    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1015l\x1b[?1006h\x1b[?1000l\x1b[?1002h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    const moved = terminalCellPoint(pane, frame, 3, 3);
    before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    try pointerInput(harness, iface, .pointer_drag, moved, 0, .{}, 0);
    try pointerInput(harness, iface, .pointer_up, moved, 0, .{}, 0);
    try testing.expectEqualStrings("\x1b[<0;3;4M\x1b[<32;4;4M\x1b[<0;4;4m", host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);

    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1002l\x1b[?1003h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try pointerInputAdvanced(harness, iface, .pointer_down, point, .{
        .button = 2,
        .modifiers = .{ .shift = true, .control = true, .option = true, .command = true },
    });
    try pointerInputAdvanced(harness, iface, .pointer_up, point, .{ .button = 2 });
    try testing.expectEqualStrings("\x1b[<29;3;4M\x1b[<1;3;4m", host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);

    before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try pointerInputAdvanced(harness, iface, .scroll, point, .{
        .delta_x = pane.session.measuredCell().?.width,
        .delta_y = pane.session.measuredCell().?.height,
    });
    try testing.expectEqualStrings("\x1b[<64;3;4M\x1b[<66;3;4M", host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);
}

test "SGR Pixels follows frame scale at one one-and-a-half and two" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const iface = host.app();
    const pane = host.inner.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1000h\x1b[?1016h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    const frame = app.paneFrames(&host.inner.model, size)[0];
    const point = terminalCellPoint(pane, frame, 1, 2);
    const local_x = point.x - frame.x;
    const local_y = point.y - frame.y;

    for ([_]f32{ 1, 1.5, 2 }, 2..) |scale, frame_index| {
        try harness.runtime.dispatchPlatformEvent(iface, .{ .gpu_surface_frame = .{
            .label = app.canvas_label,
            .size = size,
            .scale_factor = scale,
            .frame_index = frame_index,
            .timestamp_ns = @as(u64, frame_index) * 1_000_000,
        } });
        const before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
        try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
        try pointerInput(harness, iface, .pointer_up, point, 0, .{}, 0);
        var expected: [96]u8 = undefined;
        const px: i32 = @intFromFloat(@round(local_x * scale));
        const py: i32 = @intFromFloat(@round(local_y * scale));
        const bytes = try std.fmt.bufPrint(&expected, "\x1b[<0;{d};{d}M\x1b[<0;{d};{d}m", .{ px, py, px, py });
        try testing.expectEqualStrings(bytes, host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);
    }
}

test "mouse protocol transitions reset motion dedupe and wheel residue" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const iface = host.app();
    const pane = host.inner.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    const frame = app.paneFrames(&host.inner.model, size)[0];
    const point = terminalCellPoint(pane, frame, 3, 3);
    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1003h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    var before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try pointerInput(harness, iface, .pointer_move, point, 0, .{}, 0);
    const one_motion = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try testing.expect(one_motion > before);
    try pointerInput(harness, iface, .pointer_move, point, 0, .{}, 0);
    try testing.expectEqual(one_motion, host.inner.effects.ptyWrittenBytes(pane.pty_key).len);

    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1003l");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1003h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try pointerInput(harness, iface, .pointer_move, point, 0, .{}, 0);
    try testing.expect(host.inner.effects.ptyWrittenBytes(pane.pty_key).len > one_motion);

    const half = pane.session.measuredCell().?.height / 2;
    try pointerInputAdvanced(harness, iface, .scroll, point, .{ .delta_y = half });
    before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1006l\x1b[?1015h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try pointerInputAdvanced(harness, iface, .scroll, point, .{ .delta_y = half });
    try testing.expectEqual(before, host.inner.effects.ptyWrittenBytes(pane.pty_key).len);
}

test "wheel reporting is axis-fair and does not consume local residue" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const iface = host.app();
    const pane = host.inner.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    const frame = app.paneFrames(&host.inner.model, size)[0];
    const point = terminalCellPoint(pane, frame, 2, 2);
    try pointerInputAdvanced(harness, iface, .scroll, point, .{ .delta_y = pane.session.measuredCell().?.height / 2 });
    const local_residue = pane.scrollback_wheel_accum;
    try testing.expect(local_residue > 0);

    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1003h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    const before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try pointerInputAdvanced(harness, iface, .scroll, point, .{
        .delta_x = std.math.floatMax(f32),
        .delta_y = std.math.floatMax(f32),
    });
    const reports = host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..];
    try testing.expectEqual(@as(usize, 32), std.mem.count(u8, reports, "\x1b[<64;"));
    try testing.expectEqual(@as(usize, 32), std.mem.count(u8, reports, "\x1b[<66;"));
    try testing.expectEqual(local_residue, pane.scrollback_wheel_accum);
    try testing.expect(pane.mouse_wheel_y_accum >= pane.session.measuredCell().?.height);
    try testing.expect(pane.mouse_wheel_x_accum >= pane.session.measuredCell().?.width);
}

test "mouse capture is fenced by its press protocol fingerprint" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const iface = host.app();
    const pane = host.inner.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1002h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    const frame = app.paneFrames(&host.inner.model, size)[0];
    const point = terminalCellPoint(pane, frame, 2, 2);
    try pointerInputAdvanced(harness, iface, .pointer_down, point, .{});
    const capture = activePointerCapture(&host.inner.model, 7) orelse return error.TestExpectedCapture;
    try testing.expectEqual(pane.mouse_protocol_fingerprint, capture.mouse_protocol_fingerprint);
    const before_transition = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1006l\x1b[?1015h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));
    try pointerInputAdvanced(harness, iface, .pointer_drag, terminalCellPoint(pane, frame, 3, 2), .{});
    try testing.expectEqual(before_transition, host.inner.effects.ptyWrittenBytes(pane.pty_key).len);
}
