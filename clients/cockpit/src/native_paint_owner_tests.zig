//! Exact-source native display-list evidence, through real Model/FFI providers.
//! These assertions inspect packets, not AppKit/CoreText rasterization.
const std = @import("std");
const testing = std.testing;
const sdk = @import("native_sdk");
const canvas = sdk.canvas;
const support = @import("cockpit/phux_support.zig");
const Model = @import("cockpit/model.zig").Model;
const Engine = @import("cockpit/native/ts_engine.zig").Engine;
const layout = @import("cockpit/layout.zig");
const painter = @import("cockpit/native/terminal_painter.zig");
const search_painter = @import("cockpit/native/search_painter.zig");

const Pair = struct {
    engine: *Engine,
    first: *support.PhuxProvider,
    second: *support.PhuxProvider,
    ref: support.TerminalRef,

    fn start() !Pair {
        const engine = try Engine.create(testing.allocator, testing.io);
        errdefer engine.destroy();
        const model = engine.model;
        const first = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/unused-paint-owner" }, null, "first");
        model.phux_provider = first;
        try model.ensurePeerSlots(1);
        const second = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/unused-paint-owner" }, null, "second");
        model.peers.items[0].provider = second;
        try support.PhuxProvider.test_support.attachHost(first.host);
        try support.PhuxProvider.test_support.attachHost(second.host);
        const ref: support.TerminalRef = .{ .provider_id = first.providerId(), .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, 7, "") } };
        const second_ws = model.openWindow(1).?;
        model.primary = .{};
        model.primary.tabs[0] = layout.Tree.initLeaf(ref);
        model.primary.tabs[0].attachment_id = first.context_id;
        model.primary.tab_count = 1;
        second_ws.tabs[0] = layout.Tree.initLeaf(ref);
        second_ws.tabs[0].attachment_id = second.context_id;
        second_ws.tab_count = 1;
        model.remote_ui[0] = .{ .terminal_ref = ref, .owner = first.owner(ref).? };
        model.remote_ui[1] = .{ .terminal_ref = ref, .owner = second.owner(ref).? };
        try output(first, "\x1b[2J\x1b[HFIRST-ONLY");
        try output(second, "\x1b[2J\x1b[HSECOND-ONLY");
        return .{ .engine = engine, .first = first, .second = second, .ref = ref };
    }
};

/// RESOURCE_OUTPUT TLVs, validated by the production FFI decoder. Keep this
/// fixture short enough for a single-byte bytes-field length.
fn output(remote: *support.PhuxProvider, text: []const u8) !void {
    try testing.expect(text.len < 128);
    var storage: [256]u8 = undefined;
    var writer: std.Io.Writer = .fixed(&storage);
    try writer.writeAll(&.{ 0, 0, 0, 0, 0x90, 1, 4, 5, 0, 0, 0, 0, 7, 2, 4, 8 });
    try writer.writeInt(u64, 1, .big);
    try writer.writeAll(&.{ 3, 4 });
    try writer.writeByte(@intCast(text.len));
    try writer.writeAll(text);
    try writer.writeAll(&.{ 4, 4, 8 });
    try writer.writeInt(u64, 7, .big);
    try writer.writeAll(&.{ 5, 4, 8 });
    try writer.writeInt(u64, 1, .big);
    const bytes = writer.buffered();
    std.mem.writeInt(u32, bytes[0..4], @intCast(bytes.len - 4), .big);
    try testing.expect(remote.bridge.incoming.stage(bytes));
    _ = try remote.drainReadiness();
}

fn firstRow(list: canvas.DisplayList) !canvas.CellGrid {
    for (list.commands) |command| {
        if (command == .cell_grid) return command.cell_grid;
    }
    return error.MissingTerminalPacket;
}

fn expectText(row: canvas.CellGrid, text: []const u8) !void {
    for (text, 0..) |byte, index| {
        const cell = row.at(index, 0) orelse return error.MissingCell;
        try testing.expectEqualSlices(u8, &.{byte}, cell.cluster(row.text));
    }
}

