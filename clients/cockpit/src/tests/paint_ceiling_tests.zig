//! Cockpit pkg3b / Metal hybrid C: measure paint bind points, then keep the
//! proposal honest. These tests paint adversarial screens the same way
//! `adversarial_isolation_tests.zig` does, at the product max grid, and
//! compare equal-cut partitioning with Hybrid C. They do not bump the SDK
//! pin. Numbers print only with `-Dmeasure=true`.

const std = @import("std");
const native_sdk = @import("native_sdk");
const app = @import("../native_test_root.zig");
const grid = @import("../terminal/grid.zig");
const layout = @import("../cockpit/layout.zig");
const support = @import("support.zig");
const measured = @import("measured.zig");
const paint_budget = @import("../cockpit/native/paint_budget.zig");
const painter = @import("../cockpit/native/terminal_painter.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const testing = std.testing;

const createSession = support.createSession;
const startFocusedTerminal = support.startFocusedTerminal;
const destroyModelSessions = app.deinitModel;
const TerminalApp = support.TerminalApp;

const product_cols: u16 = @intCast(grid.max_cols);
const product_rows: u16 = @intCast(grid.max_rows);
const huge_frame = geometry.RectF.init(0, 0, 8000, 4000);
const huge_size = geometry.SizeF.init(8000, 4000);

const Use = struct {
    commands: usize,
    cells: usize,
    text: usize,
    paths: usize,
    rows: usize,
    painted_cells: usize,
    store: ?canvas.DisplayListStore,
};

fn textLen(builder: *const canvas.Builder) usize {
    return builder.text_byte_len;
}

fn pathLen(builder: *const canvas.Builder) usize {
    return builder.path_element_len;
}

fn capture(builder: *canvas.Builder) Use {
    const view = support.findCellGrid(builder.displayList());
    return .{
        .commands = builder.len,
        .cells = builder.cell_len,
        .text = textLen(builder),
        .paths = pathLen(builder),
        .rows = if (view) |grid_view| grid_view.rows() else 0,
        .painted_cells = if (view) |grid_view| grid_view.cellCount() else 0,
        .store = if (builder.degradation) |loss| loss.store else null,
    };
}

fn heapBuilder(gpa: std.mem.Allocator) !*canvas.Builder {
    const storage = try gpa.alloc(canvas.CanvasCommand, canvas.max_display_list_commands);
    errdefer gpa.free(storage);
    const builder = try gpa.create(canvas.Builder);
    builder.* = canvas.Builder.init(storage);
    return builder;
}

fn destroyBuilder(gpa: std.mem.Allocator, builder: *canvas.Builder) void {
    gpa.free(builder.commands);
    gpa.destroy(builder);
}

fn paintUnbounded(builder: *canvas.Builder, session: *grid.Session, id_base: u64) !Use {
    builder.* = canvas.Builder.init(builder.commands);
    try grid.paint(session, builder, .{
        .frame = huge_frame,
        .tokens = .{},
        .running = true,
        .selecting = false,
        .id_base = id_base,
    });
    return capture(builder);
}

fn feedAscii(session: *grid.Session, cols: usize, rows: usize) void {
    var line: [512]u8 = undefined;
    const width = @min(cols, line.len - 2);
    @memset(line[0..width], 'A');
    line[width] = '\r';
    line[width + 1] = '\n';
    for (0..rows) |_| session.feed(line[0 .. width + 2]);
}

fn feedTruecolor(session: *grid.Session, cols: usize, rows: usize) void {
    var line: [32768]u8 = undefined;
    for (0..rows) |row| {
        var w: usize = 0;
        for (0..cols) |col| {
            const tint: u8 = @intCast((row * cols + col) % 251);
            const seq = std.fmt.bufPrint(
                line[w..],
                "\x1b[38;2;{d};{d};{d}m\x1b[48;2;{d};{d};{d}mX",
                .{ tint, 255 - tint, tint / 2, 255 - tint, tint, tint / 3 },
            ) catch break;
            w += seq.len;
        }
        session.feed(line[0..w]);
        session.feed("\r\n");
    }
}

fn feedUniqueCjk(session: *grid.Session, cols: usize, rows: usize) void {
    var line: [4096]u8 = undefined;
    var code: u21 = 0x4E00;
    for (0..rows) |_| {
        var w: usize = 0;
        for (0..cols) |_| {
            w += std.unicode.utf8Encode(code, line[w..]) catch 0;
            code += 1;
            if (code > 0x9FFF) code = 0x4E00;
        }
        line[w] = '\r';
        line[w + 1] = '\n';
        session.feed(line[0 .. w + 2]);
    }
}

fn feedBox(session: *grid.Session, cols: usize, rows: usize) void {
    for (0..rows) |_| {
        for (0..cols) |_| session.feed("\u{256C}");
        session.feed("\r\n");
    }
}

fn printUse(label: []const u8, used: Use) void {
    measured.print(
        "MEASURED paint {s}: commands={d} cells={d} text={d} paths={d} rows={d} painted_cells={d} degradation={s}\n",
        .{
            label,
            used.commands,
            used.cells,
            used.text,
            used.paths,
            used.rows,
            used.painted_cells,
            if (used.store) |store| @tagName(store) else "none",
        },
    );
}

test "MEASURED: SDK paint tables and Hybrid C constants" {
    const cell_size = @sizeOf(canvas.Cell);
    const command_size = @sizeOf(canvas.CanvasCommand);
    measured.print(
        "MEASURED-BASIS paint-ceiling host=linux pin=c188459a derive=zig-build-test-Dmeasure\n",
        .{},
    );
    measured.print(
        "MEASURED sdk: commands={d} paths={d} glyphs={d} cells={d} text={d} widget_cmd={d} widget_text={d} widget_glyph={d} widget_path={d} widget_cell={d} chrome_envelope={d}\n",
        .{
            native_sdk.runtime.max_canvas_commands_per_view,
            paint_budget.path_store,
            native_sdk.runtime.max_canvas_glyphs_per_view,
            canvas.max_display_list_cells,
            canvas.max_display_list_text_bytes,
            canvas.terminal_grid.widget_command_reserve,
            paint_budget.widget_text_reserve,
            paint_budget.glyph_budget,
            paint_budget.widget_path_reserve,
            canvas.terminal_grid.widget_cell_reserve,
            paint_budget.command_envelope,
        },
    );
    measured.print(
        "MEASURED product: max_cols={d} max_rows={d} max_cells={d} max_panes={d} cockpit_windows={d} platform_views={d} full_cells={d} max_full_panes={d} degraded_rows={d} degraded_cell_cap={d}\n",
        .{
            grid.max_cols,
            grid.max_rows,
            grid.max_cells,
            layout.max_panes,
            app.max_windows,
            native_sdk.platform.max_views,
            paint_budget.full_cells,
            paint_budget.maxFullPanesThatFit(),
            paint_budget.degraded_rows,
            paint_budget.degraded_cell_cap,
        },
    );
    measured.print(
        "MEASURED sizeof: Cell={d} CanvasCommand={d} builder_cell_store_bytes={d}\n",
        .{ cell_size, command_size, cell_size * paint_budget.cell_store },
    );
    const proposed_4x = paint_budget.cell_store * 4;
    const proposed_2x = paint_budget.cell_store * 2;
    measured.print(
        "MEASURED proposed cell bump: 2x={d} (builder={d} KiB, builder+retained={d} KiB, x{d} cockpit windows={d} KiB, x{d} view slots={d} KiB) 4x={d} (builder={d} KiB, builder+retained={d} KiB, x{d} cockpit windows={d} KiB, x{d} view slots={d} KiB)\n",
        .{
            proposed_2x,
            proposed_2x * cell_size / 1024,
            proposed_2x * cell_size * 2 / 1024,
            app.max_windows,
            proposed_2x * cell_size * 2 * app.max_windows / 1024,
            native_sdk.platform.max_views,
            proposed_2x * cell_size * 2 * native_sdk.platform.max_views / 1024,
            proposed_4x,
            proposed_4x * cell_size / 1024,
            proposed_4x * cell_size * 2 / 1024,
            app.max_windows,
            proposed_4x * cell_size * 2 * app.max_windows / 1024,
            native_sdk.platform.max_views,
            proposed_4x * cell_size * 2 * native_sdk.platform.max_views / 1024,
        },
    );
    try testing.expectEqual(grid.max_cells, grid.max_cols * grid.max_rows);
    try testing.expectEqual(@as(usize, 20), cell_size);
    try testing.expect(paint_budget.cell_store < grid.max_cells * 2);
    try testing.expectEqual(native_sdk.runtime.max_canvas_commands_per_view - canvas.terminal_grid.widget_command_reserve, paint_budget.command_envelope);
}

test "MEASURED: unbounded bind points for one 320x96 grid" {
    const gpa = testing.allocator;
    const builder = try heapBuilder(gpa);
    defer destroyBuilder(gpa, builder);

    const ascii = try createSession(product_cols, product_rows);
    defer ascii.destroy();
    feedAscii(ascii, grid.max_cols, grid.max_rows);
    printUse("unbounded-ascii-320x96", try paintUnbounded(builder, ascii, grid.paneIdBase(0)));

    const color = try createSession(product_cols, product_rows);
    defer color.destroy();
    feedTruecolor(color, grid.max_cols, grid.max_rows);
    const color_use = try paintUnbounded(builder, color, grid.paneIdBase(0));
    printUse("unbounded-truecolor-320x96", color_use);
    try testing.expectEqual(grid.max_rows, color_use.rows);
    try testing.expectEqual(grid.max_cells, color_use.painted_cells);
    try testing.expectEqual(@as(?canvas.DisplayListStore, null), color_use.store);

    const cjk = try createSession(product_cols, product_rows);
    defer cjk.destroy();
    feedUniqueCjk(cjk, grid.max_cols, grid.max_rows);
    printUse("unbounded-unique-cjk-320x96", try paintUnbounded(builder, cjk, grid.paneIdBase(0)));

    const box = try createSession(40, 24);
    defer box.destroy();
    feedBox(box, 40, 24);
    printUse("unbounded-box-40x24", try paintUnbounded(builder, box, grid.paneIdBase(0)));
}

const FleetKind = enum { hybrid, equal_cut };

fn paintFleet(
    builder: *canvas.Builder,
    sessions: []const *grid.Session,
    kind: FleetKind,
    focused_index: usize,
) !void {
    builder.* = canvas.Builder.init(builder.commands);
    const count = sessions.len;
    var focused_flags: [layout.max_panes]bool = @splat(false);
    focused_flags[focused_index] = true;
    const planned = paint_budget.plan(.{
        .window_active = true,
        .pane_count = count,
        .focused = focused_flags[0..count],
    });
    const prologue: usize = 0;
    for (sessions, 0..) |session, index| {
        const remaining = count - 1 - index;
        const alloc: paint_budget.Allocation = switch (kind) {
            .hybrid => planned.forPane(index, prologue),
            .equal_cut => .{
                .fidelity = .degraded,
                .command_budget = prologue + paint_budget.command_envelope * (index + 1) / count,
                .text_reserve = paint_budget.widget_text_reserve + (paint_budget.text_store - paint_budget.widget_text_reserve) / count * remaining,
                .path_reserve = paint_budget.widget_path_reserve + (paint_budget.path_store - paint_budget.widget_path_reserve) / count * remaining,
                .glyph_budget = paint_budget.equalCutGlyphShare(count),
                .cell_reserve = paint_budget.equalCutCellShare(count) * remaining,
            },
        };
        const before_cells = builder.cell_len;
        const before_commands = builder.len;
        try grid.paint(session, builder, .{
            .frame = huge_frame,
            .tokens = .{},
            .running = true,
            .focused = index == focused_index,
            .selecting = false,
            .command_budget = alloc.command_budget,
            .text_reserve = alloc.text_reserve,
            .glyph_budget = alloc.glyph_budget,
            .path_reserve = alloc.path_reserve,
            .cell_reserve = alloc.cell_reserve,
            .id_base = grid.paneIdBase(index),
        });
        const view = support.findPaneCellGrid(builder.displayList(), index);
        measured.print(
            "MEASURED fleet n={d} kind={s} pane={d} focused={d} fidelity={s} d_commands={d} d_cells={d} rows={d} painted_cells={d} glyph_budget={d} cell_reserve={d} degradation={s}\n",
            .{
                count,
                @tagName(kind),
                index,
                @intFromBool(index == focused_index),
                if (kind == .hybrid) @tagName(planned.fidelities[index]) else "equal",
                builder.len - before_commands,
                builder.cell_len - before_cells,
                if (view) |grid_view| grid_view.rows() else 0,
                if (view) |grid_view| grid_view.cellCount() else 0,
                alloc.glyph_budget,
                alloc.cell_reserve,
                if (builder.degradation) |loss| @tagName(loss.store) else "none",
            },
        );
    }
}

fn fleetSessions(n: usize) ![]const *grid.Session {
    const sessions = try testing.allocator.alloc(*grid.Session, n);
    var created: usize = 0;
    errdefer {
        for (sessions[0..created]) |session| session.destroy();
        testing.allocator.free(sessions);
    }
    while (created < n) : (created += 1) {
        const session = try createSession(product_cols, product_rows);
        feedTruecolor(session, grid.max_cols, grid.max_rows);
        sessions[created] = session;
    }
    return sessions;
}

fn destroySessions(sessions: []const *grid.Session) void {
    for (sessions) |session| session.destroy();
    testing.allocator.free(sessions);
}

test "MEASURED: Hybrid C vs equal-cut truecolor fleets at N=1,2,4,8" {
    const gpa = testing.allocator;
    const builder = try heapBuilder(gpa);
    defer destroyBuilder(gpa, builder);

    const ns = [_]usize{ 1, 2, 4, 8 };
    for (ns) |n| {
        const sessions = try fleetSessions(n);
        defer destroySessions(sessions);
        try paintFleet(builder, sessions, .equal_cut, 0);
        try paintFleet(builder, sessions, .hybrid, 0);
        if (n > 1) try paintFleet(builder, sessions, .hybrid, n - 1);
    }

    // N=16 is arithmetic only: feeding sixteen 320x96 truecolor screens is
    // not what Hybrid C is for, and the cell store cannot hold them.
    measured.print(
        "MEASURED n=16 arithmetic: equal_cut_cells={d} hybrid_full={d} leftover={d} degraded_share={d} max_panes={d} sixteen_full={d}\n",
        .{
            paint_budget.equalCutCellShare(16),
            paint_budget.full_cells,
            paint_budget.cell_store - paint_budget.full_cells,
            @min((paint_budget.cell_store - paint_budget.full_cells) / 15, paint_budget.degraded_cell_cap),
            layout.max_panes,
            grid.max_cells * layout.max_panes,
        },
    );

    const two = paint_budget.plan(.{
        .window_active = true,
        .pane_count = 2,
        .focused = &.{ true, false },
    });
    try testing.expect(two.forPane(0, 0).glyph_budget > paint_budget.equalCutGlyphShare(2));
    try testing.expectEqual(paint_budget.cell_store - paint_budget.full_cells, two.forPane(0, 0).cell_reserve);
}

test "Hybrid C painter gives the focused split more cells than equal-cut" {
    const gpa = testing.allocator;
    const sessions = try fleetSessions(4);
    defer destroySessions(sessions);
    const builder = try heapBuilder(gpa);
    defer destroyBuilder(gpa, builder);

    try paintFleet(builder, sessions, .hybrid, 0);
    const focused = support.findPaneCellGrid(builder.displayList(), 0) orelse return error.MissingFocusedPane;
    try testing.expectEqual(grid.max_rows, focused.rows());
    try testing.expectEqual(grid.max_cells, focused.cellCount());

    const neighbour = support.findPaneCellGrid(builder.displayList(), 1);
    const neighbour_cells = if (neighbour) |view| view.cellCount() else 0;
    try testing.expect(neighbour_cells < paint_budget.equalCutCellShare(4));
    try testing.expect(neighbour_cells <= paint_budget.degraded_cell_cap);
    try testing.expect(focused.cellCount() > paint_budget.equalCutCellShare(4));
}

fn splitUntil(state: *TerminalApp, harness: anytype, want: usize) !void {
    const app_iface = state.app();
    while (state.model.wsConst().selectedTreeConst().?.paneCount() < want) {
        const before = state.model.provider.liveShellCount();
        try state.dispatch(&harness.runtime, 1, .split_right);
        if (state.model.provider.liveShellCount() == before) return error.SplitRefused;
        try harness.runtime.dispatchPlatformEvent(app_iface, .wake);
    }
}

fn driveHugeFrames(harness: anytype, app_iface: anytype, start_index: u64) !void {
    var frame_index = start_index;
    while (frame_index < start_index + 8) : (frame_index += 1) {
        try harness.runtime.dispatchPlatformEvent(app_iface, .{ .gpu_surface_frame = .{
            .label = app.canvas_label,
            .size = huge_size,
            .scale_factor = 2,
            .frame_index = @intCast(frame_index),
            .timestamp_ns = frame_index * 1_000_000,
        } });
    }
}

test "MEASURED: real painter path at N=2,4,8 on a huge window" {
    try support.requireLiveShells(8);
    const gpa = testing.allocator;
    const ns = [_]usize{ 2, 4, 8 };
    for (ns) |n| {
        const harness = try native_sdk.TestHarness().create(gpa, .{ .size = geometry.SizeF.init(980, 640) });
        defer harness.destroy(gpa);
        const app_state = try startFocusedTerminal(gpa, harness);
        defer gpa.destroy(app_state);
        defer destroyModelSessions(&app_state.model);
        defer app_state.deinit();
        const app_iface = app_state.app();
        try splitUntil(app_state, harness, n);
        try driveHugeFrames(harness, app_iface, 2);

        for (support.activeSlots(&app_state.model), 0..) |pane, slot| {
            feedTruecolor(pane.session, pane.cols, pane.rows);
            measured.print(
                "MEASURED painter-prep n={d} slot={d} pty_cols={d} pty_rows={d} cells={d}\n",
                .{ n, slot, pane.cols, pane.rows, @as(usize, pane.cols) * @as(usize, pane.rows) },
            );
        }

        const builder = try heapBuilder(gpa);
        defer destroyBuilder(gpa, builder);
        try painter.paintWindowIndex(&app_state.model, builder, 0, huge_size, .{}, 0);
        const tree = app_state.model.wsConst().selectedTreeConst().?;
        var panes: [layout.max_panes]layout.Pane = undefined;
        const count = app.resolvePanes(&app_state.model, huge_size, &panes);
        try testing.expectEqual(n, count);
        for (panes[0..count], 0..) |pane, index| {
            const paint_index = painter.terminalPaintIndex(&app_state.model, pane.terminal);
            const view = support.findPaneCellGrid(builder.displayList(), paint_index);
            measured.print(
                "MEASURED painter n={d} pane={d} focused={d} rows={d} cells={d} rect={d:.0}x{d:.0}\n",
                .{
                    n,
                    index,
                    @intFromBool(pane.node == tree.focus),
                    if (view) |grid_view| grid_view.rows() else 0,
                    if (view) |grid_view| grid_view.cellCount() else 0,
                    pane.rect.width,
                    pane.rect.height,
                },
            );
        }
        measured.print(
            "MEASURED painter n={d} totals: commands={d} cells={d} text={d} paths={d} degradation={s}\n",
            .{
                n,
                builder.len,
                builder.cell_len,
                textLen(builder),
                pathLen(builder),
                if (builder.degradation) |loss| @tagName(loss.store) else "none",
            },
        );
    }
}

test "drive-shell-ceiling is macOS live PTY evidence, not a paint bind" {
    // scripts/drive-shell-ceiling.sh opens real shells in the shipped .app
    // until max_live_shells. This environment is Linux, so that script cannot
    // run. The paint bind is the cell store, measured above; the shell
    // ceiling is a different SDK table (max_effect_ptys) already recorded
    // historically at ~2.7 MiB rss per shell.
    try testing.expect(app.max_panes_per_tab <= 16);
}
