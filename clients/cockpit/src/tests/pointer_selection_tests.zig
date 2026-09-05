const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const support = @import("support.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const destroyModelSessions = app.deinitModel;
const typeCanvasText = support.typeCanvasText;
const pressCanvasKey = support.pressCanvasKey;
const releaseCanvasKey = support.releaseCanvasKey;
const terminalInteractionFrame = support.terminalInteractionFrame;
const rectCenter = support.rectCenter;

const pointer_support = @import("pointer_support.zig");
const startPointerHost = pointer_support.startPointerHost;
const pointerInput = pointer_support.pointerInput;
const pointerInputAdvanced = pointer_support.pointerInputAdvanced;
const terminalCellPoint = pointer_support.terminalCellPoint;
const expectPointerSelectionText = pointer_support.expectPointerSelectionText;
const activePointerCaptureCount = pointer_support.activePointerCaptureCount;
const activePointerCapture = pointer_support.activePointerCapture;

const max_effect_mouse_test_bytes: usize = 64 * 32 + 1;

test "cmd+click opens the URL under the pointer, and only a URL" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const app_iface = host.app();
    const model = &host.inner.model;
    const pane = model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;

    try host.inner.effects.feedPtyOutput(pane.pty_key, "open https://example.com/docs now\r\n");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    const frame = terminalInteractionFrame(harness, "example.com") orelse return error.TestExpectedTerminalInteractionSurface;

    // Column 10 of row 0 is inside "https://example.com/docs".
    const on_link = terminalCellPoint(pane, frame, 10, 0);
    try pointerInput(harness, app_iface, .pointer_down, on_link, 0, .{ .command = true }, 0);
    try testing.expectEqual(@as(u32, 1), model.opened_url_count);
    try testing.expectEqualStrings("https://example.com/docs", model.openedUrl());
    // The press was CONSUMED: no capture to leave dangling, no half-started
    // selection.
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(model));
    try testing.expect(!pane.session.selectionActive());

    // Column 1 is the word "open" - ordinary text falls through to the normal
    // gesture rather than being swallowed by the link chord.
    const off_link = terminalCellPoint(pane, frame, 1, 0);
    try pointerInput(harness, app_iface, .pointer_down, off_link, 0, .{ .command = true }, 0);
    try testing.expectEqual(@as(u32, 1), model.opened_url_count);
    try pointerInput(harness, app_iface, .pointer_up, off_link, 0, .{ .command = true }, 0);

    // A plain click on the link is a SELECTION, not a navigation.
    try pointerInput(harness, app_iface, .pointer_down, on_link, 0, .{}, 0);
    try testing.expectEqual(@as(u32, 1), model.opened_url_count);
    try pointerInput(harness, app_iface, .pointer_up, on_link, 0, .{}, 0);
}

test "pointer drag selects Ghostty cells and Shift overrides TUI mouse reporting" {
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
    try host.inner.effects.feedPtyOutput(pane.pty_key, "alpha beta\r\n\x1b[?1003h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    const frame = terminalInteractionFrame(harness, "alpha beta") orelse return error.TestExpectedTerminalInteractionSurface;
    const start = geometry.PointF.init(frame.x + pane.session.measuredCell().?.width * 0.25, frame.y + pane.session.measuredCell().?.height * 0.5);
    const finish = geometry.PointF.init(frame.x + pane.session.measuredCell().?.width * 4.75, start.y);
    const shift = native_sdk.platform.ShortcutModifiers{ .shift = true };
    try pointerInput(harness, app_iface, .pointer_down, start, 0, shift, 0);
    try pointerInput(harness, app_iface, .pointer_drag, finish, 0, shift, 0);
    try pointerInput(harness, app_iface, .pointer_up, finish, 0, shift, 0);

    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));
    try testing.expect(pane.session.selectionActive());
    const selected = (try pane.session.selectionText(gpa)) orelse return error.TestExpectedSelection;
    defer gpa.free(selected);
    try testing.expectEqualStrings("alpha", selected);
    try testing.expectEqualStrings("", host.inner.effects.ptyWrittenBytes(pane.pty_key));
}

