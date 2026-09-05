const std = @import("std");
const builtin = @import("builtin");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const support = @import("support.zig");

const geometry = native_sdk.geometry;
const testing = std.testing;
const TerminalApp = support.TerminalApp;

const createSession = support.createSession;
const activeSlots = support.activeSlots;
const destroyModelSessions = app.deinitModel;
const expectCursorPaintKind = support.expectCursorPaintKind;
const startFocusedTerminal = support.startFocusedTerminal;
const typeCanvasText = support.typeCanvasText;
const pressCanvasKey = support.pressCanvasKey;
const releaseCanvasKey = support.releaseCanvasKey;

test "typing reaches the pty before the first output batch (empty-prompt shell)" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;

    const session = try createSession(80, 24);
    const app_state = try gpa.create(TerminalApp);
    defer gpa.destroy(app_state);
    app_state.* = TerminalApp.init(std.heap.page_allocator, app.initialModel(session), app.appOptions());
    defer app.deinitModel(&app_state.model);
    defer app_state.deinit();
    app_state.effects.executor = .fake;
    const app_iface = app_state.app();

    try harness.start(app_iface);
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = geometry.SizeF.init(980, 640),
        .scale_factor = 2,
        .frame_index = 1,
        .timestamp_ns = 1_000_000,
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    // The shell spawned (init_fx) but produced NO output — phase is
    // still .starting, never .live. Typing must still reach the pty.
    try testing.expectEqual(app.Phase.starting, app_state.model.provider.slots[0].phase);
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .pointer_down,
        .x = 200,
        .y = 200,
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .text = "whoami",
    } });
    try testing.expectEqualStrings("whoami", app_state.effects.ptyWrittenBytes(1));
}

test "terminal lifecycle focus rebuilds the custom cursor fill" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try testing.expect(app_state.model.focused);
    try expectCursorPaintKind(harness.runtime.views[0].canvasDisplayList(), .filled);

    try harness.runtime.dispatchPlatformEvent(app_iface, .app_deactivated);
    try testing.expect(!app_state.model.focused);
    try expectCursorPaintKind(harness.runtime.views[0].canvasDisplayList(), .hollow);

    try harness.runtime.dispatchPlatformEvent(app_iface, .app_activated);
    try testing.expect(app_state.model.focused);
    try expectCursorPaintKind(harness.runtime.views[0].canvasDisplayList(), .filled);
}

test "inactive application gates terminal key and text input" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try harness.runtime.dispatchPlatformEvent(app_iface, .app_deactivated);
    try pressCanvasKey(harness, app_iface, "enter", .{});
    try typeCanvasText(harness, app_iface, "blocked");
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));

    try harness.runtime.dispatchPlatformEvent(app_iface, .app_activated);
    try pressCanvasKey(harness, app_iface, "enter", .{});
    try typeCanvasText(harness, app_iface, "live");
    try testing.expectEqualStrings("\rlive", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
}

test "IME: a preedit is provisional; only the commit reaches the pty" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Compose Japanese: the preedit must NOT reach the pty (provisional).
    try harness.runtime.dispatchPlatformEvent(app_iface, .{
        .gpu_surface_input = .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .ime_set_composition,
            .text = "\xe3\x81\x8b", // か
        },
    });
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(1));

    // The host commits the marked text UNCHANGED — an empty commit; the
    // composed bytes come from the buffered preedit and reach the pty.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .ime_commit_composition,
        .text = "",
    } });
    try testing.expectEqualStrings("\xe3\x81\x8b", app_state.effects.ptyWrittenBytes(1));
}

test "IME: composition keys never encode into the pty - and the commit releases them" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // DURING the composition, keys belong to the input method: the
    // candidate-navigation arrow and the confirming Enter (which hosts
    // that surface the key before the commit deliver mid-composition)
    // must not reach the emulator's encoder — a CR here would submit
    // the half-composed command.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .ime_set_composition,
        .text = "\xe3\x81\x8b",
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "arrowdown",
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "enter",
    } });
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(1));

    // The commit inserts the composed bytes and RELEASES the keys: a
    // candidate can be committed by mouse in the OS popup with no
    // trailing key at all, so the next Enter is genuine typing and
    // encodes CR.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .ime_commit_composition,
        .text = "",
    } });
    try testing.expectEqualStrings("\xe3\x81\x8b", app_state.effects.ptyWrittenBytes(1));
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "enter",
    } });
    try testing.expectEqualStrings("\xe3\x81\x8b\r", app_state.effects.ptyWrittenBytes(1));
}

test "a command-chorded text event never types a literal character into the pty" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Some hosts emit a text_input alongside a Ctrl/Cmd shortcut. The
    // chord is not typing: the runtime's text gate (the focused-widget
    // rule, applied target-less too) must stop the literal "c" — the
    // child sees the ENCODED control byte from the key channel only,
    // never the chord plus a stray character.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .key = "c",
        .text = "c",
        .modifiers = .{ .control = true },
    } });
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(1));

    // Unmodified typing still flows.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .text = "c",
    } });
    try testing.expectEqualStrings("c", app_state.effects.ptyWrittenBytes(1));
}

