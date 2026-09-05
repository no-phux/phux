//! ADVERSARIAL PROBES, written by a validator who did not build this.
//!
//! These do not trust the build agents' tests. Each one is designed to
//! FAIL if the two panes are secretly sharing emulator state, sharing a
//! display-list id namespace, running with an unbounded (0) command
//! budget, or routing input to the wrong pty.

const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const vt = @import("ghostty-vt");
const support = @import("support.zig");
const measured = @import("measured.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const TerminalApp = native_sdk.UiApp(app.Model, app.Msg);

const createSession = support.createSession;
const createSessions = support.createSessions;
const activeSlots = support.activeSlots;
const destroyModelSessions = app.deinitModel;

fn startCockpit(gpa: std.mem.Allocator, harness: anytype) !*TerminalApp {
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;
    harness.runtime.options.shortcuts = &app.cockpit_shortcuts;
    const session = try createSession(80, 24);
    const app_state = try gpa.create(TerminalApp);
    app_state.* = TerminalApp.init(std.heap.page_allocator, app.initialModel(session), app.appOptions());
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
    // Two TABS. The app opens with one terminal and mints the rest lazily,
    // so the second is created through the real message path.
    try app_state.dispatch(&harness.runtime, 1, .new_terminal);
    try app_state.dispatch(&harness.runtime, 1, .{ .select_position = 0 });
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .pointer_down,
        .x = 200,
        .y = 200,
    } });
    try app_state.effects.feedPtyOutput(app.ptyKey(0), "PANEALPHA\r\n");
    try app_state.effects.feedPtyOutput(app.ptyKey(1), "PANEBRAVO\r\n");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    return app_state;
}

// ------------------------------------------------------- A1: state sharing

test "ADVERSARIAL: a torrent into pane 0 leaves pane 1's emulator bit-identical" {
    // If the two panes shared one `vt.Terminal` (or one `*Session`), a
    // hundred lines fed to pane 0 would move pane 1's cursor, its
    // scrollback offset, and its viewport text. Nothing here reads a
    // pane index out of the painter: it compares pane 1 to itself.
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    const model = &app_state.model;

    // Distinct heap objects, distinct emulators, distinct pty keys.
    try testing.expect(model.provider.slots[0].session != model.provider.slots[1].session);
    try testing.expect(&model.provider.slots[0].session.term != &model.provider.slots[1].session.term);
    try testing.expect(&model.provider.slots[0].session.render != &model.provider.slots[1].session.render);
    try testing.expect(model.provider.slots[0].pty_key != model.provider.slots[1].pty_key);
    try testing.expect(model.provider.slots[0].session.response_buffer.ptr != model.provider.slots[1].session.response_buffer.ptr);

    const before_text = try model.provider.slots[1].session.plainText(gpa);
    defer gpa.free(before_text);
    const before_offset = model.provider.slots[1].session.scrollbar().offset;
    const before_bytes = model.provider.slots[1].output_bytes;
    const before_cols = model.provider.slots[1].session.cols();
    const before_rows = model.provider.slots[1].session.rows();

    var line: [64]u8 = undefined;
    for (0..300) |i| {
        const bytes = std.fmt.bufPrint(&line, "\x1b[3{d}mtorrent {d}\x1b[0m\r\n", .{ i % 8, i }) catch unreachable;
        try app_state.effects.feedPtyOutput(app.ptyKey(0), bytes);
        try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    }
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    // Pane 0 really did move.
    const alpha = try model.provider.slots[0].session.plainText(gpa);
    defer gpa.free(alpha);
    try testing.expect(std.mem.indexOf(u8, alpha, "torrent 299") != null);
    try testing.expect(model.provider.slots[0].session.scrollbar().offset > 0);

    // Pane 1 did not, in ANY observable dimension.
    const after_text = try model.provider.slots[1].session.plainText(gpa);
    defer gpa.free(after_text);
    try testing.expectEqualStrings(before_text, after_text);
    try testing.expectEqual(before_offset, model.provider.slots[1].session.scrollbar().offset);
    try testing.expectEqual(before_bytes, model.provider.slots[1].output_bytes);
    try testing.expectEqual(before_cols, model.provider.slots[1].session.cols());
    try testing.expectEqual(before_rows, model.provider.slots[1].session.rows());
    try testing.expect(std.mem.indexOf(u8, after_text, "torrent") == null);
    try testing.expect(std.mem.indexOf(u8, after_text, "PANEBRAVO") != null);
    // ...and pane 0 never learned pane 1's content either.
    try testing.expect(std.mem.indexOf(u8, alpha, "PANEBRAVO") == null);
}