test "pointer ownership survives focus and reorder but never crosses close generation" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const app_iface = host.app();
    try host.inner.dispatch(&harness.runtime, 1, .split_right);
    const first = host.inner.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    const second = host.inner.model.provider.terminal(app.initialTerminalRef(1)) orelse return error.TestExpectedTerminal;
    try host.inner.effects.feedPtyOutput(first.pty_key, "\x1b[?1002h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    const frames = app.paneFrames(&host.inner.model, size);
    const start = rectCenter(frames[0]);
    const across = rectCenter(frames[1]);

    try pointerInput(harness, app_iface, .pointer_down, start, 0, .{}, 0);
    try testing.expectEqual(app.LocalTerminalId.terminal_1, activePointerCapture(&host.inner.model, 7).?.terminal_id);
    try host.inner.dispatch(&harness.runtime, 1, .{ .cycle_pane = 1 });
    try host.inner.dispatch(&harness.runtime, 1, .{ .move_terminal = -1 });
    try pointerInput(harness, app_iface, .pointer_drag, across, 0, .{}, 0);
    try pointerInput(harness, app_iface, .pointer_up, across, 0, .{}, 0);
    try testing.expect(host.inner.effects.ptyWrittenBytes(first.pty_key).len > 0);
    try testing.expectEqualStrings("", host.inner.effects.ptyWrittenBytes(second.pty_key));

    try host.inner.dispatch(&harness.runtime, 1, .{ .cycle_pane = -1 });
    try pointerInput(harness, app_iface, .pointer_down, start, 0, .{}, 0);
    const before_close = host.inner.effects.ptyWrittenBytes(first.pty_key).len;
    try host.inner.dispatch(&harness.runtime, 1, .close_terminal);
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));
    const after_close = host.inner.effects.ptyWrittenBytes(first.pty_key).len;
    try testing.expect(after_close > before_close); // the live generation received its one release
    try pointerInput(harness, app_iface, .pointer_up, across, 0, .{}, 0);
    try testing.expectEqual(after_close, host.inner.effects.ptyWrittenBytes(first.pty_key).len);
    try testing.expectEqualStrings("", host.inner.effects.ptyWrittenBytes(second.pty_key));
}

test "Ghostty pointer selection keeps single word line gestures distinct from keyboard mode and typing clears" {
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
    pane.session.reset();
    try host.inner.effects.feedPtyOutput(pane.pty_key, "alpha beta gamma\r\nsecond line");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    const frame = terminalInteractionFrame(harness, "alpha beta gamma") orelse return error.TestExpectedTerminalInteractionSurface;

    const alpha_start = terminalCellPoint(pane, frame, 0, 0);
    const alpha_end = terminalCellPoint(pane, frame, 5, 0);
    try pointerInputAdvanced(harness, iface, .pointer_down, alpha_start, .{ .timestamp_ns = 1_000_000_000 });
    try pointerInputAdvanced(harness, iface, .pointer_drag, alpha_end, .{ .timestamp_ns = 1_010_000_000 });
    try pointerInputAdvanced(harness, iface, .pointer_up, alpha_end, .{ .timestamp_ns = 1_020_000_000 });
    try expectPointerSelectionText(pane, "alpha");
    try testing.expect(!pane.selecting);

    const beta = terminalCellPoint(pane, frame, 7, 0);
    try pointerInputAdvanced(harness, iface, .pointer_down, beta, .{ .timestamp_ns = 2_000_000_000 });
    try pointerInputAdvanced(harness, iface, .pointer_up, beta, .{ .timestamp_ns = 2_010_000_000 });
    try pointerInputAdvanced(harness, iface, .pointer_down, beta, .{ .timestamp_ns = 2_100_000_000 });
    try pointerInputAdvanced(harness, iface, .pointer_up, beta, .{ .timestamp_ns = 2_110_000_000 });
    try expectPointerSelectionText(pane, "beta");

    try pointerInputAdvanced(harness, iface, .pointer_down, beta, .{ .timestamp_ns = 2_200_000_000 });
    try pointerInputAdvanced(harness, iface, .pointer_up, beta, .{ .timestamp_ns = 2_210_000_000 });
    try expectPointerSelectionText(pane, "alpha beta gamma");

    try host.inner.dispatch(&harness.runtime, 1, .copy_selection);
    const copy = host.inner.effects.pendingClipboardAt(0) orelse return error.TestExpectedClipboardWrite;
    try testing.expectEqualStrings("alpha beta gamma", copy.text);
    try host.inner.effects.feedClipboardResult(app.clipboard_key, .ok, "");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);

    // Start a new selection over the persistent copied highlight, then prove
    // ordinary committed text clears pointer selection and reaches the PTY.
    try pointerInputAdvanced(harness, iface, .pointer_down, alpha_start, .{ .timestamp_ns = 3_000_000_000 });
    try pointerInputAdvanced(harness, iface, .pointer_drag, alpha_end, .{ .timestamp_ns = 3_010_000_000 });
    try pointerInputAdvanced(harness, iface, .pointer_up, alpha_end, .{ .timestamp_ns = 3_020_000_000 });
    try expectPointerSelectionText(pane, "alpha");

    const before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try typeCanvasText(harness, iface, "x");
    try testing.expect(!pane.session.selectionActive());
    try testing.expect(!pane.selecting);
    try testing.expectEqualStrings("x", host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);
}

