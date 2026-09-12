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
    const first_command = builder.len;
    const first_cell = builder.cell_len;
    try canvas.terminal_grid.paint(terminal_grid, builder, .{
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
    applyContrast(terminal_grid, builder, options.minimum_contrast, first_command, first_cell);
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