test "ADVERSARIAL: scrollback search and selection remain terminal-local" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    const model = &app_state.model;

    for (0..48) |index| {
        var line_buffer: [64]u8 = undefined;
        const alpha = try std.fmt.bufPrint(&line_buffer, "alpha-history-{d}\r\n", .{index});
        try app_state.effects.feedPtyOutput(app.ptyKey(0), alpha);
        const bravo = try std.fmt.bufPrint(&line_buffer, "bravo-history-{d}\r\n", .{index});
        try app_state.effects.feedPtyOutput(app.ptyKey(1), bravo);
        try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    }
    try app_state.effects.feedPtyOutput(app.ptyKey(0), "ONLY_ALPHA_NEEDLE\r\n");
    try app_state.effects.feedPtyOutput(app.ptyKey(1), "ONLY_BRAVO_NEEDLE\r\n");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);

    const bravo_offset = model.provider.slots[1].session.scrollbar().offset;
    model.provider.slots[0].session.scrollLines(-6);
    try testing.expect(model.provider.slots[0].session.scrollbar().offset < bravo_offset);
    try testing.expectEqual(bravo_offset, model.provider.slots[1].session.scrollbar().offset);

    model.provider.slots[0].session.beginSelection(false);
    model.provider.slots[0].session.moveSelection(-4, 0, true);
    try testing.expect(model.provider.slots[0].session.selectionActive());
    try testing.expect(!model.provider.slots[1].session.selectionActive());

    var alpha_in_alpha = try vt.search.Active.init(gpa, "ONLY_ALPHA_NEEDLE");
    defer alpha_in_alpha.deinit();
    _ = try alpha_in_alpha.update(&model.provider.slots[0].session.term.screens.active.pages);
    try testing.expect(alpha_in_alpha.next() != null);

    var alpha_in_bravo = try vt.search.Active.init(gpa, "ONLY_ALPHA_NEEDLE");
    defer alpha_in_bravo.deinit();
    _ = try alpha_in_bravo.update(&model.provider.slots[1].session.term.screens.active.pages);
    try testing.expect(alpha_in_bravo.next() == null);

    var bravo_in_bravo = try vt.search.Active.init(gpa, "ONLY_BRAVO_NEEDLE");
    defer bravo_in_bravo.deinit();
    _ = try bravo_in_bravo.update(&model.provider.slots[1].session.term.screens.active.pages);
    try testing.expect(bravo_in_bravo.next() != null);

    model.provider.slots[0].session.clearSelection();
    try testing.expect(!model.provider.slots[0].session.selectionActive());
    try testing.expect(!model.provider.slots[1].session.selectionActive());

    // The probes above drive `vt.search.Active` by hand against each
    // emulator, which proves the SCREENS are separate but says nothing about
    // the product feature. Now that cmd+F exists, the same claim has to hold
    // for the search STATE a terminal carries: an open field, its needle, its
    // match set, and its current match are per terminal, and none of them
    // follow focus to the neighbour.
    const alpha_session = model.provider.slots[0].session;
    const bravo_session = model.provider.slots[1].session;
    alpha_session.searchOpen();
    try testing.expect(alpha_session.searchInput("ONLY_ALPHA_NEEDLE"));
    try testing.expect(alpha_session.searchMatchCount() > 0);
    try testing.expect(alpha_session.searchMatchOrdinal() > 0);
    try testing.expect(!bravo_session.search.open);
    try testing.expectEqualStrings("", bravo_session.searchNeedle());
    try testing.expectEqual(@as(usize, 0), bravo_session.searchMatchCount());

    // The neighbour searching for the SAME needle finds nothing, and finding
    // nothing does not disturb the terminal that did find something.
    bravo_session.searchOpen();
    try testing.expect(bravo_session.searchInput("ONLY_ALPHA_NEEDLE"));
    try testing.expectEqual(@as(usize, 0), bravo_session.searchMatchCount());
    try testing.expect(alpha_session.searchMatchCount() > 0);

    // Closing one field closes exactly one.
    bravo_session.searchClose();
    try testing.expect(!bravo_session.search.open);
    try testing.expect(alpha_session.search.open);
    try testing.expectEqualStrings("ONLY_ALPHA_NEEDLE", alpha_session.searchNeedle());
}

test "ADVERSARIAL: a hard reset of pane 0 does not reset pane 1" {
    // `spawnPane` calls `session.reset()`. Shared emulator state would
    // blank the neighbour.
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    const model = &app_state.model;

    try app_state.effects.feedPtyExit(app.ptyKey(0), 0, 0, .spawn_failed, 0); // a spawn failure: the one end that leaves the pane standing
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try testing.expectEqual(app.Phase.failed, model.provider.slots[0].phase);
    try testing.expectEqual(app.Phase.live, model.provider.slots[1].phase);

    // cmd+R restarts the FOCUSED (ended) pane; `spawnPane` hard-resets
    // its emulator. A shared emulator would blank the neighbour too.
    try pressKey(harness, app_iface, "r", .{ .primary = true });
    const alpha = try model.provider.slots[0].session.plainText(gpa);
    defer gpa.free(alpha);
    const bravo = try model.provider.slots[1].session.plainText(gpa);
    defer gpa.free(bravo);
    try testing.expect(std.mem.indexOf(u8, alpha, "PANEALPHA") == null); // reset
    try testing.expect(std.mem.indexOf(u8, bravo, "PANEBRAVO") != null); // untouched
}

// ---------------------------------------------- A2: id namespace collisions

/// Every object id in the list, checked for repeats. The retained diff
/// rejects a repeated id, so a collision is a whole failed frame.
fn expectNoDuplicateIds(gpa: std.mem.Allocator, commands: []const canvas.CanvasCommand) !void {
    var seen = std.AutoHashMap(canvas.ObjectId, usize).init(gpa);
    defer seen.deinit();
    for (commands, 0..) |command, index| {
        const id = command.objectId() orelse continue;
        const gop = try seen.getOrPut(id);
        if (gop.found_existing) {
            std.debug.print(
                "DUPLICATE ID 0x{x} at command {d} (first seen at {d})\n",
                .{ id, index, gop.value_ptr.* },
            );
            return error.DuplicateObjectId;
        }
        gop.value_ptr.* = index;
    }
}

