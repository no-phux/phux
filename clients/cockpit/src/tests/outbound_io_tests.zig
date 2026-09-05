const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const support = @import("support.zig");

const geometry = native_sdk.geometry;
const testing = std.testing;
const automation = native_sdk.automation;

const destroyModelSessions = app.deinitModel;
const startFocusedTerminal = support.startFocusedTerminal;
const typeCanvasText = support.typeCanvasText;

test "stdin order holds: a retained reply reaches the child before newer typing" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    const session = app_state.model.provider.slots[0].session;

    // A DSR reply is generated while the ring is full: retained.
    session.feed("\x1b[6n");
    const reply = try gpa.dupe(u8, session.pendingResponses());
    defer gpa.free(reply);
    app_state.effects.fake_pty_write_full = true;
    app_state.model.provider.slots[0].outbound_len = app_state.model.provider.slots[0].outbound_buffer.len;
    app.moveResponsesToOutbound(&app_state.model.provider.slots[0], &app_state.effects);
    try testing.expectEqual(reply.len, session.pendingResponses().len);

    // Typing while the reply is stuck must not jump the stdin queue:
    // the keystroke drops counted, the reply stays first in line.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .text = "y",
    } });
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(1));
    try testing.expectEqual(@as(u64, 1), app_state.model.provider.slots[0].outbound_dropped);
    try testing.expectEqual(reply.len, session.pendingResponses().len);

    // The ring frees (the child read): the next keystroke moves the
    // retained reply FIRST, then itself — the child's stdin order.
    app_state.effects.fake_pty_write_full = false;
    app_state.model.provider.slots[0].outbound_len = 0;
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .text = "x",
    } });
    const written = app_state.effects.ptyWrittenBytes(1);
    try testing.expectEqual(reply.len + 1, written.len);
    try testing.expect(std.mem.startsWith(u8, written, reply));
    try testing.expect(std.mem.endsWith(u8, written, "x"));
    try testing.expectEqual(@as(usize, 0), session.pendingResponses().len);
}

test "retained replies keep accumulating while further output feeds - the buffer grows" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const session = app_state.model.provider.slots[0].session;

    // The outbound ring is full and the child keeps pipelining DSR
    // queries — more reply bytes than the buffer's initial capacity.
    // Every reply must accumulate (the buffer grows), none dropped:
    // clearing or dropping would strand a child blocked on an answer.
    app_state.effects.fake_pty_write_full = true;
    app_state.model.provider.slots[0].outbound_len = app_state.model.provider.slots[0].outbound_buffer.len;
    const burst = "\x1b[6n" ** 6000; // ~36 KiB of replies, > 16 KiB initial
    session.feed(burst);
    app.moveResponsesToOutbound(&app_state.model.provider.slots[0], &app_state.effects);
    try testing.expectEqual(@as(u64, 0), session.response_bytes_dropped);
    try testing.expect(session.pendingResponses().len > grid.Session.response_capacity);

    // The ring drains; the whole accumulated batch moves and clears.
    app_state.effects.fake_pty_write_full = false;
    app_state.model.provider.slots[0].outbound_len = 0;
    app.moveResponsesToOutbound(&app_state.model.provider.slots[0], &app_state.effects);
    try testing.expectEqual(@as(usize, 0), session.pendingResponses().len);
    try testing.expectEqual(@as(u64, 0), app_state.model.provider.slots[0].outbound_dropped);
}

test "a query reply refused by a full ring is retained and retried, never cleared" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();

    // The child pipelines a DSR query; its reply waits in the
    // emulator's buffer. With the pending ring full RIGHT NOW, the move
    // must leave the reply IN PLACE — clearing it would strand a child
    // blocked on the answer.
    app_state.model.provider.slots[0].session.feed("\x1b[6n");
    const reply_len = app_state.model.provider.slots[0].session.pendingResponses().len;
    try testing.expect(reply_len > 0);
    app_state.effects.fake_pty_write_full = true;
    app_state.model.provider.slots[0].outbound_len = app_state.model.provider.slots[0].outbound_buffer.len;
    app.moveResponsesToOutbound(&app_state.model.provider.slots[0], &app_state.effects);
    try testing.expectEqual(reply_len, app_state.model.provider.slots[0].session.pendingResponses().len);
    try testing.expectEqual(@as(u64, 0), app_state.model.provider.slots[0].outbound_dropped);

    // The ring drains (the child read); the retry moves the reply whole
    // and it reaches the pty.
    app_state.effects.fake_pty_write_full = false;
    app_state.model.provider.slots[0].outbound_len = 0;
    app.moveResponsesToOutbound(&app_state.model.provider.slots[0], &app_state.effects);
    try testing.expectEqual(@as(usize, 0), app_state.model.provider.slots[0].session.pendingResponses().len);
    const written = app_state.effects.ptyWrittenBytes(1);
    try testing.expectEqual(reply_len, written.len);
    try testing.expect(std.mem.startsWith(u8, written, "\x1b["));
}

test "session exit counts retained reply bytes as loss, never silent" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // A DSR reply waits retained when the child dies: those bytes can
    // never land, so they count as outbound loss — a zero tally over
    // vanished bytes would misreport the session as lossless.
    app_state.model.provider.slots[0].session.feed("\x1b[6n");
    const reply_len = app_state.model.provider.slots[0].session.pendingResponses().len;
    try testing.expect(reply_len > 0);
    try app_state.effects.feedPtyExit(1, 0, 0, .spawn_failed, 0); // a spawn failure: the one end that leaves the pane standing
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expectEqual(app.Phase.failed, app_state.model.provider.slots[0].phase);
    try testing.expectEqual(@as(u64, reply_len), app_state.model.provider.slots[0].outbound_dropped);
    try testing.expectEqual(@as(usize, 0), app_state.model.provider.slots[0].session.pendingResponses().len);
}

test "a payload the outbound ring cannot hold whole is dropped whole, never torn" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // One committed payload larger than the whole pending ring: a
    // prefix cut at the ring edge could tear an escape sequence, so
    // admission is all-or-nothing — dropped whole and counted, nothing
    // queued, nothing written.
    const oversized = try gpa.alloc(u8, app_state.model.provider.slots[0].outbound_buffer.len + 1);
    defer gpa.free(oversized);
    @memset(oversized, 'z');
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .text = oversized,
    } });
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(1));
    try testing.expectEqual(@as(u64, oversized.len), app_state.model.provider.slots[0].outbound_dropped);

    // The stream is intact past the drop: the next keystroke flows.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .text = "ok",
    } });
    try testing.expectEqualStrings("ok", app_state.effects.ptyWrittenBytes(1));
}
