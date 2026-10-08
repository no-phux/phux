//! Display-list evidence, not a claim about CoreText pixels on glass.
const std = @import("std");
const sdk = @import("native_sdk");
const canvas = sdk.canvas;
const testing = std.testing;
const render = @import("render.zig");
const support = @import("../tests/support.zig");
const app = @import("../native_test_root.zig");
const fonts = @import("fonts.zig");

const dark = canvas.Color.rgb8(9, 11, 15);
const low = canvas.Color.rgb8(29, 31, 33);
const white = canvas.CellColor.fromColor(canvas.Color.rgb8(255, 255, 255));

const Frame = struct {
    commands: [512]canvas.CanvasCommand = undefined,
    builder: canvas.Builder,

    fn create() !*Frame {
        const self = try testing.allocator.create(Frame);
        self.builder.initAt(&self.commands);
        return self;
    }

    fn row(self: *const Frame, index: usize) !canvas.CellGrid {
        var found: usize = 0;
        for (self.builder.displayList().commands) |command| {
            if (command != .cell_grid) continue;
            if (found == index) return command.cell_grid;
            found += 1;
        }
        return error.MissingPaintedRow;
    }

    fn paint(self: *Frame, grid: canvas.TerminalGrid, floor: f32) !void {
        self.builder.reset();
        try render.paintTerminalGrid(grid, &self.builder, options(floor));
    }
};

fn options(floor: f32) render.PaintOptions {
    return .{
        .frame = sdk.geometry.RectF.init(0, 0, 400, 200),
        .tokens = .{},
        .running = true,
        .selecting = false,
        .minimum_contrast = floor,
    };
}

fn gridOf(rows: []const canvas.TerminalRow) canvas.TerminalGrid {
    return .{
        .rows = rows,
        .background = dark,
        .foreground = low,
        .cursor_color = canvas.Color.rgb8(80, 100, 120),
        .selection_color = canvas.Color.rgb8(50, 70, 90),
    };
}

test "provider grid contrast changes existing output and keeps unaffected row fingerprints" {
    const cells = [_]canvas.TerminalCell{
        .{ .cp = 'A', .cluster = "A", .fg = low },
        .{ .cp = 'B', .cluster = "B", .fg = canvas.Color.rgb8(102, 102, 102) },
    };
    const rows = [_]canvas.TerminalRow{ .{ .cells = cells[0..1] }, .{ .cells = cells[1..2] } };
    const grid = gridOf(&rows);
    const frame = try Frame.create();
    defer testing.allocator.destroy(frame);
    try frame.paint(grid, 1);
    const old = try frame.row(0);
    const old_id = old.id;
    const old_hash = canvas.cellGridFingerprint(old);
    const dim_hash = canvas.cellGridFingerprint(try frame.row(1));
    try testing.expectEqual(canvas.CellColor.fromColor(low), old.cells[0].fg);

    // No new terminal bytes, only the user's setting changed. This assertion
    // fails against 3eeba41f: paintTerminalGrid ignores minimum_contrast.
    try frame.paint(grid, 3);
    const changed = try frame.row(0);
    try testing.expectEqual(white, changed.cells[0].fg);
    try testing.expectEqual(old_id, changed.id);
    try testing.expect(old_hash != canvas.cellGridFingerprint(changed));
    try testing.expectEqual(dim_hash, canvas.cellGridFingerprint(try frame.row(1)));
    const new_hash = canvas.cellGridFingerprint(changed);
    try frame.paint(grid, 3);
    try testing.expectEqual(new_hash, canvas.cellGridFingerprint(try frame.row(0)));
    try frame.paint(grid, 1);
    try testing.expectEqual(old_hash, canvas.cellGridFingerprint(try frame.row(0)));
    try testing.expectEqual(low, cells[0].fg); // the provider's source is immutable
}

