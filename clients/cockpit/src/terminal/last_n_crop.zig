//! Product last-N crop for Hybrid C degraded panes.
//!
//! The SDK painter emits top-first and stops at the first row a budget
//! rejects, so leftover-budget truncation is first-N and hides the prompt.
//! Degraded product paint crops the snapshot to the last `allowance/cols`
//! rows (capped at `max_rows/4`) and shifts the cursor into that window
//! before the SDK painter runs. Full-fidelity panes do not crop.

const native_sdk = @import("native_sdk");
const session = @import("session.zig");

const canvas = native_sdk.canvas;

/// Last-N thumbnail height, derived from the product row ceiling.
pub const degraded_row_cap: usize = session.max_rows / 4;

/// Rows a degraded pane may keep: last `allowance/cols`, never more than
/// `max_rows/4`. Zero columns or zero allowance yields zero rows.
pub fn lastNRows(cols: usize, cell_allowance: usize) usize {
    if (cols == 0) return 0;
    return @min(cell_allowance / cols, degraded_row_cap);
}

/// Keep the bottom of the viewport so a leftover budget shows the prompt.
/// Cursor and select-head that sit above the crop are dropped; those inside
/// shift by the dropped row count. Rows themselves are a slice of the
/// producer-owned snapshot, so this allocates nothing.
pub fn cropLastN(grid: canvas.TerminalGrid, cell_allowance: usize) canvas.TerminalGrid {
    const cols = gridCols(grid);
    const keep = lastNRows(cols, cell_allowance);
    if (keep >= grid.rows.len) return grid;
    const dropped = grid.rows.len - keep;
    var cropped = grid;
    cropped.rows = grid.rows[dropped..];
    cropped.cursor = shiftCursor(grid.cursor, dropped);
    cropped.select_head = shiftCellPos(grid.select_head, dropped);
    return cropped;
}

fn gridCols(grid: canvas.TerminalGrid) usize {
    var cols: usize = 0;
    for (grid.rows) |row| cols = @max(cols, row.cells.len);
    return cols;
}

fn shiftCursor(cursor: ?canvas.TerminalCursor, dropped: usize) ?canvas.TerminalCursor {
    const value = cursor orelse return null;
    if (value.y < dropped) return null;
    var shifted = value;
    shifted.y = @intCast(@as(usize, value.y) - dropped);
    return shifted;
}

fn shiftCellPos(pos: ?canvas.TerminalCellPos, dropped: usize) ?canvas.TerminalCellPos {
    const value = pos orelse return null;
    if (value.y < dropped) return null;
    var shifted = value;
    shifted.y = @intCast(@as(usize, value.y) - dropped);
    return shifted;
}

test "last-N row count is allowance/cols capped at max_rows/4" {
    const testing = @import("std").testing;
    try testing.expectEqual(session.max_rows / 4, degraded_row_cap);
    try testing.expectEqual(@as(usize, 0), lastNRows(0, session.max_cols * degraded_row_cap));
    try testing.expectEqual(@as(usize, 0), lastNRows(session.max_cols, 0));
    try testing.expectEqual(@as(usize, 6), lastNRows(session.max_cols, 6 * session.max_cols + (session.max_cols - 1)));
    try testing.expectEqual(degraded_row_cap, lastNRows(session.max_cols, session.max_cols * degraded_row_cap));
    const extra_cols: usize = 80;
    try testing.expectEqual(degraded_row_cap, lastNRows(extra_cols, extra_cols * (degraded_row_cap + 1)));
}

test "cropLastN keeps the bottom rows and shifts the cursor" {
    const testing = @import("std").testing;
    const dark = canvas.Color.rgb8(9, 11, 15);
    var cells: [4]canvas.TerminalCell = .{
        .{ .cp = 'A', .cluster = "A" },
        .{ .cp = 'B', .cluster = "B" },
        .{ .cp = 'C', .cluster = "C" },
        .{ .cp = 'D', .cluster = "D" },
    };
    const rows = [_]canvas.TerminalRow{
        .{ .cells = cells[0..1] },
        .{ .cells = cells[1..2] },
        .{ .cells = cells[2..3] },
        .{ .cells = cells[3..4] },
    };
    const grid: canvas.TerminalGrid = .{
        .rows = &rows,
        .background = dark,
        .foreground = dark,
        .cursor_color = dark,
        .selection_color = dark,
        .cursor = .{ .x = 0, .y = 3 },
        .select_head = .{ .x = 0, .y = 2 },
    };

    const cropped = cropLastN(grid, 2);
    try testing.expectEqual(@as(usize, 2), cropped.rows.len);
    try testing.expectEqual(@as(u21, 'C'), cropped.rows[0].cells[0].cp);
    try testing.expectEqual(@as(u21, 'D'), cropped.rows[1].cells[0].cp);
    try testing.expectEqual(@as(u16, 1), cropped.cursor.?.y);
    try testing.expectEqual(@as(u16, 0), cropped.select_head.?.y);

    const hidden = cropLastN(grid, 1);
    try testing.expectEqual(@as(usize, 1), hidden.rows.len);
    try testing.expectEqual(@as(u21, 'D'), hidden.rows[0].cells[0].cp);
    // The live cursor sits on the kept last row, so it stays and shifts to 0.
    try testing.expectEqual(@as(u16, 0), hidden.cursor.?.y);
    try testing.expectEqual(@as(?canvas.TerminalCellPos, null), hidden.select_head);

    var above = grid;
    above.cursor = .{ .x = 0, .y = 0 };
    const dropped_cursor = cropLastN(above, 1);
    try testing.expectEqual(@as(?canvas.TerminalCursor, null), dropped_cursor.cursor);

    const intact = cropLastN(grid, 8);
    try testing.expectEqual(@as(usize, 4), intact.rows.len);
    try testing.expectEqual(@as(u16, 3), intact.cursor.?.y);
}

test "cropLastN does not invent first-N when the allowance covers the grid" {
    const testing = @import("std").testing;
    const dark = canvas.Color.rgb8(9, 11, 15);
    const cells = [_]canvas.TerminalCell{.{ .cp = 'Z', .cluster = "Z" }};
    const rows = [_]canvas.TerminalRow{.{ .cells = &cells }};
    const grid: canvas.TerminalGrid = .{
        .rows = &rows,
        .background = dark,
        .foreground = dark,
        .cursor_color = dark,
        .selection_color = dark,
        .cursor = .{ .x = 0, .y = 0 },
    };
    const cropped = cropLastN(grid, session.max_cols);
    try testing.expectEqual(@as(usize, 1), cropped.rows.len);
    try testing.expectEqual(@as(u21, 'Z'), cropped.rows[0].cells[0].cp);
    try testing.expectEqual(@as(u16, 0), cropped.cursor.?.y);
}
