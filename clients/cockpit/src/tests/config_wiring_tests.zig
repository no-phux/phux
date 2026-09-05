//! The config is only real when it CHANGES something. `config_tests.zig`
//! proves the parser; this file proves each knob reaches the pixels, the
//! emulator, or the geometry it names.

const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const support = @import("support.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;
const startCockpit = support.startCockpit;
const stopCockpit = support.stopCockpit;
const pressCanvasKey = support.pressCanvasKey;
const releaseCanvasKey = support.releaseCanvasKey;
const pointerInput = @import("pointer_support.zig").pointerInput;
const widgetFrameBySemantics = support.widgetFrameBySemantics;
const rectCenter = support.rectCenter;

/// The first widget whose accessibility label starts with `prefix`, or null.
/// Reading the LAYOUT tree rather than the model is the point: it is the only
/// evidence that a thing the model believes in reached the glass.
fn semanticsLabelWithPrefix(harness: anytype, prefix: []const u8) ?[]const u8 {
    for (harness.runtime.views[0].widgetLayoutTree().nodes) |node| {
        if (std.mem.startsWith(u8, node.widget.semantics.label, prefix)) return node.widget.semantics.label;
    }
    return null;
}

test "font-size drives the terminal cell box and leaves the chrome alone" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);

    const small = canvas.terminalCellMetrics(app.terminalTokens(&state.model));
    state.model.config = app.parseConfig("font-size = 26");
    const large = canvas.terminalCellMetrics(app.terminalTokens(&state.model));

    try testing.expect(large.height > small.height);
    try testing.expect(large.width > small.width);
    try testing.expectEqual(@as(f32, 26), large.font_size);
    // The chrome register does NOT follow the terminal: growing the terminal
    // must not grow the tab strip's labels.
    try testing.expectEqual(small.font_size, canvas.terminalCellMetrics(app.cockpitTokens(&state.model)).font_size);
}

// Every test below uses `font-size = 26`, never the default 13, and that is
// load-bearing rather than arbitrary. The bug they guard was a pair of
// hardcoded metric defaults, `cell_width = 8` and `cell_height = 18`. The SDK
// derives an unmeasured cell as `round(font_size * 0.6)` by
// `round(font_size * 1.4)`, and at font-size 13 that is round(7.8) = 8 by
// round(18.2) = 18 — the wrong values and the right ones are the same
// numbers. A test written at the default font cannot fail no matter how
// broken the metrics path is, which is precisely why this survived to ship.
// At 26 the mono cell is 15.6 x 36 and the two states are far apart.
const metric_test_font_size: f32 = 26;

