const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const local = @import("../providers/local/provider.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

pub const TerminalApp = native_sdk.UiApp(app.Model, app.Msg);

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

/// The registry's live panes, as a slice. Valid only while no slot has been
/// freed, which holds for fixtures that only ever ADD terminals — a closed
/// terminal leaves a hole whose session is gone.
/// A `ChromeContext` naming the MAIN window — what the runtime hands
/// `web_panes` and `build_window` when it is painting the scene's own window.
/// Secondary windows are distinguished by `is_main = false` and their own
/// canvas label.
pub fn mainChromeContext() app.TerminalApp.ChromeContext {
    return .{
        .canvas_label = app.canvas_label,
        .window_id = 1,
        .size = geometry.SizeF.init(980, 640),
        .tokens = .{},
        .is_main = true,
    };
}

pub fn activeSlots(model: *app.Model) []app.Pane {
    var end: usize = 0;
    for (model.provider.states, 0..) |state, index| {
        if (state == .active) end = index + 1;
    }
    return model.provider.slots[0..end];
}

/// Open terminals until the effects layer's pty table is full.
///
/// No single chord can do this any more, which is phux-cockpit-ipg in one
/// sentence: `local.max_live_shells` (32, from the SDK's pty table) is larger
/// than `topology.max_tabs` (16) and larger than `layout.max_panes` (16), so
/// repeating cmd+T stops at 16 tabs and repeating cmd+D stops at 16 panes,
/// both well short of the shell ceiling. That is the intended shape: the
/// binding constraint is now an app-owned number a person can see, not an
/// invisible SDK one. It does mean any test that wants the shell ceiling has
/// to fill a tab, start another, and fill that one too.
///
/// Splits come first because a split's refusal at the pane ceiling is CLEAN —
/// `splitFocusedPane` hands the minted terminal back rather than leaking it,
/// and sets no latch — so it is safe to use as the probe for "this tab is
/// full". The caller gets a model with no refusal latched.
/// Require the SDK resource table to back the app-owned topology a test needs.
/// This used to skip under the four-PTY pin, which made the tests disappear at
/// exactly the capacity regression they were meant to expose.
pub fn requireLiveShells(needed: usize) !void {
    try testing.expect(local.max_live_shells >= needed);
}

pub fn fillLiveShells(state: *TerminalApp) !void {
    while (state.model.provider.liveShellCount() < local.max_live_shells) {
        const before = state.model.provider.liveShellCount();
        app.update(&state.model, .split_right, &state.effects);
        if (state.model.provider.liveShellCount() == before) {
            app.update(&state.model, .new_terminal, &state.effects);
        }
        // A round that adds nothing would spin forever. Fail on the first one
        // instead of hanging the suite on a ceiling nobody expected.
        if (state.model.provider.liveShellCount() == before) return error.ShellCeilingUnreachable;
    }
}

pub fn remoteTerminalRef(id: u32) !app.TerminalRef {
    return .{
        .provider_id = .phux,
        .terminal_id = .{ .phux = try app.RemoteTerminalId.fromPhux(0, id, "local") },
    };
}

// --------------------------------------------------------- cell grids
//
// A terminal screen paints as `cell_grid` commands: a packed cols x rows
// lattice of 20-byte cells, each carrying its own background, cluster
// (an offset into the grid's interned byte pool), foreground, underline
// colour, and style bits. Every renderer expands the lattice itself.
//
// The painter emits ONE `cell_grid` command PER ROW, not one per screen.
// That is deliberate: a retained command is the unit of incremental
// change, so a screen-wide lattice made every keystroke re-encode and
// re-upload every cell (the SDK measured 31 upserts and 8.4 KB per
// keystroke against 1 upsert and ~400 bytes once rows became their own
// commands). A row command therefore carries `rows == 1`, its own
// `origin.y`, and its own interned text blob.
//
// What that means for a TEST:
//   - There are no per-run `fill_rect` / `draw_text` commands for
//     terminal CONTENT any more, so a test that wants to know what
//     reached the glass reads CELLS.
//   - A screen costs roughly ONE COMMAND PER PAINTED ROW plus a small
//     fixed prologue/epilogue (surface fill, clip push, cursor, caret,
//     scrollbar thumb, clip pop) — NOT a constant, and not a function of
//     how dense the content is either. The only content that still
//     prices in commands is what the lattice deliberately cannot carry:
//     box-drawing geometry at exact cell bounds, and the selection wash.
//   - "How much painted" is therefore painted ROWS and inked CELLS.
//
// Everything below is that one seam: `CellGridView` puts the SCREEN back
// together from a pane's row commands, so every accessor keeps meaning
// what it always meant (`rows()` is the pane's painted height, `at(x, y)`
// addresses the whole screen with y = 0 at the top). No test decodes a
// cell, or a row command, by hand.

/// A whole terminal SCREEN, aggregated from the per-row `cell_grid`
/// commands that share one pane's command-id namespace.
///
/// Deliberately SMALL (a slice plus a namespace, 24 bytes): this is
/// returned BY VALUE from the finders below, and a debug build
/// materialises a returned aggregate in the caller's frame for the rest
/// of that function. An inline array of 96 `CellGrid`s here would put
/// ~9 KB on every fixture's frame — and this repo has already been
/// bitten once by multi-megabyte returned aggregates overrunning the
/// main-thread stack (see `initTerminalApp`). Rows are resolved on
/// ACCESS instead; screens in tests are at most `max_rows` tall, so the
/// linear scans cost nothing that matters.
pub const CellGridView = struct {
    /// The frame's commands. The pane's rows are whichever `cell_grid`s
    /// in here fall inside `namespace`.
    commands: []const canvas.CanvasCommand,
    /// The pane's command-id namespace: `paintIdBase(paneIdBase(i))`.
    /// Every id the painter emits for that pane lands in
    /// `[namespace, namespace + id_namespace_stride)`, and no other
    /// pane's does — which is exactly what makes a split's rows
    /// separable in one shared display list.
    namespace: u64,

    /// Whether `command` is one of THIS pane's row commands.
    fn owns(self: CellGridView, command: canvas.CanvasCommand) bool {
        if (command != .cell_grid) return false;
        // Wrapping subtraction, because `paintIdBase` is a wrapping
        // multiply: a namespace near the top of u64 wraps, and its
        // offsets have to wrap with it.
        return (command.cell_grid.id -% self.namespace) < grid.id_namespace_stride;
    }

    /// The row command holding screen row `y`, and `y`'s index WITHIN
    /// it.
    ///
    /// Rows are ordered by `origin.y` — geometry, not emission order.
    /// The painter happens to emit top-to-bottom with monotonically
    /// increasing ids, but a view that assumed either would silently
    /// mis-address the screen the day it stops (a reflow that repaints
    /// one row, a producer that emits the cursor row first). Ties are
    /// broken by id and then by position so the ordering is total.
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

    /// The rect cell (x, y) covers, in ABSOLUTE canvas points.
    ///
    /// Taken from the owning row command's OWN origin rather than from
    /// `y * cell_height`: each row carries its own origin, so this stays
    /// right even for a pane whose rows were emitted out of order or
    /// whose top row is not the viewport's first.
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

    /// The cell's SGR 58 underline colour, or null when it named none
    /// and the underline takes the cell foreground. `has_underline_color`
    /// is what says the cell carried one at all, exactly as
    /// `has_background` does for the background — the stored colour is
    /// zeroed, not absent, so reading the field alone would report opaque
    /// black for every cell that never set one.
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

    /// Cells that put ink on the glass — a cluster, an underline, a
    /// strikethrough, or an overline. The migrated measure of "this
    /// terminal genuinely painted content", now that a screen's command
    /// count tracks its painted HEIGHT rather than its density.
    pub fn inkedCells(self: CellGridView) usize {
        var total: usize = 0;
        const height = self.rows();
        var y: usize = 0;
        while (y < height) : (y += 1) {
            const row = self.rowAt(y) orelse continue;
            var x: usize = 0;
            while (x < row.grid.cols) : (x += 1) {
                const cell = row.grid.at(x, row.local) orelse continue;
                if (cell.hasInk()) total += 1;
            }
        }
        return total;
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

/// The screen belonging to pane `index`, by the painter's own command-id
/// namespace. A split emits one screen per pane, so tests that care
/// WHICH terminal they are reading go through here rather than taking
/// the first. Null when that pane painted no row at all — which is a
/// real outcome (a budget that rejected even the first row), and one
/// callers must not confuse with an empty screen.
pub fn findPaneCellGrid(display_list: anytype, index: usize) ?CellGridView {
    const view = CellGridView{
        .commands = display_list.commands,
        .namespace = grid.idNamespace(grid.paneIdBase(index)),
    };
    if (view.rows() == 0) return null;
    return view;
}

pub fn expectPaneCellGrid(display_list: anytype, index: usize) !CellGridView {
    return findPaneCellGrid(display_list, index) orelse error.TestExpectedCellGrid;
}

/// One view per PANE that painted into the list, in the order the panes'
/// first rows were emitted. Not one per row command: the rows of one
/// terminal are one screen.
pub fn collectCellGrids(display_list: anytype, out: []CellGridView) []CellGridView {
    var count: usize = 0;
    outer: for (display_list.commands) |command| {
        if (command != .cell_grid) continue;
        const namespace = grid.idNamespaceOf(command.cell_grid.id);
        for (out[0..count]) |seen| {
            if (seen.namespace == namespace) continue :outer;
        }
        if (count == out.len) break;
        out[count] = .{ .commands = display_list.commands, .namespace = namespace };
        count += 1;
    }
    return out[0..count];
}

comptime {
    // The view is a slice plus a namespace and stays that way: it is
    // returned by value from every finder above, and this repo has
    // already paid for large returned aggregates once (see
    // `initTerminalApp`). 24 bytes = 16 (slice) + 8 (namespace).
    std.debug.assert(@sizeOf(CellGridView) == 24);
}

/// Commands one paint spends OUTSIDE its rows: the surface fill, the
/// clip push and pop, the cursor, the keyboard caret, and the scrollback
/// thumb. The SDK painter prices exactly this as a fixed 8-command
/// prologue plus an 8-command epilogue reserve (`terminal_grid.zig`,
/// `fixed_overhead` / `epilogue_reserve`), neither of which it exports;
/// every use of it is an INEQUALITY bounding "commands minus rows", so
/// the bound is what matters and not the exact figure.
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

/// Build a `TerminalApp` DIRECTLY into caller-owned storage.
///
/// `TerminalApp` is ~4.5 MB by value, and a debug build materialises a
/// returned aggregate as a temporary in the CALLER's frame that then
/// lives for the rest of that function — so every fixture that wrote
/// `app_state.* = TerminalApp.init(...)` inline was carrying 4.5 MB of
/// dead stack through its whole body, and a fixture that also built one
/// other multi-megabyte value carried both at once.
///
/// That never mattered until the packed cell grid landed: `canvas.Builder`
/// now embeds a 32k-cell store (640 KB), and the SDK's chrome installer
/// holds TWO builders plus two per-view command arrays on one frame,
/// ~4.1 MB. Deep fixtures plus that frame overran the 16 MB main-thread
/// stack. Constructing in a LEAF helper keeps the temporary in a frame
/// that pops immediately, which is the difference between ~9 MB of live
/// stack at the deepest point and ~17 MB.
pub fn initTerminalApp(slot: *TerminalApp, session: *grid.Session) void {
    slot.* = TerminalApp.init(std.heap.page_allocator, app.initialModel(session), app.appOptions());
}

pub fn startFocusedTerminal(gpa: std.mem.Allocator, harness: anytype) !*TerminalApp {
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;
    harness.runtime.options.shortcuts = &app.cockpit_shortcuts;
    const session = try createSession(80, 24);
    const app_state = gpa.create(TerminalApp) catch |err| {
        session.destroy();
        return err;
    };
    initTerminalApp(app_state, session);
    errdefer {
        app_state.deinit();
        app.deinitModel(&app_state.model);
        gpa.destroy(app_state);
    }
    app_state.effects.executor = .fake;
    const app_iface = app_state.app();
    try harness.start(app_iface);
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = geometry.SizeF.init(980, 640),
        .scale_factor = 2,
        .frame_index = 1,
        .timestamp_ns = 1_000_000,
    } });
    // Production creates dormant WebKit and registers the weighted companions
    // only after this first canvas present. Direct-UiApp fixtures call the same
    // post-present seam explicitly because they bypass CockpitHost.
    try app.installPostPresentResources(&harness.runtime, &app_state.model, 1);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .pointer_down,
        .x = 200,
        .y = 200,
    } });
    return app_state;
}

