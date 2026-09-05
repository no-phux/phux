//! Tabs own trees; trees own panes. These pin the behavior the two-pane
//! attachment model could not express, and the ONE geometry derivation that
//! the painter, the hit targets, and the PTY pump all share.

const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const support = @import("support.zig");

const geometry = native_sdk.geometry;
const testing = std.testing;

const createSession = support.createSession;
const activeSlots = support.activeSlots;
const destroyModelSessions = app.deinitModel;
const expectPaneCursorPaintKind = support.expectPaneCursorPaintKind;
const startFocusedTerminal = support.startFocusedTerminal;
const startTwoPaneCockpit = support.startTwoPaneCockpit;
const startSplitCockpit = support.startSplitCockpit;
const clickCanvas = support.clickCanvas;
const typeCanvasText = support.typeCanvasText;
const pressCanvasKey = support.pressCanvasKey;
const releaseCanvasKey = support.releaseCanvasKey;
const terminalInteractionFrame = support.terminalInteractionFrame;
const rectCenter = support.rectCenter;
const expectDisplayListMarker = support.expectDisplayListMarker;

const surface = geometry.SizeF.init(980, 640);

test "a fresh window is one tab of one pane filling the content area" {
    const session = try createSession(80, 24);
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
    try testing.expect(model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));

    var panes: [app.max_panes_per_tab]app.LayoutPane = undefined;
    const count = app.resolvePanes(&model, surface, &panes);
    try testing.expectEqual(@as(usize, 1), count);
    const content = app.workspaceChrome(&model, surface).content;
    try testing.expectEqualDeep(content, panes[0].rect);

    // The web surface takes the content area away from every pane.
    model.selectWeb();
    try testing.expectEqual(@as(usize, 0), app.resolvePanes(&model, surface, &panes));
}

test "Cmd+D on a fresh window creates a NEW shell beside the first" {
    // The owner's loudest complaint: the old toggle only flipped a flag and
    // dragged an existing tab into pane two, and it needed two tabs first.
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try testing.expectEqual(@as(usize, 1), app_state.model.ws().tab_count);
    try testing.expectEqual(@as(usize, 1), app_state.model.provider.activeCount());
    const first_session = app_state.model.provider.slots[0].session;

    try pressCanvasKey(harness, app_iface, "d", .{ .primary = true });
    try releaseCanvasKey(harness, app_iface, "d", .{});

    // A second SHELL exists, in the SAME tab, and focus moved into it.
    try testing.expectEqual(@as(usize, 1), app_state.model.ws().tab_count);
    try testing.expectEqual(@as(usize, 2), app_state.model.ws().tabs[0].paneCount());
    try testing.expectEqual(@as(usize, 2), app_state.model.provider.activeCount());
    try testing.expectEqual(@as(usize, 2), app_state.effects.pendingPtyCount());
    try testing.expect(app_state.model.provider.slots[0].session == first_session);
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));

    var panes: [app.max_panes_per_tab]app.LayoutPane = undefined;
    const count = app.resolvePanes(&app_state.model, surface, &panes);
    try testing.expectEqual(@as(usize, 2), count);
    try testing.expect(panes[0].rect.x < panes[1].rect.x);
    try testing.expectApproxEqAbs(panes[0].rect.height, panes[1].rect.height, 0.001);

    // Cmd+Shift+D divides the focused pane the other way.
    try pressCanvasKey(harness, app_iface, "d", .{ .primary = true, .shift = true });
    try releaseCanvasKey(harness, app_iface, "d", .{});
    try testing.expectEqual(@as(usize, 3), app_state.model.ws().tabs[0].paneCount());
    const nested = app.resolvePanes(&app_state.model, surface, &panes);
    try testing.expectEqual(@as(usize, 3), nested);
    // The original pane still spans the full height; the right column stacks.
    try testing.expectApproxEqAbs(app.workspaceChrome(&app_state.model, surface).content.height, panes[0].rect.height, 0.001);
    try testing.expect(panes[1].rect.y < panes[2].rect.y);
}

