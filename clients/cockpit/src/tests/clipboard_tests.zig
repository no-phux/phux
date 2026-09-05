const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const support = @import("support.zig");

const geometry = native_sdk.geometry;
const testing = std.testing;
const automation = native_sdk.automation;

const destroyModelSessions = app.deinitModel;
const startFocusedTerminal = support.startFocusedTerminal;
const startTwoPaneCockpit = support.startTwoPaneCockpit;
const pressCanvasKey = support.pressCanvasKey;
const releaseCanvasKey = support.releaseCanvasKey;

test "Cmd+V reads the clipboard and normalizes plain-paste newlines" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    for (0..120) |index| {
        var line: [24]u8 = undefined;
        app_state.model.provider.slots[0].session.feed(std.fmt.bufPrint(&line, "history {d}\r\n", .{index}) catch unreachable);
    }
    app_state.model.provider.slots[0].session.scrollToTop();
    const history_offset = app_state.model.provider.slots[0].session.scrollbar().offset;

    try pressCanvasKey(harness, app_iface, "v", .{ .primary = true, .command = true });
    try testing.expect(app_state.model.paste_inflight);
    try testing.expect(app_state.model.paste_owner.eql(
        app_state.model.provider.owner(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminalOwner,
    ));
    try testing.expect(app.paste_clipboard_key != app.clipboard_key);
    const request = app_state.effects.pendingClipboardAt(0) orelse return error.TestExpectedClipboardRead;
    try testing.expectEqual(app.paste_clipboard_key, request.key);
    try testing.expectEqual(native_sdk.EffectClipboardOp.read, request.op);

    try app_state.effects.feedClipboardResult(app.paste_clipboard_key, .ok, "alpha\nbeta\r\ngamma");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expectEqualStrings("alpha\rbeta\r\rgamma", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expect(!app_state.model.paste_inflight);
    try testing.expect(!app_state.model.paste_failed);
    try testing.expect(app_state.model.provider.slots[0].session.scrollbar().offset > history_offset);
}

test "Cmd+V uses bracketed paste framing and preserves newlines" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    app_state.model.provider.slots[0].session.feed("\x1b[?2004h");
    try pressCanvasKey(harness, app_iface, "v", .{ .primary = true, .command = true });
    try app_state.effects.feedClipboardResult(app.paste_clipboard_key, .ok, "alpha\nbeta");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);

    try testing.expectEqualStrings(
        "\x1b[200~alpha\nbeta\x1b[201~",
        app_state.effects.ptyWrittenBytes(app.ptyKey(0)),
    );
}

test "Cmd+V replaces dangerous control bytes before writing" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try pressCanvasKey(harness, app_iface, "v", .{ .primary = true, .command = true });
    try app_state.effects.feedClipboardResult(app.paste_clipboard_key, .ok, "one\x03two\x1bthree\x00");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expectEqualStrings("one two three ", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
}

test "clipboard paste stays with its requesting terminal while Web is selected" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try pressCanvasKey(harness, app_iface, "v", .{ .primary = true, .command = true });
    // The Web surface left the tab strip; cmd+shift+B is its own chord.
    try pressCanvasKey(harness, app_iface, "b", .{ .primary = true, .command = true, .shift = true });
    try testing.expect(app_state.model.selectedSurface().eql(.web));
    try testing.expectEqual(@as(?app.TerminalRef, null), app_state.model.selectedTerminalRef());
    try app_state.effects.feedClipboardResult(app.paste_clipboard_key, .ok, "original owner");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);

    try testing.expectEqualStrings("original owner", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));
}

test "clipboard result after pane exit is a visible paste failure" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try pressCanvasKey(harness, app_iface, "v", .{ .primary = true, .command = true });
    // An ABNORMAL end: a clean one closes the pane, and there would be no
    // requesting terminal left to fail the paste against.
    try app_state.effects.feedPtyExit(app.ptyKey(0), 0, 0, .spawn_failed, 0);
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try app_state.effects.feedClipboardResult(app.paste_clipboard_key, .ok, "too late");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);

    try testing.expect(app_state.model.paste_failed);
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqual(@as(usize, 0), app_state.model.provider.slots[0].outbound_len);
}

test "Cmd+V release never leaks under kitty reporting" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    app_state.model.provider.slots[0].session.feed("\x1b[>11u");
    try pressCanvasKey(harness, app_iface, "v", .{ .primary = true, .command = true });
    try testing.expect(app_state.model.consumed_shortcut_keys_held != 0);
    try app_state.effects.feedClipboardResult(app.paste_clipboard_key, .ok, "paste");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    // Hosts commonly omit Command when the key comes up after the
    // modifier. The app-level latch still owns this physical release.
    try releaseCanvasKey(harness, app_iface, "v", .{});
    try testing.expectEqual(@as(u32, 0), app_state.model.consumed_shortcut_keys_held);
    try testing.expectEqualStrings("paste", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
}

