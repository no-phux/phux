//! Adversarial probes: each FAILS if panes share emulator state or a
//! display-list id namespace, run with an unbounded budget, or route input
//! to the wrong pty.

const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const support = @import("support.zig");
const measured = @import("measured.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const createSession = support.createSession;
const createSessions = support.createSessions;

// ------------------------------------------------------- A1: state sharing

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

// --------------------------------------------------- A3/A4: budget reality

/// Rows the pane put on screen this paint, read off its `cell_grid` row
/// commands.
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

/// A distinct truecolor fg and bg on every cell: it spends the cell store
/// (box drawing is what exercises the command envelope).
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
    // Paint the same hostile box-drawing screen bounded and unbounded: an
    // unbounded (0) budget would produce equal lengths.
    const gpa = testing.allocator;
    const cols = 60;
    const rows = 40;
    const session = try grid.Session.create(std.heap.page_allocator, testing.io, cols, rows);
    defer session.destroy();
    feedBoxRows(session, cols, rows);

    // Re-derived from the SDK's own price so the screen provably exceeds the
    // envelope.
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

    // Truncation must be reported on the builder, not look like a short
    // screen.
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
    // The cell store bounds a terminal: paint the truecolor screen with the
    // whole store and with ten rows' worth, and the bound must show in the
    // painted rows.
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
        // A command is a painted row (plus fixed overhead), so equal spend
        // means equal screens.
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

// -------------------------------------------------- A7: the validator's eyes
