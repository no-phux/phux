//! Shared retained-canvas paint seam for local and provider-projected grids.

const native_sdk = @import("native_sdk");
const session_module = @import("session.zig");
const Palette = @import("palette.zig").Palette;

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const Session = session_module.Session;

const grid_id_base: u64 = 0x7e21;

test {
    _ = @import("appearance_tests.zig");
}

/// The caller id for pane `index`. The framework applies its own retained-ID
/// stride, so each pane needs only a distinct small caller id.
pub fn paneIdBase(index: usize) u64 {
    return grid_id_base + index;
}

pub fn cursorCommandId(id_base: u64) u64 {
    return canvas.terminal_grid.paintIdBase(id_base) +% 0x61_0002;
}

/// The command-id NAMESPACE a pane's paint occupies.
///
/// The SDK painter derives every id it emits from `paintIdBase(id_base)`
/// — a wrapping multiply by 2^24 — and keeps every offset under that
/// stride, so a pane's commands are exactly the ids in
/// `[idNamespace(id_base), + id_namespace_stride)` and two panes with
/// different bases can never overlap.
///
/// This is the seam for picking ONE pane's commands out of a frame that
/// holds several. It replaced a single `cellGridCommandId`: a terminal
/// screen used to be one `cell_grid` command at one fixed offset, and is
/// now one command PER ROW (the retained diff's unit of change is a
/// command, so a screen-wide lattice re-encoded every cell on every
/// keystroke). There is no single id to ask for any more — there is a
/// namespace, and the rows are whichever grids fall inside it.
pub fn idNamespace(id_base: u64) u64 {
    return canvas.terminal_grid.paintIdBase(id_base);
}

/// The stride between namespaces: the painter's own 24-bit slot space.
pub const id_namespace_stride: u64 = 1 << 24;

/// The namespace an emitted command id belongs to — the inverse of
/// `idNamespace`, for a reader holding an id rather than a pane index.
pub fn idNamespaceOf(id: u64) u64 {
    return id & ~(id_namespace_stride - 1);
}

pub const cursor_command_id: u64 = cursorCommandId(grid_id_base);

/// How a grid that does not fit the cell budget is reduced before paint.
/// The SDK painter itself is top-first: it emits rows 0..N and drops the
/// rest, which hides the prompt. Hybrid C degraded panes must not keep
/// that as the lasting tier.
pub const RowFit = enum {
    /// Leave the snapshot alone. Truncation is SDK first-N.
    from_top,
    /// Crop to the trailing rows that fit, then paint. The prompt stays.
    last_n,
};

pub const PaintOptions = struct {
    frame: geometry.RectF,
    tokens: canvas.DesignTokens,
    running: bool,
    focused: bool = true,
    selecting: bool,
    /// Zero leaves the corresponding painter budget unbounded.
    command_budget: usize = 0,
    text_reserve: usize = 0,
    background_frame: ?geometry.RectF = null,
    glyph_budget: usize = 0,
    path_reserve: usize = 0,
    /// Packed-grid CELLS held back for the terminals painted after this
    /// one. A screen's cost is CELLS now, not commands, so this is the
    /// budget that actually bounds how much of a dense screen reaches
    /// the glass — the command envelope only prices box geometry and
    /// selection washes. Zero leaves it unbounded.
    cell_reserve: usize = 0,
    /// Default `from_top` preserves existing `grid.paint` callers. The
    /// Hybrid C window painter sets `last_n` so a truncated pane keeps
    /// the trailing rows (the prompt), not the top of the scrollback.
    row_fit: RowFit = .from_top,
    /// Extra row ceiling applied when `row_fit == .last_n`. Zero means
    /// only the cell store / `cell_reserve` bound the crop. Degraded
    /// panes pass `max_rows / 4`.
    row_cap: usize = 0,
    /// The user's client-side `minimum-contrast`, for every provider. Resolve
    /// terminal defaults, application colors, inverse and faint first; apply
    /// the floor to those final colors without changing the source grid.
    ///
    /// Defaults to 1 (no floor) rather than to the config default, so the
    /// value can only ever come from a caller that actually holds the user's
    /// config. A default that turned the floor ON here would make every
    /// synthetic PaintOptions in the codebase quietly apply it.
    minimum_contrast: f32 = 1,
    /// Use `paneIdBase` for multiple grids in one retained view.
    id_base: u64 = grid_id_base,
};

