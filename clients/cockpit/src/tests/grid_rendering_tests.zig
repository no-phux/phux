const std = @import("std");
const native_sdk = @import("native_sdk");
const vt = @import("ghostty-vt");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const support = @import("support.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const createSession = support.createSession;
const createSessions = support.createSessions;
const expectCursorPaintKind = support.expectCursorPaintKind;

// Screens paint as one packed `cell_grid` command per row (see support.zig),
// so these tests read cells: colours compare exactly (8-bit), and command
// counts track painted height, not density.

const expectCellGrid = support.expectCellGrid;

const paint_fixed_commands = support.paint_fixed_commands;

/// A pane's worst case for run merging: every cell its own style, so no
/// two adjacent cells share a run.
fn feedAdversarialRows(session: *grid.Session, cols: usize, rows: usize) void {
    var line: [1024]u8 = undefined;
    for (0..rows) |_| {
        var w: usize = 0;
        for (0..cols) |col| {
            const code: u8 = if (col % 2 == 0) 31 else 32;
            w += (std.fmt.bufPrint(line[w..], "\x1b[{d}mX", .{code}) catch break).len;
        }
        session.feed(line[0..w]);
        session.feed("\r\n");
    }
}

test "the grid paints real text runs with the engine's ANSI palette and exact truecolor" {
    const session = try createSession(30, 4);
    defer session.destroy();
    session.feed("plain \x1b[31mred\x1b[0m \x1b[38;2;10;200;30mexact\x1b[0m\r\n");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const tokens: canvas.DesignTokens = .{};
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = tokens,
        .running = true,
        .selecting = false,
    });
    // Colour is a property of a CELL now, not of a merged run, so the
    // three words are located by the columns they occupy and each
    // word's first cell is read directly. Nothing merges and nothing
    // can silently re-colour a neighbour.
    const view = try expectCellGrid(builder.displayList());
    const plain_x = view.findInRow(0, "plain") orelse return error.TestExpectedCell;
    const red_x = view.findInRow(0, "red") orelse return error.TestExpectedCell;
    const exact_x = view.findInRow(0, "exact") orelse return error.TestExpectedCell;
    try testing.expectEqualStrings("p", view.cluster(plain_x, 0));
    try testing.expectEqualStrings("r", view.cluster(red_x, 0));
    try testing.expectEqualStrings("e", view.cluster(exact_x, 0));

    // Default fg is the theme text token.
    const plain_fg = view.foreground(plain_x, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(canvas.CellColor.fromColor(tokens.colors.text), plain_fg);

    // ANSI 1 is the emulator's red, not the UI's destructive token.
    const expected = vt.color.default[1];
    const red_fg = view.foreground(red_x, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(expected.r, red_fg.r);
    try testing.expectEqual(expected.g, red_fg.g);
    try testing.expectEqual(expected.b, red_fg.b);
    // And it is specifically NOT the design token any more.
    try testing.expect(!canvas.CellColor.fromColor(tokens.colors.destructive).eql(red_fg));

    // Truecolor passes through exactly.
    const exact_fg = view.foreground(exact_x, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(@as(u8, 10), exact_fg.r);
    try testing.expectEqual(@as(u8, 200), exact_fg.g);
    try testing.expectEqual(@as(u8, 30), exact_fg.b);
}

test "the custom cursor fills only while focused and live" {
    const session = try createSession(20, 4);
    defer session.destroy();

    // Builder stores cells inline; three live stack copies overflow macOS
    // after the 4x cell bump. Heap-allocate one and reuse it.
    const builder = try testing.allocator.create(canvas.Builder);
    defer testing.allocator.destroy(builder);
    var commands: [64]canvas.CanvasCommand = undefined;
    builder.initAt(&commands);
    try grid.paint(session, builder, .{
        .frame = geometry.RectF.init(0, 0, 200, 100),
        .tokens = .{},
        .running = true,
        .focused = true,
        .selecting = false,
    });
    try expectCursorPaintKind(builder.displayList(), .filled);

    builder.reset();
    try grid.paint(session, builder, .{
        .frame = geometry.RectF.init(0, 0, 200, 100),
        .tokens = .{},
        .running = true,
        .focused = false,
        .selecting = false,
    });
    try expectCursorPaintKind(builder.displayList(), .hollow);

    builder.reset();
    try grid.paint(session, builder, .{
        .frame = geometry.RectF.init(0, 0, 200, 100),
        .tokens = .{},
        .running = false,
        .focused = true,
        .selecting = false,
    });
    try expectCursorPaintKind(builder.displayList(), .hollow);
}

test "a styled wide character's background covers both of its cells" {
    const session = try createSession(30, 4);
    defer session.destroy();
    // Red background behind a double-width glyph: ghostty styles only
    // the PRIMARY cell, so the spacer tail must extend the same run or
    // the right half renders on the default background.
    session.feed("\x1b[41m\xe7\x95\x8c\x1b[0m\r\n");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const tokens: canvas.DesignTokens = .{};
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = tokens,
        .running = true,
        .selecting = false,
    });
    const cell_w = session.measuredCell().?.width;
    // What is under test is geometry: the spacer column carries the wide
    // cell's background, so the red covers two adjacent cells.
    const ansi_red = vt.color.default[1];
    const view = try expectCellGrid(builder.displayList());
    const primary = view.style(0, 0) orelse return error.TestExpectedCell;
    const tail = view.style(1, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(canvas.CellWidth.wide, primary.width);
    try testing.expectEqual(canvas.CellWidth.spacer, tail.width);

    const left = view.background(0, 0) orelse return error.TestExpectedCell;
    const right = view.background(1, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(ansi_red.r, left.r);
    try testing.expectEqual(ansi_red.g, left.g);
    try testing.expectEqual(ansi_red.b, left.b);
    try testing.expect(left.eql(right));

    // Adjacent columns, so the fill is continuous across both halves.
    try testing.expectApproxEqAbs(@as(f32, 0), view.cellRect(0, 0).x, 0.001);
    try testing.expectApproxEqAbs(cell_w, view.cellRect(1, 0).x, 0.01);
    const tail_rect = view.cellRect(1, 0);
    try testing.expectApproxEqAbs(cell_w * 2, tail_rect.x + tail_rect.width, 0.01);

    // The wide cluster inks once, from the primary; the spacer paints no
    // ink of its own, which is what keeps the glyph from doubling.
    try testing.expectEqualStrings("\xe7\x95\x8c", view.cluster(0, 0));
    try testing.expectEqual(@as(usize, 0), view.cluster(1, 0).len);
}

test "the glyph budget degrades row-wise before the atlas can overflow" {
    const session = try createSession(30, 4);
    defer session.destroy();
    // Two rows of eight distinct CJK scalars each: sixteen distinct
    // code points total.
    session.feed("\xe4\xb8\x80\xe4\xba\x8c\xe4\xb8\x89\xe5\x9b\x9b\xe4\xba\x94\xe5\x85\xad\xe4\xb8\x83\xe5\x85\xab\r\n");
    session.feed("\xe4\xb9\x9d\xe5\x8d\x81\xe7\x99\xbe\xe5\x8d\x83\xe4\xb8\x87\xe5\x84\x84\xe5\x85\x86\xe4\xba\xac\r\n");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    // Ten distinct code points (forty atlas entries, four subpixel variants
    // each): the first row fits, and painting stops before the second.
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = .{},
        .running = true,
        .selecting = false,
        .glyph_budget = 40,
    });
    // The lattice is emitted at its PAINTED height, so "the second row
    // never reached the glass" is now "the grid has exactly one row" —
    // a stronger statement than the old "no text command carried its
    // first scalar", because it also rules out a partially filled row.
    const view = try expectCellGrid(builder.displayList());
    try testing.expectEqual(@as(usize, 1), view.rows());
    try testing.expectEqualStrings("\xe4\xb8\x80", view.cluster(0, 0));
    try testing.expect(view.find("\xe4\xb9\x9d") == null);
    try testing.expect(view.at(0, 1) == null);

    // Degrading is never silent: the stop is recorded on the frame, by
    // the store that caused it and with the rows it cost.
    const loss = builder.degradation orelse return error.TestExpectedDegradation;
    try testing.expectEqual(canvas.DisplayListStore.glyphs, loss.store);
    try testing.expectEqual(@as(usize, 1), loss.produced);
    try testing.expectEqual(@as(usize, 4), loss.requested);
}