// GUARD: unmeasured-cell-is-null
test "a pane proposes no viewport until its cell box has been measured" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(1301, 807);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);

    state.model.config = app.parseConfig("font-size = 26");
    // A brand new pane, exactly as a split or a cmd+T mints one: it has a
    // grid, and nothing has ever painted it.
    app.update(&state.model, .split_right, &state.effects);

    // BEFORE any paint the proposer must say "I do not know yet" about the new
    // pane. This is the assertion the old code could not make: the guard it
    // stood on read `cell_width <= 0`, and 8 is not <= 0, so the proposer
    // sized every fresh pane against a cell it had invented and the shell took
    // a SIGWINCH for a width it was about to be told to abandon.
    //
    // One proposal, not zero: the pane that startCockpit already painted keeps
    // its own, which is the established contract (`ProposedViewports` stops at
    // the first unmeasured pane and reports what it had). What must not happen
    // is a proposal for the pane nobody has measured.
    const before = app.proposedViewportsIn(&state.model, &state.model.primary, size);
    try testing.expect(before.incomplete);
    try testing.expectEqual(@as(usize, 1), before.count);

    // AFTER a paint the proposal exists, and it is derived from the real
    // 26-point cell rather than from 8 x 18.
    try support.pumpPaint(harness, state.app(), app.canvas_label, size);
    const after = app.proposedViewportsIn(&state.model, &state.model.primary, size);
    try testing.expect(!after.incomplete);
    try testing.expectEqual(@as(usize, 2), after.count);

    var panes: [app.max_panes_per_tab]app.LayoutPane = undefined;
    const count = app.resolvePanes(&state.model, size, &panes);
    try testing.expectEqual(@as(usize, 2), count);
    for (after.slice(), panes[0..count]) |proposal, pane| {
        const terminal = state.model.provider.terminal(proposal.terminal) orelse
            return error.TestExpectedTerminal;
        const cell = terminal.session.measuredCell() orelse return error.TestExpectedMeasuredCell;
        // The cell really is the 26-point mono box, not the stale default.
        try testing.expectApproxEqAbs(
            metric_test_font_size * canvas.mono_advance_em,
            cell.width,
            0.01,
        );
        try testing.expectEqual(@as(f32, 36), cell.height); // round(26 * 1.4)

        const expected = grid.Session.clampGrid(
            @intFromFloat(@max(2, pane.rect.width / cell.width)),
            @intFromFloat(@max(2, pane.rect.height / cell.height)),
        );
        try testing.expectEqual(expected.x, proposal.cols);
        try testing.expectEqual(expected.y, proposal.rows);

        // And the number the OLD defaults would have produced is a genuinely
        // different one, so the equality above is not passing by coincidence.
        const stale = grid.Session.clampGrid(
            @intFromFloat(@max(2, pane.rect.width / 8.0)),
            @intFromFloat(@max(2, pane.rect.height / 18.0)),
        );
        try testing.expect(stale.x != proposal.cols);
    }
}

// GUARD: remote-pane-mono-metrics
test "a pane sized without a text-measure provider uses the mono cell, not a sans estimate" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);

    state.model.config = app.parseConfig("font-size = 26");
    const tokens = app.terminalTokens(&state.model);
    // The condition under test: these tokens were built outside the painter,
    // so they carry no provider. Remote (phux) panes are sized from exactly
    // this token set.
    try testing.expect(tokens.text_measure == null);
    try testing.expectEqual(canvas.min_registered_font_id, tokens.typography.mono_font_id);

    const corrected = app.terminalCellMetricsFor(tokens);
    try testing.expectApproxEqAbs(
        metric_test_font_size * canvas.mono_advance_em,
        corrected.width,
        0.01,
    );

    // The negative control, and the measurement in the bead. Asking the SDK
    // directly still returns the proportional estimate, because its estimator
    // only knows `default_mono_font_id` is monospace and cockpit's face is
    // registered at 64. Recorded so the size of the error stays visible:
    //   26 * 0.877 em = 22.802 vs 26 * 0.6 em = 15.6, a ratio of 1.4617.
    const uncorrected = canvas.terminalCellMetrics(tokens);
    try testing.expect(uncorrected.width > corrected.width * 1.4);
    try testing.expect(uncorrected.width < corrected.width * 1.5);
}

test "the registered terminal face is a 0.6 em monospace" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);

    // What licenses `terminalCellMetricsFor` to substitute the SDK's mono id
    // when no provider is stamped: cockpit's own terminal face has the same
    // 0.6 em advance that constant describes. Measured through the REAL
    // provider, against the id the app actually registers.
    //
    // A font swap to a face with a different pitch must fail HERE, loudly,
    // rather than silently re-opening the remote-pane sizing bug.
    const provider = harness.runtime.textMeasureProvider() orelse
        return error.TestExpectedTextMeasureProvider;
    const measured = canvas.measureTextWidthForFont(
        provider,
        app.terminal_font_id,
        "MMMMMMMMMMMMMMMM",
        metric_test_font_size,
    );
    try testing.expectApproxEqAbs(
        metric_test_font_size * canvas.mono_advance_em,
        measured / 16,
        0.01,
    );
}