test "typing carried on the key event reaches the pty - the no-separate-text-event host shape" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Hosts without a separate text event for plain typing deliver the
    // printable on the key_down itself; the committed-text channel must
    // carry it to the pty or typing is silently lost there.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "j",
        .text = "j",
    } });
    try testing.expectEqualStrings("j", app_state.effects.ptyWrittenBytes(1));
}

test "kitty report-all encodes committed text as CSI-u, and legacy passes it raw" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Legacy mode first: a committed "a" reaches the child as the raw
    // byte, exactly as before the encoder routing.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .text = "a",
    } });
    try testing.expectEqualStrings("a", app_state.effects.ptyWrittenBytes(1));

    // The TUI pushes kitty "report all keys as escape codes": the same
    // committed "a" must now encode as CSI 97 u — raw bytes would
    // desynchronize the application's key decoding.
    app_state.model.provider.slots[0].session.feed("\x1b[>8u");
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .text = "a",
    } });
    const written = app_state.effects.ptyWrittenBytes(1);
    try testing.expect(std.mem.endsWith(u8, written, "\x1b[97u"));
}

test "kitty event reporting hears key releases; legacy modes never do" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Legacy: a release encodes nothing.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_up,
        .key = "enter",
    } });
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(1));

    // The TUI enables kitty event reporting (with report-all): the
    // release of a printable now reaches the child as a CSI-u release
    // event (`:3` event type) — without it, key-driven state sticks.
    app_state.model.provider.slots[0].session.feed("\x1b[>11u");
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "a",
        .text = "a",
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_up,
        .key = "a",
    } });
    const written = app_state.effects.ptyWrittenBytes(1);
    try testing.expect(std.mem.indexOf(u8, written, ":3u") != null);
}

test "consumed app shortcut releases never leak under kitty reporting" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    // A second TAB, so cmd+2 has somewhere to go.
    try app_state.dispatch(&harness.runtime, 1, .new_terminal);
    try app_state.dispatch(&harness.runtime, 1, .{ .select_position = 0 });

    // Both children request key-release reports. Releases deliberately
    // omit Command to model the modifier coming up before the key.
    for (activeSlots(&app_state.model)) |*pane| pane.session.feed("\x1b[>11u");

    try pressCanvasKey(harness, app_iface, "2", .{ .primary = true, .command = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    try testing.expect(app_state.model.consumed_shortcut_keys_held != 0);
    try releaseCanvasKey(harness, app_iface, "2", .{});
    try testing.expectEqual(@as(u32, 0), app_state.model.consumed_shortcut_keys_held);

    // Toggle selection on and back off. The second release arrives while
    // selection is no longer armed, so only the app-level latch can stop it.
    for (0..2) |_| {
        try pressCanvasKey(harness, app_iface, "space", .{ .primary = true, .command = true, .shift = true });
        try testing.expect(app_state.model.consumed_shortcut_keys_held != 0);
        try releaseCanvasKey(harness, app_iface, "space", .{});
        try testing.expectEqual(@as(u32, 0), app_state.model.consumed_shortcut_keys_held);
    }
    try testing.expect(!app_state.model.provider.slots[1].selecting);

    // An emulator selection can be copied without keyboard-selection mode.
    // That keeps the release path otherwise open and proves copy is latched.
    app_state.model.provider.slots[1].session.feed("copy");
    app_state.model.provider.slots[1].session.beginSelection(false);
    try pressCanvasKey(harness, app_iface, "c", .{ .primary = true, .command = true });
    try testing.expect(app_state.model.consumed_shortcut_keys_held != 0);
    try releaseCanvasKey(harness, app_iface, "c", .{});
    try testing.expectEqual(@as(u32, 0), app_state.model.consumed_shortcut_keys_held);

    try pressCanvasKey(harness, app_iface, "arrowup", .{ .primary = true, .command = true });
    try testing.expect(app_state.model.consumed_shortcut_keys_held != 0);
    try releaseCanvasKey(harness, app_iface, "arrowup", .{});
    try testing.expectEqual(@as(u32, 0), app_state.model.consumed_shortcut_keys_held);

    // Escape turns selection off immediately, and Enter may turn it off
    // before key-up when clipboard completion is fast. Both releases still
    // belong to the app action rather than the kitty-reporting child.
    app_state.model.provider.slots[1].session.feed("select me");
    app_state.model.provider.slots[1].selecting = true;
    app_state.model.provider.slots[1].session.beginSelection(false);
    try pressCanvasKey(harness, app_iface, "escape", .{});
    try testing.expect(!app_state.model.provider.slots[1].selecting);
    try releaseCanvasKey(harness, app_iface, "escape", .{});

    app_state.model.provider.slots[1].selecting = true;
    app_state.model.provider.slots[1].session.beginSelection(false);
    app_state.model.provider.slots[1].session.moveSelection(-1, 0, true);
    try pressCanvasKey(harness, app_iface, "enter", .{});
    try app_state.effects.feedClipboardResult(app.clipboard_key, .ok, "");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expect(!app_state.model.provider.slots[1].selecting);
    try releaseCanvasKey(harness, app_iface, "enter", .{});

    try app_state.effects.feedPtyExit(app.ptyKey(1), 0, 0, .spawn_failed, 0); // a spawn failure: the one end that leaves the pane standing
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try pressCanvasKey(harness, app_iface, "r", .{ .primary = true, .command = true });
    try testing.expectEqual(app.Phase.starting, app_state.model.provider.slots[1].phase);
    try testing.expect(app_state.model.consumed_shortcut_keys_held != 0);
    try releaseCanvasKey(harness, app_iface, "r", .{});
    try testing.expectEqual(@as(u32, 0), app_state.model.consumed_shortcut_keys_held);

    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));
}