/// Project and paint one local emulator session through the shared painter.
pub fn paint(session: *Session, builder: *canvas.Builder, options: PaintOptions) !void {
    const metrics = canvas.terminalCellMetrics(options.tokens);
    session.font_size = metrics.font_size;
    // The ONLY writer of the measured cell box in the app, and deliberately
    // so: `options.tokens` are the runtime's, with its text-measure provider
    // stamped on, so `metrics.width` is the mono face's real advance. Anywhere
    // else in the app those tokens are unavailable and the same call silently
    // degrades to an estimate — see `workspace_projection.terminalCellMetricsFor`.
    session.setMeasuredCell(metrics.width, metrics.height);
    // Written every frame from the live config, alongside the font size, for
    // the reason `theme.zig` spells out: a colour policy that is stamped onto
    // the session once at spawn needs an explicit re-apply on every config
    // change, and a re-apply is a thing to forget. Rewriting it here means
    // `minimum-contrast` moves the moment `Model.config` does, with nothing to
    // invalidate and no stale copy for an old value to hide in.
    session.minimum_contrast = options.minimum_contrast;

    const snap = try session.snapshot(options.tokens, options.running, options.selecting);
    try paintTerminalGrid(snap, builder, options);
}

/// Paint an already-projected provider grid with the same budgets and retained
/// identity behavior as a local session.
pub fn paintTerminalGrid(terminal_grid: canvas.TerminalGrid, builder: *canvas.Builder, options: PaintOptions) !void {
    const fitted = switch (options.row_fit) {
        .from_top => terminal_grid,
        .last_n => cropToFit(terminal_grid, builder, options),
    };
    const first_command = builder.len;
    const first_cell = builder.cell_len;
    try canvas.terminal_grid.paint(fitted, builder, .{
        .frame = options.frame,
        .tokens = options.tokens,
        .focused = options.focused,
        .background_frame = options.background_frame,
        .id_base = options.id_base,
        .command_budget = options.command_budget,
        .text_reserve = options.text_reserve,
        .path_reserve = options.path_reserve,
        .glyph_budget = options.glyph_budget,
        .cell_reserve = options.cell_reserve,
    });
    applyContrast(fitted, builder, options.minimum_contrast, first_command, first_cell);
}

fn gridCols(grid: canvas.TerminalGrid) usize {
    var cols: usize = 0;
    for (grid.rows) |row| cols = @max(cols, row.cells.len);
    return cols;
}

fn shiftY(y: u16, skip: usize) ?u16 {
    if (@as(usize, y) < skip) return null;
    return @intCast(@as(usize, y) - skip);
}

/// Keep the trailing `keep_rows` of `grid`, moving cursor and select-head
/// with the crop. Rows above the crop drop those overlays rather than
/// leave them floating on the wrong line.
pub fn cropLastN(grid: canvas.TerminalGrid, keep_rows: usize) canvas.TerminalGrid {
    if (keep_rows >= grid.rows.len) return grid;
    const skip = grid.rows.len - keep_rows;
    var cropped = grid;
    cropped.rows = grid.rows[skip..];
    if (cropped.cursor) |cursor| {
        cropped.cursor = if (shiftY(cursor.y, skip)) |y| blk: {
            var moved = cursor;
            moved.y = y;
            break :blk moved;
        } else null;
    }
    if (cropped.select_head) |head| {
        cropped.select_head = if (shiftY(head.y, skip)) |y|
            .{ .x = head.x, .y = y }
        else
            null;
    }
    if (cropped.scrollbar.total != 0 or cropped.scrollbar.len != 0) {
        cropped.scrollbar.offset +|= @intCast(skip);
        cropped.scrollbar.len = @intCast(keep_rows);
    }
    return cropped;
}