test "a bigger font reflows the pty to fewer columns" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    const iface = state.app();

    const pane = state.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    const cols_before = pane.cols;
    try testing.expect(cols_before > 2);

    state.model.config = app.parseConfig("font-size = 30");
    // Two pumps: the painter writes the new cell metrics on the first frame,
    // and the viewport pump reads them on the next. Nothing here touches the
    // pty directly — the reflow rides the ordinary onFrame path.
    for (2..8) |frame_index| {
        try harness.runtime.dispatchPlatformEvent(iface, .{ .gpu_surface_frame = .{
            .label = app.canvas_label,
            .size = size,
            .scale_factor = 2,
            .frame_index = @intCast(frame_index),
            .timestamp_ns = @as(u64, frame_index) * 1_000_000,
        } });
        try harness.runtime.dispatchPlatformEvent(iface, .frame_requested);
    }
    try testing.expect(pane.cols < cols_before);
    try testing.expectEqual(pane.cols, pane.session.cols());
}

test "cmd+= cmd+- and cmd+0 step and reset the live font size" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    const iface = state.app();

    // The configured size is the origin cmd+0 returns to, not a hardcoded 13.
    state.model.config = app.parseConfig("font-size = 15");
    try testing.expectEqual(@as(f32, 15), state.model.fontSize());

    // Each chord needs its own press/release edge: a consumed shortcut owns
    // its key until key-up, so a HELD cmd+- must not step twice.
    try pressCanvasKey(harness, iface, "=", .{ .primary = true });
    try testing.expectEqual(@as(f32, 16), state.model.fontSize());
    try pressCanvasKey(harness, iface, "=", .{ .primary = true });
    try testing.expectEqual(@as(f32, 16), state.model.fontSize());
    try releaseCanvasKey(harness, iface, "=", .{ .primary = true });

    try pressCanvasKey(harness, iface, "-", .{ .primary = true });
    try releaseCanvasKey(harness, iface, "-", .{ .primary = true });
    try pressCanvasKey(harness, iface, "-", .{ .primary = true });
    try releaseCanvasKey(harness, iface, "-", .{ .primary = true });
    try testing.expectEqual(@as(f32, 14), state.model.fontSize());

    try pressCanvasKey(harness, iface, "0", .{ .primary = true });
    try testing.expectEqual(@as(f32, 15), state.model.fontSize());
}

test "font sizing clamps at both ends without stranding the key" {
    var model: app.Model = .{ .provider = undefined, .config = app.parseConfig("font-size = 71") };
    try testing.expect(model.stepFontSize(1));
    try testing.expectEqual(@as(f32, 72), model.fontSize());
    // At the ceiling the step is refused rather than banked, so ONE step back
    // moves the size instead of unwinding invisible headroom first.
    try testing.expect(!model.stepFontSize(1));
    try testing.expect(!model.stepFontSize(1));
    try testing.expect(model.stepFontSize(-1));
    try testing.expectEqual(@as(f32, 71), model.fontSize());
    // Back at the configured size the offset is genuinely zero, so cmd+0 has
    // nothing to undo and says so.
    try testing.expect(!model.resetFontSize());
    try testing.expect(model.stepFontSize(-10));
    try testing.expectEqual(@as(f32, 61), model.fontSize());
    try testing.expect(model.resetFontSize());
    try testing.expectEqual(@as(f32, 71), model.fontSize());
}

test "background foreground and selection colors reach the terminal tokens" {
    var model: app.Model = .{
        .provider = undefined,
        .config = app.parseConfig(
            \\background = #102030
            \\foreground = #a0b0c0
            \\selection-background = #ff0000
        ),
    };
    const tokens = app.terminalTokens(&model);
    try testing.expectEqual(canvas.Color.rgb8(0x10, 0x20, 0x30), tokens.colors.background);
    try testing.expectEqual(canvas.Color.rgb8(0xa0, 0xb0, 0xc0), tokens.colors.text);
    // The selection wash reads `accent` (see terminal/palette.zig).
    try testing.expectEqual(canvas.Color.rgb8(0xff, 0, 0), tokens.colors.accent);

    // An unset color leaves the app's own register untouched rather than
    // resolving to black.
    var bare: app.Model = .{ .provider = undefined };
    const default_tokens = app.terminalTokens(&bare);
    try testing.expectEqual(app.cockpitTokens(&bare).colors.background, default_tokens.colors.background);
}