test "provider contrast respects cell backgrounds graphics and overlay colors" {
    const light = canvas.Color.rgb8(240, 240, 240);
    const cells = [_]canvas.TerminalCell{
        .{ .cp = 'A', .cluster = "A", .fg = low },
        .{ .cp = 'B', .cluster = "B", .fg = light, .bg = light },
        .{ .cp = 0xe0b0, .cluster = "\u{e0b0}", .fg = low },
        .{ .cp = 0x2588, .cluster = "\u{2588}", .fg = low },
        .{ .cp = 'C', .cluster = "C", .fg = canvas.Color.rgb8(204, 102, 102) },
    };
    const rows = [_]canvas.TerminalRow{.{ .cells = &cells, .selection = .{ 0, 1 } }};
    var grid = gridOf(&rows);
    grid.cursor = .{ .x = 0, .y = 0 };
    const frame = try Frame.create();
    defer testing.allocator.destroy(frame);
    try frame.paint(grid, 1);
    const before = frame.builder.displayList().commands;
    var overlay_hashes: [512]canvas.CanvasCommand = undefined;
    @memcpy(overlay_hashes[0..before.len], before);
    const command_count = before.len;
    try frame.paint(grid, 3);
    const painted = try frame.row(0);
    try testing.expectEqual(white, painted.cells[0].fg);
    try testing.expectEqual(canvas.CellColor.fromColor(canvas.Color.rgb8(0, 0, 0)), painted.cells[1].fg);
    for (cells[2..], painted.cells[2..]) |source, actual| {
        try testing.expectEqual(canvas.CellColor.fromColor(source.fg), actual.fg);
    }
    try testing.expectEqual(canvas.CellColor.fromColor(light), painted.cells[1].bg);
    try testing.expectEqual(command_count, frame.builder.len);
    for (frame.builder.displayList().commands, overlay_hashes[0..command_count]) |actual, previous| {
        if (actual == .cell_grid) continue;
        try testing.expectEqualDeep(previous, actual);
    }
}

fn feedFixture(remote: *app.PhuxProvider, comptime name: []const u8) !void {
    try testing.expect(remote.bridge.incoming.stage(@embedFile("../providers/phux/style_fixture/" ++ name ++ ".bin")));
    _ = try remote.drainReadiness();
}

test "actual Phux FFI and local emulator resolve equivalent low contrast text" {
    if (comptime !app.phux_enabled) return error.SkipZigTest;
    const remote = try app.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/unused-appearance" }, null, "appearance");
    defer remote.destroy();
    try remote.host.start("appearance");
    inline for (.{ "hello", "attached", "begin", "chunk", "ready", "attach-ready" }) |name| try feedFixture(remote, name);
    var refs: [1]app.TerminalRef = undefined;
    try testing.expectEqual(@as(usize, 1), remote.terminalRefs(&refs));
    const remote_grid = remote.presentation(refs[0]).?.grid;
    // E in canonical cockpit_style_fixture.rs is bold truecolor #102030 on
    // OSC 11 #102840. Replay those exact inputs through the local emulator.
    const session = try support.createSession(16, 6);
    defer session.destroy();
    session.feed("\x1b]11;#102840\x1b\\\x1b[1;38;2;16;32;48mE");
    const frame = try Frame.create();
    defer testing.allocator.destroy(frame);
    for ([_]f32{ 1, 3, 1 }) |floor| {
        try frame.paint(remote_grid, floor);
        const remote_color = (try frame.row(5)).cells[2].fg;
        frame.builder.reset();
        try render.paint(session, &frame.builder, options(floor));
        const local_color = (try frame.row(0)).cells[0].fg;
        try testing.expectEqual(local_color, remote_color);
        if (floor == 3) try testing.expectEqual(white, remote_color);
    }
    try testing.expectEqual(canvas.Color.rgb8(16, 32, 48), remote_grid.rows[5].cells[2].fg);
}

fn measuredWidth(_: ?*anyopaque, font: canvas.FontId, _: f32, text: []const u8) f32 {
    // Distinct advances make a renderer-only substitution observable. Actual
    // shipped faces happen to share a 0.6em pitch, hiding that wiring defect.
    const advance: f32 = if (font == canvas.default_mono_font_id) 9.25 else 7.5;
    return @as(f32, @floatFromInt(text.len)) * advance;
}

test "host bundled Geist face is byte identical to the reference renderer" {
    const reference = canvas.font_ttf.geist_mono_bytes;
    // The app's build/test gate runs from the Cockpit source root, the same
    // asset root the native SDK packages and its unbundled host activates.
    const bundled = try std.Io.Dir.cwd().readFileAlloc(testing.io, "assets/fonts/GeistMono-Regular.ttf", testing.allocator, .limited(reference.len + 1));
    defer testing.allocator.free(bundled);
    try testing.expectEqualSlices(u8, reference, bundled);
}

