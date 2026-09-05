const std = @import("std");
const builtin = @import("builtin");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const support = @import("support.zig");
const measured = @import("measured.zig");

const canvas = native_sdk.canvas;
const testing = std.testing;

const createSession = support.createSession;
const activeSlots = support.activeSlots;

test "Phux Cockpit identity and macOS pane commands are exact" {
    try testing.expectEqualStrings("Phux Cockpit", app.app_name);
    try testing.expectEqualStrings("dev.phux.cockpit", app.bundle_id);
    try testing.expectEqualStrings("phux-cockpit-canvas", app.canvas_label);
    try testing.expectEqualStrings(app.app_name, app.shell_scene.windows[0].title.?);
    try testing.expectEqualStrings(app.canvas_label, app.shell_scene.windows[0].views[0].label);
    // Dormant WebKit is a post-present dynamic child, never startup-scene
    // work ahead of the first canvas flush.
    try testing.expectEqual(@as(usize, 1), app.shell_scene.windows[0].views.len);
    try testing.expect(app.shell_scene.windows[0].views[0].kind == .gpu_surface);
    try testing.expectEqualStrings(app.app_name, app.appOptions().name);
    try testing.expectEqualStrings(app.canvas_label, app.appOptions().canvas_label);

    if (comptime builtin.os.tag != .macos) return;
    try testing.expectEqualSlices([]const u8, &.{ "/bin/zsh", "-l", "-c", "cd \"$HOME\" && exec /bin/zsh -i" }, app.paneArgv(0));
    try testing.expectEqualSlices([]const u8, &.{ "/bin/zsh", "-l", "-c", "cd \"$HOME\" && exec /bin/zsh -i" }, app.paneArgv(1));
}

test "MEASURED: the Msg union stays small enough to pass by value" {
    // Every Msg is passed BY VALUE through every dispatch, so the union's size
    // is a per-message cost paid on the hot path — including by the frame pump
    // that runs on each window every frame.
    //
    // This is pinned because the obvious fix for the one-pane-per-frame resize
    // convergence is to carry a batch of (ref, cols, rows) triples in the
    // viewport arm, and a naive 16-entry array of full triples would have
    // inflated this by ~320 bytes to buy back a few frames of latency — a
    // plausible regression against the very latency it was meant to fix.
    //
    // If this number moves, that is not automatically wrong; it means measure
    // again and say why in the commit.
    // 320 bytes, measured before and after the batched-resize work — which is
    // the point: the convergence fix cost the union nothing, because the batch
    // lives in the commit path rather than in the message.
    measured.print("\nMEASURED Msg: size={d} align={d}\n", .{ @sizeOf(app.Msg), @alignOf(app.Msg) });
    try testing.expectEqual(@as(usize, 320), @sizeOf(app.Msg));
}

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