test "a grapheme cluster the emulator holds paints whole - down to the last mark" {
    // One cell: base + 100 combining acutes + a final enclosing mark,
    // 204 bytes. Inside the packed cell's reach the original contract is
    // unchanged and is what this pins: the cluster arrives WHOLE, every
    // mark present, never truncated to fit a paint-tier buffer.
    const cluster = "a" ++ ("\u{0301}" ** 100) ++ "\u{20DD}";
    try testing.expectEqual(@as(usize, 204), cluster.len);

    const session = try createSession(30, 4);
    defer session.destroy();
    session.feed(cluster);

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = .{},
        .running = true,
        .selecting = false,
    });
    const view = try expectCellGrid(builder.displayList());
    try testing.expectEqualStrings(cluster, view.cluster(0, 0));
}

test "a grapheme past the packed cell's reach is dropped whole, never torn" {
    // A cell addresses its cluster with a u8 length, so a 404-byte grapheme
    // paints no ink. It must be dropped whole, never torn into a different
    // character; the cell keeps its background.
    const cluster = "a" ++ ("\u{0301}" ** 200) ++ "\u{20DD}";
    try testing.expect(cluster.len > 255);

    const session = try createSession(30, 4);
    defer session.destroy();
    session.feed(cluster);

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = .{},
        .running = true,
        .selecting = false,
    });
    const view = try expectCellGrid(builder.displayList());
    // Whole, not torn: nothing, rather than a prefix that would render
    // as "a" wearing the wrong marks.
    try testing.expectEqual(@as(usize, 0), view.cluster(0, 0).len);
    // The row still painted — one unrepresentable cluster does not cost
    // the screen its row.
    try testing.expect(view.rows() >= 1);
}