test "font family changes row fingerprints and the measured pointer cell together" {
    const scene = @import("../cockpit/native/scene.zig");
    const Choice = @import("../config/config.zig").FontChoice;
    const session = try support.createSession(16, 4);
    defer session.destroy();
    session.feed("A\x1b[1mB\x1b[3mC\x1b[22mD");
    const frame = try Frame.create();
    defer testing.allocator.destroy(frame);
    const measure = canvas.TextMeasureProvider{ .measure_fn = measuredWidth };
    var opts = options(3);
    opts.tokens.text_measure = &measure;
    var hashes: [2]u64 = undefined;
    for ([_]Choice{ .bundled, .geist }, 0..) |choice, index| {
        fonts.apply(&opts.tokens, choice);
        frame.builder.reset();
        try render.paint(session, &frame.builder, opts);
        const row = try frame.row(0);
        hashes[index] = canvas.cellGridFingerprint(row);
        try testing.expectEqual(opts.tokens.typography.mono_font_id, row.font_id);
        try testing.expectEqual(opts.tokens.typography.mono_bold_font_id, row.bold_font_id);
        try testing.expectEqual(opts.tokens.typography.mono_italic_font_id, row.italic_font_id);
        try testing.expectEqual(opts.tokens.typography.mono_bold_italic_font_id, row.bold_italic_font_id);
        const expected_width: f32 = if (choice == .geist) 9.25 else 7.5;
        try testing.expectEqual(expected_width, row.cell_width);
        try testing.expectEqual(row.cell_width, session.measuredCell().?.width);
        try testing.expectEqual(row.cell_height, session.measuredCell().?.height);
    }
    try testing.expect(hashes[0] != hashes[1]);
    const geist = try frame.row(0);
    try testing.expectEqual(@as(canvas.FontId, 2), geist.font_id);
    try testing.expectEqual(@as(canvas.FontId, 0), geist.bold_font_id);
    try testing.expectEqual(@as(canvas.FontId, 0), geist.italic_font_id);
    try testing.expectEqual(@as(canvas.FontId, 0), geist.bold_italic_font_id);
    fonts.apply(&opts.tokens, Choice.bundled);
    try testing.expectEqual(scene.cockpit_fonts[0].id, opts.tokens.typography.mono_font_id);
    try testing.expectEqual(scene.cockpit_fonts[1].id, opts.tokens.typography.mono_bold_font_id);
    try testing.expectEqual(scene.cockpit_fonts[2].id, opts.tokens.typography.mono_italic_font_id);
    try testing.expectEqual(scene.cockpit_fonts[3].id, opts.tokens.typography.mono_bold_italic_font_id);
    frame.builder.reset();
    try render.paint(session, &frame.builder, opts);
    try testing.expectEqual(hashes[0], canvas.cellGridFingerprint(try frame.row(0)));
}

test "contrast obeys row budgets and cannot recolor a preceding pane" {
    const cells = [_]canvas.TerminalCell{.{ .cp = 'A', .cluster = "A", .fg = low }};
    const rows = [_]canvas.TerminalRow{ .{ .cells = &cells }, .{ .cells = &cells } };
    const grid = gridOf(&rows);
    const frame = try Frame.create();
    defer testing.allocator.destroy(frame);
    try frame.paint(grid, 1);
    const previous = canvas.cellGridFingerprint(try frame.row(0));
    var opts = options(3);
    opts.id_base = render.paneIdBase(1);
    // Leave room for exactly one more row in the shared packed-cell store.
    opts.cell_reserve = frame.builder.cells.len - frame.builder.cell_len - 1;
    try render.paintTerminalGrid(grid, &frame.builder, opts);
    try testing.expectEqual(previous, canvas.cellGridFingerprint(try frame.row(0)));
    try testing.expectEqual(white, (try frame.row(2)).cells[0].fg);
    try testing.expectError(error.MissingPaintedRow, frame.row(3));
    try testing.expectEqual(@as(usize, 3), frame.builder.cell_len);
}

test "Paper host assets equal registered reference faces and synthesize only italics" {
    const scene = @import("../cockpit/native/scene.zig");
    for (scene.cockpit_fonts[4..], [_][]const u8{ "assets/fonts/PaperMono-Regular.ttf", "assets/fonts/PaperMono-Bold.ttf" }) |registration, path| {
        const bundled = try std.Io.Dir.cwd().readFileAlloc(testing.io, path, testing.allocator, .limited(registration.ttf.len + 1));
        defer testing.allocator.free(bundled);
        try testing.expectEqualSlices(u8, registration.ttf, bundled);
        _ = try canvas.font_ttf.Face.parse(registration.ttf);
    }
    const paper_row: canvas.CellGrid = .{ .font_id = scene.paper_font_id, .bold_font_id = scene.paper_bold_font_id };
    try testing.expectEqual(scene.paper_font_id, paper_row.face(.{ .italic = true }).font_id);
    try testing.expect(paper_row.face(.{ .italic = true }).synthetic_italic);
    const bold_italic = paper_row.face(.{ .bold = true, .italic = true });
    try testing.expectEqual(scene.paper_bold_font_id, bold_italic.font_id);
    try testing.expect(bold_italic.synthetic_italic);
    try testing.expect(!bold_italic.synthetic_bold);
}

