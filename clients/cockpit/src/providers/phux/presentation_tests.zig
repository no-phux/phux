//! Structural projection proof through the real C client and canonical Rust
//! wire fixtures. This does not exercise AppKit/CoreText rendering.
const std = @import("std");
const testing = std.testing;
const canvas = @import("native_sdk").canvas;
const c = @import("abi.zig").c;
const presentation = @import("presentation.zig");
const projection = @import("cell_projection.zig");

pub fn feed(client: *c.PhuxClient, bytes: []const u8) !void {
    try testing.expectEqual(c.PHUX_CLIENT_OK, c.phux_client_feed_frame(client, bytes.ptr, bytes.len));
}

pub fn fixtureClient() !*c.PhuxClient {
    var options = std.mem.zeroes(c.PhuxClientOptions);
    options.size = @sizeOf(c.PhuxClientOptions);
    options.version = c.PHUX_CLIENT_ABI_VERSION;
    options.max_bootstrap_chunk_bytes = 1024;
    options.max_history_page_bytes = 1024;
    options.max_history_page_rows = 128;
    options.max_history_cache_bytes = 4096;
    options.max_history_materialized_rows = 1024;
    options.history_prefetch_rows = 64;
    var client: ?*c.PhuxClient = null;
    try testing.expectEqual(c.PHUX_CLIENT_OK, c.phux_client_new(&options, &client));
    errdefer c.phux_client_free(client);
    const name = "styles";
    try testing.expectEqual(c.PHUX_CLIENT_OK, c.phux_client_queue_hello(client, .{ .data = name.ptr, .len = name.len }));
    try testing.expectEqual(c.PHUX_CLIENT_OK, c.phux_client_outgoing_clear(client));
    try feed(client.?, @embedFile("style_fixture/hello.bin"));
    var attach = std.mem.zeroes(c.PhuxAttachOptions);
    attach.size = @sizeOf(c.PhuxAttachOptions);
    attach.version = c.PHUX_CLIENT_ABI_VERSION;
    attach.attach_id = 1;
    attach.target_kind = c.PHUX_ATTACH_BY_ID;
    attach.session_id = 1;
    attach.cols = 16;
    attach.rows = 6;
    try testing.expectEqual(c.PHUX_CLIENT_OK, c.phux_client_queue_attach(client, &attach));
    try testing.expectEqual(c.PHUX_CLIENT_OK, c.phux_client_outgoing_clear(client));
    inline for (.{ "attached", "begin", "chunk", "ready", "attach-ready" }) |frame| {
        try feed(client.?, @embedFile("style_fixture/" ++ frame ++ ".bin"));
    }
    return client.?;
}

pub fn fixtureGrid(client: *c.PhuxClient) !c.PhuxTerminalGridView {
    var id = std.mem.zeroes(c.PhuxTerminalId);
    id.kind = c.PHUX_TERMINAL_LOCAL;
    id.id = 7;
    var view = std.mem.zeroes(c.PhuxTerminalGridView);
    try testing.expectEqual(c.PHUX_CLIENT_OK, c.phux_client_terminal_grid(client, &id, &view));
    return view;
}