test "paste follows retained replies and atomic admission refuses every byte" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    const pane = &app_state.model.provider.slots[0];

    // A terminal query reply predates the paste. It must reach stdin
    // first even though its ring admission was initially blocked.
    pane.session.feed("\x1b[6n");
    const reply = try gpa.dupe(u8, pane.session.pendingResponses());
    defer gpa.free(reply);
    app_state.effects.fake_pty_write_full = true;
    pane.outbound_len = pane.outbound_buffer.len;
    app.moveResponsesToOutbound(pane, &app_state.effects);
    try pressCanvasKey(harness, app_iface, "v", .{ .primary = true, .command = true });
    app_state.effects.fake_pty_write_full = false;
    pane.outbound_len = 0;
    try app_state.effects.feedClipboardResult(app.paste_clipboard_key, .ok, "after");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    const ordered = app_state.effects.ptyWrittenBytes(app.ptyKey(0));
    try testing.expect(std.mem.startsWith(u8, ordered, reply));
    try testing.expect(std.mem.endsWith(u8, ordered, "after"));
    try releaseCanvasKey(harness, app_iface, "v", .{});

    // In bracketed mode, less free ring space than the complete framed
    // paste refuses it whole: no opening fence can be queued by itself.
    pane.session.feed("\x1b[?2004h");
    app_state.effects.fake_pty_write_full = true;
    pane.outbound_head = 0;
    pane.outbound_len = pane.outbound_buffer.len - 4;
    const before_len = pane.outbound_len;
    const before_loss = pane.outbound_dropped;
    try pressCanvasKey(harness, app_iface, "v", .{ .primary = true, .command = true });
    try app_state.effects.feedClipboardResult(app.paste_clipboard_key, .ok, "body");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expectEqual(before_len, pane.outbound_len);
    try testing.expectEqual(before_loss + "\x1b[200~body\x1b[201~".len, pane.outbound_dropped);
    try testing.expect(app_state.model.paste_failed);
}

test "a second copy while the write is in flight is a no-op, never a false failure" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    app_state.model.provider.slots[0].session.feed("copy me");
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "space",
        .modifiers = .{ .primary = true, .shift = true },
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "arrowleft",
        .modifiers = .{ .shift = true },
    } });
    // Enter twice before the async result drains: the second press must
    // not issue a duplicate-key request whose rejection would overwrite
    // the first copy's success.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "enter",
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "enter",
    } });
    try app_state.effects.feedClipboardResult(app.clipboard_key, .ok, "");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expect(!app_state.model.provider.slots[0].copy_failed);
    try testing.expect(!app_state.model.provider.slots[0].selecting);
    try testing.expect(!app_state.model.copy_inflight);
}

test "restart cancels an owned clipboard read and ignores its stale result" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try pressCanvasKey(harness, app_iface, "v", .{ .primary = true, .command = true });
    const old_generation = app_state.model.provider.slots[0].session_generation;
    try testing.expect(app_state.model.paste_inflight);
    try app_state.effects.feedPtyExit(app.ptyKey(0), 0, 0, .spawn_failed, 0);
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try pressCanvasKey(harness, app_iface, "r", .{ .primary = true, .command = true });
    try testing.expect(app_state.model.provider.slots[0].session_generation != old_generation);
    try testing.expect(app_state.model.paste_inflight);

    // The cancellation terminal is delivered after the replacement
    // shell exists. Generation pinning makes it state-only cleanup: no
    // bytes and no failure are attributed to the new session.
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expect(!app_state.model.paste_inflight);
    try testing.expect(!app_state.model.paste_failed);
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
}

test "restart cancels an owned clipboard write and ignores its stale result" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    const pane = &app_state.model.provider.slots[0];

    pane.session.feed("copy me");
    pane.selecting = true;
    pane.session.beginSelection(false);
    pane.session.moveSelection(-1, 0, true);
    try pressCanvasKey(harness, app_iface, "enter", .{});
    const old_generation = pane.session_generation;
    try testing.expect(app_state.model.copy_inflight);

    try app_state.effects.feedPtyExit(app.ptyKey(0), 0, 0, .spawn_failed, 0);
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try pressCanvasKey(harness, app_iface, "r", .{ .primary = true, .command = true });
    try testing.expect(pane.session_generation != old_generation);
    try testing.expect(app_state.model.copy_inflight);

    // Cancellation arrives after reset and cannot clear or fail a selection
    // belonging to the replacement shell.
    pane.selecting = true;
    pane.session.beginSelection(false);
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expect(!app_state.model.copy_inflight);
    try testing.expect(pane.selecting);
    try testing.expect(!pane.copy_failed);
}

