//! Borrowed Phux C-grid translation into reusable canvas presentation buffers.

const std = @import("std");
const native_sdk = @import("native_sdk");
const c = @import("abi.zig").c;

const canvas = native_sdk.canvas;
const projection = @import("cell_projection.zig");
const grid_metadata = @import("grid_metadata.zig");
const default_tokens: canvas.DesignTokens = .{};

/// Admission is bounded independently from one frame's text budget. The
/// painter degrades rows atomically while this store retains the complete valid
/// viewport instead of disconnecting on ordinary dense Unicode content.
pub const max_cell_utf8_bytes: usize = 64;
pub const max_grid_utf8_bytes: usize = canvas.max_terminal_cells * max_cell_utf8_bytes;

pub const CanvasStore = struct {
    cells: std.ArrayListUnmanaged(canvas.TerminalCell) = .empty,
    rows: std.ArrayListUnmanaged(canvas.TerminalRow) = .empty,
    utf8: std.ArrayListUnmanaged(u8) = .empty,
    screen_text: std.ArrayListUnmanaged(u8) = .empty,
    /// The SDK cell has no URI field. Keep explicit OSC 8 targets alongside
    /// its row-major cells, in an independently bounded owned arena.
    hyperlinks: std.ArrayListUnmanaged(projection.Span) = .empty,
    hyperlink_utf8: std.ArrayListUnmanaged(u8) = .empty,
    cursor: ?canvas.TerminalCursor = null,
    scrollbar: canvas.TerminalScrollbar = .{},
    selection_active: bool = false,
    background: canvas.Color = default_tokens.colors.background,
    foreground: canvas.Color = default_tokens.colors.text,
    cursor_color: canvas.Color = default_tokens.colors.accent,
    selection_color: canvas.Color = default_tokens.colors.accent,

    pub fn deinit(store: *CanvasStore, gpa: std.mem.Allocator) void {
        store.cells.deinit(gpa);
        store.rows.deinit(gpa);
        store.utf8.deinit(gpa);
        store.screen_text.deinit(gpa);
        store.hyperlinks.deinit(gpa);
        store.hyperlink_utf8.deinit(gpa);
    }

    fn reserve(store: *CanvasStore, gpa: std.mem.Allocator) !void {
        try store.utf8.ensureTotalCapacity(gpa, max_grid_utf8_bytes);
        try store.cells.ensureTotalCapacity(gpa, canvas.max_terminal_cells);
        try store.rows.ensureTotalCapacity(gpa, canvas.max_terminal_rows);
        try store.screen_text.ensureTotalCapacity(gpa, max_grid_utf8_bytes + canvas.max_terminal_rows);
        try store.hyperlinks.ensureTotalCapacity(gpa, canvas.max_terminal_cells);
    }

    pub fn copyBorrowed(store: *CanvasStore, gpa: std.mem.Allocator, view: *const c.PhuxTerminalGridView) !void {
        return store.copy(gpa, view, null, .{});
    }

    pub fn copyClient(store: *CanvasStore, gpa: std.mem.Allocator, client: *const c.PhuxClient, view: *const c.PhuxTerminalGridView) !void {
        const metadata = try grid_metadata.read(client, view);
        return store.copyWithMetadata(gpa, view, &metadata, .{});
    }

    pub fn copyWithMetadata(store: *CanvasStore, gpa: std.mem.Allocator, view: *const c.PhuxTerminalGridView, metadata: *const c.PhuxTerminalGridMetadata, policy: grid_metadata.Policy) !void {
        return store.copy(gpa, view, metadata, policy);
    }

    fn copy(store: *CanvasStore, gpa: std.mem.Allocator, view: *const c.PhuxTerminalGridView, metadata: ?*const c.PhuxTerminalGridMetadata, policy: grid_metadata.Policy) !void {
        const source = try projection.validate(view, max_grid_utf8_bytes);
        if (metadata) |meta| try grid_metadata.validate(view, meta);
        const rows: usize = view.rows;
        try store.reserve(gpa);
        try store.hyperlink_utf8.ensureTotalCapacity(gpa, source.hyperlink_bytes);
        store.utf8.clearRetainingCapacity();
        store.hyperlink_utf8.clearRetainingCapacity();
        store.cells.items.len = view.cell_count;
        store.hyperlinks.items.len = view.cell_count;
        store.rows.items.len = rows;
        store.screen_text.clearRetainingCapacity();
        store.selection_active = false;

        for (store.cells.items, 0..) |*cell, index| {
            const raw = source.cells[index];
            const source_cluster = projection.text(raw).slice(source.utf8);
            const compact_start = store.utf8.items.len;
            store.utf8.appendSliceAssumeCapacity(source_cluster);
            const cluster = store.utf8.items[compact_start..][0..source_cluster.len];
            const cell_metadata: ?projection.CellMetadata = if (metadata) |meta| .{
                .grid = meta,
                .cell = meta.cells[index],
                .policy = policy,
            } else null;
            cell.* = projection.project(raw, cluster, cell_metadata);
            const uri = projection.hyperlink(raw).slice(source.utf8);
            store.hyperlinks.items[index] = .{ .start = store.hyperlink_utf8.items.len, .len = uri.len };
            store.hyperlink_utf8.appendSliceAssumeCapacity(uri);
            store.selection_active = store.selection_active or raw.flags & c.PHUX_CLIENT_CELL_SELECTED != 0;
        }
        store.copyRows(source, view.cols);

        store.copyAppearance(view, metadata, policy);
        store.scrollbar = .{
            .offset = saturatingU32(view.history_viewport_offset),
            .len = saturatingU32(view.history_visible_rows),
            .total = saturatingU32(view.history_total_rows),
        };
    }

    fn copyAppearance(store: *CanvasStore, view: *const c.PhuxTerminalGridView, metadata: ?*const c.PhuxTerminalGridMetadata, policy: grid_metadata.Policy) void {
        store.background = default_tokens.colors.background;
        store.foreground = default_tokens.colors.text;
        store.cursor_color = policy.cursor_fallback;
        store.selection_color = policy.selection_color;
        store.cursor = cursor(view);
        const meta = metadata orelse return;
        store.background = grid_metadata.background(meta, policy);
        store.foreground = grid_metadata.foreground(meta, policy);
        if (meta.has_cursor_color) store.cursor_color = grid_metadata.rgb(meta.cursor_color);
        if (store.cursor) |*value| {
            value.blinking = meta.cursor_blinking;
            value.wide = meta.cursor_wide;
            if (meta.cursor_at_wide_tail) value.x -= 1;
        }
    }

    fn copyRows(store: *CanvasStore, source: projection.Source, cols: usize) void {
        for (store.rows.items, 0..) |*row, index| {
            const first = index * cols;
            const last = first + cols;
            const raw = source.cells[first..last];
            row.* = .{ .cells = store.cells.items[first..last], .selection = projection.selection(raw) };
            for (raw) |cell| store.screen_text.appendSliceAssumeCapacity(projection.text(cell).slice(source.utf8));
            if (index + 1 < store.rows.items.len) store.screen_text.appendAssumeCapacity('\n');
        }
    }

    /// Borrowed until the next successful copy or deinit, like grid().
    pub fn hyperlinkAt(store: *const CanvasStore, row: usize, col: usize) ?[]const u8 {
        if (row >= store.rows.items.len) return null;
        const cols = store.rows.items[row].cells.len;
        if (col >= cols) return null;
        const span = store.hyperlinks.items[row * cols + col];
        if (span.len == 0) return null;
        return span.slice(store.hyperlink_utf8.items);
    }

    pub fn grid(store: *const CanvasStore, running: bool) canvas.TerminalGrid {
        return .{
            .rows = store.rows.items,
            .background = store.background,
            .foreground = store.foreground,
            .cursor_color = store.cursor_color,
            .selection_color = store.selection_color,
            .cursor = store.cursor,
            .running = running,
            .scrollbar = store.scrollbar,
            .screen_text = store.screen_text.items,
            .selection_active = store.selection_active,
        };
    }
};