test "the painter, the hit targets, and the PTY pump agree on one set of rects" {
    // An audit found a 294pt zone where text painted with no hit target
    // behind it, because three code paths derived rects independently.
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startSplitCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Nest a vertical split under the horizontal one, so the check spans
    // both orientations.
    try app_state.dispatch(&harness.runtime, 1, .split_down);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    var panes: [app.max_panes_per_tab]app.LayoutPane = undefined;
    const count = app.resolvePanes(&app_state.model, surface, &panes);
    try testing.expectEqual(@as(usize, 3), count);

    for (panes[0..count]) |pane| {
        const local = app.LocalTerminalId;
        _ = local;
        const widget_id = native_sdk.canvas.globalWidgetId(.terminal, .{
            .index = @intCast(@intFromEnum(app.localId(pane.terminal).?)),
        });
        var found: ?geometry.RectF = null;
        for (harness.runtime.views[0].widgetLayoutTree().nodes) |node| {
            if (node.widget.id == widget_id) found = node.frame;
        }
        const laid_out = found orelse return error.TestExpectedTerminalInteractionSurface;
        try testing.expectApproxEqAbs(pane.rect.x, laid_out.x, 0.25);
        try testing.expectApproxEqAbs(pane.rect.y, laid_out.y, 0.25);
        try testing.expectApproxEqAbs(pane.rect.width, laid_out.width, 0.25);
        try testing.expectApproxEqAbs(pane.rect.height, laid_out.height, 0.25);

        // And the hit test resolves the same rect back to the same pane.
        const centre = rectCenter(pane.rect);
        const hit = app.paneAtPoint(&app_state.model, surface, centre.x, centre.y) orelse
            return error.TestExpectedPaneHit;
        try testing.expect(hit.terminal.eql(pane.terminal));
    }

    // The PTY pump sizes each pane against the very same rect.
    for (2..10) |frame_index| try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = surface,
        .scale_factor = 2,
        .frame_index = @intCast(frame_index),
        .timestamp_ns = @as(u64, frame_index) * 1_000_000,
    } });
    for (panes[0..count]) |pane| {
        const terminal = app_state.model.provider.terminal(pane.terminal) orelse return error.TestExpectedTerminal;
        const expected = grid.Session.clampGrid(
            @intFromFloat(@max(2, pane.rect.width / terminal.session.measuredCell().?.width)),
            @intFromFloat(@max(2, pane.rect.height / terminal.session.measuredCell().?.height)),
        );
        try testing.expectEqual(expected.x, terminal.cols);
        try testing.expectEqual(expected.y, terminal.rows);
    }
}

test "each split pane paints inside its own rect and keeps its own live session" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startSplitCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    var panes: [app.max_panes_per_tab]app.LayoutPane = undefined;
    const count = app.resolvePanes(&app_state.model, surface, &panes);
    try testing.expectEqual(@as(usize, 2), count);
    try testing.expect(panes[0].rect.width >= app.split_pane_min_width);
    try testing.expect(panes[1].rect.width >= app.split_pane_min_width);
    try testing.expectApproxEqAbs(
        surface.width - 2 * 8,
        panes[0].rect.width + app.split_divider_width + panes[1].rect.width,
        0.001,
    );
    try expectDisplayListMarker(harness.runtime.views[0].canvasDisplayList(), "PANEALPHA", panes[0].rect);
    try expectDisplayListMarker(harness.runtime.views[0].canvasDisplayList(), "PANEBRAVO", panes[1].rect);
    // The focused pane owns the filled caret; the other renders it hollow.
    try expectPaneCursorPaintKind(harness.runtime.views[0].canvasDisplayList(), 0, .hollow);
    try expectPaneCursorPaintKind(harness.runtime.views[0].canvasDisplayList(), 1, .filled);

    // A click moves pane focus, and typing follows it.
    const left_target = rectCenter(panes[0].rect);
    try clickCanvas(harness, app_iface, left_target.x, left_target.y);
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));
    try typeCanvasText(harness, app_iface, "left");
    try testing.expectEqualStrings("left", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));
}