/// Last-N crop sized to the cells this paint can still take, then
/// `options.row_cap` if set. Used by degraded Hybrid C panes.
pub fn cropToFit(grid: canvas.TerminalGrid, builder: *const canvas.Builder, options: PaintOptions) canvas.TerminalGrid {
    const cols = gridCols(grid);
    const cell_ceiling = builder.cells.len -| options.cell_reserve;
    const cells_available = cell_ceiling -| builder.cell_len;
    const by_cells = if (cols == 0) grid.rows.len else cells_available / cols;
    var keep = @min(grid.rows.len, by_cells);
    if (options.row_cap > 0) keep = @min(keep, options.row_cap);
    return cropLastN(grid, keep);
}

/// The SDK stages contiguous, row-atomic cells in builder-owned storage. Apply
/// presentation policy there, before retained fingerprints/packets are made.
/// This touches only emitted rows, allocates nothing, preserves row IDs and
/// interned text, and never writes through a provider's borrowed grid slices.
/// Cursor/selection overlays and graphics geometry remain the SDK's paint.
fn applyContrast(grid: canvas.TerminalGrid, builder: *canvas.Builder, floor: f32, first_command: usize, first_cell: usize) void {
    if (!(floor > 1)) return;
    var row_index: usize = 0;
    var cell_offset = first_cell;
    for (builder.commands[first_command..builder.len]) |command| {
        if (command != .cell_grid) continue;
        const count = command.cell_grid.cells.len;
        contrastRow(grid.rows[row_index], grid.background, builder.cells[cell_offset..][0..count], floor);
        cell_offset += count;
        row_index += 1;
    }
}

fn contrastRow(row: canvas.TerminalRow, background: canvas.Color, cells: []canvas.Cell, floor: f32) void {
    const count = @min(row.cells.len, cells.len);
    for (row.cells[0..count], cells[0..count]) |source, *cell| {
        // Use the original float colors, rather than quantized packed bytes,
        // so local faint blends and provider-resolved colors share the exact
        // existing WCAG algorithm and its graphics exclusions.
        cell.fg = canvas.CellColor.fromColor(Palette.contrasted(floor, source.fg, source.bg orelse background, source.cp));
    }
}

test "cropLastN keeps the trailing rows and moves the cursor with them" {
    const testing = @import("std").testing;
    const marks = [_][]const u8{ "0", "1", "2", "3", "4", "5", "6", "7" };
    var cells: [8]canvas.TerminalCell = undefined;
    var rows: [8]canvas.TerminalRow = undefined;
    for (marks, 0..) |mark, index| {
        cells[index] = .{ .cp = mark[0], .cluster = mark };
        rows[index] = .{ .cells = cells[index .. index + 1] };
    }
    const source: canvas.TerminalGrid = .{
        .rows = &rows,
        .background = canvas.Color.rgb8(0, 0, 0),
        .foreground = canvas.Color.rgb8(255, 255, 255),
        .cursor_color = canvas.Color.rgb8(255, 255, 255),
        .selection_color = canvas.Color.rgb8(80, 80, 80),
        .cursor = .{ .x = 0, .y = 7 },
        .select_head = .{ .x = 0, .y = 1 },
        .scrollbar = .{ .offset = 10, .len = 8, .total = 40 },
    };

    const cropped = cropLastN(source, 3);
    try testing.expectEqual(@as(usize, 3), cropped.rows.len);
    try testing.expectEqual(@as(u21, '5'), cropped.rows[0].cells[0].cp);
    try testing.expectEqual(@as(u21, '7'), cropped.rows[2].cells[0].cp);
    try testing.expectEqual(@as(u16, 2), cropped.cursor.?.y);
    try testing.expect(cropped.select_head == null);
    try testing.expectEqual(@as(u32, 15), cropped.scrollbar.offset);
    try testing.expectEqual(@as(u32, 3), cropped.scrollbar.len);

    const unchanged = cropLastN(source, source.rows.len);
    try testing.expectEqual(source.rows.len, unchanged.rows.len);
    try testing.expectEqual(@as(u16, 7), unchanged.cursor.?.y);

    // A select-head inside the crop shifts with it; a cursor above the crop
    // is dropped rather than left floating on a kept row.
    var inverted = source;
    inverted.cursor = .{ .x = 0, .y = 0 };
    inverted.select_head = .{ .x = 0, .y = 6 };
    const moved = cropLastN(inverted, 3);
    try testing.expect(moved.cursor == null);
    try testing.expectEqual(@as(u16, 1), moved.select_head.?.y);
}

