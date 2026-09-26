//! Hybrid C paint budgets (Cockpit pkg3b / Metal).
//!
//! Every number is derived from the pinned SDK paint tables and the product
//! grid. There are no literals here for cells, glyphs, commands, text, or
//! paths. The policy is:
//!
//!   * if `plan` is given measured cols×rows per visible pane and those
//!     cells fit in the store, every one of them paints full;
//!   * otherwise Hybrid C: focused pane of the active window: full product
//!     grid, if the store still has one after holding degraded shares for
//!     later panes;
//!   * every other pane, and every pane in an inactive window: degraded —
//!     leftover after the full pane(s), cropped last-N at
//!     `max_rows / 4` rows at `max_cols` (SDK first-N that hides the
//!     prompt is not the lasting tier);
//!   * glyphs are not an equal cut with no slack. Focused takes what remains
//!     after degraded holds. Commands/text/paths keep forward-slack inside
//!     a tier so a later full pane cannot be stolen by an earlier thumbnail;
//!   * `widget_cell_reserve` is the SDK's two-pane leftover (`store / 2`).
//!     Production does not read it. A lone 320x96 grid needs the whole
//!     store, not half of it.
//!
//! The cell store on the current pin holds more than one full product
//! grid (`maxFullPanesThatFit`). Hybrid C still refuses to treat
//! `layout.max_panes` full grids as one envelope.

const native_sdk = @import("native_sdk");
const grid = @import("../../terminal/grid.zig");
const layout = @import("../layout.zig");
const projection = @import("workspace_projection.zig");

const canvas = native_sdk.canvas;

pub const cell_store: usize = canvas.max_display_list_cells;
pub const text_store: usize = canvas.max_display_list_text_bytes;
pub const path_store: usize = canvas.max_chart_path_elements_per_frame;
pub const glyph_budget: usize = canvas.terminal_grid.widget_glyph_budget;
pub const command_envelope: usize = projection.chrome_command_envelope;
pub const widget_text_reserve: usize = canvas.terminal_grid.widget_text_reserve;
pub const widget_path_reserve: usize = canvas.terminal_grid.widget_path_reserve;

/// One full-fidelity product grid, or the whole store if the store is smaller.
pub const full_cells: usize = @min(grid.max_cells, cell_store);

/// How many full product grids the current cell store can hold at once.
/// Integer division: leftover cells do not count as another full pane.
pub fn maxFullPanesThatFit() usize {
    if (full_cells == 0) return 0;
    return cell_store / full_cells;
}

/// Last-N thumbnail height, derived from the product row ceiling. 96 / 4 = 24
/// matches the box-drawing adversarial grid already used to bind commands.
pub const degraded_rows: usize = grid.max_rows / 4;

/// Cell cap for one degraded pane at the product column ceiling.
pub const degraded_cell_cap: usize = grid.max_cols * degraded_rows;

/// Packed `cell_grid` is one command per row. Box-drawing is a different
/// bind and does not fit even N=1 at 320x96; these shares size the packed
/// path, not U+256C geometry.
pub const full_command_share: usize = grid.max_rows;
pub const degraded_command_share: usize = degraded_rows;

pub const Fidelity = enum { full, degraded };

pub const Allocation = struct {
    fidelity: Fidelity,
    command_budget: usize,
    text_reserve: usize,
    path_reserve: usize,
    glyph_budget: usize,
    cell_reserve: usize,

    /// Extra last-N row ceiling. Full panes take the cell store only;
    /// degraded panes never keep more than `max_rows / 4`.
    pub fn rowCap(self: Allocation) usize {
        return switch (self.fidelity) {
            .full => 0,
            .degraded => degraded_rows,
        };
    }
};