fn paint(model: *const Model, builder: *canvas.Builder, window: usize) !void {
    builder.* = canvas.Builder.init(builder.commands);
    try painter.paintWindowIndex(model, builder, window, .{ .width = 980, .height = 640 }, .{}, @intCast(window));
}

test "exact-source paint packets keep identical coordinator refs in their own windows" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const pair = try Pair.start();
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const first_owner = pair.first.owner(pair.ref).?;
    const second_owner = pair.second.owner(pair.ref).?;
    try testing.expect(first_owner.terminal_ref.eql(second_owner.terminal_ref));
    try testing.expect(first_owner.generation.sameReplica(second_owner.generation));
    try testing.expect(!first_owner.eql(second_owner));
    try testing.expect(model.phuxForRefConst(pair.ref) == null);
    const commands = try testing.allocator.alloc(canvas.CanvasCommand, canvas.max_display_list_commands);
    defer testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try paint(model, &builder, 0);
    try expectText(try firstRow(builder.displayList()), "FIRST-ONLY");
    try paint(model, &builder, 1);
    try expectText(try firstRow(builder.displayList()), "SECOND-ONLY");
    model.active_window = 1;
    try paint(model, &builder, 0);
    try expectText(try firstRow(builder.displayList()), "FIRST-ONLY");
}

fn openSearch(remote: *support.PhuxProvider, state: *@import("cockpit/model.zig").RemoteUiState, needle: []const u8) !void {
    const matches = try remote.search(state.owner, needle);
    state.search.open = true;
    @memcpy(state.search.needle_buf[0..needle.len], needle);
    state.search.needle_len = needle.len;
    state.search.count = matches.len;
}

fn commandText(list: canvas.DisplayList, id: u64) ?[]const u8 {
    for (list.commands) |command| {
        if (command != .draw_text) continue;
        if (command.draw_text.id == id) return command.draw_text.text;
    }
    return null;
}

// The search BAND's height comes from `projection.searchRevealedIn`, which
// still reads the ambient `remoteUiConst(ref)`; with two owners of one ref it
// is null and the band is zero-height, so no 0x0d01 packet is emitted yet.
// Until the projection resolves reveal state from the selected tree, this
// asserts the painter-owned seam: the View each window's band would paint.
test "exact-source search view carries each owners field and status" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const pair = try Pair.start();
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const first_tree = model.primary.selectedTreeConst().?;
    const second_tree = model.wsAtConst(1).?.selectedTreeConst().?;
    try testing.expect(search_painter.viewIn(model, first_tree, pair.ref) == null);
    try openSearch(pair.first, &model.remote_ui[0], "FIRST");
    try openSearch(pair.second, &model.remote_ui[1], "O");
    try testing.expectEqual(@as(usize, 1), model.remote_ui[0].search.count);
    try testing.expectEqual(@as(usize, 2), model.remote_ui[1].search.count);
    const first = search_painter.viewIn(model, first_tree, pair.ref) orelse return error.MissingSearchView;
    try testing.expectEqualStrings("FIRST", first.needle);
    try testing.expectEqual(@as(usize, 1), first.count);
    const second = search_painter.viewIn(model, second_tree, pair.ref) orelse return error.MissingSearchView;
    try testing.expectEqualStrings("O", second.needle);
    try testing.expectEqual(@as(usize, 2), second.count);
    model.remote_ui[1].search.open = false;
    try testing.expect(search_painter.viewIn(model, second_tree, pair.ref) == null);
    try testing.expectEqualStrings("FIRST", search_painter.viewIn(model, first_tree, pair.ref).?.needle);
    // A stale explicit attachment has no search view, not a sibling's.
    model.remote_ui[1].search.open = true;
    model.wsAt(1).?.tabs[0].attachment_id = std.math.maxInt(u64);
    try testing.expect(search_painter.viewIn(model, model.wsAtConst(1).?.selectedTreeConst().?, pair.ref) == null);
}