test "palette overrides cursor color and cursor style reach the emulator" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);

    const pane = state.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    // Before: the emulator's own ANSI red and no cursor override.
    try testing.expect(pane.session.term.colors.cursor.override == null);
    const default_red = pane.session.term.colors.palette.current[1];

    state.model.config = app.parseConfig(
        \\palette = 1=#ff8800
        \\cursor-color = #00ff00
        \\cursor-style = bar
        \\cursor-style-blink = false
    );
    // Restart is a spawn, and every spawn re-applies the config after the
    // emulator reset that spawning performs.
    pane.phase = .ended;
    try state.dispatch(&harness.runtime, 1, .{ .restart = pane.id });

    try testing.expectEqual(@as(u8, 0xff), pane.session.term.colors.palette.current[1].r);
    try testing.expectEqual(@as(u8, 0x88), pane.session.term.colors.palette.current[1].g);
    try testing.expect(!std.meta.eql(default_red, pane.session.term.colors.palette.current[1]));
    const cursor = pane.session.term.colors.cursor.override orelse return error.TestExpectedCursorOverride;
    try testing.expectEqual(@as(u8, 0xff), cursor.g);
    try testing.expectEqual(@as(u8, 0), cursor.r);
    try testing.expectEqual(@as(?bool, false), pane.session.term.cursor.default_blink);
}

test "restart is not required for the FIRST spawn to be configured" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;
    harness.runtime.options.shortcuts = &app.cockpit_shortcuts;

    const session = try support.createDefaultSession();
    var model = app.initialModelWithIo(testing.allocator, testing.io, session) catch |err| {
        session.destroy();
        return err;
    };
    model.config = app.parseConfig("palette = 4=#123456");
    const state = try testing.allocator.create(support.TerminalApp);
    state.* = support.TerminalApp.init(std.heap.page_allocator, model, app.appOptions());
    defer stopCockpit(state);
    state.effects.executor = .fake;
    try harness.start(state.app());
    try harness.runtime.dispatchPlatformEvent(state.app(), .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = geometry.SizeF.init(980, 640),
        .scale_factor = 2,
        .frame_index = 1,
        .timestamp_ns = 1_000_000,
    } });
    try harness.runtime.dispatchPlatformEvent(state.app(), .frame_requested);

    const pane = state.model.provider.terminal(app.initialTerminalRef(0)) orelse return error.TestExpectedTerminal;
    try testing.expectEqual(@as(u8, 0x12), pane.session.term.colors.palette.current[4].r);
    try testing.expectEqual(@as(u8, 0x56), pane.session.term.colors.palette.current[4].b);
}

test "a new tab and a split start in the focused pane's directory" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);
    const iface = state.app();

    // Nothing reported yet: the new terminal takes the plain argv, exactly as
    // the very first terminal does.
    try state.dispatch(&harness.runtime, 1, .new_terminal);
    try testing.expectEqualSlices([]const u8, app.paneArgv(0), state.model.provider.slots[1].argv);

    // OSC 7 from the focused shell, then a split.
    try state.dispatch(&harness.runtime, 1, .{ .select_position = 0 });
    try state.effects.feedPtyOutput(app.ptyKey(0), "\x1b]7;file://host/Users/phall/workspace\x1b\\");
    try harness.runtime.dispatchPlatformEvent(iface, .wake);
    try testing.expectEqualStrings("/Users/phall/workspace", state.model.provider.slots[0].pwd());

    try state.dispatch(&harness.runtime, 1, .split_right);
    const split_argv = state.model.provider.slots[2].argv;
    var saw_cwd = false;
    for (split_argv) |word| {
        if (std.mem.indexOf(u8, word, "/Users/phall/workspace") != null) saw_cwd = true;
    }
    try testing.expect(saw_cwd);
    // The argv slices into MODEL-owned storage, which outlives the pane and
    // survives the Restart that re-reads it.
    try testing.expect(@intFromPtr(split_argv.ptr) >= @intFromPtr(&state.model.cwd_argv));
    try testing.expect(@intFromPtr(split_argv.ptr) < @intFromPtr(&state.model.cwd_argv) + @sizeOf(@TypeOf(state.model.cwd_argv)));

    // Turning the knob off restores the plain argv.
    state.model.config = app.parseConfig("inherit-working-directory = false");
    try state.dispatch(&harness.runtime, 1, .new_terminal);
    try testing.expectEqualSlices([]const u8, app.paneArgv(0), state.model.provider.slots[3].argv);
}