pub const Plan = struct {
    count: usize,
    n_full: usize,
    n_degraded: usize,
    leftover_cells: usize,
    unused_cells: usize,
    degraded_cell_share: usize,
    full_text_share: usize,
    degraded_text_share: usize,
    unused_text: usize,
    full_path_share: usize,
    degraded_path_share: usize,
    unused_paths: usize,
    full_glyph_share: usize,
    degraded_glyph_share: usize,
    /// Cells held for each later full pane. Hybrid C uses `full_cells`
    /// (product max). Measured panes that fit hold nothing extra: they
    /// already fit, and charging `full_cells` apiece would crop a typical
    /// split that the store can actually keep.
    full_cell_hold: usize = full_cells,
    fidelities: [layout.max_panes]Fidelity = @splat(.degraded),

    pub fn forPane(self: Plan, index: usize, prologue: usize) Allocation {
        const remaining = self.remainingAfter(index);
        const fidelity = self.fidelities[index];
        const held_commands = remaining.full * full_command_share + remaining.degraded * degraded_command_share;
        const command_budget = prologue + command_envelope - @min(command_envelope, held_commands);
        const text_reserve = widget_text_reserve + remaining.full * self.full_text_share + remaining.degraded * self.degraded_text_share + self.unused_text;
        const path_reserve = widget_path_reserve + remaining.full * self.full_path_share + remaining.degraded * self.degraded_path_share + self.unused_paths;
        // Unused leftover is held so a later thumbnail cannot spend the
        // rest of the store. A last (or only) full pane is not a
        // thumbnail: it keeps that slack. A last degraded pane still holds
        // it, or leftover after the 24-row cap would paint nearly full.
        const hold_unused = remaining.full + remaining.degraded > 0 or fidelity == .degraded;
        const cell_reserve = remaining.full * self.full_cell_hold + remaining.degraded * self.degraded_cell_share +
            if (hold_unused) self.unused_cells else 0;
        const this_glyphs = switch (fidelity) {
            .full => self.full_glyph_share,
            .degraded => self.degraded_glyph_share,
        };
        return .{
            .fidelity = fidelity,
            .command_budget = command_budget,
            .text_reserve = text_reserve,
            .path_reserve = path_reserve,
            .glyph_budget = this_glyphs,
            .cell_reserve = cell_reserve,
        };
    }

    fn remainingAfter(self: Plan, index: usize) struct { full: usize, degraded: usize } {
        var full: usize = 0;
        var degraded: usize = 0;
        var i: usize = index + 1;
        while (i < self.count) : (i += 1) {
            switch (self.fidelities[i]) {
                .full => full += 1,
                .degraded => degraded += 1,
            }
        }
        return .{ .full = full, .degraded = degraded };
    }
};

pub const PlanArgs = struct {
    window_active: bool,
    pane_count: usize,
    focused: []const bool,
    /// Measured cols×rows for each visible pane, same order as `focused`.
    /// Empty (the default) means Hybrid C at the product max. When the
    /// slice length matches `pane_count` and the cells fit in the store,
    /// every visible pane paints full.
    pane_cells: []const usize = &.{},
};

/// Split a store the way Hybrid C splits cells: each degraded pane holds a
/// full-grid proportion of `degraded_cell_share`, then the focused pane takes
/// the remainder. After the 4x cell bump the cap binds, so N degraded panes
/// can ask for more than the store (`n * cap > full_cells`). Saturate so
/// `plan` cannot overflow; later full panes still get whatever is left.
fn sharesAfterDegraded(total: usize, n_full: usize, n_degraded: usize, degraded_cell_share: usize) struct { full: usize, degraded: usize, unused: usize } {
    const proportional: usize = if (n_degraded == 0 or full_cells == 0)
        0
    else
        total * degraded_cell_share / full_cells;
    const degraded: usize = if (n_degraded == 0) 0 else @min(proportional, total / n_degraded);
    const rest = total -| n_degraded * degraded;
    const full: usize = if (n_full == 0) 0 else rest;
    const unused = total -| n_full * full -| n_degraded * degraded;
    return .{ .full = full, .degraded = degraded, .unused = unused };
}

fn measuredCellsFit(args: PlanArgs) bool {
    if (args.pane_cells.len != args.pane_count) return false;
    var sum: usize = 0;
    for (args.pane_cells) |cells| {
        sum +|= cells;
        if (sum > cell_store) return false;
    }
    return true;
}