fn selectionWash(list: canvas.DisplayList) ?sdk.geometry.RectF {
    // The SDK's terminal selection overlay uses a 0.30-alpha fill, separate
    // from cell backgrounds and the focus/cursor commands.
    for (list.commands) |command| {
        if (command != .fill_rect) continue;
        if (command.fill_rect.fill.color.a == 0.30) return command.fill_rect.rect;
    }
    return null;
}

test "exact-source paint packets isolate selection and measured cell receipts" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const pair = try Pair.start();
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const commands = try testing.allocator.alloc(canvas.CanvasCommand, canvas.max_display_list_commands);
    defer testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try paint(model, &builder, 1);
    try testing.expect(selectionWash(builder.displayList()) == null);
    try testing.expect(pair.first.presentation(pair.ref).?.measured_cell == null);
    try testing.expect(pair.second.presentation(pair.ref).?.measured_cell != null);
    const owner = pair.second.owner(pair.ref).?;
    const start = try pair.second.createAnchor(owner, .{ .space = .viewport, .row = 0, .column = 0 });
    defer pair.second.releaseAnchor(owner, start);
    const end = try pair.second.createAnchor(owner, .{ .space = .viewport, .row = 0, .column = 5 });
    defer pair.second.releaseAnchor(owner, end);
    try pair.second.setSelection(owner, start, end, false);
    model.remote_ui[1].selecting = true;
    try paint(model, &builder, 1);
    const row = try firstRow(builder.displayList());
    const wash = selectionWash(builder.displayList()) orelse return error.MissingSelectionPacket;
    try testing.expectEqual(row.origin.x, wash.x);
    try testing.expectEqual(row.origin.y, wash.y);
    try testing.expectEqual(row.cell_width * 6, wash.width);
    try paint(model, &builder, 0);
    try testing.expect(selectionWash(builder.displayList()) == null);
    try testing.expect(pair.first.presentation(pair.ref).?.measured_cell != null);
}

test "exact-source paint packets never fall back from a stale explicit attachment" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const pair = try Pair.start();
    defer pair.engine.destroy();
    const model = pair.engine.model;
    try openSearch(pair.first, &model.remote_ui[0], "FIRST");
    try openSearch(pair.second, &model.remote_ui[1], "SECOND");
    const commands = try testing.allocator.alloc(canvas.CanvasCommand, canvas.max_display_list_commands);
    defer testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try paint(model, &builder, 1);
    try expectText(try firstRow(builder.displayList()), "SECOND-ONLY");
    model.wsAt(1).?.tabs[0].attachment_id = std.math.maxInt(u64);
    try paint(model, &builder, 1);
    try testing.expectError(error.MissingTerminalPacket, firstRow(builder.displayList()));
    try testing.expect(commandText(builder.displayList(), 0x0d01) == null);
    try paint(model, &builder, 0);
    try expectText(try firstRow(builder.displayList()), "FIRST-ONLY");
}

test "exact-source paint policy updates only the provider being painted" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const pair = try Pair.start();
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const initial = pair.first.presentation(pair.ref).?.grid.background;
    const configured = canvas.Color.rgb8(23, 37, 59);
    try testing.expect(!std.meta.eql(initial, configured));
    model.config.background = .{ .r = 23, .g = 37, .b = 59 };
    const commands = try testing.allocator.alloc(canvas.CanvasCommand, canvas.max_display_list_commands);
    defer testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try paint(model, &builder, 1);
    try testing.expectEqual(configured, pair.second.presentation(pair.ref).?.grid.background);
    try testing.expectEqual(initial, pair.first.presentation(pair.ref).?.grid.background);
    try paint(model, &builder, 0);
    try testing.expectEqual(configured, pair.first.presentation(pair.ref).?.grid.background);
}