test "ADVERSARIAL: hostile terminal tab switches retain collision-free ids" {
    // Each selected terminal drives every id-emitting path, then the
    // retained diff switches between their disjoint id namespaces.
    const gpa = testing.allocator;
    const sessions = try createSessions(120, 40);
    defer for (sessions) |each| each.destroy();

    var line: [4096]u8 = undefined;
    for (sessions, 0..) |session, pane| {
        for (0..200) |row| {
            var w: usize = 0;
            for (0..40) |col| {
                // Underline + strike + a distinct SGR per cell, then box
                // glyphs, so the run merger cannot collapse anything.
                const seq = std.fmt.bufPrint(line[w..], "\x1b[4;9;3{d}m\u{256C}\x1b[0m\u{2500}A", .{(col + row + pane) % 8}) catch break;
                w += seq.len;
            }
            session.feed(line[0..w]);
            session.feed("\r\n");
        }
        // Scrolled back, so the scrollbar thumb paints too.
        session.scrollLines(-5);
        session.beginSelection(false);
        session.moveSelection(20, 3, true);
    }

    var storage: [2][]canvas.CanvasCommand = undefined;
    var allocated: usize = 0;
    defer for (storage[0..allocated]) |commands| gpa.free(commands);
    var lists: [2]canvas.DisplayList = undefined;
    for (sessions, 0..) |session, index| {
        storage[index] = try gpa.alloc(canvas.CanvasCommand, 32 * 1024);
        allocated += 1;
        var builder = canvas.Builder.init(storage[index]);
        try grid.paint(session, &builder, .{
            .frame = geometry.RectF.init(226, 60, 746, 572),
            .background_frame = geometry.RectF.init(0, 0, 980, 640),
            .tokens = .{},
            .running = true,
            .focused = true,
            .selecting = true,
            .id_base = grid.paneIdBase(index),
        });
        lists[index] = builder.displayList();
        try testing.expect(lists[index].commands.len > 2000);
        try expectNoDuplicateIds(gpa, lists[index].commands);
    }
    const changes = try gpa.alloc(canvas.DiffChange, 64 * 1024);
    defer gpa.free(changes);
    _ = try canvas.DisplayList.diff(lists[0], lists[1], changes);
}

test "ADVERSARIAL: each selected terminal frame has unique ids and no hidden grid band" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    var line: [64]u8 = undefined;
    for (0..60) |i| {
        const bytes = std.fmt.bufPrint(&line, "\x1b[4;3{d}mrow {d} \u{2500}\u{256C}\x1b[0m\r\n", .{ i % 8, i }) catch unreachable;
        // Drain each batch: the fake pty's queue is short.
        try app_state.effects.feedPtyOutput(app.ptyKey(0), bytes);
        try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
        try app_state.effects.feedPtyOutput(app.ptyKey(1), bytes);
        try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    }
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    const stride: u64 = 1 << 24;
    const base0 = canvas.terminal_grid.paintIdBase(grid.paneIdBase(0));
    const base1 = canvas.terminal_grid.paintIdBase(grid.paneIdBase(1));
    for ([_]struct { key: []const u8, selected_base: u64, hidden_base: u64 }{
        .{ .key = "1", .selected_base = base0, .hidden_base = base1 },
        .{ .key = "2", .selected_base = base1, .hidden_base = base0 },
    }) |case| {
        try pressKey(harness, app_iface, case.key, .{ .primary = true });
        try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
        const list = harness.runtime.views[0].canvasDisplayList();
        try testing.expect(list.commands.len > 100);
        try expectNoDuplicateIds(gpa, list.commands);
        var selected_commands: usize = 0;
        var hidden_commands: usize = 0;
        for (list.commands) |command| {
            const id = command.objectId() orelse continue;
            if (id >= case.selected_base and id < case.selected_base + stride) selected_commands += 1;
            if (id >= case.hidden_base and id < case.hidden_base + stride) hidden_commands += 1;
        }
        try testing.expect(selected_commands > 20);
        try testing.expectEqual(@as(usize, 0), hidden_commands);
    }
}