test "directional focus and pane cycling move between panes, not tabs" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startSplitCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Focus is in the pane the split made: the right one.
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    try pressCanvasKey(harness, app_iface, "arrowleft", .{ .primary = true, .option = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));
    try releaseCanvasKey(harness, app_iface, "arrowleft", .{});
    try pressCanvasKey(harness, app_iface, "arrowright", .{ .primary = true, .option = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    try releaseCanvasKey(harness, app_iface, "arrowright", .{});
    // Nothing lies above the right pane of a left/right split.
    try pressCanvasKey(harness, app_iface, "arrowup", .{ .primary = true, .option = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    try releaseCanvasKey(harness, app_iface, "arrowup", .{});

    // Cmd+[ / Cmd+] cycle panes; the tab count never moves.
    try pressCanvasKey(harness, app_iface, "[", .{ .primary = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));
    try testing.expectEqual(@as(usize, 1), app_state.model.ws().tab_count);
    try releaseCanvasKey(harness, app_iface, "[", .{});
    try pressCanvasKey(harness, app_iface, "]", .{ .primary = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    try releaseCanvasKey(harness, app_iface, "]", .{});
}

test "Cmd+W closes the focused pane, then the tab, then the window" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startSplitCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try testing.expectEqual(@as(usize, 2), app_state.model.provider.activeCount());
    // Closing the focused pane promotes the sibling into the whole area and
    // FREES the emulator — it does not sit there as a tombstone.
    try pressCanvasKey(harness, app_iface, "w", .{ .primary = true });
    try releaseCanvasKey(harness, app_iface, "w", .{});
    try testing.expectEqual(@as(usize, 1), app_state.model.ws().tab_count);
    try testing.expectEqual(@as(usize, 1), app_state.model.ws().tabs[0].paneCount());
    try testing.expectEqual(@as(usize, 1), app_state.model.provider.activeCount());
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));
    var panes: [app.max_panes_per_tab]app.LayoutPane = undefined;
    try testing.expectEqual(@as(usize, 1), app.resolvePanes(&app_state.model, surface, &panes));
    try testing.expectEqualDeep(app.workspaceChrome(&app_state.model, surface).content, panes[0].rect);

    // Closing the last pane closes its tab, and the last tab closes the
    // window through the platform's own verb.
    try testing.expectEqual(@as(u32, 0), app_state.effects.windowActionState().close_count);
    try pressCanvasKey(harness, app_iface, "w", .{ .primary = true });
    try testing.expectEqual(@as(usize, 0), app_state.model.ws().tab_count);
    try testing.expectEqual(@as(usize, 0), app_state.model.provider.activeCount());
    try testing.expectEqual(@as(u32, 1), app_state.effects.windowActionState().close_count);
    try testing.expectEqualStrings(app.main_window_label, app_state.effects.windowActionState().lastLabel());
    // Closing the window is not enough to END the app: AppKit keeps a process
    // alive after its last window unless the delegate opts in, so closing the
    // final tab used to leave a running cockpit with no window and no way back
    // to it. Quit has to be asked for explicitly.
    try testing.expectEqual(@as(u32, 1), app_state.effects.windowActionState().quit_count);
}

test "any shell exit closes its pane; only a failed spawn keeps it for Restart" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startSplitCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Typing `exit` in the right pane. It must not leave a zombie grid
    // behind an "EXIT 0" badge.
    try app_state.effects.feedPtyExit(app.ptyKey(1), 0, 0, .exited, 0);
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expectEqual(@as(usize, 1), app_state.model.ws().tabs[0].paneCount());
    try testing.expectEqual(@as(usize, 1), app_state.model.provider.activeCount());
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));
}