test "pointer lifecycle matrix releases live owners once and fences restart generation" {
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
    try host.inner.effects.feedPtyOutput(pane.pty_key, "MATRIX\r\n\x1b[?1002h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    var frame = terminalInteractionFrame(harness, "MATRIX") orelse return error.TestExpectedTerminalInteractionSurface;
    var point = terminalCellPoint(pane, frame, 1, 0);

    // Web switch releases the live reporting generation; its stale up is inert.
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    var before_fence = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try host.inner.dispatch(&harness.runtime, 1, .{ .select_surface = .web });
    var after_fence = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try testing.expect(after_fence > before_fence);
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));
    try pointerInput(harness, iface, .pointer_up, point, 0, .{}, 0);
    try testing.expectEqual(after_fence, host.inner.effects.ptyWrittenBytes(pane.pty_key).len);

    try host.inner.dispatch(&harness.runtime, 1, .{ .select_surface = .{ .terminal = pane.id } });
    try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    frame = terminalInteractionFrame(harness, "MATRIX") orelse return error.TestExpectedTerminalInteractionSurface;
    point = terminalCellPoint(pane, frame, 1, 0);

    // Hiding the pane behind the web surface, and deactivation, each release
    // the capture exactly once while the owner is still live. (Detach/attach
    // no longer exist: a pane belongs to a tree, not to a placement slot.)
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    before_fence = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try host.inner.dispatch(&harness.runtime, 1, .{ .select_surface = .web });
    after_fence = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try testing.expect(after_fence > before_fence);
    try host.inner.dispatch(&harness.runtime, 1, .{ .select_surface = .{ .terminal = pane.id } });
    try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    frame = terminalInteractionFrame(harness, "MATRIX") orelse return error.TestExpectedTerminalInteractionSurface;
    point = terminalCellPoint(pane, frame, 1, 0);
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    before_fence = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try harness.runtime.dispatchPlatformEvent(iface, .app_deactivated);
    after_fence = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try testing.expect(after_fence > before_fence);
    try harness.runtime.dispatchPlatformEvent(iface, .app_activated);

    // Cancel and a replacement down close exactly one old capture each.
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    before_fence = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try pointerInput(harness, iface, .pointer_cancel, point, 0, .{}, 0);
    after_fence = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try testing.expect(after_fence > before_fence);
    try pointerInput(harness, iface, .pointer_cancel, point, 0, .{}, 0);
    try testing.expectEqual(after_fence, host.inner.effects.ptyWrittenBytes(pane.pty_key).len);
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    before_fence = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    after_fence = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    try testing.expect(after_fence > before_fence);
    try testing.expectEqual(@as(usize, 1), activePointerCaptureCount(&host.inner.model));
    try pointerInput(harness, iface, .pointer_up, point, 0, .{}, 0);

    // An ended session can still be selected locally; restart cancels that
    // gesture and stale motion cannot select in the replacement generation.
    try host.inner.effects.feedPtyExit(pane.pty_key, 0, 0, .spawn_failed, 0); // a spawn failure: the one end that leaves the pane standing
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    // The abnormal end pulls the band back, which moves the content area
    // down: re-read the pane's rect before aiming at it again.
    try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    frame = terminalInteractionFrame(harness, "MATRIX") orelse return error.TestExpectedTerminalInteractionSurface;
    point = terminalCellPoint(pane, frame, 1, 0);
    const old_generation = pane.session_generation;
    try pointerInputAdvanced(harness, iface, .pointer_down, point, .{});
    try testing.expectEqual(@as(usize, 1), activePointerCaptureCount(&host.inner.model));
    try host.inner.dispatch(&harness.runtime, 1, .{ .restart = pane.id });
    try testing.expect(pane.session_generation != old_generation);
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));
    try pointerInputAdvanced(harness, iface, .pointer_drag, terminalCellPoint(pane, frame, 4, 0), .{});
    try testing.expect(!pane.session.selectionActive());

    // A generation mismatch retires capture without writing to the replacement.
    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1002h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try pointerInput(harness, iface, .pointer_down, point, 0, .{}, 0);
    const before_loss = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;
    pane.session_generation += 1;
    try pointerInput(harness, iface, .pointer_drag, point, 0, .{}, 0);
    try testing.expectEqual(before_loss, host.inner.effects.ptyWrittenBytes(pane.pty_key).len);
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));
}