test "ADVERSARIAL: split paints both hostile terminals inside one collision-free envelope" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(1100, 640) });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    feedHostileRows(app_state.model.provider.slots[0].session, 58, 40);
    for (0..40) |_| {
        app_state.model.provider.slots[1].session.feed("\u{256C}" ** 58);
        app_state.model.provider.slots[1].session.feed("\r\n");
    }
    // Cmd+D mints a THIRD terminal beside terminal 1 in the same tab. The id
    // namespace is keyed by the REGISTRY SLOT, not by a pane index, so the
    // check reads the slots the resolved panes actually occupy.
    try pressKey(harness, app_iface, "d", .{ .primary = true });
    var resolved: [app.max_panes_per_tab]app.LayoutPane = undefined;
    const resolved_count = app.resolvePanes(&app_state.model, geometry.SizeF.init(980, 640), &resolved);
    try testing.expectEqual(@as(usize, 2), resolved_count);
    // Feed the pane the split actually created. Pane 0 already carries the
    // hostile rows; the new pane gets the dense box-glyph rows, so the two
    // halves of the shared envelope are both genuinely exercised.
    const created = app_state.model.provider.terminal(resolved[1].terminal) orelse return error.TestExpectedTerminal;
    for (0..40) |_| {
        try app_state.effects.feedPtyOutput(created.pty_key, "\u{256C}" ** 58 ++ "\r\n");
    }
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    const list = harness.runtime.views[0].canvasDisplayList();
    try expectNoDuplicateIds(gpa, list.commands);
    try testing.expect(list.commands.len <= native_sdk.runtime.max_canvas_commands_per_view);

    const stride: u64 = 1 << 24;
    var per_pane: [2]usize = @splat(0);
    for (list.commands) |command| {
        const id = command.objectId() orelse continue;
        for (&per_pane, 0..) |*count, index| {
            const slot = app_state.model.provider.slotIndex(resolved[index].terminal) orelse continue;
            const base = canvas.terminal_grid.paintIdBase(grid.paneIdBase(slot));
            if (id >= base and id < base + stride) count.* += 1;
        }
    }
    // Both panes own commands in their OWN namespace: that half of the
    // claim is unchanged. What is no longer a CONTENT measure is HOW
    // MANY — a screen is one packed `cell_grid` command per painted ROW
    // now, so the count tracks a pane's height and its box geometry, and
    // a dense truecolor pane costs exactly what a blank one of the same
    // height costs.
    try testing.expect(per_pane[0] > 0);
    try testing.expect(per_pane[1] > 0);

    // "Both terminals genuinely painted a lot" is therefore re-pinned on
    // what still scales with content, which is now split by KIND: a
    // glyph screen's ink is inked CELLS in that pane's own lattice, and
    // a box-drawing screen's ink is geometry COMMANDS in that pane's own
    // namespace (box cells deliberately carry no cluster — their ink is
    // exact rects at cell bounds). Pane 0 here is the hostile ANSI
    // screen and pane 1 the dense box screen, so the two halves of the
    // envelope are measured by the channel each one actually uses. A
    // pane that painted nothing, or that painted into its neighbour's
    // namespace, fails here exactly as it used to fail on the raw
    // command count.
    var pane_rows: usize = 0;
    for (0..2) |index| {
        const slot = app_state.model.provider.slotIndex(resolved[index].terminal) orelse
            return error.TestExpectedTerminal;
        const view = try support.expectPaneCellGrid(list, slot);
        try testing.expect(view.rows() > 0);
        try testing.expect(view.inkedCells() + per_pane[index] > 20);
        pane_rows += view.rows();
    }

    // The two panes' row commands PARTITION the frame's grids: every
    // `cell_grid` in the list belongs to exactly one of their
    // namespaces, none to both, none to neither. That is what makes a
    // per-pane screen readable out of a shared display list at all, and
    // it is a sharper form of the old hidden-band claim — a third
    // namespace (a hidden tab still painting, a pane painting under a
    // stale id) shows up here as a grid neither pane owns.
    var grid_commands: usize = 0;
    for (list.commands) |command| {
        if (command == .cell_grid) grid_commands += 1;
    }
    try testing.expectEqual(grid_commands, pane_rows);
}

// --------------------------------------------------- A3/A4: budget reality

/// Rows the pane actually put on screen this paint.
///
/// A screen paints as one packed `cell_grid` command PER ROW, and
/// `CellGridView` aggregates the rows of one pane's namespace back into
/// a screen — so the painted-row count is read straight off the lattice
/// instead of inferred from distinct text-run baselines. Same number,
/// exact instead of derived. Reading one row command's `rows` field
/// would report 1 for every screen, which is precisely the mistake this
/// seam exists to prevent.
fn paintedRows(display_list: canvas.DisplayList) usize {
    const view = support.findCellGrid(display_list) orelse return 0;
    return view.rows();
}

fn feedBoxRows(session: *grid.Session, cols: usize, rows: usize) void {
    for (0..rows) |_| {
        for (0..cols) |_| session.feed("\u{256C}");
        session.feed("\r\n");
    }
}

fn feedHostileRows(session: *grid.Session, cols: usize, rows: usize) void {
    var line: [8192]u8 = undefined;
    for (0..rows) |_| {
        var w: usize = 0;
        for (0..cols) |col| {
            const seq = std.fmt.bufPrint(line[w..], "\x1b[{d}mX", .{if (col % 2 == 0) @as(u8, 31) else 32}) catch break;
            w += seq.len;
        }
        session.feed(line[0..w]);
        session.feed("\r\n");
    }
}

/// The true worst case for a display-list painter: a distinct truecolor
/// foreground AND background on every cell, so no two neighbours can merge
/// into one run and each cell costs its own background rect plus its own
/// text run.
///
/// `feedHostileRows` alternates two ANSI colors, which used to overflow the
/// envelope comfortably — back when every cell of it cost a background rect
/// and a text run. Nothing about a colour costs a COMMAND any more: the
/// whole row is one packed `cell_grid`, so this screen is priced in cells
/// and could never demonstrate a command bound again. The rule it was
/// written for still holds and now runs the other way: density has to
/// outrun whichever ceiling a test is measuring, so the command envelope is
/// exercised with BOX drawing (the one ink the lattice cannot carry) and
/// this truecolor screen is what exercises the CELL store.
fn feedTruecolorRows(session: *grid.Session, cols: usize, rows: usize) void {
    var line: [8192]u8 = undefined;
    for (0..rows) |row| {
        var w: usize = 0;
        for (0..cols) |col| {
            const tint: u8 = @intCast((row * cols + col) % 251);
            const seq = std.fmt.bufPrint(
                line[w..],
                "\x1b[38;2;{d};{d};{d}m\x1b[48;2;{d};{d};{d}mX",
                .{ tint, 255 - tint, tint / 2, 255 - tint, tint, tint / 3 },
            ) catch break;
            w += seq.len;
        }
        session.feed(line[0..w]);
        session.feed("\r\n");
    }
}