pub fn plan(args: PlanArgs) Plan {
    const count = args.pane_count;
    if (measuredCellsFit(args)) return planAllFull(count, args.pane_cells);
    var fidelities: [layout.max_panes]Fidelity = @splat(.degraded);
    var n_full: usize = 0;
    const full_slots = maxFullPanesThatFit();
    var index: usize = 0;
    while (index < count) : (index += 1) {
        const want_full = args.window_active and index < args.focused.len and args.focused[index];
        if (want_full and n_full < full_slots) {
            fidelities[index] = .full;
            n_full += 1;
        }
    }
    const n_degraded = count - n_full;
    const leftover_cells = cell_store - n_full * full_cells;
    const degraded_cell_share = if (n_degraded == 0)
        0
    else
        @min(leftover_cells / n_degraded, degraded_cell_cap);

    const text = sharesAfterDegraded(text_store - widget_text_reserve, n_full, n_degraded, degraded_cell_share);
    const paths = sharesAfterDegraded(path_store - widget_path_reserve, n_full, n_degraded, degraded_cell_share);
    const glyphs = sharesAfterDegraded(glyph_budget, n_full, n_degraded, degraded_cell_share);

    return .{
        .count = count,
        .n_full = n_full,
        .n_degraded = n_degraded,
        .leftover_cells = leftover_cells,
        .unused_cells = leftover_cells -| n_degraded * degraded_cell_share,
        .degraded_cell_share = degraded_cell_share,
        .full_text_share = text.full,
        .degraded_text_share = text.degraded,
        .unused_text = text.unused,
        .full_path_share = paths.full,
        .degraded_path_share = paths.degraded,
        .unused_paths = paths.unused,
        .full_glyph_share = if (n_full == 0) glyph_budget else glyphs.full,
        .degraded_glyph_share = glyphs.degraded,
        .fidelities = fidelities,
    };
}

fn planAllFull(count: usize, pane_cells: []const usize) Plan {
    var fidelities: [layout.max_panes]Fidelity = @splat(.degraded);
    var used_cells: usize = 0;
    var index: usize = 0;
    while (index < count) : (index += 1) {
        fidelities[index] = .full;
        used_cells +|= pane_cells[index];
    }
    return .{
        .count = count,
        .n_full = count,
        .n_degraded = 0,
        .leftover_cells = cell_store -| used_cells,
        .unused_cells = 0,
        .degraded_cell_share = 0,
        .full_text_share = 0,
        .degraded_text_share = 0,
        .unused_text = 0,
        .full_path_share = 0,
        .degraded_path_share = 0,
        .unused_paths = 0,
        .full_glyph_share = glyph_budget,
        .degraded_glyph_share = 0,
        .full_cell_hold = 0,
        .fidelities = fidelities,
    };
}

/// The equal-cut the painter used before Hybrid C. Kept for measurement
/// comparison, not as a production policy.
pub fn equalCutCellShare(pane_count: usize) usize {
    return cell_store / @max(@as(usize, 1), pane_count);
}

pub fn equalCutGlyphShare(pane_count: usize) usize {
    return glyph_budget / @max(@as(usize, 1), pane_count);
}

/// Trailing rows a pane may keep under last-N crop. Full panes are bounded
/// only by the cell store; degraded panes are also capped at `degraded_rows`.
pub fn keepRows(fidelity: Fidelity, cols: usize, source_rows: usize, cell_reserve: usize, cells_used: usize) usize {
    const cells_available = cell_store -| cell_reserve -| cells_used;
    const by_cells = if (cols == 0) source_rows else cells_available / cols;
    const cap = switch (fidelity) {
        .full => source_rows,
        .degraded => @min(source_rows, degraded_rows),
    };
    return @min(cap, by_cells);
}

test "a lone active pane keeps the whole cell store" {
    const testing = @import("std").testing;
    const planned = plan(.{
        .window_active = true,
        .pane_count = 1,
        .focused = &.{true},
    });
    try testing.expectEqual(@as(usize, 1), planned.n_full);
    try testing.expectEqual(Fidelity.full, planned.fidelities[0]);
    const alloc = planned.forPane(0, 0);
    // Leftover after one full product grid stays in unused_cells
    // (`cell_store - full_cells`). That is slack, not the SDK two-pane
    // floor, and a lone full pane is not charged it: nothing later needs
    // holding.
    try testing.expectEqual(cell_store - full_cells, planned.unused_cells);
    try testing.expectEqual(@as(usize, 0), alloc.cell_reserve);
    try testing.expectEqual(command_envelope, alloc.command_budget);
    try testing.expectEqual(glyph_budget, alloc.glyph_budget);
    // The SDK two-pane leftover must not become a production floor.
    // After the 4x cell bump, half the store exceeds one product grid;
    // production still does not read `widget_cell_reserve`.
    try testing.expect(canvas.terminal_grid.widget_cell_reserve * 2 == cell_store);
    try testing.expect(alloc.cell_reserve != canvas.terminal_grid.widget_cell_reserve);
    try testing.expect(full_cells != canvas.terminal_grid.widget_cell_reserve);
}