/// Two TABS, each with one terminal — the shape most routing tests want.
/// The second terminal is minted through the real `new_terminal` path, so
/// the fixture exercises lazy session allocation instead of pre-seeding it.
pub fn startTwoPaneCockpit(gpa: std.mem.Allocator, harness: anytype) !*TerminalApp {
    const app_state = try startFocusedTerminal(gpa, harness);
    errdefer {
        app_state.deinit();
        app.deinitModel(&app_state.model);
        gpa.destroy(app_state);
    }
    const app_iface = app_state.app();
    try app_state.dispatch(&harness.runtime, 1, .new_terminal);
    try app_state.dispatch(&harness.runtime, 1, .{ .select_position = 0 });
    try app_state.effects.feedPtyOutput(app.ptyKey(0), "PANEALPHA\r\n");
    try app_state.effects.feedPtyOutput(app.ptyKey(1), "PANEBRAVO\r\n");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    // Land canvas widget focus on the selected terminal AFTER the tab list
    // settled: a rebuild between the click and the assertion clears it.
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .pointer_down,
        .x = 200,
        .y = 200,
    } });
    return app_state;
}

/// Two PANES in ONE tab, produced by a real split. `PANEALPHA` is the
/// original (left/top), `PANEBRAVO` the pane the split created.
pub fn startSplitCockpit(gpa: std.mem.Allocator, harness: anytype) !*TerminalApp {
    const app_state = try startFocusedTerminal(gpa, harness);
    errdefer {
        app_state.deinit();
        app.deinitModel(&app_state.model);
        gpa.destroy(app_state);
    }
    const app_iface = app_state.app();
    try app_state.dispatch(&harness.runtime, 1, .split_right);
    try app_state.effects.feedPtyOutput(app.ptyKey(0), "PANEALPHA\r\n");
    try app_state.effects.feedPtyOutput(app.ptyKey(1), "PANEBRAVO\r\n");
    try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
    return app_state;
}