test "a concealed row never blanks the rows painted after it" {
    const session = try createSession(80, 6);
    defer session.destroy();
    // Row 0: sixty concealed cells (SGR 8) — painting emits NO text for
    // them. Row 1: ordinary visible text.
    session.feed("\x1b[8m" ++ ("x" ** 60) ++ "\x1b[0m\r\nvisible\r\n");

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    // A text store smaller than the concealed row's raw bytes: a preflight
    // counting suppressed bytes would stop early and blank later rows.
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 800, 200),
        .tokens = .{},
        .running = true,
        .selecting = false,
        .text_reserve = canvas.max_display_list_text_bytes - 32,
    });
    const view = try expectCellGrid(builder.displayList());
    var row: [512]u8 = undefined;
    // Every row still reached the glass — the concealed row cost nothing.
    try testing.expectEqual(@as(usize, 6), view.rows());
    // The concealed row inks nothing anywhere in the lattice (SGR 8
    // resolves to a cell with no cluster, so `x` is not in the grid's
    // interned text either)...
    try testing.expectEqual(@as(usize, 0), view.rowText(0, &row).len);
    try testing.expect(view.find("x") == null);
    // ...and the row after it paints in full.
    try testing.expectEqualStrings("visible", view.rowText(1, &row));
    // Nothing was lost to a budget: a suppressed row is not a spent one.
    try testing.expectEqual(@as(?canvas.DisplayListDegradation, null), builder.degradation);
}