test "ADVERSARIAL: the selected terminal command envelope genuinely binds" {
    // A budget that was secretly 0 (unbounded) would still pass a test
    // that only asserts `len <= budget` on a quiet screen. This compares
    // the SAME hostile screen painted bounded vs unbounded: if the bound
    // did nothing, the two lists would be the same length.
    //
    // The SCREEN had to change, following this file's own stated rule
    // that density has to outrun the ceiling for the assertion to keep
    // meaning what it says. A truecolor screen no longer costs commands
    // by DENSITY at all — a whole row of it is one packed `cell_grid`
    // command, so bounded and unbounded would differ only by however
    // many rows fit, and a 40-row screen fits either way. Box drawing is
    // what still prices in COMMANDS (it renders as exact geometry at
    // cell bounds, never glyphs), so that is the screen the command
    // envelope is measured against. The cell budget, which is what the
    // truecolor screen actually spends, is pinned by its own test below.
    const gpa = testing.allocator;
    const cols = 60;
    const rows = 40;
    const session = try grid.Session.create(std.heap.page_allocator, testing.io, cols, rows);
    defer session.destroy();
    feedBoxRows(session, cols, rows);

    // The anti-tautology check, re-derived from the SDK's own price for
    // this glyph rather than from a remembered number: one U+256C costs
    // `maxCommands` commands, a row costs that per column plus its own
    // grid command, and the screen has to cost more than the envelope
    // for "the bound binds" to be demonstrable at all. The envelope
    // itself moved under this test (the SDK's per-view ceiling went back
    // from 4096 to 2048 once terminals stopped costing a command per
    // run), which is exactly the kind of move this expression survives
    // and a hardcoded one would not.
    const row_command_cost = cols * canvas.terminal_box.maxCommands(0x256C) + 1;
    try testing.expect(rows * row_command_cost > app.chrome_command_envelope);

    const big = try gpa.alloc(canvas.CanvasCommand, 32 * 1024);
    defer gpa.free(big);

    var unbounded = canvas.Builder.init(big);
    try grid.paint(session, &unbounded, .{
        .frame = geometry.RectF.init(0, 0, 480, 800),
        .tokens = .{},
        .running = true,
        .selecting = false,
        .id_base = grid.paneIdBase(0),
    });
    const unbounded_len = unbounded.displayList().commands.len;
    const unbounded_rows = paintedRows(unbounded.displayList());

    const small = try gpa.alloc(canvas.CanvasCommand, 32 * 1024);
    defer gpa.free(small);
    var bounded = canvas.Builder.init(small);
    try grid.paint(session, &bounded, .{
        .frame = geometry.RectF.init(0, 0, 480, 800),
        .tokens = .{},
        .running = true,
        .selecting = false,
        .command_budget = app.chrome_command_envelope,
        .text_reserve = canvas.terminal_grid.widget_text_reserve,
        .glyph_budget = canvas.terminal_grid.widget_glyph_budget,
        .id_base = grid.paneIdBase(0),
    });
    const bounded_len = bounded.displayList().commands.len;
    const bounded_rows = paintedRows(bounded.displayList());

    measured.print(
        "\nMEASURED budget bind: unbounded={d}/{d} rows bounded={d}/{d} rows budget={d}\n",
        .{ unbounded_len, unbounded_rows, bounded_len, bounded_rows, app.chrome_command_envelope },
    );
    try testing.expect(app.chrome_command_envelope > 0);
    try testing.expect(unbounded_len > app.chrome_command_envelope); // the screen CAN overflow
    try testing.expect(bounded_len < unbounded_len); // the bound truncated it
    try testing.expect(bounded_len <= app.chrome_command_envelope);
    // ...and it cost real SCREEN, not just commands: fewer rows reached
    // the glass under the bound than without it.
    try testing.expect(bounded_rows < unbounded_rows);

    // Truncation must be LOUD. Rows dropped off the bottom of the glass used
    // to be indistinguishable from a short screen: the frame presented
    // successfully and the missing rows were bare background. The painter now
    // records the loss on the builder, which is the only way a caller can
    // tell "the shell printed 40 rows and you are seeing all of them" from
    // "you are seeing the first 30".
    const loss = bounded.degradation orelse return error.TruncationWentUnreported;
    try testing.expectEqual(canvas.DisplayListStore.commands, loss.store);
    try testing.expectEqual(bounded_rows, loss.produced);
    try testing.expect(loss.produced < loss.requested);
    // The whole screen was asked for, and what survived is what the
    // envelope can actually hold at this row cost — both sides derived,
    // so a painter that dropped rows for some unrelated reason cannot
    // satisfy this by dropping MORE of them.
    try testing.expectEqual(@as(usize, rows), loss.requested);
    try testing.expect(bounded_rows <= app.chrome_command_envelope / row_command_cost);

    // ...and the unbounded paint of the same screen reports nothing, so the
    // signal tracks real loss rather than firing on every dense frame.
    try testing.expectEqual(@as(?canvas.DisplayListDegradation, null), unbounded.degradation);
}