/// The pane rects the painter, the hit targets, and the PTY pump all use.
pub fn resolvedPanes(model: *const app.Model, size: geometry.SizeF, out: []app.LayoutPane) usize {
    return app.resolvePanes(model, size, out);
}

pub fn paneRect(model: *const app.Model, size: geometry.SizeF, terminal_ref: app.TerminalRef) ?geometry.RectF {
    return app.paneFrameFor(model, size, terminal_ref);
}

pub fn startCockpit(harness: anytype) !*TerminalApp {
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;
    harness.runtime.options.shortcuts = &app.cockpit_shortcuts;
    const session = try createDefaultSession();
    const model = app.initialModelWithIo(testing.allocator, testing.io, session) catch |err| {
        session.destroy();
        return err;
    };
    const state = testing.allocator.create(TerminalApp) catch |err| {
        var owned_model = model;
        app.deinitModel(&owned_model);
        return err;
    };
    state.* = TerminalApp.init(std.heap.page_allocator, model, app.appOptions());
    errdefer stopCockpit(state);
    state.effects.executor = .fake;
    try harness.start(state.app());
    try dispatchInitialFrame(harness, state);
    return state;
}

pub fn startProductionCockpit(harness: anytype) !*TerminalApp {
    harness.null_platform.gpu_surfaces = true;
    harness.runtime.options.security.navigation.allowed_origins = &app.web_origins;
    harness.runtime.options.shortcuts = &app.cockpit_shortcuts;
    const session = try grid.Session.create(testing.allocator, testing.io, 80, 24);
    const model = app.initialProductionModelWithIo(testing.allocator, testing.io, session) catch |err| {
        session.destroy();
        return err;
    };
    const state = testing.allocator.create(TerminalApp) catch |err| {
        var owned_model = model;
        app.deinitModel(&owned_model);
        return err;
    };
    state.* = TerminalApp.init(std.heap.page_allocator, model, app.appOptions());
    errdefer stopCockpit(state);
    state.effects.executor = .fake;
    try harness.start(state.app());
    try dispatchInitialFrame(harness, state);
    return state;
}