test "multiple pointer captures are isolated and hostile wheel values stay bounded" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const host = try startPointerHost(gpa, harness, size);
    defer gpa.destroy(host);
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    const iface = host.app();
    try host.inner.dispatch(&harness.runtime, 1, .split_right);
    const left = host.inner.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    const right = host.inner.model.provider.terminal(app.initialTerminalRef(1)) orelse return error.TestExpectedTerminal;
    try host.inner.effects.feedPtyOutput(left.pty_key, "\x1b[?1002h\x1b[?1006h");
    try host.inner.effects.feedPtyOutput(right.pty_key, "\x1b[?1002h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    const frames = app.paneFrames(&host.inner.model, size);
    const left_point = terminalCellPoint(left, frames[0], 2, 2);
    const right_point = terminalCellPoint(right, frames[1], 2, 2);
    try pointerInputAdvanced(harness, iface, .pointer_down, left_point, .{ .pointer_id = 7 });
    try pointerInputAdvanced(harness, iface, .pointer_down, right_point, .{ .pointer_id = 8 });
    try testing.expectEqual(@as(usize, 2), activePointerCaptureCount(&host.inner.model));
    try pointerInputAdvanced(harness, iface, .pointer_cancel, right_point, .{ .pointer_id = 99 });
    try testing.expectEqual(@as(usize, 2), activePointerCaptureCount(&host.inner.model));
    const left_before = host.inner.effects.ptyWrittenBytes(left.pty_key).len;
    const right_before = host.inner.effects.ptyWrittenBytes(right.pty_key).len;
    try pointerInputAdvanced(harness, iface, .pointer_drag, right_point, .{ .pointer_id = 7 });
    try pointerInputAdvanced(harness, iface, .pointer_drag, left_point, .{ .pointer_id = 8 });
    try testing.expect(host.inner.effects.ptyWrittenBytes(left.pty_key).len > left_before);
    try testing.expect(host.inner.effects.ptyWrittenBytes(right.pty_key).len > right_before);
    try pointerInputAdvanced(harness, iface, .pointer_up, right_point, .{ .pointer_id = 7 });
    try testing.expectEqual(@as(usize, 1), activePointerCaptureCount(&host.inner.model));
    try pointerInputAdvanced(harness, iface, .pointer_up, left_point, .{ .pointer_id = 8 });
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));

    try host.inner.effects.feedPtyOutput(left.pty_key, "\x1b[?1002l\x1b[?1003h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    const before_wheel = host.inner.effects.ptyWrittenBytes(left.pty_key).len;
    try pointerInputAdvanced(harness, iface, .scroll, left_point, .{ .delta_x = std.math.floatMax(f32), .delta_y = std.math.floatMax(f32) });
    const bounded = host.inner.effects.ptyWrittenBytes(left.pty_key).len - before_wheel;
    try testing.expect(bounded > 0 and bounded < max_effect_mouse_test_bytes);
    const after_wheel = host.inner.effects.ptyWrittenBytes(left.pty_key).len;
    try pointerInputAdvanced(harness, iface, .scroll, left_point, .{ .delta_x = std.math.nan(f32), .delta_y = std.math.inf(f32) });
    try testing.expectEqual(after_wheel, host.inner.effects.ptyWrittenBytes(left.pty_key).len);
    try testing.expect(std.math.isFinite(left.scrollback_wheel_accum));
    try testing.expect(std.math.isFinite(left.mouse_wheel_y_accum));
    try testing.expect(std.math.isFinite(left.mouse_wheel_x_accum));
}