// GUARD: scrollback-limit-reaches-emulator
test "scrollback-limit reaches the emulator instead of being stored and ignored" {
    // The regression this pins: `scrollback_bytes` parsed and stored, while
    // the session was built from a comptime const that merely HAPPENED to
    // equal the config default. Anyone setting the knob got the default and
    // no indication of it.
    const gpa = testing.allocator;
    const small = try grid.Session.createWithScrollback(gpa, testing.io, 80, 24, 1024 * 1024);
    defer small.destroy();
    const large = try grid.Session.createWithScrollback(gpa, testing.io, 80, 24, 64 * 1024 * 1024);
    defer large.destroy();

    try testing.expect(small.term.screens.active.pages.maxSize() != large.term.screens.active.pages.maxSize());
    try testing.expect(small.term.screens.active.pages.maxSize() < large.term.screens.active.pages.maxSize());
}

test "a configured shell reaches the argv of every pane, old and new" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);

    // The pane that already exists is re-pointed, because the config cannot be
    // read until after the first pane is built.
    try testing.expect(state.model.provider.setShellCommand("/opt/homebrew/bin/fish"));
    const first = state.model.provider.slots[0].argv;
    try testing.expectEqualStrings("exec /opt/homebrew/bin/fish", first[first.len - 1]);

    // ...and so is every pane minted afterwards.
    try state.dispatch(&harness.runtime, 1, .new_terminal);
    const second = state.model.provider.slots[1].argv;
    try testing.expectEqualStrings("exec /opt/homebrew/bin/fish", second[second.len - 1]);
}

test "a rejected shell value leaves the built-in shell standing" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);

    // A NUL would be truncated at the C boundary, and an over-long value does
    // not fit the argv budget. Both degrade to the default shell rather than
    // to a pane that cannot open.
    try testing.expect(!state.model.provider.setShellCommand(""));
    try testing.expect(!state.model.provider.setShellCommand("/bin/sh\x00rm -rf /"));
    try testing.expect(!state.model.provider.setShellCommand("x" ** 4096));
    try testing.expectEqualSlices([]const u8, app.paneArgv(0), state.model.provider.slots[0].argv);
}

test "a command line with arguments survives as a command line" {
    // `command = tmux attach` has to keep its argument. Single-quoting the
    // whole value the way a working DIRECTORY is quoted would turn this into
    // one nonexistent program named "tmux attach".
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);

    try testing.expect(state.model.provider.setShellCommand("tmux attach"));
    const argv = state.model.provider.slots[0].argv;
    try testing.expectEqualStrings("exec tmux attach", argv[argv.len - 1]);
}

test "window-padding moves the content rect and the header band" {
    var tight: app.Model = .{ .provider = undefined, .config = app.parseConfig("window-padding = 0") };
    var loose: app.Model = .{ .provider = undefined, .config = app.parseConfig("window-padding = 24") };
    const size = geometry.SizeF.init(980, 640);
    const tight_chrome = app.workspaceChrome(&tight, size);
    const loose_chrome = app.workspaceChrome(&loose, size);

    try testing.expectEqual(@as(f32, 0), tight_chrome.header.x);
    try testing.expectEqual(@as(f32, 24), loose_chrome.header.x);
    try testing.expect(loose_chrome.content.width < tight_chrome.content.width);
    try testing.expectEqual(@as(f32, 980), tight_chrome.content.width);
}

