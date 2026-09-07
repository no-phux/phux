//! Actual C-client metadata and source-confirmed local Palette semantics.
const std = @import("std");
const testing = std.testing;
const canvas = @import("native_sdk").canvas;
const c = @import("abi.zig").c;
const fixture = @import("presentation_tests.zig");
const metadata_module = @import("grid_metadata.zig");
const CanvasStore = @import("presentation.zig").CanvasStore;
const rgb = metadata_module.rgb;

// GUARD: remote-grid-metadata
test "C grid metadata restores local color provenance and cursor semantics" {
    const client = try fixture.fixtureClient();
    var live = true;
    defer if (live) c.phux_client_free(client);
    const view = try fixture.fixtureGrid(client);
    const meta = try metadata_module.read(client, &view);
    try testing.expectEqual(view.cell_count, meta.cell_count);
    try testing.expectEqual(c.PHUX_GRID_COLOR_PALETTE, meta.cells[5 * 16].foreground_kind);
    try testing.expectEqual(@as(u8, 1), meta.cells[5 * 16].foreground_palette_index);
    try testing.expect(meta.cells[5 * 16 + 1].underline_color_is_default);
    try testing.expect(!meta.cells[5 * 16 + 8].underline_color_is_default);
    try testing.expectEqual(c.PHUX_GRID_COLOR_RGB, meta.cells[5 * 16 + 2].foreground_kind);
    try testing.expectEqual(c.PHUX_CELL_BACKGROUND_PALETTE, view.cells[4 * 16 + 3].content_tag);
    try testing.expectEqual(c.PHUX_CELL_BACKGROUND_RGB, view.cells[4 * 16 + 7].content_tag);
    try testing.expect(!meta.cells[4 * 16 + 3].background_color_is_default);
    try testing.expect(!meta.cells[4 * 16 + 7].background_color_is_default);
    try testing.expect(meta.has_cursor_color and meta.cursor_blinking and meta.cursor_wide);
    try testing.expect(!meta.cursor_at_wide_tail);
    try testing.expectEqual(canvas.Color.rgb8(192, 176, 160), rgb(meta.foreground));
    try testing.expectEqual(canvas.Color.rgb8(16, 40, 64), rgb(meta.background));
    try testing.expectEqual(canvas.Color.rgb8(128, 160, 192), rgb(meta.cursor_color));

    var store: CanvasStore = .{};
    defer store.deinit(testing.allocator);
    try store.copyClient(testing.allocator, client, &view);
    c.phux_client_free(client);
    live = false;
    // Everything below reads owned projection or by-value global metadata.
    const grid = store.grid(true);
    try testing.expectEqual(rgb(meta.foreground), grid.foreground);
    try testing.expectEqual(rgb(meta.background), grid.background);
    try testing.expectEqual(rgb(meta.cursor_color), grid.cursor_color);
    try testing.expect(grid.cursor.?.blinking);
    try testing.expect(grid.cursor.?.wide);
    try testing.expectEqual(@as(u16, 6), grid.cursor.?.x);
    try testing.expectEqual(@as(u16, 2), grid.cursor.?.y);
    try testing.expectEqual(rgb(meta.palette[4]), grid.rows[4].cells[3].bg.?);
    try testing.expectEqual(canvas.Color.rgb8(20, 60, 100), grid.rows[4].cells[7].bg.?);
    try expectPaletteSemantics(grid, meta);
}

