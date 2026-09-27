const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const local = @import("../providers/local/provider.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

pub fn createSession(cols: u16, rows: u16) !*grid.Session {
    return grid.Session.create(std.heap.page_allocator, testing.io, cols, rows);
}

/// The app now opens with exactly ONE terminal and mints the rest lazily, so
/// the fixtures hand over one session and ask the model for more.
pub fn createDefaultSession() !*grid.Session {
    return grid.Session.create(testing.allocator, testing.io, 80, 24);
}

/// A bare pair of emulators for tests that paint or feed sessions directly,
/// with no model or provider involved.
pub fn createSessions(cols: u16, rows: u16) ![2]*grid.Session {
    var sessions: [2]*grid.Session = undefined;
    var created: usize = 0;
    errdefer for (sessions[0..created]) |session| session.destroy();
    while (created < sessions.len) : (created += 1) {
        sessions[created] = try createSession(cols, rows);
    }
    return sessions;
}

pub fn remoteTerminalRef(id: u32) !app.TerminalRef {
    return .{
        .provider_id = .phux,
        .terminal_id = .{ .phux = try app.RemoteResourceId.fromPhux(0, id, "local") },
    };
}

// --------------------------------------------------------- cell grids
//
// The painter emits one `cell_grid` command per row (the unit of incremental
// change), so tests read cells, not per-run text commands. A screen costs
// about one command per painted row plus a small fixed overhead.
// `CellGridView` reassembles a pane's rows into a screen: `rows()` is the
// painted height and `at(x, y)` addresses the whole screen from the top.

/// A whole terminal screen aggregated from one pane's per-row `cell_grid`
/// commands. Kept small (returned by value); rows resolve on access.
pub const CellGridView = struct {
    /// The frame's commands. The pane's rows are whichever `cell_grid`s
    /// in here fall inside `namespace`.
    commands: []const canvas.CanvasCommand,
    /// The pane's command-id namespace; every id the painter emits for the
    /// pane lies in `[namespace, namespace + id_namespace_stride)`.
    namespace: u64,

    /// Whether `command` is one of THIS pane's row commands.
    fn owns(self: CellGridView, command: canvas.CanvasCommand) bool {
        if (command != .cell_grid) return false;
        // Wrapping subtraction, because `paintIdBase` is a wrapping
        // multiply: a namespace near the top of u64 wraps, and its
        // offsets have to wrap with it.
        return (command.cell_grid.id -% self.namespace) < grid.id_namespace_stride;
    }

    /// The row command holding screen row `y`, and `y`'s index within it.
    /// Rows are ordered by geometry (`origin.y`), ties by id then position,
    /// never by emission order.
    fn rowAt(self: CellGridView, y: usize) ?RowRef {
        for (self.commands, 0..) |command, index| {
            if (!self.owns(command)) continue;
            const row = command.cell_grid;
            // This row's index in the sorted order is the number of the
            // pane's OTHER rows that sort before it.
            var rank: usize = 0;
            for (self.commands, 0..) |other_command, other_index| {
                if (other_index == index) continue;
                if (!self.owns(other_command)) continue;
                const other = other_command.cell_grid;
                const before = if (other.origin.y != row.origin.y)
                    other.origin.y < row.origin.y
                else if (other.id != row.id)
                    other.id < row.id
                else
                    other_index < index;
                if (before) rank += other.rows;
            }
            if (y >= rank and y < rank + row.rows) return .{ .grid = row, .local = y - rank };
        }
        return null;
    }

    /// Columns the pane's lattice is wide. Every row command a paint
    /// emits carries the same `cols` (the painter squares the lattice
    /// off the widest producer row), so the max is that width.
    pub fn cols(self: CellGridView) usize {
        var widest: usize = 0;
        for (self.commands) |command| {
            if (!self.owns(command)) continue;
            widest = @max(widest, command.cell_grid.cols);
        }
        return widest;
    }

    /// Rows the painter actually PUT on the glass, across every row
    /// command of this pane — the migrated form of "how many distinct
    /// text baselines did the paint produce", and the number a budget
    /// truncation shows up in.
    pub fn rows(self: CellGridView) usize {
        var total: usize = 0;
        for (self.commands) |command| {
            if (!self.owns(command)) continue;
            total += command.cell_grid.rows;
        }
        return total;
    }

    pub fn cellCount(self: CellGridView) usize {
        var total: usize = 0;
        for (self.commands) |command| {
            if (!self.owns(command)) continue;
            total += command.cell_grid.cellCount();
        }
        return total;
    }

    pub fn at(self: CellGridView, x: usize, y: usize) ?canvas.Cell {
        const row = self.rowAt(y) orelse return null;
        return row.grid.at(x, row.local);
    }

    /// The rect cell (x, y) covers, in absolute canvas points, from the
    /// owning row's own origin.
    pub fn cellRect(self: CellGridView, x: usize, y: usize) geometry.RectF {
        const row = self.rowAt(y) orelse return .{};
        return row.grid.cellRect(x, row.local);
    }

    /// The cell's grapheme cluster bytes; empty for a cell that inks
    /// nothing (a blank, a wide cell's spacer, a box-drawing cell whose
    /// ink is geometry).
    pub fn cluster(self: CellGridView, x: usize, y: usize) []const u8 {
        const row = self.rowAt(y) orelse return "";
        return rowCluster(row, x);
    }

    pub fn style(self: CellGridView, x: usize, y: usize) ?canvas.CellFlags {
        const cell = self.at(x, y) orelse return null;
        return cell.style();
    }

    /// The cell's resolved foreground, at the terminal's own 8-bit
    /// precision.
    pub fn foreground(self: CellGridView, x: usize, y: usize) ?canvas.CellColor {
        const cell = self.at(x, y) orelse return null;
        return cell.fg;
    }

    /// The cell's own background, or null when it paints none and the
    /// grid's surface shows through. The distinction is load-bearing:
    /// `has_background` is what says a cell painted a background at all.
    pub fn background(self: CellGridView, x: usize, y: usize) ?canvas.CellColor {
        const cell = self.at(x, y) orelse return null;
        if (!cell.style().has_background) return null;
        return cell.bg;
    }

    /// The cell's SGR 58 underline colour, or null (the stored colour is
    /// zeroed, so `has_underline_color` decides).
    pub fn underlineColor(self: CellGridView, x: usize, y: usize) ?canvas.CellColor {
        const cell = self.at(x, y) orelse return null;
        if (!cell.style().has_underline_color) return null;
        return cell.underline_color;
    }

    /// Row `y` as a renderer would ink it: every cell's cluster bytes,
    /// left to right. Cells that ink nothing contribute nothing, so a
    /// row's trailing blanks never reach the string.
    pub fn rowText(self: CellGridView, y: usize, out: []u8) []const u8 {
        const row = self.rowAt(y) orelse return out[0..0];
        var len: usize = 0;
        var x: usize = 0;
        while (x < row.grid.cols) : (x += 1) {
            const bytes = rowCluster(row, x);
            if (len + bytes.len > out.len) break;
            @memcpy(out[len..][0..bytes.len], bytes);
            len += bytes.len;
        }
        return out[0..len];
    }

    /// Where `needle` starts in the screen, scanning rows top to bottom
    /// — the column of the first CELL whose cluster begins the match.
    /// Null when no row carries it.
    pub fn find(self: CellGridView, needle: []const u8) ?CellPos {
        const total = self.rows();
        var y: usize = 0;
        while (y < total) : (y += 1) {
            if (self.findInRow(y, needle)) |x| return .{ .x = x, .y = y };
        }
        return null;
    }

    /// Where `needle` starts in row `y`, as a COLUMN. Built by walking
    /// the row's clusters and remembering which column each byte came
    /// from, so a multi-byte or multi-cell match still reports the cell
    /// it began in.
    pub fn findInRow(self: CellGridView, y: usize, needle: []const u8) ?usize {
        if (needle.len == 0) return null;
        const row = self.rowAt(y) orelse return null;
        var text: [row_text_capacity]u8 = undefined;
        var columns: [row_text_capacity]u16 = undefined;
        var len: usize = 0;
        var x: usize = 0;
        while (x < row.grid.cols) : (x += 1) {
            const bytes = rowCluster(row, x);
            if (len + bytes.len > text.len) break;
            for (bytes, 0..) |byte, offset| {
                text[len + offset] = byte;
                columns[len + offset] = @intCast(x);
            }
            len += bytes.len;
        }
        const hit = std.mem.indexOf(u8, text[0..len], needle) orelse return null;
        return columns[hit];
    }
};

/// One resolved screen row: the command that holds it and the row's
/// index inside that command (0 for the one-row commands the painter
/// emits; the field keeps the view correct if a producer ever hands over
/// a multi-row lattice again).
const RowRef = struct { grid: canvas.CellGrid, local: usize };

fn rowCluster(row: RowRef, x: usize) []const u8 {
    const cell = row.grid.at(x, row.local) orelse return "";
    return cell.cluster(row.grid.text);
}

pub const CellPos = struct { x: usize, y: usize };

/// A row's cluster bytes can exceed one byte per column (combining
/// marks, wide clusters); sized well past `terminal_grid.max_cols` so a
/// dense row still resolves whole.
const row_text_capacity = 8192;

/// The screen of the FIRST pane that painted into this list — the shape
/// a single-terminal paint produces. The pane is identified by the
/// namespace its first row command sits in, so this aggregates that
/// terminal's rows and nothing else, even in a frame that holds several.
pub fn findCellGrid(display_list: anytype) ?CellGridView {
    for (display_list.commands) |command| {
        if (command != .cell_grid) continue;
        return .{
            .commands = display_list.commands,
            .namespace = grid.idNamespaceOf(command.cell_grid.id),
        };
    }
    return null;
}

pub fn expectCellGrid(display_list: anytype) !CellGridView {
    return findCellGrid(display_list) orelse error.TestExpectedCellGrid;
}

/// The screen belonging to pane `index`, by the painter's command-id
/// namespace. Null when the pane painted no row at all, which is distinct
/// from an empty screen.
pub fn findPaneCellGrid(display_list: anytype, index: usize) ?CellGridView {
    const view = CellGridView{
        .commands = display_list.commands,
        .namespace = grid.idNamespace(grid.paneIdBase(index)),
    };
    if (view.rows() == 0) return null;
    return view;
}

comptime {
    // The view is a slice plus a namespace and stays that way: it is
    // returned by value from every finder above, and this repo has
    // already paid for large returned aggregates once (see
    // `initTerminalApp`). 24 bytes = 16 (slice) + 8 (namespace).
    std.debug.assert(@sizeOf(CellGridView) == 24);
}

/// Commands one paint spends outside its rows (the SDK's fixed prologue and
/// epilogue reserve); used only as an upper bound.
pub const paint_fixed_commands: usize = 16;

pub const CursorPaintKind = enum { filled, hollow };

pub fn expectCursorPaintKind(display_list: anytype, expected: CursorPaintKind) !void {
    return expectPaneCursorPaintKind(display_list, 0, expected);
}

pub fn expectPaneCursorPaintKind(display_list: anytype, index: usize, expected: CursorPaintKind) !void {
    const id = grid.cursorCommandId(grid.paneIdBase(index));
    const command = display_list.findCommandById(id) orelse return error.TestExpectedCursor;
    switch (command.command) {
        .fill_rect => try testing.expectEqual(CursorPaintKind.filled, expected),
        .stroke_rect => try testing.expectEqual(CursorPaintKind.hollow, expected),
        else => return error.TestUnexpectedCursorCommand,
    }
}