test "hide-chrome-when-single = false keeps the strip on one healthy tab" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const state = try startCockpit(harness);
    defer stopCockpit(state);

    // The default is to hide: one healthy terminal shows no band at all.
    try testing.expect(!app.chromeRevealed(&state.model));
    state.model.config = app.parseConfig("hide-chrome-when-single = false");
    try testing.expect(app.chromeRevealed(&state.model));
    try testing.expect(app.workspaceChrome(&state.model, geometry.SizeF.init(980, 640)).header.height > 0);
}

test "one config diagnostic names the problem as well as the line" {
    // With a single problem there is room for what it was, so the band says it
    // rather than making the user go and look. A `missing_separator` has no key
    // to quote and must not print an empty pair of quotes.
    var model: app.Model = .{ .provider = undefined, .config = app.parseConfig("font-familly = Comic Mono") };
    var storage: [app.config_notice_bytes]u8 = undefined;
    try testing.expectEqualStrings(
        "Config line 1: unknown setting 'font-familly'",
        app.configNoticeLine(&model, &storage),
    );

    model.config = app.parseConfig("cursor-style\n");
    try testing.expectEqualStrings(
        "Config line 1: no '=' on this line",
        app.configNoticeLine(&model, &storage),
    );

    // A clean config says nothing at all, and no band exists to say it in.
    model.config = app.parseConfig("font-size = 14");
    try testing.expectEqualStrings("", app.configNoticeLine(&model, &storage));
    try testing.expect(!app.configNoticeRevealed(&model));
    try testing.expectEqual(
        @as(f32, 0),
        app.workspaceChrome(&model, geometry.SizeF.init(980, 640)).notice.height,
    );
}

test "tab-placement from the config file selects the side rail" {
    try testing.expectEqual(app.ConfigTabPlacement.side, app.parseConfig("tab-placement = sidebar").tab_placement);
    try testing.expectEqual(app.ConfigTabPlacement.top, app.parseConfig("").tab_placement);
}

test "a missing config file is a silent no-op with defaults" {
    // No bytes at all is the NORMAL case, not an error.
    const defaults = app.loadConfigOrDefault(null);
    try testing.expectEqual(@as(usize, 0), defaults.diagnostic_count);
    const pristine: app.Config = .{};
    try testing.expectEqual(pristine.font_size, defaults.font_size);
    try testing.expectEqual(pristine.window_padding, defaults.window_padding);

    // And an unresolvable directory (no HOME) yields no path rather than an
    // error the caller has to decide what to do about.
    var dir_storage: [512]u8 = undefined;
    var path_storage: [512]u8 = undefined;
    try testing.expectEqual(
        @as(?[]const u8, null),
        app.resolveConfigPath(.{}, null, &dir_storage, &path_storage),
    );
}

test "the config path is the app-dirs config directory, and the env override wins" {
    var dir_storage: [512]u8 = undefined;
    var path_storage: [512]u8 = undefined;
    const explicit = app.resolveConfigPath(
        .{ .home = "/Users/alice" },
        "/tmp/somewhere/else",
        &dir_storage,
        &path_storage,
    ) orelse return error.TestExpectedPath;
    try testing.expectEqualStrings("/tmp/somewhere/else", explicit);

    const resolved = app.resolveConfigPath(
        .{ .home = "/Users/alice" },
        null,
        &dir_storage,
        &path_storage,
    ) orelse return error.TestExpectedPath;
    try testing.expect(std.mem.startsWith(u8, resolved, "/Users/alice/"));
    try testing.expect(std.mem.endsWith(u8, resolved, "/config"));
}