test "secondary report gesture survives protocol disable without menu takeover" {
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
    pane.session.reset();
    try host.inner.effects.feedPtyOutput(pane.pty_key, "transition\r\n\x1b[?1002h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    const frame = terminalInteractionFrame(harness, "transition") orelse return error.TestExpectedTerminalInteractionSurface;
    const point = terminalCellPoint(pane, frame, 2, 2);
    const hit = harness.runtime.views[0].widgetLayoutTree().hitTest(point) orelse return error.TestExpectedTerminalInteractionSurface;
    const before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;

    try pointerInputAdvanced(harness, iface, .pointer_down, point, .{ .button = 1 });
    try testing.expectEqualStrings("\x1b[<2;3;3M", host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);
    try testing.expectEqual(@as(usize, 1), activePointerCaptureCount(&host.inner.model));
    try testing.expectEqual(hit.id, harness.runtime.views[0].canvas_widget_pressed_id);
    try testing.expectEqual(.ordinary, harness.runtime.views[0].canvas_widget_secondary_gesture_owner);

    try host.inner.effects.feedPtyOutput(pane.pty_key, "\x1b[?1002l");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));
    try testing.expectEqualStrings("\x1b[<2;3;3M", host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);
    try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    const rebuilt = harness.runtime.views[0].widgetLayoutTree().findById(hit.id) orelse return error.TestExpectedTerminalInteractionSurface;
    try testing.expectEqual(canvas.WidgetContextMenuPolicy.automatic, rebuilt.widget.semantics.context_menu_policy);
    try testing.expectEqual(hit.id, harness.runtime.views[0].canvas_widget_pressed_id);
    try testing.expectEqual(.ordinary, harness.runtime.views[0].canvas_widget_secondary_gesture_owner);

    try pointerInputAdvanced(harness, iface, .pointer_up, point, .{ .button = 1 });
    try testing.expectEqual(@as(canvas.ObjectId, 0), harness.runtime.views[0].canvas_widget_pressed_id);
    try testing.expectEqual(.none, harness.runtime.views[0].canvas_widget_secondary_gesture_owner);
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));
    try testing.expectEqualStrings("\x1b[<2;3;3M", host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);
    try testing.expectEqual(@as(usize, 0), harness.null_platform.context_menu_request_count);
}