test "ADVERSARIAL: the packed cell budget genuinely binds a dense screen" {
    // The other half of the split above, and the budget that actually
    // bounds a terminal now: a screen costs CELLS, 20 bytes each, and
    // the frame's cell store is what a split has to divide between
    // panes. The same anti-tautology discipline applies — the truecolor
    // screen is painted with the whole store and again with only ten
    // rows' worth of it left unreserved, and the bound has to be visible
    // in the painted rows, not merely satisfied.
    const gpa = testing.allocator;
    const cols = 60;
    const rows = 40;
    // The rows the reserve leaves room for. Everything below derives
    // from it and from the SDK's store size, so the numbers stay
    // meaningful if either the store or the cell size moves.
    const affordable_rows = 10;
    const session = try grid.Session.create(std.heap.page_allocator, testing.io, cols, rows);
    defer session.destroy();
    feedTruecolorRows(session, cols, rows);

    const big = try gpa.alloc(canvas.CanvasCommand, 32 * 1024);
    defer gpa.free(big);
    var unbounded = canvas.Builder.init(big);
    try grid.paint(session, &unbounded, .{
        .frame = geometry.RectF.init(0, 0, 480, 600),
        .tokens = .{},
        .running = true,
        .selecting = false,
        .id_base = grid.paneIdBase(0),
    });
    const unbounded_rows = paintedRows(unbounded.displayList());

    const small = try gpa.alloc(canvas.CanvasCommand, 32 * 1024);
    defer gpa.free(small);
    var bounded = canvas.Builder.init(small);
    try grid.paint(session, &bounded, .{
        .frame = geometry.RectF.init(0, 0, 480, 600),
        .tokens = .{},
        .running = true,
        .selecting = false,
        // Exactly `affordable_rows` rows' worth of store left for this
        // pane; the rest is reserved for the panes painting after it.
        .cell_reserve = canvas.max_display_list_cells - affordable_rows * cols,
        .id_base = grid.paneIdBase(0),
    });
    const bounded_rows = paintedRows(bounded.displayList());

    measured.print(
        "\nMEASURED cell bind: unbounded={d} rows bounded={d} rows store={d} cells\n",
        .{ unbounded_rows, bounded_rows, canvas.max_display_list_cells },
    );
    try testing.expect(unbounded_rows > affordable_rows); // the screen CAN outgrow the reserve
    try testing.expect(bounded_rows < unbounded_rows); // the bound truncated it
    try testing.expectEqual(@as(usize, affordable_rows), bounded_rows);

    // Loud, by the store that caused it.
    const loss = bounded.degradation orelse return error.TruncationWentUnreported;
    try testing.expectEqual(canvas.DisplayListStore.cells, loss.store);
    try testing.expectEqual(bounded_rows, loss.produced);
    try testing.expect(loss.produced < loss.requested);
    try testing.expectEqual(@as(?canvas.DisplayListDegradation, null), unbounded.degradation);
}

test "ADVERSARIAL: either selected tab gets the same full hostile-content budget" {
    const gpa = testing.allocator;
    const sessions = try createSessions(58, 40);
    defer for (sessions) |each| each.destroy();
    for (sessions) |session| feedHostileRows(session, 58, 40);

    const own = try createSession(58, 40);
    var model = app.initialModel(own);
    defer app.deinitModel(&model);
    // A second TAB, so selecting either one hands the whole content area to
    // its single pane.
    const second = try model.provider.createTerminal();
    _ = model.admitTab(second.id);
    var painted: [2]usize = @splat(0);
    var used: [2]usize = @splat(0);
    for (sessions, 0..) |session, index| {
        try testing.expect(model.selectTab(index));
        const frames = app.paneFrames(&model, geometry.SizeF.init(980, 640));
        // The SELECTED tab's only pane owns the full content area, whichever
        // tab it is: the budget can never depend on which one you picked.
        try testing.expect(frames[0].width > 0);
        try testing.expectEqual(@as(f32, 0), frames[1].width);

        const storage = try gpa.alloc(canvas.CanvasCommand, 32 * 1024);
        defer gpa.free(storage);
        var builder = canvas.Builder.init(storage);
        try grid.paint(session, &builder, .{
            .frame = frames[0],
            .background_frame = frames[0],
            .tokens = .{},
            .running = true,
            .focused = true,
            .selecting = false,
            .command_budget = app.chrome_command_envelope,
            .text_reserve = canvas.terminal_grid.widget_text_reserve,
            .glyph_budget = canvas.terminal_grid.widget_glyph_budget,
            .id_base = grid.paneIdBase(index),
        });
        used[index] = builder.displayList().commands.len;
        painted[index] = paintedRows(builder.displayList());
        try testing.expect(used[index] <= app.chrome_command_envelope);
        // The command count is only comparable across the two tabs
        // because a command IS a painted row now (plus the paint's fixed
        // prologue and epilogue). Pinning that relation is what keeps
        // "both tabs spent the same" from being satisfiable by two panes
        // that painted different screens into the same overhead.
        try testing.expect(used[index] >= painted[index]);
        try testing.expect(used[index] <= painted[index] + support.paint_fixed_commands);
        // Whichever tab is selected, nothing of its screen was lost to a
        // budget — the symmetry claim now covers the truncation report
        // too, so "both tabs painted the same amount" cannot be
        // satisfied by both of them losing the same rows.
        try testing.expectEqual(@as(?canvas.DisplayListDegradation, null), builder.degradation);
    }
    measured.print(
        "\nMEASURED selected symmetry: rows={{ {d}, {d} }} commands={{ {d}, {d} }} envelope={d}\n",
        .{ painted[0], painted[1], used[0], used[1], app.chrome_command_envelope },
    );
    try testing.expect(painted[0] >= 5);
    try testing.expectEqual(painted[0], painted[1]);
    try testing.expectEqual(used[0], used[1]);
}

// ------------------------------------------------------- A5/A6: input paths

fn pressKey(harness: anytype, app_iface: anytype, key: []const u8, modifiers: native_sdk.platform.ShortcutModifiers) !void {
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = key,
        .modifiers = modifiers,
    } });
}