test "focused of two panes takes a full grid; the neighbour takes leftover" {
    const testing = @import("std").testing;
    const planned = plan(.{
        .window_active = true,
        .pane_count = 2,
        .focused = &.{ true, false },
    });
    try testing.expectEqual(@as(usize, 1), planned.n_full);
    try testing.expectEqual(@as(usize, 1), planned.n_degraded);
    try testing.expectEqual(cell_store - full_cells, planned.leftover_cells);
    try testing.expectEqual(@min(planned.leftover_cells, degraded_cell_cap), planned.degraded_cell_share);
    try testing.expect(planned.leftover_cells > planned.degraded_cell_share);
    const focused = planned.forPane(0, 0);
    const other = planned.forPane(1, 0);
    try testing.expectEqual(Fidelity.full, focused.fidelity);
    try testing.expectEqual(Fidelity.degraded, other.fidelity);
    try testing.expectEqual(planned.leftover_cells, focused.cell_reserve);
    try testing.expectEqual(planned.unused_cells, other.cell_reserve);
    try testing.expect(focused.glyph_budget > equalCutGlyphShare(2));
    try testing.expect(focused.glyph_budget > other.glyph_budget);
}

test "an unfocused pane that paints first still holds the focused grid" {
    const testing = @import("std").testing;
    const planned = plan(.{
        .window_active = true,
        .pane_count = 2,
        .focused = &.{ false, true },
    });
    const first = planned.forPane(0, 0);
    const focused = planned.forPane(1, 0);
    try testing.expectEqual(Fidelity.degraded, first.fidelity);
    try testing.expectEqual(Fidelity.full, focused.fidelity);
    try testing.expectEqual(full_cells + planned.unused_cells, first.cell_reserve);
    try testing.expectEqual(@as(usize, 0), focused.cell_reserve);
}

test "inactive windows degrade every pane" {
    const testing = @import("std").testing;
    const planned = plan(.{
        .window_active = false,
        .pane_count = 2,
        .focused = &.{ true, false },
    });
    try testing.expectEqual(@as(usize, 0), planned.n_full);
    try testing.expectEqual(@as(usize, 2), planned.n_degraded);
    try testing.expectEqual(Fidelity.degraded, planned.fidelities[0]);
    try testing.expectEqual(Fidelity.degraded, planned.fidelities[1]);
    try testing.expectEqual(degraded_cell_cap, planned.degraded_cell_share);
    const first = planned.forPane(0, 0);
    const last = planned.forPane(1, 0);
    try testing.expectEqual(cell_store - planned.degraded_cell_share, first.cell_reserve);
    try testing.expectEqual(planned.unused_cells, last.cell_reserve);
}

test "the current pin holds two full product grids, not sixteen" {
    const testing = @import("std").testing;
    // Cockpit pkg3b / Metal Hybrid C signed bump. The first assertion is
    // the pin: two full product grids share one envelope. The second is
    // the forever claim: N = layout.max_panes full grids do not.
    try testing.expect(grid.max_cells * 2 < cell_store);
    try testing.expect(grid.max_cells * layout.max_panes > cell_store);
    try testing.expectEqual(cell_store / grid.max_cells, maxFullPanesThatFit());
    try testing.expect(maxFullPanesThatFit() >= 2);
    try testing.expect(maxFullPanesThatFit() < layout.max_panes);
    try testing.expectEqual(grid.max_cols * grid.max_rows, grid.max_cells);
}

test "plan shares fit the stores at eight panes" {
    const testing = @import("std").testing;
    const planned = plan(.{
        .window_active = true,
        .pane_count = 8,
        .focused = &.{ true, false, false, false, false, false, false, false },
    });
    try testing.expectEqual(@as(usize, 1), planned.n_full);
    try testing.expectEqual(@as(usize, 7), planned.n_degraded);
    const grid_text = text_store - widget_text_reserve;
    try testing.expect(planned.n_full * planned.full_text_share + planned.n_degraded * planned.degraded_text_share + planned.unused_text == grid_text);
    try testing.expect(planned.n_degraded * planned.degraded_glyph_share <= glyph_budget);
    try testing.expect(planned.full_glyph_share + planned.n_degraded * planned.degraded_glyph_share <= glyph_budget);
    _ = planned.forPane(0, 0);
    _ = planned.forPane(7, 0);
}

