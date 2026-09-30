//! Read-only live attachment projection. Registry aliases are labels, never
//! authority: even legacy Connect to Host peers retain context/epoch fences.
const std = @import("std");
const Model = @import("../model.zig").Model;
const support = @import("../phux_support.zig");
const runtime = @import("machine_runtime.zig");
const navigation = @import("ts_navigation.zig");
const windows = @import("ts_window_navigation.zig");
pub const target_len = 34;
pub const target_tag = 6;

const Row = struct {
    entry: ?runtime.Entry,
    index: u16,
};

fn target(model: *const Model, entry: ?runtime.Entry) [target_len]u8 {
    var bytes = [_]u8{0} ** target_len;
    bytes[0] = target_tag;
    bytes[1] = @intCast(model.active_window);
    std.mem.writeInt(u64, bytes[2..10], model.window_epochs[model.active_window], .little);
    std.mem.writeInt(u64, bytes[18..26], model.provider.context_id, .little);
    if (comptime support.phux_enabled) if (entry) |live| {
        std.mem.writeInt(u64, bytes[10..18], live.provider.context_id, .little);
        std.mem.writeInt(u64, bytes[18..26], live.provider.host.context_id, .little);
        std.mem.writeInt(u64, bytes[26..34], live.provider.connectionEpoch(), .little);
    };
    return bytes;
}

/// Returns the exact attachment, or zero for the in-process local provider.
/// A reconnect, endpoint retarget, closed window or recycled slot invalidates it.
pub fn resolve(model: *const Model, bytes: []const u8) ?u64 {
    if (bytes.len != target_len or bytes[0] != target_tag) return null;
    const origin: windows.Target = .{ .window = bytes[1], .epoch = std.mem.readInt(u64, bytes[2..10], .little) };
    if (!origin.validWindow(model)) return null;
    const id = std.mem.readInt(u64, bytes[10..18], .little);
    const context = std.mem.readInt(u64, bytes[18..26], .little);
    const epoch = std.mem.readInt(u64, bytes[26..34], .little);
    if (id == 0) return if (context == model.provider.context_id and epoch == 0 and hasPlacedLocalTerminals(model)) 0 else null;
    if (comptime !support.phux_enabled) return null;
    const provider = model.phuxForAttachmentConst(id) orelse return null;
    if (provider.pending_retarget != null) return null;
    if (provider.host.context_id != context or provider.connectionEpoch() != epoch) return null;
    return id;
}

fn append(rows: *[navigation.page_size]Row, count: *usize, total: *u16, offset: u16, entry: ?runtime.Entry) navigation.Error!void {
    if (total.* == std.math.maxInt(u16)) return error.CatalogTooLarge;
    if (total.* >= offset and count.* < rows.len) {
        rows[count.*] = .{ .entry = entry, .index = total.* };
        count.* += 1;
    }
    total.* += 1;
}

fn hasPlacedLocalTerminals(model: *const Model) bool {
    for (model.provider.states, 0..) |state, index| {
        if (state != .active) continue;
        if (model.locateTerminal(model.provider.slots[index].id) != null) return true;
    }
    return false;
}

pub fn encode(model: *const Model, request: []const u8, offset: u16, out: []u8) navigation.Error![]const u8 {
    var rows: [navigation.page_size]Row = undefined;
    var count: usize = 0;
    var total: u16 = 0;
    // Only actual local terminal placements represent an ephemeral host.
    // An empty in-process provider is not a connected local coordinator.
    if (model.localPhuxProviderConst() == null and hasPlacedLocalTerminals(model)) try append(&rows, &count, &total, offset, null);
    if (comptime support.phux_enabled) {
        var iterator: runtime.Iterator = .{ .model = model };
        while (iterator.next()) |entry| try append(&rows, &count, &total, offset, entry);
    }
    if (offset > total) return error.InvalidRequest;
    if (out.len < request.len + 3) return error.BufferTooSmall;
    @memcpy(out[0..request.len], request);
    std.mem.writeInt(u16, out[request.len..][0..2], total, .little);
    out[request.len + 2] = @intCast(count);
    var at = request.len + 3;
    for (rows[0..count]) |row| at = try encodeRow(model, row, out, at);
    if (at == out.len) return error.BufferTooSmall;
    out[at] = 0x4e;
    at += 1;
    for (rows[0..count]) |row| at = try metadata(model, row, out, at);
    return out[0..at];
}

fn encodeRow(model: *const Model, row: Row, out: []u8, at: usize) navigation.Error!usize {
    var label_buffer: [navigation.max_label_bytes]u8 = undefined;
    var name: []const u8 = "This Mac";
    if (comptime support.phux_enabled) if (row.entry) |entry| {
        name = entry.provider.remoteLabel() orelse "This Mac";
    };
    const label = navigation.displayText(name, &label_buffer);
    const captured = target(model, row.entry);
    if (at + 5 + captured.len + label.len > out.len) return error.BufferTooSmall;
    std.mem.writeInt(u16, out[at..][0..2], row.index, .little);
    out[at + 2] = @intCast(label.len);
    std.mem.writeInt(u16, out[at + 3 ..][0..2], captured.len, .little);
    @memcpy(out[at + 5 ..][0..captured.len], &captured);
    @memcpy(out[at + 5 + captured.len ..][0..label.len], label);
    return at + 5 + captured.len + label.len;
}

fn attachmentDetail(entry: runtime.Entry, out: []u8) []const u8 {
    if (comptime !support.phux_enabled) return "Unavailable";
    const status: []const u8 = switch (runtime.connection(entry)) {
        .connected => "Connected",
        .connecting => "Connecting…",
        .reconnecting => "Reconnecting…",
        .failed => "Disconnected",
        .not_connected => "Not connected",
    };
    const session = entry.provider.currentSessionName() orelse "";
    // Endpoint URLs can carry credentials in userinfo or query parameters.
    // The menu needs status and session, not transport configuration.
    return std.fmt.bufPrint(out, "{s}{s}{s}", .{
        status,
        if (session.len > 0) " · " else "",
        session,
    }) catch status;
}

fn metadata(model: *const Model, row: Row, out: []u8, at: usize) navigation.Error!usize {
    var current = model.phuxForWindowConst(model.active_window) == null;
    var selectable = true;
    var text: []const u8 = "Local terminals";
    var full: [1024]u8 = undefined;
    var bounded: [navigation.max_detail_bytes]u8 = undefined;
    if (comptime support.phux_enabled) if (row.entry) |entry| {
        current = model.phuxForWindowConst(model.active_window) == entry.provider;
        const state = runtime.connection(entry);
        selectable = state == .connected and entry.provider.pending_retarget == null;
        text = attachmentDetail(entry, &full);
    };
    const detail = navigation.displayText(text, &bounded);
    if (at + 3 + detail.len > out.len) return error.BufferTooSmall;
    out[at] = 3; // Host filter, never a catalog activation target.
    out[at + 1] = @as(u8, @intFromBool(selectable)) | (@as(u8, @intFromBool(current)) << 1);
    out[at + 2] = @intCast(detail.len);
    @memcpy(out[at + 3 ..][0..detail.len], detail);
    return at + 3 + detail.len;
}