test "Phux startup precedence treats empty environment values as unset" {
    const parsed = app.parseConfig(
        "phux-socket = /tmp/from-config.sock\n" ++
            "phux-session = config-session\n",
    );
    const overridden = app.resolvePhuxConfig(parsed, .{
        .socket = "/tmp/from-environment.sock",
        .session = "environment-session",
        .runtime_dir = "/run/user/501",
        .uid = "501",
    });
    try testing.expectEqualStrings("/tmp/from-environment.sock", overridden.phux_socket.slice());
    try testing.expectEqualStrings("environment-session", overridden.phux_session.slice());
    try testing.expectEqual(.environment, overridden.phux_socket_source);
    try testing.expectEqual(.environment, overridden.phux_session_source);

    const empty_environment = app.resolvePhuxConfig(parsed, .{
        .socket = "",
        .session = "",
        .runtime_dir = "/run/user/501",
        .uid = "501",
    });
    try testing.expectEqualStrings("/tmp/from-config.sock", empty_environment.phux_socket.slice());
    try testing.expectEqualStrings("config-session", empty_environment.phux_session.slice());
    try testing.expectEqual(.config, empty_environment.phux_socket_source);
    try testing.expectEqual(.config, empty_environment.phux_session_source);

    // A malformed debug override follows the other environment conventions:
    // it cannot replace a safe config value.
    const invalid_environment = app.resolvePhuxConfig(parsed, .{
        .socket = "relative.sock",
        .session = "bad\xffname",
    });
    try testing.expectEqualStrings("/tmp/from-config.sock", invalid_environment.phux_socket.slice());
    try testing.expectEqualStrings("config-session", invalid_environment.phux_session.slice());
}

test "Phux startup resolves a bounded local default without naming a session" {
    const xdg = app.resolvePhuxConfig(.{}, .{ .runtime_dir = "/run/user/501", .uid = "501" });
    try testing.expectEqualStrings("/run/user/501/phux/phux.sock", xdg.phux_socket.slice());
    try testing.expectEqualStrings("", xdg.phux_session.slice());
    try testing.expectEqual(.default, xdg.phux_socket_source);
    try testing.expectEqual(.default, xdg.phux_session_source);

    // Empty XDG_RUNTIME_DIR is unset, not the filesystem root.
    const fallback = app.resolvePhuxConfig(.{}, .{ .runtime_dir = "", .uid = "501" });
    try testing.expectEqualStrings("/tmp/phux-501/phux.sock", fallback.phux_socket.slice());
    try testing.expect(!std.mem.eql(u8, "/", fallback.phux_socket.slice()));

    const invalid_config = app.resolvePhuxConfig(
        app.parseConfig("phux-socket = relative.sock\nphux-session = bad\xffname"),
        .{ .uid = "501" },
    );
    try testing.expectEqualStrings("/tmp/phux-501/phux.sock", invalid_config.phux_socket.slice());
    try testing.expectEqualStrings("", invalid_config.phux_session.slice());
    try testing.expectEqual(@as(usize, 2), invalid_config.diagnostic_count);
}

test "Phux provider construction owns resolved socket and session bytes" {
    if (comptime !app.phux_enabled) return error.SkipZigTest;

    var resolved = app.resolvePhuxConfig(
        app.parseConfig(
            "phux-socket = /tmp/owned-at-construction.sock\n" ++
                "phux-session = owned-session\n",
        ),
        .{},
    );
    const provider = (try app.createPhuxProviderFromConfig(testing.allocator, testing.io, &resolved)) orelse
        return error.TestExpectedPhuxProvider;
    defer provider.destroy();

    try resolved.phux_socket.set("/tmp/mutated-after-construction.sock");
    try resolved.phux_session.set("mutated-session");
    try testing.expectEqualStrings("/tmp/owned-at-construction.sock", app.configuredPhuxSocket(provider));
    try testing.expectEqualStrings("owned-session", app.configuredPhuxSession(provider).?);
}
