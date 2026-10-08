//! Nerd symbols are a glyph fallback, not Paper's bold/italic companions.
//! The pinned reference renderer has no font cascade. Split packed rows at
//! family boundaries so both rasterizers see the same symbol face and keep
//! Paper's measured lattice, backgrounds, decorations and selection order.
const std = @import("std");
const canvas = @import("native_sdk").canvas;
const fonts = @import("fonts.zig");
const config = @import("../config/config.zig");

const paper = canvas.font_ttf.Face.parse(@embedFile("../fonts/PaperMono-Regular.ttf")) catch unreachable;
const nerd = canvas.font_ttf.Face.parse(@embedFile("../fonts/JetBrainsMonoNLNerdFontMono-Regular.ttf")) catch unreachable;

fn isNerd(cp: u21) bool {
    // The pinned Nerd TTF's cmap12 has one supplementary group:
    // F0001..F1AF0 (verified by verify-paper-mono.py). The SDK reference
    // parser only supports cmap4, so use that group for host routing while
    // retaining the reference renderer's existing supplementary limitation.
    if (cp > 0xffff) return cp >= 0xf0001 and cp <= 0xf1af0;
    return cp >= 0xe000 and cp <= 0xf8ff and paper.glyphIndex(cp) == 0 and nerd.glyphIndex(cp) != 0;
}

fn clusterIsNerd(cluster: []const u8) bool {
    var iterator = std.unicode.Utf8Iterator{ .bytes = cluster, .i = 0 };
    return isNerd(iterator.nextCodepoint() orelse 0);
}

fn extraRowCommands(row: canvas.TerminalRow, cols: usize) usize {
    var previous = false;
    var extra: usize = 0;
    for (row.cells[0..@min(row.cells.len, canvas.terminal_grid.max_cols)], 0..) |cell, index| {
        const fallback = if (cell.wide == .spacer) previous else isNerd(cell.cp);
        if (index != 0 and fallback != previous) extra += 1;
        previous = fallback;
    }
    if (row.cells.len < cols and previous) extra += 1;
    return extra;
}

pub fn needsReservation(grid: canvas.TerminalGrid) bool {
    const cols = gridCols(grid);
    for (grid.rows) |row| if (extraRowCommands(row, cols) != 0) return true;
    return false;
}

fn gridCols(grid: canvas.TerminalGrid) usize {
    var cols: usize = 0;
    for (grid.rows) |row| cols = @max(cols, @min(row.cells.len, canvas.terminal_grid.max_cols));
    return cols;
}

/// Reserve only rows that can seat their family runs plus SDK fixed overhead.
/// The caller applies its existing top/last-N cropping policy to this count;
/// SDK preflight still handles box geometry, text and cell-store ceilings.
pub fn reservation(grid: canvas.TerminalGrid, available: usize, available_text: usize, from_bottom: bool) struct { rows: usize, extra: usize, text: usize, store: ?canvas.DisplayListStore } {
    var result: @TypeOf(reservation(grid, available, available_text, from_bottom)) = .{ .rows = 0, .extra = 0, .text = 0, .store = null };
    var remaining = available -| 8;
    const cols = gridCols(grid);
    var text_remaining = available_text;
    for (0..@min(grid.rows.len, canvas.terminal_grid.max_rows)) |offset| {
        const index = if (from_bottom) grid.rows.len - 1 - offset else offset;
        const extra = extraRowCommands(grid.rows[index], cols);
        const text = if (extra == 0) 0 else expandedRowText(grid.rows[index]);
        if (extra + 1 > remaining) {
            result.store = .commands;
            break;
        }
        if (text > text_remaining) {
            result.store = .text_bytes;
            break;
        }
        text_remaining -= text;
        result.text += text;
        remaining -= extra + 1;
        result.rows += 1;
        result.extra += extra;
    }
    if (result.rows < grid.rows.len and result.store == null) result.store = .commands;
    return result;
}