fn cursor(view: *const c.PhuxTerminalGridView) ?canvas.TerminalCursor {
    return if (view.cursor_visible) .{
        .x = view.cursor_col,
        .y = view.cursor_row,
        .shape = switch (view.cursor_style) {
            c.PHUX_CURSOR_BAR => .bar,
            c.PHUX_CURSOR_UNDERLINE => .underline,
            c.PHUX_CURSOR_BLOCK_HOLLOW => .block_hollow,
            else => .block,
        },
    } else null;
}

fn saturatingU32(value: u64) u32 {
    return @intCast(@min(value, @as(u64, std.math.maxInt(u32))));
}

test {
    _ = @import("presentation_tests.zig");
    _ = @import("grid_metadata_tests.zig");
}

test "dense text is compacted from a hyperlink-heavy remote UTF-8 arena" {
    // The claim is that the projection keeps every visible byte even when the
    // PAINTER's text store cannot carry them all, so the screen has to
    // genuinely outgrow that store or the test proves nothing. Both extents
    // are derived from the SDK's own constants rather than written down: a
    // fixed 120x96 stopped exceeding the budget the moment the store grew
    // from 32 KiB to 64 KiB, and the assertion below went quietly vacuous
    // until CI caught it.
    // Every cell here inks one three-byte euro sign. Note this is NOT
    // `max_cell_utf8_bytes` (64) — that is the per-cell CEILING a cluster may
    // reach, and sizing against it would ask for 20x fewer columns than the
    // fixture actually needs.
    const cell_utf8_bytes: usize = 3;
    const rows: usize = canvas.max_terminal_rows;
    const cols: usize = @min(
        canvas.max_terminal_cols,
        canvas.max_display_list_text_bytes / cell_utf8_bytes / rows + 2,
    );
    const cell_count = cols * rows;
    comptime {
        // The densest grid the projection accepts must still outgrow the
        // painter's store, or no choice of cols could make this test bite.
        std.debug.assert(canvas.max_terminal_cols * canvas.max_terminal_rows * cell_utf8_bytes >
            canvas.max_display_list_text_bytes);
    }
    const cells = try std.testing.allocator.alloc(c.PhuxTerminalCell, cell_count);
    defer std.testing.allocator.free(cells);
    const visible_len = cell_count * 3;
    const utf8 = try std.testing.allocator.alloc(u8, 65 * 1024 * 1024);
    defer std.testing.allocator.free(utf8);
    @memset(cells, std.mem.zeroes(c.PhuxTerminalCell));
    for (cells, 0..) |*cell, index| {
        const start = index * 3;
        @memcpy(utf8[start..][0..3], "\xe2\x82\xac");
        cell.utf8_offset = @intCast(start);
        cell.utf8_len = 3;
        cell.hyperlink_offset = @intCast(visible_len);
        cell.hyperlink_len = @intCast(utf8.len - visible_len);
    }
    @memcpy(utf8[0..3], "\xe2\x94\x80");
    try std.testing.expect(visible_len > canvas.max_display_list_text_bytes);
    try std.testing.expect(utf8.len > max_grid_utf8_bytes);

    var view = std.mem.zeroes(c.PhuxTerminalGridView);
    view.cols = cols;
    view.rows = rows;
    view.cells = cells.ptr;
    view.cell_count = cell_count;
    view.utf8 = .{ .data = utf8.ptr, .len = utf8.len };
    var store: CanvasStore = .{};
    defer store.deinit(std.testing.allocator);
    try store.copyBorrowed(std.testing.allocator, &view);
    try std.testing.expectEqual(visible_len, store.utf8.items.len);
    try std.testing.expectEqualStrings("", store.rows.items[0].cells[0].cluster);
    try std.testing.expect(std.mem.startsWith(u8, store.screen_text.items, "\xe2\x94\x80"));
    try std.testing.expectEqualStrings("\xe2\x82\xac", store.rows.items[rows - 1].cells[cols - 1].cluster);
}
