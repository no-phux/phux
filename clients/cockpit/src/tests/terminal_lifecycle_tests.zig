const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const local = @import("../providers/local/provider.zig");
const support = @import("support.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;
const automation = native_sdk.automation;

const destroyModelSessions = app.deinitModel;
const startFocusedTerminal = support.startFocusedTerminal;
const startTwoPaneCockpit = support.startTwoPaneCockpit;
const clickCanvas = support.clickCanvas;
const pressCanvasKey = support.pressCanvasKey;
const releaseCanvasKey = support.releaseCanvasKey;
const widgetFrameBySemantics = support.widgetFrameBySemantics;
const rectCenter = support.rectCenter;

test "restart during starting is a no-op - the original session is not duplicated" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Still .starting (no output yet), one live pty. Cmd+R must not
    // respawn onto the occupied key.
    try testing.expectEqual(app.Phase.starting, app_state.model.provider.slots[0].phase);
    try testing.expectEqual(@as(usize, 1), app_state.effects.pendingPtyCount());
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "r",
        .modifiers = .{ .primary = true },
    } });
    try testing.expectEqual(app.Phase.starting, app_state.model.provider.slots[0].phase);
    try testing.expectEqual(@as(usize, 1), app_state.effects.pendingPtyCount());
}

test "restart resets every per-session counter and exit field" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // The first session ends with transport drops on record: the exit
    // carries them into the model, where the status tally renders them.
    //
    // A SPAWN failure, because that is the only end that leaves a pane
    // standing for a Restart to target — a shell that ran and ended closes its
    // pane at any status, and a closed pane has no counters left to reset.
    try app_state.effects.feedPtyOutput(1, "demo$ ");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try app_state.effects.feedPtyExit(1, 0, 0, .spawn_failed, 3);
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expectEqual(app.Phase.failed, app_state.model.provider.slots[0].phase);
    try testing.expectEqual(@as(u32, 3), app_state.model.provider.slots[0].native_delivery_failures);
    const pane = &app_state.model.provider.slots[0];
    const previous_generation = pane.session_generation;
    pane.selecting = true;
    pane.copied_bytes = 12;
    pane.copy_failed = true;
    pane.macos_natural_keys_held = 7;
    pane.scrollback_wheel_accum = 4.5;
    pane.mouse_wheel_y_accum = 3.5;
    pane.mouse_wheel_x_accum = 2.5;
    pane.outbound_head = 9;
    pane.outbound_len = 11;
    pane.outbound_dropped = 13;
    pane.session.response_bytes_dropped = 2;
    app_state.model.paste_owner = app_state.model.provider.owner(pane.id) orelse return error.TestExpectedTerminalOwner;
    app_state.model.paste_failed = true;
    try testing.expect(pane.output_batches > 0);
    try testing.expect(pane.output_bytes > 0);
    try testing.expectEqual(@as(i32, -1), pane.exit_code);
    try testing.expectEqual(@as(i32, 0), pane.exit_signal);

    // Cmd+R: the new shell's tally is its own — zero, not the dead
    // session's drops.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "r",
        .modifiers = .{ .primary = true },
    } });
    try testing.expectEqual(app.Phase.starting, pane.phase);
    try testing.expectEqual(@as(i32, 0), pane.exit_code);
    try testing.expectEqual(@as(i32, 0), pane.exit_signal);
    try testing.expectEqual(native_sdk.EffectExitReason.exited, pane.exit_reason);
    try testing.expect(!pane.selecting);
    try testing.expectEqual(@as(u64, 0), pane.copied_bytes);
    try testing.expect(!pane.copy_failed);
    try testing.expectEqual(@as(u8, 0), pane.macos_natural_keys_held);
    try testing.expectEqual(@as(f32, 0), pane.scrollback_wheel_accum);
    try testing.expectEqual(@as(f32, 0), pane.mouse_wheel_y_accum);
    try testing.expectEqual(@as(f32, 0), pane.mouse_wheel_x_accum);
    try testing.expectEqual(@as(u64, 0), pane.output_batches);
    try testing.expectEqual(@as(u64, 0), pane.output_bytes);
    try testing.expectEqual(@as(u32, 0), pane.write_refusals);
    try testing.expectEqual(@as(u32, 0), pane.write_refusals_total);
    try testing.expectEqual(@as(u32, 0), pane.native_delivery_failures);
    try testing.expectEqual(@as(usize, 0), pane.outbound_head);
    try testing.expectEqual(@as(usize, 0), pane.outbound_len);
    try testing.expectEqual(@as(u64, 0), pane.outbound_dropped);
    try testing.expectEqual(@as(u64, 0), pane.session.response_bytes_dropped);
    try testing.expect(pane.session_generation != previous_generation);
    try testing.expect(!app_state.model.paste_failed);
}