test "ADVERSARIAL: encoded KEYS (not just text) reach one pty only" {
    // The build agents' acceptance gate uses `.text_input`, which takes
    // the `sendCommittedText` path. Specials and chords take a different
    // path (`encodeKeyEvent`) through the emulator's own encoder, from
    // the SESSION of whichever pane is focused. A shared session would
    // put the bytes on the wrong stream.
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    try pressKey(harness, app_iface, "arrowup", .{});
    try pressKey(harness, app_iface, "tab", .{});
    const alpha_after_keys = app_state.effects.ptyWrittenBytes(app.ptyKey(0));
    try testing.expect(alpha_after_keys.len > 0);
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));

    // Move focus and repeat: the bytes must switch streams entirely.
    try pressKey(harness, app_iface, "2", .{ .primary = true });
    try testing.expect(app_state.model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    try pressKey(harness, app_iface, "arrowup", .{});
    try testing.expect(app_state.effects.ptyWrittenBytes(app.ptyKey(1)).len > 0);
    // Pane 0's stream did not grow.
    try testing.expectEqualStrings(alpha_after_keys, app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
}

test "ADVERSARIAL: the focus chord itself never leaks a byte to either child" {
    const gpa = testing.allocator;
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Press AND release under kitty release reporting. The release omits
    // Command to model the modifier coming up first; the model-level
    // physical-key latch must still recognize and swallow it.
    for (activeSlots(&app_state.model)) |*pane| pane.session.feed("\x1b[>11u");
    try pressKey(harness, app_iface, "2", .{ .primary = true });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_up,
        .key = "2",
        .modifiers = .{},
    } });
    measured.print(
        "\nMEASURED chord leak: pane0={d} bytes pane1={d} bytes\n",
        .{ app_state.effects.ptyWrittenBytes(app.ptyKey(0)).len, app_state.effects.ptyWrittenBytes(app.ptyKey(1)).len },
    );
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(0)));
    try testing.expectEqualStrings("", app_state.effects.ptyWrittenBytes(app.ptyKey(1)));
}

test "ADVERSARIAL: a wheel over native tab chrome reaches neither terminal" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    const model = &app_state.model;

    var line: [32]u8 = undefined;
    for (0..200) |i| {
        const bytes = std.fmt.bufPrint(&line, "history {d}\r\n", .{i}) catch unreachable;
        model.provider.slots[0].session.feed(bytes);
        model.provider.slots[1].session.feed(bytes);
    }
    for (2..8) |frame_index| {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
            .label = app.canvas_label,
            .size = size,
            .scale_factor = 2,
            .frame_index = @intCast(frame_index),
            .timestamp_ns = @as(u64, frame_index) * 1_000_000,
        } });
    }
    try testing.expect(model.ws().surface_size.width > 0);

    const bottom0 = model.provider.slots[0].session.scrollbar().offset;
    const bottom1 = model.provider.slots[1].session.scrollbar().offset;
    try testing.expect(bottom0 > 0 and bottom1 > 0);
    try testing.expect(model.selectedTerminalRef().?.eql(app.initialTerminalRef(0)));
    const cell_h = model.provider.slots[0].session.measuredCell().?.height;
    for (0..6) |_| {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .scroll,
            .x = 100,
            .y = 30,
            .delta_y = cell_h,
        } });
    }
    measured.print(
        "\nMEASURED rail wheel isolation: pane0 {d}->{d}  pane1 {d}->{d}\n",
        .{ bottom0, model.provider.slots[0].session.scrollbar().offset, bottom1, model.provider.slots[1].session.scrollbar().offset },
    );
    try testing.expectEqual(bottom0, model.provider.slots[0].session.scrollbar().offset);
    try testing.expectEqual(bottom1, model.provider.slots[1].session.scrollbar().offset);
}

test "ADVERSARIAL: split wheel routing scrolls only the pane under the pointer" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    const model = &app_state.model;

    for (activeSlots(model)) |*pane| {
        var line: [32]u8 = undefined;
        for (0..200) |i| pane.session.feed(std.fmt.bufPrint(&line, "history {d}\r\n", .{i}) catch unreachable);
    }
    try pressKey(harness, app_iface, "d", .{ .primary = true });
    for (2..8) |frame_index| {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
            .label = app.canvas_label,
            .size = size,
            .scale_factor = 2,
            .frame_index = @intCast(frame_index),
            .timestamp_ns = @as(u64, frame_index) * 1_000_000,
        } });
    }
    // Cmd+D made a THIRD terminal beside terminal 1. Address the two panes
    // by what `resolvePanes` says is there, not by registry slot.
    var resolved: [app.max_panes_per_tab]app.LayoutPane = undefined;
    const resolved_count = app.resolvePanes(model, size, &resolved);
    try testing.expectEqual(@as(usize, 2), resolved_count);
    const left = model.provider.terminal(resolved[0].terminal) orelse return error.TestExpectedTerminal;
    const right = model.provider.terminal(resolved[1].terminal) orelse return error.TestExpectedTerminal;
    var filler: [32]u8 = undefined;
    for (0..200) |i| right.session.feed(std.fmt.bufPrint(&filler, "history {d}\r\n", .{i}) catch unreachable);
    const bottom0 = left.session.scrollbar().offset;
    const bottom1 = right.session.scrollbar().offset;
    try testing.expect(bottom0 > 0 and bottom1 > 0);

    for (0..4) |_| {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .scroll,
            .x = resolved[1].rect.x + resolved[1].rect.width / 2,
            .y = resolved[1].rect.y + resolved[1].rect.height / 2,
            .delta_y = right.session.measuredCell().?.height,
        } });
    }
    try testing.expectEqual(bottom0, left.session.scrollbar().offset);
    try testing.expect(right.session.scrollbar().offset < bottom1);
}