fn expectPaletteSemantics(grid: canvas.TerminalGrid, meta: c.PhuxTerminalGridMetadata) !void {
    const cells = grid.rows[5].cells;
    // Palette.resolveFgRaw brightens only bold ANSI 0..7, never an RGB
    // value equal to that palette entry, an already-bright entry or the cube.
    try testing.expectEqual(rgb(meta.palette[9]), cells[0].fg);
    try testing.expect(cells[0].bold);
    try testing.expectEqual(rgb(meta.palette[1]), cells[2].fg);
    try testing.expectEqual(rgb(meta.palette[9]), cells[3].fg);
    try testing.expectEqual(rgb(meta.palette[200]), cells[4].fg);
    // Palette.resolveUnderlineColor's .none is null; explicit RGB is not,
    // even when equal to the pre-inverse foreground. Never infer from RGB.
    try testing.expect(cells[1].underline_color == null);
    try testing.expectEqual(rgb(meta.background), cells[1].fg);
    try testing.expectEqual(rgb(meta.palette[1]), cells[8].underline_color.?);
    try testing.expectEqual(rgb(meta.background), cells[8].fg);
    // Palette.resolveFg blends toward TERMINAL default background, not cell
    // background. Both D and Q get (192,176,160)/2 + (16,40,64)/2.
    try expectColor(canvas.Color.rgb8(104, 108, 112), cells[5].fg);
    try testing.expectEqual(cells[5].fg, cells[6].fg);
    try testing.expect(!std.meta.eql(cells[5].bg, cells[6].bg));
    try testing.expect(cells[5].underline_color == null);
    // Inverse is applied ONCE, after brightening, and overrides faint.
    try testing.expectEqual(rgb(meta.background), cells[7].fg);
    try testing.expectEqual(rgb(meta.palette[9]), cells[7].bg.?);
    try testing.expect(cells[7].underline_color == null);
    try testing.expectEqual(grid.rows[2].cells[0].fg, grid.rows[2].cells[2].fg);
}

test "C metadata query is read-only and rejects expired or undersized borrows" {
    const client = try fixture.fixtureClient();
    defer c.phux_client_free(client);
    var id = std.mem.zeroes(c.PhuxTerminalId);
    id.id = 7;
    var meta = initializedMetadata();
    try testing.expectEqual(c.PHUX_CLIENT_INVALID_STATE, c.phux_client_terminal_grid_metadata(client, &id, &meta));
    const view = try fixture.fixtureGrid(client);
    meta = try metadata_module.read(client, &view);
    const again = try metadata_module.read(client, &view);
    try testing.expectEqual(meta.cells, again.cells);
    try testing.expectEqual(@as(u16, 1), view.cells[0].utf8_len);
    // Header-only storage plus a canary: invalid size/version must be checked
    // before creating a whole-struct reference or writing the output.
    const Header = extern struct { size: usize, version: u32, canary: u32 };
    var header: Header = .{ .size = @sizeOf(Header), .version = c.PHUX_CLIENT_ABI_VERSION, .canary = 0x12345678 };
    try testing.expectEqual(c.PHUX_CLIENT_INVALID_ARGUMENT, c.phux_client_terminal_grid_metadata(client, &id, @ptrCast(&header)));
    try testing.expectEqual(@as(u32, 0x12345678), header.canary);
    meta.version += 1;
    try testing.expectEqual(c.PHUX_CLIENT_INVALID_ARGUMENT, c.phux_client_terminal_grid_metadata(client, &id, &meta));
    try testing.expectEqual(c.PHUX_CLIENT_INVALID_ARGUMENT, c.phux_client_terminal_grid_metadata(client, &id, null));
    try testing.expectEqual(c.PHUX_CLIENT_OK, c.phux_client_outgoing_clear(client));
    meta = initializedMetadata();
    try testing.expectEqual(c.PHUX_CLIENT_INVALID_STATE, c.phux_client_terminal_grid_metadata(client, &id, &meta));
    try testing.expectEqual(@as(usize, 0), meta.cell_count);
    const fresh = try fixture.fixtureGrid(client);
    _ = try metadata_module.read(client, &fresh);
}

test "metadata honors renderer bright policy and fences mismatched grid state" {
    const client = try fixture.fixtureClient();
    defer c.phux_client_free(client);
    const view = try fixture.fixtureGrid(client);
    var meta = try metadata_module.read(client, &view);
    var store: CanvasStore = .{};
    defer store.deinit(testing.allocator);
    try store.copyWithMetadata(testing.allocator, &view, &meta, .{ .bold_as_bright = false });
    try testing.expectEqual(rgb(meta.palette[1]), store.rows.items[5].cells[0].fg);
    try testing.expect(store.rows.items[5].cells[0].bold);
    const owned = store.foreground;
    meta.document_revision += 1;
    try testing.expectError(error.Protocol, store.copyWithMetadata(testing.allocator, &view, &meta, .{}));
    meta.document_revision -= 1;
    meta.cols += 1;
    try testing.expectError(error.Protocol, store.copyWithMetadata(testing.allocator, &view, &meta, .{}));
    meta.cols -= 1;
    const cells = meta.cells;
    meta.cells = null;
    try testing.expectError(error.Protocol, store.copyWithMetadata(testing.allocator, &view, &meta, .{}));
    meta.cells = cells;
    var invalid_cursor = view;
    invalid_cursor.cursor_col = view.cols;
    try testing.expectError(error.Protocol, store.copyWithMetadata(testing.allocator, &invalid_cursor, &meta, .{}));
    try testing.expectEqual(owned, store.foreground);
}