fn expandedRowText(row: canvas.TerminalRow) usize {
    var bytes: usize = 0;
    for (row.cells[0..@min(row.cells.len, canvas.terminal_grid.max_cols)]) |cell| bytes += @min(cell.cluster.len, 255);
    return bytes;
}

/// Runs share builder-owned cells; multi-run rows get compact text blobs so
/// retained storage charges each cluster once, not the whole row per run.
/// Reverse traversal leaves original row and overlay indices valid. Run IDs
/// encode row and starting column in a free part of the pane's 24-bit space.
pub fn splitRows(builder: *canvas.Builder, first_command: usize, id_base: u64) void {
    var index = builder.len;
    while (index > first_command) {
        index -= 1;
        if (builder.commands[index] != .cell_grid) continue;
        splitRow(builder, index, id_base);
    }
}

fn cellIsNerd(cell: canvas.Cell, text: []const u8, previous: bool) bool {
    const flags: canvas.CellFlags = @bitCast(cell.flags);
    return if (flags.width == .spacer) previous else clusterIsNerd(cell.cluster(text));
}

fn splitRow(builder: *canvas.Builder, index: usize, id_base: u64) void {
    const row = builder.commands[index].cell_grid;
    var runs: [canvas.terminal_grid.max_cols]canvas.CanvasCommand = undefined;
    var len: usize = 0;
    var start: usize = 0;
    while (start < row.cells.len) {
        const fallback = clusterIsNerd(row.cells[start].cluster(row.text));
        var end = start + 1;
        while (end < row.cells.len and cellIsNerd(row.cells[end], row.text, fallback) == fallback) : (end += 1) {}
        var run = row;
        run.origin.x += @as(f32, @floatFromInt(start)) * row.cell_width;
        run.cols = @intCast(end - start);
        run.cells = row.cells[start..end];
        // row.id uses SDK's 0x60_0000 + row offset. Keep this namespace even
        // when the caller is anonymous, where all IDs must stay zero.
        run.id = if (row.id == 0) 0 else canvas.terminal_grid.paintIdBase(id_base) + 0x70_0000 + (row.id & 0xffff) * canvas.terminal_grid.max_cols + start;
        if (fallback) {
            var tokens: canvas.DesignTokens = .{};
            fonts.apply(&tokens, config.FontChoice.bundled);
            run.font_id = tokens.typography.mono_font_id;
            run.bold_font_id = tokens.typography.mono_bold_font_id;
            run.italic_font_id = tokens.typography.mono_italic_font_id;
            run.bold_italic_font_id = tokens.typography.mono_bold_italic_font_id;
        }
        runs[len] = .{ .cell_grid = run };
        len += 1;
        start = end;
    }
    replaceRow(builder, index, runs[0..len], row.font_id);
}

fn replaceRow(builder: *canvas.Builder, index: usize, runs: []canvas.CanvasCommand, original_font: canvas.FontId) void {
    if (runs.len == 0) return;
    if (runs.len == 1 and runs[0].cell_grid.font_id == original_font) return;
    if (runs.len > 1) {
        for (runs) |*command| compactRunText(builder, &command.cell_grid);
    }
    const extra = runs.len - 1;
    std.debug.assert(builder.len + extra <= builder.commands.len);
    std.mem.copyBackwards(canvas.CanvasCommand, builder.commands[index + runs.len .. builder.len + extra], builder.commands[index + 1 .. builder.len]);
    @memcpy(builder.commands[index..][0..runs.len], runs);
    builder.len += extra;
}

fn compactRunText(builder: *canvas.Builder, run: *canvas.CellGrid) void {
    const start = builder.text_byte_len;
    // SDK paint staged these cells in builder.cells; no provider-owned slice
    // is mutated. Old row bytes remain intact while new compact blobs append.
    for (@constCast(run.cells)) |*cell| {
        const cluster = cell.cluster(run.text);
        cell.text_offset = @intCast(builder.text_byte_len - start);
        _ = builder.allocTextBytes(cluster) catch unreachable; // reserved before SDK paint
    }
    run.text = builder.text_bytes[start..builder.text_byte_len];
}