// GUARD: ended-pane-closes-at-any-status
test "a non-zero exit closes its pane too, and the sibling reclaims the rect" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startSplitCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // THE regression this pins. `exit` in a shell inherits the last command's
    // status, so an ordinary session ended after an ordinary failed command
    // reports a non-zero code — and that used to leave a dead grid holding a
    // full pane rect behind an `EXIT 1` badge, permanently, with its sibling
    // never getting the space back. A shell that ENDED is done at any status.
    var panes: [app.max_panes_per_tab]app.LayoutPane = undefined;
    try testing.expectEqual(@as(usize, 2), app.resolvePanes(&app_state.model, surface, &panes));
    const before_width = panes[0].rect.width;

    try app_state.effects.feedPtyExit(app.ptyKey(1), 1, 0, .exited, 0);
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expectEqual(@as(usize, 1), app_state.model.ws().tabs[0].paneCount());
    try testing.expectEqual(@as(usize, 1), app_state.model.provider.activeCount());

    // The survivor does not merely persist, it GROWS: a promotion that left
    // the sibling on its old half is the same hole with one fewer occupant.
    try testing.expectEqual(@as(usize, 1), app.resolvePanes(&app_state.model, surface, &panes));
    try testing.expect(panes[0].rect.width > before_width);

    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    for (harness.runtime.views[0].widgetLayoutTree().nodes) |node| {
        try testing.expect(!std.mem.eql(u8, node.widget.text, "Restart"));
    }
}

test "the last clean exit in the last tab closes the window" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try app_state.effects.feedPtyExit(app.ptyKey(0), 0, 0, .exited, 0);
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expectEqual(@as(usize, 0), app_state.model.ws().tab_count);
    try testing.expectEqual(@as(u32, 1), app_state.effects.windowActionState().close_count);
    try testing.expectEqualStrings(app.main_window_label, app_state.effects.windowActionState().lastLabel());
    // A shell that ends on its own has to end the app the same way cmd+W
    // does — otherwise typing `exit` in the last terminal leaves a headless
    // process behind.
    try testing.expectEqual(@as(u32, 1), app_state.effects.windowActionState().quit_count);
}