// GUARD: remote-cell-styles
test "canonical C grid preserves remote styles in owned canvas cells" {
    const client = try fixtureClient();
    var client_live = true;
    defer if (client_live) c.phux_client_free(client);
    const view = try fixtureGrid(client);
    try testing.expectEqual(@as(usize, 16 * 6), view.cell_count);
    // Establish that the engine/ABI really supplied these attributes first.
    const raw = view.cells[0];
    const indexed = view.cells[5 * 16];
    const inverse_underline = view.cells[5 * 16 + 1];
    try testing.expect(indexed.flags & c.PHUX_CLIENT_CELL_BOLD != 0);
    try testing.expect(inverse_underline.flags & c.PHUX_CLIENT_CELL_INVERSE != 0);
    try testing.expectEqual(c.PHUX_UNDERLINE_SINGLE, inverse_underline.underline);
    const flags = c.PHUX_CLIENT_CELL_BOLD | c.PHUX_CLIENT_CELL_ITALIC |
        c.PHUX_CLIENT_CELL_STRIKETHROUGH | c.PHUX_CLIENT_CELL_OVERLINE;
    try testing.expectEqual(flags, raw.flags & flags);
    try testing.expectEqual(c.PHUX_UNDERLINE_CURLY, raw.underline);
    var store: presentation.CanvasStore = .{};
    defer store.deinit(testing.allocator);
    try store.copyBorrowed(testing.allocator, &view);
    // Destroy the source engine AND its borrowed arena before reading output.
    c.phux_client_free(client);
    client_live = false;
    const styled = store.grid(true).rows[0].cells[0];
    try testing.expect(styled.bold);
    try testing.expect(styled.italic);
    try testing.expect(styled.strikethrough);
    try testing.expect(styled.overline);
    try testing.expect(styled.underline);
    try testing.expectEqual(canvas.terminal_grid.TerminalUnderline.curly, styled.underline_style);
    try testing.expectEqual(canvas.Color.rgb8(60, 90, 150), styled.underline_color.?);
    try testing.expectEqual(canvas.Color.rgb8(180, 120, 60), styled.fg);
    try testing.expectEqual(canvas.Color.rgb8(20, 40, 80), styled.bg.?);
    try testing.expectEqualStrings("A", styled.cluster);
    try testing.expect(store.rows.items[5].cells[0].bold);
    try testing.expectEqual(
        canvas.Color.rgb8(indexed.foreground_r, indexed.foreground_g, indexed.foreground_b),
        store.rows.items[5].cells[0].fg,
    );
    // The ABI bakes SGR 59 into raw foreground, losing default provenance.
    // Preserve that supplied color, including for inverse cells.
    try testing.expectEqual(
        canvas.Color.rgb8(inverse_underline.underline_r, inverse_underline.underline_g, inverse_underline.underline_b),
        store.rows.items[5].cells[1].underline_color.?,
    );
    const underlines = [_]canvas.terminal_grid.TerminalUnderline{ .single, .single, .double, .curly, .dotted, .dashed };
    for (underlines, 0..) |style, col| {
        const cell = store.rows.items[1].cells[col];
        try testing.expectEqual(col != 0, cell.underline);
        try testing.expectEqual(style, cell.underline_style);
    }
    try expectColorAndOccupancy(&store);
    try testing.expect(store.hyperlinkAt(2, 8) != null);
    try testing.expectEqualStrings("https://example.test/owned", store.hyperlinkAt(2, 8).?);
    try testing.expect(store.hyperlinkAt(2, 7) == null);
    try testing.expect(store.hyperlinkAt(6, 0) == null);
    try testing.expect(store.hyperlinkAt(0, 16) == null);
    try testing.expect(std.mem.indexOf(u8, store.screen_text.items, "https://") == null);
}

fn expectColorAndOccupancy(store: *const presentation.CanvasStore) !void {
    const cells = store.rows.items[2].cells;
    const fg = canvas.Color.rgb8(180, 120, 60);
    const bg = canvas.Color.rgb8(20, 40, 80);
    try testing.expectEqual(bg, cells[0].fg);
    try testing.expectEqual(fg, cells[0].bg.?);
    try testing.expectEqual(canvas.Color.rgb8(100, 80, 70), cells[1].fg);
    try testing.expectEqual(bg, cells[1].bg.?);
    try testing.expectEqual(bg, cells[2].fg); // inverse + faint: same precedence as local
    try testing.expectEqual(fg, cells[2].bg.?);
    try testing.expectEqual(@as(u21, 0), cells[3].cp);
    try testing.expectEqualStrings("", cells[3].cluster);
    try testing.expect(cells[3].strikethrough and cells[3].underline);
    try testing.expectEqual(@as(u21, '─'), cells[4].cp);
    try testing.expectEqualStrings("", cells[4].cluster);
    try testing.expectEqualStrings("e\u{301}", cells[5].cluster);
    try testing.expectEqual(canvas.TerminalWide.wide, cells[6].wide);
    try testing.expectEqualStrings("界", cells[6].cluster);
    try testing.expectEqual(canvas.TerminalWide.spacer, cells[7].wide);
    try testing.expectEqualStrings("", cells[7].cluster);
    try testing.expectEqual(canvas.TerminalWide.spacer, store.rows.items[3].cells[15].wide);
    try testing.expectEqual(canvas.TerminalWide.wide, store.rows.items[4].cells[0].wide);
}

test "frozen canvas deep owns styles clusters hyperlinks and source color provenance" {
    const client = try fixtureClient();
    defer c.phux_client_free(client);
    const view = try fixtureGrid(client);
    var source: presentation.CanvasStore = .{};
    try source.copyClient(testing.allocator, client, &view);
    var owned = source.clone(testing.allocator) catch |err| {
        source.deinit(testing.allocator);
        return err;
    };
    const policy = @import("grid_metadata.zig").Policy{ .foreground = canvas.Color.rgb8(123, 45, 67), .bold_as_bright = true };
    source.setColorPolicy(policy);
    const expected_foreground = source.foreground;
    const expected_bright = source.rows.items[5].cells[0].fg;
    source.deinit(testing.allocator);
    defer owned.deinit(testing.allocator);
    const styled = owned.grid(false).rows[0].cells[0];
    try testing.expectEqualStrings("A", styled.cluster);
    try testing.expect(styled.bold and styled.italic and styled.overline and styled.strikethrough);
    try testing.expectEqual(canvas.terminal_grid.TerminalUnderline.curly, styled.underline_style);
    try testing.expectEqual(canvas.Color.rgb8(60, 90, 150), styled.underline_color.?);
    try testing.expectEqualStrings("https://example.test/owned", owned.hyperlinkAt(2, 8).?);
    try testing.expectEqual(owned.cells.items.ptr, owned.rows.items[0].cells.ptr);
    try testing.expectEqual(owned.cells.items.len, owned.source_colors.cells.items.len);
    try testing.expect(owned.source_colors.globals.?.cells == null);
    try testing.expectEqualStrings("e\u{301}", owned.rows.items[2].cells[5].cluster);
    try testing.expectEqualStrings("界", owned.rows.items[2].cells[6].cluster);
    owned.setColorPolicy(policy);
    try testing.expectEqual(expected_foreground, owned.foreground);
    try testing.expectEqual(expected_bright, owned.rows.items[5].cells[0].fg);
    try testing.expectEqualStrings("A", owned.rows.items[0].cells[0].cluster);
}

