//! Hybrid C paint budgets (Cockpit pkg3b / Metal).
//!
//! Every number is derived from the pinned SDK paint tables and the product
//! grid. There are no literals here for cells, glyphs, commands, text, or
//! paths. The policy is:
//!
//!   * focused pane of the active window: full product grid, if the store
//!     still has one after holding degraded shares for later panes;
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
//! The cell store on the current pin holds one full product grid, not two.
//! That fact is a regression, not a forever invariant: a pin bump Metal
//! approves will raise `maxFullPanesThatFit`. Hybrid C still refuses to
//! treat `layout.max_panes` full grids as one envelope.

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
        const cell_reserve = remaining.full * full_cells + remaining.degraded * self.degraded_cell_share +
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
};

pub fn plan(args: PlanArgs) Plan {
    const count = args.pane_count;
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

    const grid_text = text_store - widget_text_reserve;
    const degraded_text_share = if (n_degraded == 0 or full_cells == 0)
        0
    else
        grid_text * degraded_cell_share / full_cells;
    const full_text_share = if (n_full == 0)
        0
    else
        grid_text - n_degraded * degraded_text_share;
    const unused_text = grid_text - n_full * full_text_share - n_degraded * degraded_text_share;

    const grid_paths = path_store - widget_path_reserve;
    const degraded_path_share = if (n_degraded == 0 or full_cells == 0)
        0
    else
        grid_paths * degraded_cell_share / full_cells;
    const full_path_share = if (n_full == 0)
        0
    else
        grid_paths - n_degraded * degraded_path_share;
    const unused_paths = grid_paths - n_full * full_path_share - n_degraded * degraded_path_share;

    const degraded_glyph_share = if (n_degraded == 0 or full_cells == 0)
        0
    else
        glyph_budget * degraded_cell_share / full_cells;
    const full_glyph_share = if (n_full == 0)
        glyph_budget
    else
        glyph_budget - n_degraded * degraded_glyph_share;

    return .{
        .count = count,
        .n_full = n_full,
        .n_degraded = n_degraded,
        .leftover_cells = leftover_cells,
        .unused_cells = leftover_cells - n_degraded * degraded_cell_share,
        .degraded_cell_share = degraded_cell_share,
        .full_text_share = full_text_share,
        .degraded_text_share = degraded_text_share,
        .unused_text = unused_text,
        .full_path_share = full_path_share,
        .degraded_path_share = degraded_path_share,
        .unused_paths = unused_paths,
        .full_glyph_share = full_glyph_share,
        .degraded_glyph_share = degraded_glyph_share,
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
    // Leftover after one full 320x96 grid stays in unused_cells / cell_reserve
    // (32768 - 30720 = 2048). That is slack, not the SDK two-pane floor.
    try testing.expectEqual(cell_store - full_cells, alloc.cell_reserve);
    try testing.expectEqual(planned.unused_cells, alloc.cell_reserve);
    try testing.expectEqual(command_envelope, alloc.command_budget);
    try testing.expectEqual(glyph_budget, alloc.glyph_budget);
    // The SDK two-pane leftover must not become a production floor: a
    // 320x96 grid is 30720 cells and `widget_cell_reserve` is half the
    // store (16384). Using it as a floor would truncate the common case.
    try testing.expect(canvas.terminal_grid.widget_cell_reserve * 2 == cell_store);
    try testing.expect(alloc.cell_reserve != canvas.terminal_grid.widget_cell_reserve);
    try testing.expect(full_cells > canvas.terminal_grid.widget_cell_reserve);
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
    try testing.expectEqual(planned.leftover_cells, planned.degraded_cell_share);
    const focused = planned.forPane(0, 0);
    const other = planned.forPane(1, 0);
    try testing.expectEqual(Fidelity.full, focused.fidelity);
    try testing.expectEqual(Fidelity.degraded, other.fidelity);
    try testing.expectEqual(planned.degraded_cell_share, focused.cell_reserve);
    try testing.expectEqual(@as(usize, 0), other.cell_reserve);
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
    try testing.expectEqual(full_cells, first.cell_reserve);
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

test "the current pin does not hold two full product grids, nor sixteen" {
    const testing = @import("std").testing;
    // Cockpit pkg3b / Metal hybrid C. The first assertion is the pin's
    // measured fact; a cell-store bump Metal approves will flip it and
    // that bump PR updates this test. The second is the forever claim:
    // N = layout.max_panes full 320x96 grids do not share one envelope.
    try testing.expect(grid.max_cells * 2 > cell_store);
    try testing.expect(grid.max_cells * layout.max_panes > cell_store);
    try testing.expectEqual(@as(usize, 1), maxFullPanesThatFit());
    try testing.expect(maxFullPanesThatFit() < layout.max_panes);
    try testing.expectEqual(grid.max_cols * grid.max_rows, grid.max_cells);
}

test "degraded last-n on this pin keeps six trailing rows at 320, not the top 24" {
    const testing = @import("std").testing;
    const leftover = cell_store - full_cells;
    try testing.expectEqual(@as(usize, 6), leftover / grid.max_cols);
    try testing.expectEqual(
        @as(usize, 6),
        keepRows(.degraded, grid.max_cols, grid.max_rows, 0, full_cells),
    );
    try testing.expect(keepRows(.degraded, grid.max_cols, grid.max_rows, 0, full_cells) < degraded_rows);

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