test "split divider drag and keyboard resize stay in lockstep with the painted rects" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startSplitCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    const branch = app_state.model.ws().tabs[0].root;
    var divider: ?geometry.RectF = null;
    for (harness.runtime.views[0].widgetLayoutTree().nodes) |node| {
        if (node.widget.kind == .split_divider) divider = node.frame;
    }
    const target = rectCenter(divider orelse return error.TestExpectedSplitDivider);
    try clickCanvas(harness, app_iface, target.x, target.y);
    const before = app_state.model.ws().tabs[0].node(branch).fraction;
    try pressCanvasKey(harness, app_iface, "arrowright", .{});
    try testing.expect(app_state.model.ws().tabs[0].node(branch).fraction > before);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    divider = null;
    for (harness.runtime.views[0].widgetLayoutTree().nodes) |node| {
        if (node.widget.kind == .split_divider) divider = node.frame;
    }
    const drag_start = rectCenter(divider orelse return error.TestExpectedSplitDivider);
    const keyboard_fraction = app_state.model.ws().tabs[0].node(branch).fraction;
    for ([_]native_sdk.platform.GpuSurfaceInputKind{ .pointer_down, .pointer_drag, .pointer_up }, [_]f32{ 0, 60, 60 }) |kind, dx| {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = kind,
            .x = drag_start.x + dx,
            .y = drag_start.y,
        } });
    }
    try testing.expect(app_state.model.ws().tabs[0].node(branch).fraction > keyboard_fraction);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    // The painter's rects and the laid-out interaction surfaces still match.
    var panes: [app.max_panes_per_tab]app.LayoutPane = undefined;
    const count = app.resolvePanes(&app_state.model, surface, &panes);
    for (panes[0..count], [_][]const u8{ "PANEALPHA", "PANEBRAVO" }) |pane, marker| {
        const laid_out = terminalInteractionFrame(harness, marker) orelse return error.TestExpectedTerminalInteractionSurface;
        try testing.expectApproxEqAbs(pane.rect.x, laid_out.x, 0.25);
        try testing.expectApproxEqAbs(pane.rect.width, laid_out.width, 0.25);
    }
}
test "vertical divider drag keyboard accessibility and child rectangles share resolved geometry" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try app_state.dispatch(&harness.runtime, 1, .split_down);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    const branch = app_state.model.ws().tabs[0].root;

    var divider: ?native_sdk.canvas.WidgetLayoutNode = null;
    for (harness.runtime.views[0].widgetLayoutTree().nodes) |node| {
        if (node.widget.kind == .split_divider) divider = node;
    }
    const initial_divider = divider orelse return error.TestExpectedSplitDivider;
    try testing.expectEqual(native_sdk.canvas.SplitAxis.vertical, initial_divider.widget.runtime_flags.split_axis);
    const content = app.workspaceChrome(&app_state.model, surface).content;
    try testing.expectApproxEqAbs(content.x, initial_divider.frame.x, 0.25);
    try testing.expectApproxEqAbs(content.width, initial_divider.frame.width, 0.25);
    try testing.expectApproxEqAbs(app.split_divider_width, initial_divider.frame.height, 0.25);

    const target = rectCenter(initial_divider.frame);
    try clickCanvas(harness, app_iface, target.x, target.y);
    try testing.expectEqual(initial_divider.widget.id, harness.runtime.views[0].canvas_widget_focused_id);
    const before_key = app_state.model.ws().tabs[0].node(branch).fraction;
    try pressCanvasKey(harness, app_iface, "arrowdown", .{});
    try testing.expect(app_state.model.ws().tabs[0].node(branch).fraction > before_key);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    divider = null;
    for (harness.runtime.views[0].widgetLayoutTree().nodes) |node| {
        if (node.widget.kind == .split_divider) divider = node;
    }
    const drag_start = rectCenter((divider orelse return error.TestExpectedSplitDivider).frame);
    const before_drag = app_state.model.ws().tabs[0].node(branch).fraction;
    for ([_]native_sdk.platform.GpuSurfaceInputKind{ .pointer_down, .pointer_drag, .pointer_up }, [_]f32{ 0, 60, 60 }) |kind, dy| {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = kind,
            .x = drag_start.x,
            .y = drag_start.y + dy,
        } });
    }
    try testing.expect(app_state.model.ws().tabs[0].node(branch).fraction > before_drag);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    const before_accessibility = app_state.model.ws().tabs[0].node(branch).fraction;
    _ = try harness.runtime.dispatchCanvasWidgetAccessibilityAction(app_iface, 1, app.canvas_label, .{
        .id = initial_divider.widget.id,
        .action = .decrement,
    });
    try testing.expect(app_state.model.ws().tabs[0].node(branch).fraction < before_accessibility);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    const snapshot = harness.runtime.automationSnapshot("Vertical split");
    var saw_adjustable_separator = false;
    for (snapshot.widgets) |widget| {
        if (widget.id != initial_divider.widget.id) continue;
        try testing.expectEqualStrings("separator", widget.role);
        try testing.expect(widget.actions.focus);
        try testing.expect(widget.actions.increment);
        try testing.expect(widget.actions.decrement);
        try testing.expectApproxEqAbs(app_state.model.ws().tabs[0].node(branch).fraction, widget.value.?, 0.0001);
        saw_adjustable_separator = true;
    }
    try testing.expect(saw_adjustable_separator);

    // Painter/input leaves are built from the same `layout.resolve()` result.
    var panes: [app.max_panes_per_tab]app.LayoutPane = undefined;
    const count = app.resolvePanes(&app_state.model, surface, &panes);
    try testing.expectEqual(@as(usize, 2), count);
    for (panes[0..count]) |pane| {
        const widget_id = native_sdk.canvas.globalWidgetId(.terminal, .{
            .index = @intCast(@intFromEnum(app.localId(pane.terminal).?)),
        });
        var laid_out: ?geometry.RectF = null;
        for (harness.runtime.views[0].widgetLayoutTree().nodes) |node| {
            if (node.widget.id == widget_id) laid_out = node.frame;
        }
        const rect = laid_out orelse return error.TestExpectedTerminalInteractionSurface;
        try testing.expectApproxEqAbs(pane.rect.x, rect.x, 0.25);
        try testing.expectApproxEqAbs(pane.rect.y, rect.y, 0.25);
        try testing.expectApproxEqAbs(pane.rect.width, rect.width, 0.25);
        try testing.expectApproxEqAbs(pane.rect.height, rect.height, 0.25);
    }
}

test "split PTY grids preserve each pane's full bounded viewport" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startSplitCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    const large = geometry.SizeF.init(4000, 2400);
    for (2..8) |frame_index| try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = large,
        .scale_factor = 2,
        .frame_index = @intCast(frame_index),
        .timestamp_ns = @as(u64, frame_index) * 1_000_000,
    } });

    for (activeSlots(&app_state.model)) |pane| {
        const cells = @as(usize, pane.cols) * @as(usize, pane.rows);
        try testing.expect(cells <= grid.max_cells);
        try testing.expectEqual(@as(u16, grid.max_rows), pane.rows);
    }
}