test "secondary report gesture survives process exit cancel without menu takeover" {
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
    pane.session.reset();
    try host.inner.effects.feedPtyOutput(pane.pty_key, "exiting\r\n\x1b[?1002h\x1b[?1006h");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    const frame = terminalInteractionFrame(harness, "exiting") orelse return error.TestExpectedTerminalInteractionSurface;
    const point = terminalCellPoint(pane, frame, 2, 2);
    const hit = harness.runtime.views[0].widgetLayoutTree().hitTest(point) orelse return error.TestExpectedTerminalInteractionSurface;
    const before = host.inner.effects.ptyWrittenBytes(pane.pty_key).len;

    try pointerInputAdvanced(harness, iface, .pointer_down, point, .{ .button = 1 });
    try testing.expectEqualStrings("\x1b[<2;3;3M", host.inner.effects.ptyWrittenBytes(pane.pty_key)[before..]);
    try testing.expectEqual(@as(usize, 1), activePointerCaptureCount(&host.inner.model));
    try testing.expectEqual(hit.id, harness.runtime.views[0].canvas_widget_pressed_id);
    try testing.expectEqual(.ordinary, harness.runtime.views[0].canvas_widget_secondary_gesture_owner);

    try host.inner.effects.feedPtyExit(pane.pty_key, 0, 0, .spawn_failed, 0); // a spawn failure: the one end that leaves the pane standing
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try testing.expectEqual(.failed, pane.phase);
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));
    // The fake executor retires its write-capture window with the transport.
    // These counters prove exit cleanup did not attempt or drop a release.
    try testing.expectEqual(@as(u32, 0), pane.write_refusals_total);
    try testing.expectEqual(@as(u64, 0), pane.outbound_dropped);
    try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    const rebuilt = harness.runtime.views[0].widgetLayoutTree().findById(hit.id) orelse return error.TestExpectedTerminalInteractionSurface;
    try testing.expectEqual(canvas.WidgetContextMenuPolicy.automatic, rebuilt.widget.semantics.context_menu_policy);
    try testing.expectEqual(hit.id, harness.runtime.views[0].canvas_widget_pressed_id);
    try testing.expectEqual(.ordinary, harness.runtime.views[0].canvas_widget_secondary_gesture_owner);

    try pointerInputAdvanced(harness, iface, .pointer_cancel, point, .{ .button = 1 });
    try testing.expectEqual(@as(canvas.ObjectId, 0), harness.runtime.views[0].canvas_widget_pressed_id);
    try testing.expectEqual(.none, harness.runtime.views[0].canvas_widget_secondary_gesture_owner);
    try testing.expectEqual(@as(usize, 0), activePointerCaptureCount(&host.inner.model));
    try testing.expectEqual(@as(u32, 0), pane.write_refusals_total);
    try testing.expectEqual(@as(u64, 0), pane.outbound_dropped);
    try testing.expectEqual(@as(usize, 0), harness.null_platform.context_menu_request_count);
}

test "selection edge drag autoscrolls one Ghostty row per timer tick" {
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
    pane.session.reset();
    var lines: [2048]u8 = undefined;
    var writer = std.Io.Writer.fixed(&lines);
    for (0..80) |index| try writer.print("row {d}\r\n", .{index});
    try host.inner.effects.feedPtyOutput(pane.pty_key, writer.buffered());
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    const frame = app.paneFrames(&host.inner.model, size)[0];
    const start = terminalCellPoint(pane, frame, 2, 1);
    const above = geometry.PointF.init(start.x, frame.y - 8);
    try pointerInputAdvanced(harness, iface, .pointer_down, start, .{});
    try pointerInputAdvanced(harness, iface, .pointer_drag, above, .{});
    try testing.expect(host.selection_autoscroll_timer_active);
    const timer = harness.null_platform.startedTimer(app.selection_autoscroll_timer_id) orelse return error.TestExpectedTimer;
    try testing.expect(timer.active and timer.repeats);
    try testing.expectEqual(@as(u64, 15 * std.time.ns_per_ms), timer.interval_ns);
    const before = pane.session.scrollbar().offset;
    try harness.runtime.dispatchPlatformEvent(iface, harness.null_platform.fireTimer(app.selection_autoscroll_timer_id, 20 * std.time.ns_per_ms) orelse return error.TestExpectedTimer);
    try testing.expect(before > 0);
    try testing.expectEqual(before - 1, pane.session.scrollbar().offset);
    try pointerInputAdvanced(harness, iface, .pointer_up, above, .{});
    try testing.expect(!host.selection_autoscroll_timer_active);
    try testing.expect(!harness.null_platform.startedTimer(app.selection_autoscroll_timer_id).?.active);
}