test "Paper local and projected rows retain Nerd fallback geometry and symbol companions" {
    const scene = @import("../cockpit/native/scene.zig");
    const Choice = @import("../config/config.zig").FontChoice;
    const session = try support.createSession(16, 4);
    defer session.destroy();
    // PUA Powerline and supplementary Nerd glyphs, alongside Paper italics.
    session.feed("A\x1b[1;3m\xee\x82\xb0\x1b[22;23mB\xf3\xb0\x88\x86C");
    const frame = try Frame.create();
    defer testing.allocator.destroy(frame);
    var opts = options(3);
    fonts.apply(&opts.tokens, Choice.paper);
    try render.paint(session, &frame.builder, opts);
    const paper_run = try frame.row(0);
    const nerd_run = try frame.row(1);
    try testing.expectEqual(scene.paper_font_id, paper_run.font_id);
    try testing.expectEqual(scene.terminal_font_id, nerd_run.font_id);
    try testing.expectEqual(scene.terminal_bold_italic_font_id, nerd_run.bold_italic_font_id);
    try testing.expectApproxEqAbs(paper_run.origin.x + paper_run.cell_width, nerd_run.origin.x, 0.001);
    try testing.expectEqual(paper_run.cell_width, nerd_run.cell_width);
    try testing.expectEqualSlices(u8, "\xee\x82\xb0", nerd_run.cells[0].cluster(nerd_run.text));
    try testing.expectEqual(scene.paper_font_id, (try frame.row(2)).font_id);
    try testing.expectEqual(scene.terminal_font_id, (try frame.row(3)).font_id);
    // The provider grid uses exactly the same shared paint seam.
    const snapshot = try session.snapshot(opts.tokens, true, false);
    const fingerprint = canvas.cellGridFingerprint(nerd_run);
    frame.builder.reset();
    try render.paintTerminalGrid(snapshot, &frame.builder, opts);
    try testing.expectEqual(fingerprint, canvas.cellGridFingerprint(try frame.row(1)));
}

test "Nerd fallback keeps wide spacers together and respects tight command budgets" {
    const scene = @import("../cockpit/native/scene.zig");
    const cells = [_]canvas.TerminalCell{
        .{ .cp = 'A', .cluster = "A" },
        .{ .cp = 0xe0b0, .cluster = "\xee\x82\xb0", .wide = .wide },
        .{ .wide = .spacer },
        .{ .cp = 'B', .cluster = "B" },
    };
    const rows = [_]canvas.TerminalRow{ .{ .cells = &cells }, .{ .cells = cells[0..2] } };
    const frame = try Frame.create();
    defer testing.allocator.destroy(frame);
    var opts = options(1);
    fonts.apply(&opts.tokens, @import("../config/config.zig").FontChoice.paper);
    opts.id_base = render.paneIdBase(0);
    try render.paintTerminalGrid(gridOf(&rows), &frame.builder, opts);
    const wide = try frame.row(1);
    try testing.expectEqual(@as(u16, 2), wide.cols);
    try testing.expectEqual(scene.terminal_font_id, wide.font_id);
    try testing.expectEqual(wide.origin.x + 2 * wide.cell_width, (try frame.row(2)).origin.x);
    for (frame.builder.displayList().commands, 0..) |command, index| {
        const id = command.objectId() orelse continue;
        for (frame.builder.displayList().commands[index + 1 ..]) |other| {
            try testing.expect(id != (other.objectId() orelse continue));
        }
    }
    for ([_]usize{ 9, 12, 16 }) |budget| {
        frame.builder.reset();
        opts.command_budget = budget;
        try render.paintTerminalGrid(gridOf(&rows), &frame.builder, opts);
        try testing.expect(frame.builder.len <= budget);
    }
}