test "a copied selection persists while failure remains retryable" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    app_state.model.provider.slots[0].session.feed("copy me");
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "space",
        .modifiers = .{ .primary = true, .shift = true },
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "arrowleft",
        .modifiers = .{ .shift = true },
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "enter",
    } });
    // The write is in flight: the selection must still stand — a failed
    // result needs something to retry.
    try testing.expect(app_state.model.provider.slots[0].selecting);
    try testing.expect(app_state.model.provider.slots[0].session.selectionActive());

    // A failed write keeps selection mode armed for retry. Success exits the
    // keyboard mode but preserves the conventional terminal highlight.
    try app_state.effects.feedClipboardResult(app.clipboard_key, .rejected, "");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expect(app_state.model.provider.slots[0].copy_failed);
    try testing.expect(app_state.model.provider.slots[0].selecting);
    try testing.expect(app_state.model.provider.slots[0].session.selectionActive());
    try releaseCanvasKey(harness, app_iface, "enter", .{});
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "enter",
    } });
    try app_state.effects.feedClipboardResult(app.clipboard_key, .ok, "");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expect(!app_state.model.provider.slots[0].copy_failed);
    try testing.expect(!app_state.model.provider.slots[0].selecting);
    try testing.expect(app_state.model.provider.slots[0].session.selectionActive());
}

// phux-cockpit-zly. A successful copy used to mean two different things
// depending on which provider owned the pane: the local arm left the range
// highlighted, the remote arm told the coordinator to drop it and released
// both anchors. They agree now, on the LOCAL behaviour — the convention every
// Mac terminal this app's users arrive from already follows, and the only
// confirmation that the copy took the range they meant.
//
// The remote CLIPBOARD arm needs a live coordinator (`-Dphux-enabled=true`),
// so what is reachable from a default build is the rule itself. The local half
// of the same rule is pinned end-to-end by "a copied selection persists while
// failure remains retryable" above.
// GUARD: copy-retains-remote-selection
test "a successful copy ends selection mode on a remote pane and keeps its range" {
    var state: app.RemoteUiState = .{
        .selecting = true,
        .start_anchor = 0x51,
        .end_anchor = 0x52,
        .copied_bytes = 7,
    };
    app.retainSelectionAfterCopy(&state);
    // Keyboard-selection mode ends, exactly as `pane.selecting = false` ends
    // it locally: a further arrow key moves the cursor rather than extending a
    // range the user has finished with.
    try testing.expect(!state.selecting);
    // The anchors — and so the coordinator's own range, and so the wash on
    // screen — are untouched. This is what used to be thrown away.
    try testing.expectEqual(@as(u64, 0x51), state.start_anchor);
    try testing.expectEqual(@as(u64, 0x52), state.end_anchor);
    // A success is not a failure, and the byte count stays as the copy's own
    // report of what it took.
    try testing.expect(!state.copy_failed);
    try testing.expectEqual(@as(u64, 7), state.copied_bytes);
}

test "a copy over a vanished emulator range reports failure, never a quiet no-op" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    app_state.model.provider.slots[0].session.feed("some text\r\n");
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "space",
        .modifiers = .{ .primary = true, .shift = true },
    } });
    try testing.expect(app_state.model.provider.slots[0].selecting);

    // Simulate a failed selection re-pin: the emulator range vanished
    // while the model still holds its anchor (`applySelection` clears
    // the highlight when it cannot pin).
    app_state.model.provider.slots[0].session.term.screens.active.clearSelection();
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "c",
        .modifiers = .{ .primary = true },
    } });
    try testing.expect(app_state.model.provider.slots[0].copy_failed);
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(1));
}

test "clipboard completion follows terminal identity across attachment moves" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startTwoPaneCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const model = &app_state.model;

    try pressCanvasKey(harness, app_state.app(), "v", .{ .primary = true, .command = true });
    try testing.expect(model.paste_owner.eql(
        model.provider.owner(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminalOwner,
    ));
    // The pane moves around underneath the in-flight read: a tab switch, a
    // reorder, and back. The clipboard result must still land on the
    // terminal that ASKED for it, addressed by identity.
    app.update(model, .{ .select_position = 1 }, &app_state.effects);
    app.update(model, .{ .move_terminal = -1 }, &app_state.effects);
    app.update(model, .{ .select_surface = .{ .terminal = app.initialTerminalRef(0) } }, &app_state.effects);

    try app_state.effects.feedClipboardResult(app.paste_clipboard_key, .ok, "identity paste");
    try harness.runtime.dispatchPlatformEvent(app_state.app(), .wake);
    try testing.expectEqualStrings("identity paste", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));
}