test "inverse video paints text in the background color, not on itself" {
    const session = try createSession(20, 3);
    defer session.destroy();
    // Default colors, reverse-video on: the text must read as the theme
    // background painted over the theme foreground, never foreground on
    // an identical foreground (invisible).
    session.feed("\x1b[7mREV\x1b[0m\r\n");

    var commands: [256]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const tokens: canvas.DesignTokens = .{};
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = tokens,
        .running = true,
        .selecting = false,
    });
    const view = try expectCellGrid(builder.displayList());
    var row: [64]u8 = undefined;
    try testing.expectEqualStrings("REV", view.rowText(0, &row));
    // Ink is the background token; distinctly not the fg.
    const ink = view.foreground(0, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(canvas.CellColor.fromColor(tokens.colors.background), ink);
    try testing.expect(!canvas.CellColor.fromColor(tokens.colors.text).eql(ink));
    // The cell's own background is the swapped-in foreground. A packed
    // cell carries both halves of the swap, so the pair can be checked
    // together — the old per-run assertion could only see the ink and
    // had to trust that a separate background rect matched it.
    const wash = view.background(0, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(canvas.CellColor.fromColor(tokens.colors.text), wash);
    try testing.expect(!wash.eql(ink));
}

test "the grid never emits past its command budget" {
    const session = try createSession(80, 24);
    defer session.destroy();
    // A worst case for run-merging: alternate the foreground every cell
    // so no two adjacent cells share a style and every cell is its own
    // run. The budget must still hold.
    var line: [512]u8 = undefined;
    for (0..24) |_| {
        var w: usize = 0;
        for (0..80) |col| {
            const code: u8 = if (col % 2 == 0) 31 else 32;
            w += (std.fmt.bufPrint(line[w..], "\x1b[{d}mX", .{code}) catch break).len;
        }
        session.feed(line[0..w]);
        session.feed("\r\n");
    }
    var commands: [2048]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 900, 560),
        .tokens = .{},
        .running = true,
        .selecting = false,
        .command_budget = 1700,
    });
    try testing.expect(builder.displayList().commands.len <= 1700);
}

test "an OSC 4 palette override is honored even when it equals the default RGB" {
    const session = try createSession(20, 3);
    defer session.destroy();
    const default_red = vt.color.default[1];
    // OSC 4: set ANSI 1 (red) to EXACTLY the emulator's default red RGB,
    // then print red text. RGB equality with the default must not fool
    // the renderer into substituting the theme color — the override
    // mask says the program chose it.
    var seq: [64]u8 = undefined;
    session.feed(std.fmt.bufPrint(&seq, "\x1b]4;1;rgb:{x:0>2}/{x:0>2}/{x:0>2}\x07", .{ default_red.r, default_red.g, default_red.b }) catch unreachable);
    session.feed("\x1b[31mR\x1b[0m\r\n");

    var commands: [256]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    const tokens: canvas.DesignTokens = .{};
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = tokens,
        .running = true,
        .selecting = false,
    });
    const view = try expectCellGrid(builder.displayList());
    try testing.expectEqualStrings("R", view.cluster(0, 0));
    // The live (overridden) RGB, not the theme destructive. The packed
    // cell holds the terminal's own 8-bit channels, so what used to need
    // a 0.004 tolerance is now an exact byte-for-byte match against the
    // RGB the program set.
    const ink = view.foreground(0, 0) orelse return error.TestExpectedCell;
    try testing.expectEqual(default_red.r, ink.r);
    try testing.expectEqual(default_red.g, ink.g);
    try testing.expectEqual(default_red.b, ink.b);
    try testing.expect(!canvas.CellColor.fromColor(tokens.colors.destructive).eql(ink));
}

test "a tall sparse terminal paints its bottom row" {
    const session = try createSession(60, grid.max_rows);
    defer session.destroy();
    for (0..grid.max_rows - 1) |row| {
        if (row % 7 == 0) session.feed("\x1b[32m.\x1b[0m");
        session.feed("\r\n");
    }
    session.feed("\x1b[92mBOTTOM\x1b[0m");

    // The app's real envelope, re-derived.
    var commands: [app.chrome_command_envelope]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 1200, 2400),
        .tokens = .{},
        .running = true,
        .selecting = false,
        .command_budget = app.chrome_command_envelope,
    });
    // The bottom row must reach the surface; the envelope must hold a
    // full-height screen for that to be a statement about the painter.
    try testing.expect(app.chrome_command_envelope > grid.max_rows + paint_fixed_commands);
    const view = try expectCellGrid(builder.displayList());
    try testing.expectEqual(@as(usize, grid.max_rows), view.rows());
    const at = view.find("BOTTOM") orelse return error.TestExpectedMarker;
    try testing.expectEqual(@as(usize, grid.max_rows - 1), at.y);
    try testing.expectEqual(@as(usize, 0), at.x);
    try testing.expectEqual(@as(?canvas.DisplayListDegradation, null), builder.degradation);
}