test "Nerd run text is compact and cannot amplify the retained text budget" {
    const cells = [_]canvas.TerminalCell{
        .{ .cp = 'A', .cluster = "A" },
        .{ .cp = 0xe0b0, .cluster = "\xee\x82\xb0" },
        .{ .cp = 'B', .cluster = "B" },
        .{ .cp = 0xe0b0, .cluster = "\xee\x82\xb0" },
        .{ .cp = 'C', .cluster = "C" },
    };
    const rows = [_]canvas.TerminalRow{.{ .cells = &cells }};
    const frame = try Frame.create();
    defer testing.allocator.destroy(frame);
    var opts = options(1);
    fonts.apply(&opts.tokens, @import("../config/config.zig").FontChoice.paper);
    try render.paintTerminalGrid(gridOf(&rows), &frame.builder, opts);
    var text_bytes: usize = 0;
    for (frame.builder.displayList().commands) |command| {
        if (command != .cell_grid) continue;
        text_bytes += command.cell_grid.text.len;
        for (command.cell_grid.cells) |cell| try testing.expectEqual(@as(u32, 0), cell.text_offset);
    }
    try testing.expectEqual(@as(usize, 9), text_bytes);
    // Only the original interned row blob plus compact runs are staged.
    try testing.expectEqual(@as(usize, 15), frame.builder.text_byte_len);
    frame.builder.reset();
    opts.text_reserve = canvas.max_display_list_text_bytes - 8;
    try render.paintTerminalGrid(gridOf(&rows), &frame.builder, opts);
    try testing.expectError(error.MissingPaintedRow, frame.row(0));
}

test "Nerd reservations report command and text loss with truthful top and last-N rows" {
    const cells = [_][5]canvas.TerminalCell{
        .{ .{ .cp = '0', .cluster = "0" }, .{ .cp = 0xe0b0, .cluster = "\xee\x82\xb0" }, .{ .cp = 'A', .cluster = "A" }, .{ .cp = 0xe0b0, .cluster = "\xee\x82\xb0" }, .{ .cp = 'B', .cluster = "B" } },
        .{ .{ .cp = '1', .cluster = "1" }, .{ .cp = 0xe0b0, .cluster = "\xee\x82\xb0" }, .{ .cp = 'A', .cluster = "A" }, .{ .cp = 0xe0b0, .cluster = "\xee\x82\xb0" }, .{ .cp = 'B', .cluster = "B" } },
        .{ .{ .cp = '2', .cluster = "2" }, .{ .cp = 0xe0b0, .cluster = "\xee\x82\xb0" }, .{ .cp = 'A', .cluster = "A" }, .{ .cp = 0xe0b0, .cluster = "\xee\x82\xb0" }, .{ .cp = 'B', .cluster = "B" } },
    };
    const rows = [_]canvas.TerminalRow{ .{ .cells = &cells[0] }, .{ .cells = &cells[1] }, .{ .cells = &cells[2] } };
    const frame = try Frame.create();
    defer testing.allocator.destroy(frame);
    var opts = options(1);
    fonts.apply(&opts.tokens, @import("../config/config.zig").FontChoice.paper);
    opts.id_base = render.paneIdBase(0);
    for ([_]render.RowFit{ .from_top, .last_n }) |fit| {
        opts.row_fit = fit;
        opts.command_budget = 22;
        opts.text_reserve = 0;
        frame.builder.reset();
        try render.paintTerminalGrid(gridOf(&rows), &frame.builder, opts);
        const loss = frame.builder.degradation orelse return error.TruncationWentUnreported;
        try testing.expectEqual(canvas.DisplayListStore.commands, loss.store);
        try testing.expectEqual(@as(usize, 3), loss.requested);
        try testing.expectEqual(@as(usize, 2), loss.produced);
        try testing.expectEqual(opts.id_base, loss.id);
        const first = try frame.row(0);
        try testing.expectEqualSlices(u8, if (fit == .last_n) "1" else "0", first.cells[0].cluster(first.text));
        try testing.expectEqual(@as(f32, 0), first.origin.y);
        try testing.expect(frame.builder.len <= opts.command_budget);

        opts.command_budget = 0;
        opts.text_reserve = canvas.max_display_list_text_bytes - 20;
        frame.builder.reset();
        try render.paintTerminalGrid(gridOf(&rows), &frame.builder, opts);
        const text_loss = frame.builder.degradation orelse return error.TruncationWentUnreported;
        try testing.expectEqual(canvas.DisplayListStore.text_bytes, text_loss.store);
        try testing.expectEqual(@as(usize, 3), text_loss.requested);
        try testing.expectEqual(@as(usize, 0), text_loss.produced);
        try testing.expectError(error.MissingPaintedRow, frame.row(0));
    }
    frame.builder.reset();
    opts.command_budget = 0;
    opts.text_reserve = 0;
    try render.paintTerminalGrid(gridOf(&rows), &frame.builder, opts);
    try testing.expectEqual(@as(?canvas.DisplayListDegradation, null), frame.builder.degradation);
    var painted_cells: usize = 0;
    for (frame.builder.displayList().commands) |command| {
        if (command == .cell_grid) painted_cells += command.cell_grid.cells.len;
    }
    try testing.expectEqual(@as(usize, 15), painted_cells);
}