test "healthy single-terminal mode omits lifecycle chrome" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startTwoPaneCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();

    for (harness.runtime.views[0].widgetLayoutTree().nodes) |layout| {
        try testing.expect(!std.mem.eql(u8, layout.widget.text, "TERMINAL 1 / RUNNING"));
        try testing.expect(!std.mem.eql(u8, layout.widget.text, "TERMINAL 2 / RUNNING"));
    }
}

// ------------------------------------------------------- the shell ceiling

// phux-cockpit-pg1: "Pane renders empty".
//
// The fifth cmd+T used to open a tab whose grid stayed blank forever. The
// effects layer keeps ONE fixed pty table for the whole process
// (`native_sdk.max_effect_ptys`), so its `ptySpawn` answered `.rejected` and
// that pane never received a byte — while the tab strip, the pane rect and
// the accessibility tree all reported an ordinary terminal, and
// `dispatch_errors` stayed 0 because a refused spawn is a normal exit event
// rather than an error. Reproduced against the shipped bundle on 2026-08-12:
// four cmd+T from a fresh workspace leave Terminal 5 at SPAWN REJECTED,
// filling the content area with no `text=` attribute at all.
//
// The invariant pinned here is NOT the number four, and is not the number the
// SDK now carries either. It is that cockpit never mints a pane it cannot back
// with a shell; both bounds are read from the SDK, so a pin that grows the pty
// table grows this test with it.
//
// phux-cockpit-ipg moved that number from 4 to 32, which is what
// `support.fillLiveShells` exists for: 32 shells no longer fit in one tab
// (`layout.max_panes` is 16) nor in one tab each (`topology.max_tabs` is 16),
// so reaching the ceiling now takes both chords. That is the point of the
// change — the ceiling a person hits is an app-owned number, not an invisible
// SDK one — but it does mean this test can no longer press cmd+T N times.
test "cmd+T refuses at the shell ceiling instead of opening a pane no shell can back" {
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try support.startCockpit(harness);
    defer support.stopCockpit(state);
    const app_iface = state.app();

    // One terminal arrives with the workspace; fill the rest of the table and
    // then ask for two more than it can hold.
    try testing.expectEqual(@as(usize, 1), state.effects.pendingPtyCount());
    try support.fillLiveShells(state);
    try testing.expect(!state.model.terminal_limit_refused);
    app.update(&state.model, .new_terminal, &state.effects);
    app.update(&state.model, .new_terminal, &state.effects);

    // The registry holds exactly the terminals that got a pty. Before the fix
    // it held every chord that was pressed, two of them backed by nothing.
    try testing.expectEqual(local.max_live_shells, state.model.provider.activeCount());
    try testing.expectEqual(local.max_live_shells, state.effects.pendingPtyCount());
    try testing.expectEqual(local.max_live_shells, state.model.provider.liveShellCount());

    // Drain what the effects layer staged: this is where the refused spawns
    // deliver their `.rejected` exits and strand the dead panes.
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    for (support.activeSlots(&state.model)) |pane| {
        try testing.expect(pane.phase != .failed);
        try testing.expect(pane.exit_reason != .rejected);
    }

    // Refused, and VISIBLY: the latch is what `view.terminalLimitNotice` reads,
    // and the band reveals itself to carry it. Asserted on the model rather
    // than on the rendered tree because `app.update` is called directly here,
    // which never tells the runtime the model is dirty — the same reason the
    // window-limit test next door checks its latch and not its badge.
    try testing.expect(state.model.terminal_limit_refused);
    try testing.expect(app.chromeRevealed(&state.model));

    // Closing a terminal gives the slot back, so the refusal stops being true.
    app.update(&state.model, .close_terminal, &state.effects);
    try testing.expect(!state.model.terminal_limit_refused);
}

// The same ceiling reached through cmd+D, which has further to fall: a split
// divides the focused pane's rect FIRST, so a rejected spawn used to hand
// half of a working terminal's screen area to a grid that never painted
// anything at all.
test "cmd+D refuses at the shell ceiling instead of dividing a pane for a shell that cannot start" {
    const harness = try native_sdk.TestHarness().create(testing.allocator, .{});
    defer harness.destroy(testing.allocator);
    const state = try support.startCockpit(harness);
    defer support.stopCockpit(state);

    try support.fillLiveShells(state);
    try testing.expectEqual(local.max_live_shells, state.model.provider.activeCount());
    try testing.expect(!state.model.terminal_limit_refused);

    var refs: [app.max_panes_per_tab]app.TerminalRef = undefined;
    const before = state.model.selectedTree().?.terminals(&refs);
    app.update(&state.model, .split_right, &state.effects);
    // The tree is untouched: no new leaf, and the sibling keeps its whole rect.
    try testing.expectEqual(before, state.model.selectedTree().?.terminals(&refs));
    try testing.expectEqual(local.max_live_shells, state.model.provider.activeCount());
    try testing.expect(state.model.terminal_limit_refused);
}
