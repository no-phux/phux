const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const support = @import("support.zig");

const geometry = native_sdk.geometry;
const testing = std.testing;

pub fn startPointerHost(gpa: std.mem.Allocator, harness: anytype, size: geometry.SizeF) !*app.CockpitHost {
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;
    harness.runtime.options.shortcuts = &app.cockpit_shortcuts;
    const session = try support.createSession(80, 24);
    const host = gpa.create(app.CockpitHost) catch |err| {
        session.destroy();
        return err;
    };
    host.init(std.heap.page_allocator, app.initialModel(session), app.appOptions());
    errdefer {
        host.deinit();
        app.deinitModel(&host.inner.model);
        gpa.destroy(host);
    }
    host.inner.effects.executor = .fake;
    const app_iface = host.app();
    try harness.start(app_iface);
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = size,
        .scale_factor = 2,
        .frame_index = 1,
        .timestamp_ns = 1_000_000,
    } });
    try app.installPostPresentResources(&harness.runtime, &host.inner.model, 1);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    return host;
}

pub fn pointerInput(
    harness: anytype,
    app_iface: native_sdk.App,
    kind: native_sdk.platform.GpuSurfaceInputKind,
    point: geometry.PointF,
    button: i32,
    modifiers: native_sdk.platform.ShortcutModifiers,
    delta_y: f32,
) !void {
    try pointerInputAdvanced(harness, app_iface, kind, point, .{
        .button = button,
        .modifiers = modifiers,
        .delta_y = delta_y,
    });
}

pub const PointerInputOptions = struct {
    window_id: native_sdk.platform.WindowId = 1,
    pointer_id: u64 = 7,
    button: i32 = 0,
    modifiers: native_sdk.platform.ShortcutModifiers = .{},
    delta_x: f32 = 0,
    delta_y: f32 = 0,
    timestamp_ns: u64 = 0,
};

pub fn pointerInputAdvanced(
    harness: anytype,
    app_iface: native_sdk.App,
    kind: native_sdk.platform.GpuSurfaceInputKind,
    point: geometry.PointF,
    options: PointerInputOptions,
) !void {
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = options.window_id,
        .label = app.canvas_label,
        .kind = kind,
        .timestamp_ns = options.timestamp_ns,
        .pointer_id = options.pointer_id,
        .x = point.x,
        .y = point.y,
        .button = options.button,
        .delta_x = options.delta_x,
        .delta_y = options.delta_y,
        .modifiers = options.modifiers,
    } });
}

pub fn terminalCellPoint(pane: *const app.Pane, frame: geometry.RectF, col: usize, row: usize) geometry.PointF {
    return geometry.PointF.init(
        frame.x + (@as(f32, @floatFromInt(col)) + 0.25) * pane.session.measuredCell().?.width,
        frame.y + (@as(f32, @floatFromInt(row)) + 0.25) * pane.session.measuredCell().?.height,
    );
}

pub fn expectPointerSelectionText(pane: *app.Pane, expected: []const u8) !void {
    const selected = (try pane.session.selectionText(testing.allocator)) orelse return error.TestExpectedSelection;
    defer testing.allocator.free(selected);
    try testing.expectEqualStrings(expected, selected);
}

pub fn activePointerCaptureCount(model: *const app.Model) usize {
    var count: usize = 0;
    for (model.pointer_captures) |capture| if (capture.active) {
        count += 1;
    };
    return count;
}

pub fn activePointerCapture(model: *const app.Model, pointer_id: u64) ?app.PointerCapture {
    for (model.pointer_captures) |capture| {
        if (capture.active and capture.pointer_id == pointer_id) return capture;
    }
    return null;
}