test "box-drawing cells render as edge-to-edge geometry, never glyphs" {
    const session = try createSession(30, 4);
    defer session.destroy();
    // A border fragment: two joined horizontals, a corner, a vertical,
    // and a shade.
    session.feed("\xe2\x94\x8c\xe2\x94\x80\xe2\x94\x80\r\n\xe2\x94\x82 \xe2\x96\x92\r\n"); // ┌── / │ ▒

    var commands: [512]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 400, 200),
        .tokens = .{},
        .running = true,
        .selecting = false,
    });
    const cell_w = session.measuredCell().?.width;
    const cell_h = session.measuredCell().?.height;
    var box_texts: usize = 0;
    var merged_bar = false;
    var full_height_bar = false;
    for (builder.displayList().commands) |command| {
        switch (command) {
            .draw_text => |text| {
                // No box character ever reaches a font glyph.
                if (std.mem.indexOf(u8, text.text, "\xe2\x94") != null) box_texts += 1;
                if (std.mem.indexOf(u8, text.text, "\xe2\x96") != null) box_texts += 1;
            },
            .fill_rect => |fill| {
                // The two `─` cells merged into ONE bar spanning both,
                // continuing seamlessly from the corner's stub.
                if (fill.rect.width > cell_w * 1.9 and fill.rect.height < cell_h) merged_bar = true;
                // The `│` runs the FULL cell height - rows abut, so
                // stacked bars join with no seam.
                if (fill.rect.height == cell_h and fill.rect.width < cell_w) full_height_bar = true;
            },
            else => {},
        }
    }
    try testing.expectEqual(@as(usize, 0), box_texts);
    try testing.expect(merged_bar);
    try testing.expect(full_height_bar);
}

test "switching terminal Works retains distinct id namespaces the diff accepts" {
    const sessions = try createSessions(20, 6);
    defer for (sessions) |each| each.destroy();
    sessions[0].feed("PANEALPHA\r\n");
    sessions[1].feed("PANEBRAVO\r\n");

    var command_storage: [2][1024]canvas.CanvasCommand = undefined;
    var lists: [2]canvas.DisplayList = undefined;
    for (sessions, 0..) |session, index| {
        var builder = canvas.Builder.init(&command_storage[index]);
        try grid.paint(session, &builder, .{
            .frame = geometry.RectF.init(226, 60, 746, 572),
            .tokens = .{},
            .running = true,
            .selecting = false,
            .id_base = grid.paneIdBase(index),
        });
        lists[index] = builder.displayList();
    }

    // A tab switch diffs one selected terminal frame into the other.
    const changes = try testing.allocator.alloc(canvas.DiffChange, 4096);
    defer testing.allocator.free(changes);
    _ = try canvas.DisplayList.diff(lists[0], lists[1], changes);

    const first = lists[0].findCommandById(grid.cursorCommandId(grid.paneIdBase(0))) orelse
        return error.TestExpectedCursor;
    const second = lists[1].findCommandById(grid.cursorCommandId(grid.paneIdBase(1))) orelse
        return error.TestExpectedCursor;
    try testing.expect(first.command.objectId().? != second.command.objectId().?);
}