test "a non-shortcut re-press supersedes a stranded app shortcut latch" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    app_state.model.provider.slots[0].session.feed("\x1b[>11u");
    try pressCanvasKey(harness, app_iface, "arrowup", .{ .primary = true, .command = true });
    try testing.expect(app_state.model.consumed_shortcut_keys_held != 0);

    // The key repeats after Command came up. This is now terminal input,
    // so the old app latch must not swallow its eventual release.
    try pressCanvasKey(harness, app_iface, "arrowup", .{});
    try testing.expectEqual(@as(u32, 0), app_state.model.consumed_shortcut_keys_held);
    const before_release = app_state.effects.ptyWrittenBytes(app.ptyKey(0)).len;
    try releaseCanvasKey(harness, app_iface, "arrowup", .{});
    try testing.expect(app_state.effects.ptyWrittenBytes(app.ptyKey(0)).len > before_release);
}

test "chorded punctuation and function keys encode their control sequences" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Ctrl+\ is SIGQUIT to a terminal user — chorded punctuation has no
    // text-channel fallback, so the encoder must speak or the control
    // byte is silently lost. The emulator's legacy encoding maps it to
    // the FS control byte (0x1C).
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "\\",
        .modifiers = .{ .control = true },
    } });
    try testing.expectEqualStrings("\x1c", app_state.effects.ptyWrittenBytes(1));

    // Ctrl+[ rides the emulator's fixterms CSI-u encoding (the ESC
    // chord stays distinguishable from a bare Escape press) — the same
    // bytes the emulator's own terminal sends for this chord.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "[",
        .modifiers = .{ .control = true },
    } });
    try testing.expectEqualStrings("\x1c\x1b[91;5u", app_state.effects.ptyWrittenBytes(1));

    // F1 encodes its escape sequence (ESC O P) — function keys commit
    // no text, so the encoder is their only road to the child.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "f1",
    } });
    try testing.expectEqualStrings("\x1c\x1b[91;5u\x1bOP", app_state.effects.ptyWrittenBytes(1));
}

test "a primary-aliased Ctrl chord still encodes its C0 byte" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Hosts whose PRIMARY modifier is Ctrl report a bare Ctrl chord
    // with BOTH bits set; the runtime folds primary into `super`. The
    // encoder must still see a clean Ctrl+C and emit ETX (0x03) — a
    // stray super would demote it to a CSI-u chord the foreground shell
    // never treats as an interrupt.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = "c",
        .modifiers = .{ .control = true, .primary = true },
    } });
    try testing.expectEqualStrings("\x03", app_state.effects.ptyWrittenBytes(1));
}

test "macOS natural text arrow gestures use shell editing bindings" {
    if (comptime builtin.os.tag != .macos) return error.SkipZigTest;
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startFocusedTerminal(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Keep the platform-native bindings even after a TUI enables kitty
    // event reporting: Option moves by words (Esc-b/f), Command moves to
    // line boundaries (Ctrl-A/E), Command+Delete clears to the start
    // (Ctrl-U), and a bound release emits nothing.
    app_state.model.provider.slots[0].session.feed("\x1b[>11u");
    const events = [_]native_sdk.platform.GpuSurfaceInputEvent{
        .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .key_down,
            .key = "arrowleft",
            .modifiers = .{ .option = true },
        },
        .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .key_up,
            .key = "arrowleft",
        },
        .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .key_down,
            .key = "arrowright",
            .modifiers = .{ .option = true },
        },
        .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .key_up,
            .key = "arrowright",
        },
        .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .key_down,
            .key = "arrowleft",
            .modifiers = .{ .primary = true, .command = true },
        },
        .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .key_up,
            .key = "arrowleft",
        },
        .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .key_down,
            .key = "arrowright",
            .modifiers = .{ .primary = true, .command = true },
        },
        .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .key_up,
            .key = "arrowright",
        },
        .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .key_down,
            .key = "backspace",
            .modifiers = .{ .primary = true, .command = true },
        },
        .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .key_up,
            .key = "backspace",
        },
    };
    for (events) |event| {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = event });
    }
    try testing.expectEqualStrings("\x1bb\x1bf\x01\x05\x15", app_state.effects.ptyWrittenBytes(1));
}