test "degraded last-n on this pin is bound by the row cap, not leftover" {
    const testing = @import("std").testing;
    const leftover = cell_store - full_cells;
    try testing.expect(leftover / grid.max_cols > degraded_rows);
    try testing.expectEqual(
        degraded_rows,
        keepRows(.degraded, grid.max_cols, grid.max_rows, 0, full_cells),
    );

    const inactive = plan(.{
        .window_active = false,
        .pane_count = 2,
        .focused = &.{ true, false },
    });
    const first = inactive.forPane(0, 0);
    try testing.expectEqual(degraded_rows, first.rowCap());
    try testing.expectEqual(
        degraded_rows,
        keepRows(.degraded, grid.max_cols, grid.max_rows, first.cell_reserve, 0),
    );
    try testing.expectEqual(@as(usize, 0), plan(.{
        .window_active = true,
        .pane_count = 1,
        .focused = &.{true},
    }).forPane(0, 0).rowCap());
}

test "measured split panes that fit stay full when focus swaps" {
    const testing = @import("std").testing;
    const cols: usize = 80;
    const rows: usize = 48;
    try testing.expect(rows > degraded_rows);
    const cells = [_]usize{ cols * rows, cols * rows };
    try testing.expect(cells[0] + cells[1] <= cell_store);

    const left = plan(.{
        .window_active = true,
        .pane_count = 2,
        .focused = &.{ true, false },
        .pane_cells = &cells,
    });
    const right = plan(.{
        .window_active = true,
        .pane_count = 2,
        .focused = &.{ false, true },
        .pane_cells = &cells,
    });
    try testing.expectEqual(@as(usize, 2), left.n_full);
    try testing.expectEqual(@as(usize, 0), left.n_degraded);
    try testing.expectEqual(@as(usize, 2), right.n_full);
    try testing.expectEqual(Fidelity.full, left.fidelities[0]);
    try testing.expectEqual(Fidelity.full, left.fidelities[1]);
    try testing.expectEqual(Fidelity.full, right.fidelities[0]);
    try testing.expectEqual(Fidelity.full, right.fidelities[1]);

    const left_unfocused = left.forPane(1, 0);
    const right_unfocused = right.forPane(0, 0);
    try testing.expectEqual(@as(usize, 0), left_unfocused.rowCap());
    try testing.expectEqual(@as(usize, 0), right_unfocused.rowCap());
    try testing.expectEqual(
        rows,
        keepRows(left_unfocused.fidelity, cols, rows, left_unfocused.cell_reserve, 0),
    );
    try testing.expectEqual(
        rows,
        keepRows(right_unfocused.fidelity, cols, rows, right_unfocused.cell_reserve, 0),
    );
}

test "two measured product-max panes still fit and stay full" {
    const testing = @import("std").testing;
    try testing.expect(2 * full_cells <= cell_store);
    const cells = [_]usize{ full_cells, full_cells };
    const planned = plan(.{
        .window_active = true,
        .pane_count = 2,
        .focused = &.{ true, false },
        .pane_cells = &cells,
    });
    try testing.expectEqual(@as(usize, 2), planned.n_full);
    try testing.expectEqual(Fidelity.full, planned.fidelities[0]);
    try testing.expectEqual(Fidelity.full, planned.fidelities[1]);
    try testing.expectEqual(@as(usize, 0), planned.forPane(1, 0).rowCap());
}

test "measured panes that overflow keep Hybrid C" {
    const testing = @import("std").testing;
    const cells = [_]usize{ full_cells, full_cells, full_cells, full_cells, full_cells };
    try testing.expect(cells.len * full_cells > cell_store);
    const focused = [_]bool{ true, false, false, false, false };
    const measured = plan(.{
        .window_active = true,
        .pane_count = cells.len,
        .focused = &focused,
        .pane_cells = &cells,
    });
    const hybrid = plan(.{
        .window_active = true,
        .pane_count = cells.len,
        .focused = &focused,
    });
    try testing.expectEqual(hybrid.n_full, measured.n_full);
    try testing.expectEqual(hybrid.n_degraded, measured.n_degraded);
    try testing.expectEqual(hybrid.fidelities[0], measured.fidelities[0]);
    try testing.expectEqual(hybrid.fidelities[1], measured.fidelities[1]);
    try testing.expectEqual(hybrid.degraded_cell_share, measured.degraded_cell_share);
    try testing.expectEqual(hybrid.full_cell_hold, measured.full_cell_hold);
    try testing.expectEqual(hybrid.forPane(0, 0).rowCap(), measured.forPane(0, 0).rowCap());
    try testing.expectEqual(hybrid.forPane(1, 0).rowCap(), measured.forPane(1, 0).rowCap());
    try testing.expectEqual(Fidelity.full, measured.fidelities[0]);
    try testing.expectEqual(Fidelity.degraded, measured.fidelities[1]);
    try testing.expectEqual(degraded_rows, measured.forPane(1, 0).rowCap());
}