fn dispatchInitialFrame(harness: anytype, state: *TerminalApp) !void {
    const app_iface = state.app();
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
        .label = app.canvas_label,
        .size = geometry.SizeF.init(980, 640),
        .scale_factor = 2,
        .frame_index = 1,
        .timestamp_ns = 1_000_000,
    } });
    try app.installPostPresentResources(&harness.runtime, &state.model, 1);
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
}

/// Paint one frame for `label` through the REAL runtime path.
///
/// Tests that mint panes (`.new_window`, a split) and then read
/// `proposedViewportsIn` need this, because a pane that has never been painted
/// has no measured cell box and the proposer correctly declines to size it —
/// see `grid.CellBox`. The runtime path matters: it is what stamps the
/// text-measure provider onto the tokens, so the painter records the mono
/// face's real advance rather than the estimator's guess. Calling
/// `app.buildChromeWindow` with hand-built tokens would paint, and would
/// record a measurement that is wrong in exactly the way this all exists to
/// prevent.
///
/// The real app never needs this: a model change rebuilds and repaints before
/// the next frame arrives.
pub fn pumpPaint(harness: anytype, app_iface: native_sdk.App, label: []const u8, size: geometry.SizeF) !void {
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
        .label = label,
        .size = size,
        .scale_factor = 2,
        .frame_index = 2,
        .timestamp_ns = 2_000_000,
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .frame_requested);
}