test "each selected terminal receives the full chrome command envelope" {
    // Whichever terminal is selected gets the same budget, so content never
    // silently vanishes. Dense screens spend cells, not commands; the claim is
    // pinned on painted rows plus the painter's truncation report.
    const sessions = try createSessions(40, 40);
    defer for (sessions) |each| each.destroy();
    for (sessions) |session| feedAdversarialRows(session, 40, 40);

    var painted: [2]usize = @splat(0);
    var cells: [2]usize = @splat(0);
    for (sessions, 0..) |session, index| {
        var commands: [app.chrome_command_envelope]canvas.CanvasCommand = undefined;
        var builder = canvas.Builder.init(&commands);
        try grid.paint(session, &builder, .{
            .frame = geometry.RectF.init(0, 0, 746, 580),
            .tokens = .{},
            .running = true,
            .selecting = false,
            .command_budget = app.chrome_command_envelope,
            .text_reserve = canvas.terminal_grid.widget_text_reserve,
            .glyph_budget = canvas.terminal_grid.widget_glyph_budget,
            .cell_reserve = 0,
            .id_base = grid.paneIdBase(index),
        });
        const view = try expectCellGrid(builder.displayList());
        painted[index] = view.rows();
        cells[index] = view.cellCount();
        try testing.expect(builder.displayList().commands.len <= app.chrome_command_envelope);
        // Every row that fit the frame was painted whole: the frame is
        // 580pt of 18pt rows, so the screen ends at the viewport, not at
        // a budget. Nothing was dropped, and the painter says so.
        try testing.expectEqual(@as(?canvas.DisplayListDegradation, null), builder.degradation);
        // Selected-tab paints are one pane owning the content area, so they
        // get the whole cell store. `widget_cell_reserve` is the SDK's
        // two-pane leftover (`store / 2`), not a production floor, and the
        // Hybrid C painter does not read it.
        try testing.expectEqual(view.rows() * view.cols(), cells[index]);
        // One retained command per painted row, bracketed both ways, so a
        // painter that regressed to per-screen or per-run commands fails.
        const emitted = builder.displayList().commands.len;
        try testing.expect(emitted >= view.rows());
        try testing.expect(emitted <= view.rows() + paint_fixed_commands);
    }
    try testing.expect(painted[0] >= 5);
    try testing.expectEqual(painted[0], painted[1]);
    try testing.expectEqual(cells[0], cells[1]);
}

test "a screen of double box drawing stays inside the selected terminal budget" {
    // U+256C costs EIGHT commands (four double sides, two bars each). A
    // four-per-column reserve let the last painted row overshoot the
    // budget, and under `variable_prefix` an overshoot fails the WHOLE
    // frame rather than dropping a row.
    const box_cols = 40;
    const box_rows = 24;
    const session = try createSession(box_cols, box_rows);
    defer session.destroy();
    for (0..box_rows) |_| {
        session.feed("\u{256C}" ** box_cols);
        session.feed("\r\n");
    }

    // Re-derived from the SDK's own price, so the screen genuinely outruns
    // the envelope and the truncation assertions are not a tautology.
    const box_cell_commands = canvas.terminal_box.maxCommands(0x256C);
    try testing.expectEqual(@as(usize, 8), box_cell_commands);
    try testing.expect(box_rows * box_cols * box_cell_commands > app.chrome_command_envelope);

    // Box drawing is painted as geometry, so it is still priced in
    // commands. Storage is exactly the envelope so the budget, not the
    // builder, stops the paint.
    var commands: [app.chrome_command_envelope]canvas.CanvasCommand = undefined;
    var builder = canvas.Builder.init(&commands);
    try grid.paint(session, &builder, .{
        .frame = geometry.RectF.init(0, 0, 600, 600),
        .tokens = .{},
        .running = true,
        .selecting = false,
        .command_budget = app.chrome_command_envelope,
        .id_base = grid.paneIdBase(0),
    });
    try testing.expect(builder.displayList().commands.len <= app.chrome_command_envelope);
    // The budget genuinely BIT — this screen is denser than the envelope
    // — and it degraded row-atomically and said so, rather than
    // overshooting and failing the whole frame.
    const view = try expectCellGrid(builder.displayList());
    const loss = builder.degradation orelse return error.TestExpectedDegradation;
    try testing.expectEqual(canvas.DisplayListStore.commands, loss.store);
    try testing.expectEqual(view.rows(), loss.produced);
    try testing.expect(loss.produced > 0);
    try testing.expect(loss.produced < loss.requested);
    try testing.expectEqual(@as(usize, box_rows), loss.requested);
    // Rows that fit are bounded by envelope / (box commands per row + 1).
    try testing.expect(view.rows() <= app.chrome_command_envelope / (box_cols * box_cell_commands + 1));
}