test "C cursor wide-tail and dynamic default color changes reach owned grid" {
    const client = try fixture.fixtureClient();
    defer c.phux_client_free(client);
    try fixture.feed(client, @embedFile("style_fixture/tail.bin"));
    var view = try fixture.fixtureGrid(client);
    var meta = try metadata_module.read(client, &view);
    try testing.expect(meta.cursor_at_wide_tail and meta.cursor_wide);
    try testing.expect(!meta.cursor_blinking);
    var store: CanvasStore = .{};
    defer store.deinit(testing.allocator);
    try store.copyClient(testing.allocator, client, &view);
    try testing.expectEqual(@as(u16, 6), store.cursor.?.x);
    try testing.expect(store.cursor.?.wide);
    try testing.expect(!store.cursor.?.blinking);
    try fixture.feed(client, @embedFile("style_fixture/reset-colors.bin"));
    view = try fixture.fixtureGrid(client);
    meta = try metadata_module.read(client, &view);
    try testing.expect(!meta.has_cursor_color);
    try testing.expect(!meta.has_foreground and !meta.has_background);
    const policy: metadata_module.Policy = .{ .cursor_fallback = rgb(meta.palette[6]), .selection_color = rgb(meta.palette[5]) };
    try store.copyWithMetadata(testing.allocator, &view, &meta, policy);
    try testing.expectEqual(policy.cursor_fallback, store.grid(true).cursor_color);
    try testing.expectEqual(policy.selection_color, store.grid(true).selection_color);
    try testing.expectEqual(policy.foreground, store.rows.items[1].cells[0].fg);
    try testing.expectEqual(policy.background, store.rows.items[1].cells[0].bg.?);
    try testing.expectEqual(rgb(meta.palette[4]), store.rows.items[4].cells[3].bg.?);
    try testing.expectEqual(canvas.Color.rgb8(20, 60, 100), store.rows.items[4].cells[7].bg.?);
    const reset_fg = store.grid(true).foreground;
    const reset_bg = store.grid(true).background;
    try testing.expectEqual(policy.foreground, reset_fg);
    try testing.expectEqual(policy.background, reset_bg);
    try fixture.feed(client, @embedFile("style_fixture/reverse.bin"));
    view = try fixture.fixtureGrid(client);
    meta = try metadata_module.read(client, &view);
    try testing.expect(meta.reverse_colors);
    try store.copyClient(testing.allocator, client, &view);
    try testing.expectEqual(reset_fg, store.grid(true).background);
    try testing.expectEqual(reset_bg, store.grid(true).foreground);
    try testing.expectEqual(reset_bg, store.rows.items[1].cells[0].fg);
    try testing.expectEqual(reset_fg, store.rows.items[1].cells[0].bg.?);
}

fn expectColor(expected: canvas.Color, actual: canvas.Color) !void {
    // The local Palette blends normalized f32 channels. Compare the expected
    // RGB midpoint within one machine epsilon, not an arbitrary pixel tolerance.
    inline for (.{ "r", "g", "b", "a" }) |field|
        try testing.expectApproxEqAbs(@field(expected, field), @field(actual, field), std.math.floatEps(f32));
}

fn initializedMetadata() c.PhuxTerminalGridMetadata {
    var result = std.mem.zeroes(c.PhuxTerminalGridMetadata);
    result.size = @sizeOf(c.PhuxTerminalGridMetadata);
    result.version = c.PHUX_CLIENT_ABI_VERSION;
    return result;
}