test "last_n paint keeps the prompt row that from_top drops" {
    const testing = @import("std").testing;
    const marks = [_][]const u8{ "T", "x", "x", "x", "x", "x", "x", "P" };
    var cells: [8]canvas.TerminalCell = undefined;
    var rows: [8]canvas.TerminalRow = undefined;
    for (marks, 0..) |mark, index| {
        cells[index] = .{ .cp = mark[0], .cluster = mark };
        rows[index] = .{ .cells = cells[index .. index + 1] };
    }
    const source: canvas.TerminalGrid = .{
        .rows = &rows,
        .background = canvas.Color.rgb8(9, 11, 15),
        .foreground = canvas.Color.rgb8(255, 255, 255),
        .cursor_color = canvas.Color.rgb8(255, 255, 255),
        .selection_color = canvas.Color.rgb8(80, 80, 80),
    };

    const builder = try testing.allocator.create(canvas.Builder);
    defer testing.allocator.destroy(builder);
    var commands: [64]canvas.CanvasCommand = undefined;
    builder.initAt(&commands);
    const keep_rows: usize = 3;
    const cell_reserve = builder.cells.len - keep_rows;
    const frame = geometry.RectF.init(0, 0, 8000, 4000);
    const base = PaintOptions{
        .frame = frame,
        .tokens = .{},
        .running = true,
        .selecting = false,
        .cell_reserve = cell_reserve,
    };

    var from_top = base;
    from_top.row_fit = .from_top;
    try paintTerminalGrid(source, builder, from_top);
    try testing.expectEqual(@as(u21, 'T'), firstPaintedCp(builder));
    try testing.expect(lastPaintedCp(builder) != 'P');

    builder.reset();
    var last_n = base;
    last_n.row_fit = .last_n;
    try paintTerminalGrid(source, builder, last_n);
    try testing.expect(firstPaintedCp(builder) != 'T');
    try testing.expectEqual(@as(u21, 'P'), lastPaintedCp(builder));
}

fn firstPaintedCp(builder: *const canvas.Builder) u21 {
    for (builder.displayList().commands) |command| {
        switch (command) {
            .cell_grid => |grid_cmd| {
                if (grid_cmd.cells.len == 0) continue;
                return clusterCp(grid_cmd, grid_cmd.cells[0]);
            },
            else => {},
        }
    }
    return 0;
}

fn lastPaintedCp(builder: *const canvas.Builder) u21 {
    var last: u21 = 0;
    for (builder.displayList().commands) |command| {
        switch (command) {
            .cell_grid => |grid_cmd| {
                if (grid_cmd.cells.len == 0) continue;
                last = clusterCp(grid_cmd, grid_cmd.cells[0]);
            },
            else => {},
        }
    }
    return last;
}

fn clusterCp(grid_cmd: canvas.CellGrid, cell: canvas.Cell) u21 {
    if (cell.text_len == 0) return 0;
    const bytes = grid_cmd.text[cell.text_offset..][0..cell.text_len];
    return bytes[0];
}