test "sub-cell frame changes still update surface geometry" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startTwoPaneCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    for (2..4) |frame_index| try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = surface,
        .scale_factor = 2,
        .frame_index = @intCast(frame_index),
        .timestamp_ns = @as(u64, frame_index) * 1_000_000,
    } });
    const cols = app_state.model.provider.slots[0].cols;
    const rows = app_state.model.provider.slots[0].rows;
    const changed = geometry.SizeF.init(surface.width + 0.25, surface.height + 0.25);
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = changed,
        .scale_factor = 2,
        .frame_index = 4,
        .timestamp_ns = 4_000_000,
    } });

    try testing.expectEqual(changed.width, app_state.model.ws().surface_size.width);
    try testing.expectEqual(changed.height, app_state.model.ws().surface_size.height);
    try testing.expectEqual(cols, app_state.model.provider.slots[0].cols);
    try testing.expectEqual(rows, app_state.model.provider.slots[0].rows);
}

test "tab cycling walks terminal tabs only and sends no terminal bytes" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startTwoPaneCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Cycling walks the TERMINAL tabs and wraps within them. The web surface
    // is no longer a station on that ring — it has its own chord — so two
    // forward steps over two tabs land back where they started.
    try pressCanvasKey(harness, app_iface, "]", .{ .primary = true, .shift = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    try releaseCanvasKey(harness, app_iface, "]", .{});
    try pressCanvasKey(harness, app_iface, "]", .{ .primary = true, .shift = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));
    try releaseCanvasKey(harness, app_iface, "]", .{});
    try pressCanvasKey(harness, app_iface, "[", .{ .primary = true, .shift = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));

    // Cycling OUT of the web surface re-enters the tab list where it was,
    // rather than stranding the user on a surface the strip does not show.
    try releaseCanvasKey(harness, app_iface, "[", .{});
    app_state.model.selectWeb();
    try pressCanvasKey(harness, app_iface, "]", .{ .primary = true, .shift = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));
}

test "extra modifiers bypass exact spatial shortcuts and reach the terminal" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startTwoPaneCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try pressCanvasKey(harness, app_iface, "]", .{ .primary = true, .shift = true, .control = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));
    const after_bracket = app_state.effects.ptyWrittenBytes(app.ptyKey(0)).len;
    try testing.expect(after_bracket > 0);

    try pressCanvasKey(harness, app_iface, "arrowright", .{ .primary = true, .option = true, .shift = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));
    try testing.expect(app_state.effects.ptyWrittenBytes(app.ptyKey(0)).len > after_bracket);
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));
}

test "spatial shortcut releases never leak into kitty-reporting terminals" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;
    harness.runtime.options.shortcuts = &app.cockpit_shortcuts;
    const session = try createSession(80, 24);
    const host = try gpa.create(app.CockpitHost);
    defer gpa.destroy(host);
    host.init(std.heap.page_allocator, app.initialModel(session), app.appOptions());
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    host.inner.effects.executor = .fake;
    const app_iface = host.app();
    try harness.start(app_iface);
    try host.inner.dispatch(&harness.runtime, 1, .new_terminal);
    for (activeSlots(&host.inner.model)) |*pane| pane.session.feed("\x1b[>11u");

    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .shortcut = .{
        .id = "pane.split-right",
        .key = "d",
        .window_id = 1,
        .modifiers = .{ .primary = true },
    } });
    try testing.expectEqual(@as(usize, 2), host.inner.model.ws().tabs[host.inner.model.ws().selected_tab].paneCount());
    // The duplicate canvas edge for the same physical press is consumed.
    try pressCanvasKey(harness, app_iface, "d", .{ .primary = true });
    try testing.expectEqual(@as(usize, 2), host.inner.model.ws().tabs[host.inner.model.ws().selected_tab].paneCount());
    try releaseCanvasKey(harness, app_iface, "d", .{});

    const cases = [_]struct {
        id: []const u8,
        key: []const u8,
        modifiers: native_sdk.platform.ShortcutModifiers,
    }{
        .{ .id = "tab.next", .key = "]", .modifiers = .{ .primary = true, .shift = true } },
        .{ .id = "tab.previous", .key = "[", .modifiers = .{ .primary = true, .shift = true } },
    };
    for (cases) |case| {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .shortcut = .{
            .id = case.id,
            .key = case.key,
            .window_id = 1,
            .modifiers = case.modifiers,
        } });
        try releaseCanvasKey(harness, app_iface, case.key, .{});
    }
    try testing.expectEqualStrings("", host.inner.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqualStrings("", host.inner.effects.ptyWrittenBytes(app.ptyKey(1)));
}