// GUARD: remote-cell-hyperlinks
test "remote URI and cluster admission is bounded before replacing owned state" {
    var raw = [_]c.PhuxTerminalCell{std.mem.zeroes(c.PhuxTerminalCell)};
    var arena = [_]u8{ 'X', 'u', 'r', 'i' };
    raw[0].utf8_len = 1;
    raw[0].flags = c.PHUX_CLIENT_CELL_HYPERLINK | c.PHUX_CLIENT_CELL_SELECTED;
    raw[0].hyperlink_offset = 1;
    raw[0].hyperlink_len = 3;
    var view = std.mem.zeroes(c.PhuxTerminalGridView);
    view.cols = 1;
    view.rows = 1;
    view.cells = &raw;
    view.cell_count = 1;
    view.utf8 = .{ .data = &arena, .len = arena.len };
    var store: presentation.CanvasStore = .{};
    defer store.deinit(testing.allocator);
    try store.copyBorrowed(testing.allocator, &view);
    @memset(&arena, 'z');
    try testing.expectEqualStrings("X", store.cells.items[0].cluster);
    try testing.expect(store.hyperlinkAt(0, 0) != null);
    try testing.expectEqualStrings("uri", store.hyperlinkAt(0, 0).?);
    try testing.expectEqual([2]u16{ 0, 0 }, store.rows.items[0].selection.?);
    try testing.expect(store.selection_active);
    raw[0].hyperlink_offset = std.math.maxInt(u32);
    try testing.expectError(error.Protocol, store.copyBorrowed(testing.allocator, &view));
    raw[0].hyperlink_offset = 1;
    raw[0].hyperlink_len = std.math.maxInt(u32);
    try testing.expectError(error.Protocol, store.copyBorrowed(testing.allocator, &view));
    raw[0].hyperlink_len = 3;
    arena[1] = 0xff;
    try testing.expectError(error.Protocol, store.copyBorrowed(testing.allocator, &view));
    arena[1] = 'z';
    try testing.expectError(error.Protocol, projection.validate(&view, 2));
    _ = try projection.validate(&view, 3); // exact URI budget
    const repeated = [_]c.PhuxTerminalCell{ raw[0], raw[0] };
    var repeated_view = view;
    repeated_view.cols = 2;
    repeated_view.cell_count = 2;
    repeated_view.cells = &repeated;
    try testing.expectError(error.Protocol, projection.validate(&repeated_view, 5));
    _ = try projection.validate(&repeated_view, 6); // aggregate URI budget
    raw[0].utf8_offset = std.math.maxInt(u32);
    try testing.expectError(error.Protocol, store.copyBorrowed(testing.allocator, &view));
    try testing.expectEqualStrings("uri", store.hyperlinkAt(0, 0).?);
    raw[0].utf8_offset = 0;
    arena[0] = 0xff;
    try testing.expectError(error.Protocol, store.copyBorrowed(testing.allocator, &view));
    arena[0] = 'z';
    raw[0].flags = 0;
    try store.copyBorrowed(testing.allocator, &view);
    try testing.expect(store.hyperlinkAt(0, 0) == null);
    try testing.expect(!store.selection_active);
    const empty = std.mem.zeroes(c.PhuxTerminalGridView);
    try store.copyBorrowed(testing.allocator, &empty);
    try testing.expectEqual(@as(usize, 0), store.cells.items.len);
    try testing.expect(store.hyperlinkAt(0, 0) == null);
}

// GUARD: remote-cursor-hollow
test "remote cursor projection retains ABI-provided hollow shape" {
    var raw = std.mem.zeroes(c.PhuxTerminalCell);
    var view = std.mem.zeroes(c.PhuxTerminalGridView);
    view.cols = 1;
    view.rows = 1;
    view.cell_count = 1;
    view.cells = @ptrCast(&raw);
    view.cursor_visible = true;
    view.cursor_style = c.PHUX_CURSOR_BLOCK_HOLLOW;
    var store: presentation.CanvasStore = .{};
    defer store.deinit(testing.allocator);
    try store.copyBorrowed(testing.allocator, &view);
    try testing.expectEqual(canvas.TerminalCursorShape.block_hollow, store.cursor.?.shape);
    view.cursor_visible = false;
    try store.copyBorrowed(testing.allocator, &view);
    try testing.expect(store.cursor == null);
}