test "REGRESSION: the selected Terminal 2 frame receives wheel input before the first frame" {
    const gpa = testing.allocator;
    const size = geometry.SizeF.init(1100, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();
    const model = &app_state.model;

    // The mirror is live before any frame, and the rects it yields are real.
    try testing.expectEqual(size.width, model.ws().surface_size.width);
    try testing.expectEqual(size.height, model.ws().surface_size.height);
    try pressKey(harness, app_iface, "2", .{ .primary = true });
    try testing.expect(model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    // Only the SELECTED tab resolves a pane at all, and it is the whole
    // content area — resolve order, not attachment slot.
    try testing.expect(app.paneFrames(model, model.ws().surface_size)[0].width > 0);
    try testing.expectEqual(@as(f32, 0), app.paneFrames(model, model.ws().surface_size)[1].width);

    var line: [32]u8 = undefined;
    for (0..200) |i| {
        const bytes = std.fmt.bufPrint(&line, "history {d}\r\n", .{i}) catch unreachable;
        model.provider.slots[0].session.feed(bytes);
        model.provider.slots[1].session.feed(bytes);
    }
    const bottom0 = model.provider.slots[0].session.scrollbar().offset;
    const bottom1 = model.provider.slots[1].session.scrollbar().offset;

    // Aim squarely at the selected Terminal 2 surface.
    try testing.expect(model.selectedTerminalRef().?.eql(app.initialTerminalRef(1)));
    const target = app.paneFrames(model, model.ws().surface_size)[0];
    const cell_h = model.provider.slots[1].session.measuredCell().?.height;
    for (0..6) |_| {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = app.canvas_label,
            .kind = .scroll,
            .x = target.x + target.width / 2,
            .y = target.y + target.height / 2,
            .delta_y = cell_h,
        } });
    }
    // Terminal 2 scrolled; hidden Terminal 1 did not.
    try testing.expect(model.provider.slots[1].session.scrollbar().offset < bottom1);
    try testing.expectEqual(bottom0, model.provider.slots[0].session.scrollbar().offset);
}

// -------------------------------------------------- A7: the validator's eyes

const validator_shot_path = "zig-out/validator-native-split.png";

test "ADVERSARIAL: native-split proof shot preserves both independent terminals (env-gated)" {
    if (comptime !@import("builtin").link_libc) return error.SkipZigTest;
    if (std.c.getenv("COCKPIT_SHOTS") == null) return error.SkipZigTest;
    const gpa = testing.allocator;
    const io = testing.io;
    const size = geometry.SizeF.init(980, 640);
    const harness = try native_sdk.TestHarness().create(gpa, .{ .size = size });
    defer harness.destroy(gpa);
    const app_state = try startCockpit(gpa, harness);
    defer gpa.destroy(app_state);
    defer destroyModelSessions(&app_state.model);
    defer app_state.deinit();
    const app_iface = app_state.app();

    // Let the frame pump measure the surface so the panes take their
    // real grids.
    for (2..8) |frame_index| {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
            .label = app.canvas_label,
            .size = size,
            .scale_factor = 2,
            .frame_index = @intCast(frame_index),
            .timestamp_ns = @as(u64, frame_index) * 1_000_000,
        } });
    }

    var line: [128]u8 = undefined;
    for (0..14) |i| {
        try app_state.effects.feedPtyOutput(app.ptyKey(0), std.fmt.bufPrint(
            &line,
            "\x1b[32mLEFT\x1b[0m  build step {d}/14 \u{2500}\u{2500}\u{2500} ok\r\n",
            .{i + 1},
        ) catch unreachable);
        try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
        try app_state.effects.feedPtyOutput(app.ptyKey(1), std.fmt.bufPrint(
            &line,
            "\x1b[1;35mRIGHT\x1b[0m Tue Jul 27 04:0{d}:00 PDT 2026\r\n",
            .{i % 10},
        ) catch unreachable);
        try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    }
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    // Both streams landed on their own emulator while only one surface
    // was visible, and neither saw the other's marker.
    const left = app_state.model.provider.slots[0].session.screenText();
    const right = app_state.model.provider.slots[1].session.screenText();
    try testing.expect(std.mem.indexOf(u8, left, "build step 14/14") != null);
    try testing.expect(std.mem.indexOf(u8, left, "RIGHT") == null);
    try testing.expect(std.mem.indexOf(u8, right, "Tue Jul 27") != null);
    try testing.expect(std.mem.indexOf(u8, right, "LEFT") == null);

    // Capture both sessions through the real split substrate.
    try pressKey(harness, app_iface, "d", .{ .primary = true });
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);

    const pixel_size = try harness.runtime.canvasScreenshotPixelSize(1, app.canvas_label, null);
    const pixels = try gpa.alloc(u8, pixel_size.byte_len);
    defer gpa.free(pixels);
    const scratch = try gpa.alloc(u8, pixel_size.byte_len);
    defer gpa.free(scratch);
    const shot = try harness.runtime.renderCanvasScreenshot(1, app.canvas_label, null, pixels, scratch);
    const encoded = try gpa.alloc(u8, try canvas.png.encodedRgba8ByteLen(shot.width, shot.height));
    defer gpa.free(encoded);
    var writer = std.Io.Writer.fixed(encoded);
    try canvas.png.writeRgba8(&writer, shot.width, shot.height, shot.rgba8);
    try std.Io.Dir.cwd().createDirPath(io, "zig-out");
    try std.Io.Dir.cwd().writeFile(io, .{ .sub_path = validator_shot_path, .data = writer.buffered() });
}