test "repeated global shortcut callbacks are idempotent per physical edge" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;
    harness.runtime.options.shortcuts = &app.cockpit_shortcuts;
    const session = try createSession(80, 24);
    const host = try gpa.create(app.CockpitHost);
    defer gpa.destroy(host);
    host.init(std.heap.page_allocator, app.initialModel(session), app.appOptions());
    defer destroyModelSessions(&host.inner.model);
    defer host.deinit();
    host.inner.effects.executor = .fake;
    const app_iface = host.app();
    try harness.start(app_iface);

    const shortcut: native_sdk.ShortcutEvent = .{
        .id = "pane.split-right",
        .key = "d",
        .window_id = 1,
        .modifiers = .{ .primary = true },
    };
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .shortcut = shortcut });
    try testing.expectEqual(@as(usize, 2), host.inner.model.ws().tabs[0].paneCount());
    try testing.expect(host.global_shortcut_keys_held != 0);
    // A repeated callback for the SAME physical edge must not split again.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .shortcut = shortcut });
    try testing.expectEqual(@as(usize, 2), host.inner.model.ws().tabs[0].paneCount());

    try releaseCanvasKey(harness, app_iface, "d", .{});
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .shortcut = shortcut });
    try testing.expectEqual(@as(usize, 3), host.inner.model.ws().tabs[0].paneCount());

    for (activeSlots(&host.inner.model)) |*pane| pane.session.feed("\x1b[>11u");
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .shortcut = .{
        .id = "surface.1",
        .key = "1",
        .window_id = 1,
        .modifiers = .{ .primary = true },
    } });
    try releaseCanvasKey(harness, app_iface, "1", .{});
    try testing.expectEqualStrings("", host.inner.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqualStrings("", host.inner.effects.ptyWrittenBytes(app.ptyKey(1)));
}

test "Cmd+1 and Cmd+2 select TABS and route text only to that tab's focused pane" {
    // THE ACCEPTANCE GATE for the whole spike: mis-routed input shows
    // up as bytes on the wrong pty, not as a subtle rendering difference.
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = surface });
    defer harness.destroy(gpa);
    const app_state = try startTwoPaneCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try testing.expectEqual(@as(usize, 0), app_state.model.ws().selected_tab);
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));
    try typeCanvasText(harness, app_iface, "alpha");
    try testing.expectEqualStrings("alpha", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));

    try pressCanvasKey(harness, app_iface, "2", .{ .primary = true });
    try testing.expectEqual(@as(usize, 1), app_state.model.ws().selected_tab);
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    try typeCanvasText(harness, app_iface, "bravo");
    try testing.expectEqualStrings("bravo", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));
    try testing.expectEqualStrings("alpha", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));

    try pressCanvasKey(harness, app_iface, "1", .{ .primary = true });
    try testing.expectEqual(@as(usize, 0), app_state.model.ws().selected_tab);
    try typeCanvasText(harness, app_iface, "!");
    try testing.expectEqualStrings("alpha!", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqualStrings("bravo", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));
}

test "the tab band is at least as tall as the triggers it hosts" {
    // At 40pt the strip overflowed the band and painted its hairline and
    // underline indicator into the terminal's first row.
    const session = try createSession(80, 24);
    var model = app.initialModel(session);
    defer app.deinitModel(&model);
    try testing.expect(app.header_height >= app.tabTriggerHeight(&model));
}