pub fn stopCockpit(state: *TerminalApp) void {
    state.deinit();
    app.deinitModel(&state.model);
    testing.allocator.destroy(state);
}

pub fn clickCanvas(harness: anytype, app_iface: anytype, x: f32, y: f32) !void {
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .pointer_down,
        .x = x,
        .y = y,
    } });
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .pointer_up,
        .x = x,
        .y = y,
    } });
}

pub fn typeCanvasText(harness: anytype, app_iface: anytype, text: []const u8) !void {
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .text_input,
        .text = text,
    } });
}

pub fn pressCanvasKey(harness: anytype, app_iface: anytype, key: []const u8, modifiers: native_sdk.platform.ShortcutModifiers) !void {
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_down,
        .key = key,
        .modifiers = modifiers,
    } });
}

pub fn releaseCanvasKey(harness: anytype, app_iface: anytype, key: []const u8, modifiers: native_sdk.platform.ShortcutModifiers) !void {
    try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = app.canvas_label,
        .kind = .key_up,
        .key = key,
        .modifiers = modifiers,
    } });
}

pub fn terminalInteractionFrame(harness: anytype, marker: []const u8) ?geometry.RectF {
    const layout = harness.runtime.views[0].widgetLayoutTree();
    for (layout.nodes) |node| {
        if (node.widget.kind != .terminal) continue;
        if (std.mem.indexOf(u8, node.widget.text, marker) == null) continue;
        return node.frame;
    }
    return null;
}

pub fn widgetFrameBySemantics(harness: anytype, label: []const u8) ?geometry.RectF {
    for (harness.runtime.views[0].widgetLayoutTree().nodes) |node| {
        if (std.mem.eql(u8, node.widget.semantics.label, label)) return node.frame;
    }
    return null;
}

pub fn rectCenter(rect: geometry.RectF) geometry.PointF {
    return geometry.PointF.init(rect.x + rect.width / 2, rect.y + rect.height / 2);
}

/// A terminal's text reached the glass INSIDE `frame`.
///
/// Same two claims as before the cell grid landed — the marker is on
/// screen, and it is horizontally inside the pane that owns it — read
/// off cells instead of `draw_text` runs. The x check is now exact
/// rather than approximate: a cell's rect comes from its INDEX, so this
/// pins the column the marker actually occupies, not where a text run
/// happened to start.
pub fn expectDisplayListMarker(display_list: anytype, marker: []const u8, frame: geometry.RectF) !void {
    var grids: [max_cell_grids]CellGridView = undefined;
    for (collectCellGrids(display_list, &grids)) |view| {
        const at = view.find(marker) orelse continue;
        const rect = view.cellRect(at.x, at.y);
        try testing.expect(rect.x >= frame.x);
        try testing.expect(rect.x < frame.x + frame.width);
        try testing.expect(rect.x + rect.width <= frame.x + frame.width + 0.5);
        return;
    }
    return error.TestExpectedMarker;
}

/// No terminal on this frame carries `marker` — the hidden-pane claim.
pub fn expectDisplayListMissingMarker(display_list: anytype, marker: []const u8) !void {
    var grids: [max_cell_grids]CellGridView = undefined;
    for (collectCellGrids(display_list, &grids)) |view| {
        try testing.expect(view.find(marker) == null);
    }
}

/// Grids one frame can hold: one per pane, with slack.
const max_cell_grids = 16;
