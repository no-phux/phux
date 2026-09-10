//! Bounded catalog pages with a read revision fence and captured opaque targets.
//! Display indices never authorize identity-qualified command selection.
const std = @import("std");
const model_module = @import("../model.zig");
const projection = @import("workspace_projection.zig");
const support = @import("../phux_support.zig");
const Model = model_module.Model;
const Entry = projection.PaletteEntry;
pub const targets = @import("catalog_targets.zig");

pub const request_name = "cockpit.navigation";
// At the 420pt minimum height: 48 outer + 32 panel + 32 heading + 40
// input + 4*32 rows + 20 notice + 32 paging + 4*12 gaps = 380pt.
pub const page_size = 4;
pub const max_label_bytes = 240;
pub const max_bytes = 4096;
pub const Error = error{ InvalidRequest, StaleRevision, BufferTooSmall, CatalogTooLarge, UnavailableContext };
const empty_workspace: model_module.Workspace = .{};

pub const Connection = enum(u8) { local = 0, connecting = 1, connected = 2, offline = 3, workspace_unavailable = 4 };

pub fn connection(model: *const Model) Connection {
    if (model.phux_reconnect_after_close) return .connecting;
    if (model.phux_connection_unavailable) return .offline;
    if (comptime !support.phux_enabled) return .local;
    const remote = model.phuxConst() orelse return .local;
    if (remote.state() == .attached and (model.shared_workspace.refused or model.shared_workspace.subscription_refused)) return .workspace_unavailable;
    return switch (remote.state()) {
        .new, .hello_queued, .negotiated => .connecting,
        .attached => .connected,
        .detached, .failed => .offline,
    };
}

comptime {
    // Header + longest query + four longest labels, bounded by the host limit.
    std.debug.assert(16 + model_module.max_palette_query_bytes + page_size * (5 + targets.max_len + max_label_bytes) <= max_bytes);
}

/// Explicit visual elision, on a UTF-8 boundary. Inventory is never elided.
pub fn displayText(text: []const u8, out: []u8) []const u8 {
    if (text.len <= out.len) {
        @memcpy(out[0..text.len], text);
        return out[0..text.len];
    }
    if (out.len < 3) return "";
    var end = out.len - 3;
    while (end > 0 and (text[end] & 0xc0) == 0x80) end -= 1;
    @memcpy(out[0..end], text[0..end]);
    @memcpy(out[end..][0..3], "…");
    return out[0 .. end + 3];
}

pub fn resolve(model: *const Model, current_revision: u64, expected_revision: u64, index: u16) ?Entry {
    if (current_revision != expected_revision) return null;
    var entries: [1]Entry = undefined;
    const count = projection.paletteEntriesWindowIn(model, &empty_workspace, .{ .first = index, .count = 1 }, &entries);
    if (count == 0) return null;
    return entries[0];
}

fn identityIndex(model: *const Model, wanted: Entry) ?u16 {
    var first: usize = 0;
    var entries: [projection.palette_max_visible_rows]Entry = undefined;
    while (first <= std.math.maxInt(u16)) : (first += entries.len) {
        const count = projection.paletteEntriesWindowIn(model, &empty_workspace, .{ .first = first, .count = entries.len }, &entries);
        for (entries[0..count], first..) |entry, index| {
            if (std.meta.eql(entry, wanted)) return @intCast(index);
        }
        if (count < entries.len) return null;
    }
    return null;
}

fn terminalLabel(model: *const Model, ref: model_module.TerminalRef, out: []u8) []const u8 {
    var title_buf: [512]u8 = undefined;
    const title = projection.terminalTitleInto(model, ref, &title_buf);
    const provider = if (support.providerKind(ref) == .local) "Local" else "Phux";
    return std.fmt.bufPrint(out, "{s} · {s}", .{ provider, title }) catch provider;
}

fn entryLabel(model: *const Model, entry: Entry, out: []u8) []const u8 {
    var detail: [640]u8 = undefined;
    return switch (entry) {
        .placed_terminal => |placed| std.fmt.bufPrint(out, "Window {d} · Tab {d} · {s}", .{
            placed.window + 1, placed.tab + 1, terminalLabel(model, placed.terminal_ref, &detail),
        }) catch "Open terminal",
        .available_terminal => |ref| std.fmt.bufPrint(out, "Available · {s}", .{terminalLabel(model, ref, &detail)}) catch "Available terminal",
        .session => |id| sessionLabel(model, id, out),
    };
}

fn sessionLabel(model: *const Model, id: u32, out: []u8) []const u8 {
    const remote = model.phuxConst() orelse return "Session";
    for (remote.sessionCatalog()) |session| {
        if (session.id == id) return std.fmt.bufPrint(out, "Session · {s} · #{d}", .{ session.name, id }) catch "Session";
    }
    return "Session";
}

fn encodeEntry(model: *const Model, entry: Entry, out: []u8, start: usize) Error!usize {
    const index = identityIndex(model, entry) orelse return error.CatalogTooLarge;
    var full: [1024]u8 = undefined;
    var bounded: [max_label_bytes]u8 = undefined;
    const label = displayText(entryLabel(model, entry, &full), &bounded);
    const target = targets.capture(model, entry) orelse return error.UnavailableContext;
    var target_buffer: [targets.max_len]u8 = undefined;
    const bytes = target.encode(&target_buffer);
    if (start + 5 + label.len + bytes.len > out.len) return error.BufferTooSmall;
    std.mem.writeInt(u16, out[start..][0..2], index, .little);
    out[start + 2] = @intCast(label.len);
    std.mem.writeInt(u16, out[start + 3 ..][0..2], @intCast(bytes.len), .little);
    @memcpy(out[start + 5 ..][0..bytes.len], bytes);
    @memcpy(out[start + 5 + bytes.len ..][0..label.len], label);
    return start + 5 + bytes.len + label.len;
}

/// Request: version=1, kind=3, revision:u64, offset:u16, query_len:u8,
/// query UTF-8 (<=64). Reply echoes those 13+query bytes, followed by
/// total:u16, count:u8, then records (index:u16, label_len:u8, target_len:u16,
/// opaque target bytes, label). Only the target authorizes activation.
fn validateRequest(revision: u64, request: []const u8) Error!void {
    if (request.len < 13 or request[0] != 1 or request[1] != 3) return error.InvalidRequest;
    const query_len = request[12];
    if (query_len > model_module.max_palette_query_bytes or request.len != 13 + @as(usize, query_len)) return error.InvalidRequest;
    if (std.mem.readInt(u64, request[2..10], .little) != revision) return error.StaleRevision;
}

pub fn encode(model: *const Model, revision: u64, request: []const u8, out: []u8) Error![]const u8 {
    try validateRequest(revision, request);
    const query_len = request[12];
    var workspace: model_module.Workspace = .{};
    workspace.palette.query_len = query_len;
    @memcpy(workspace.palette.query[0..query_len], request[13..]);
    const total = projection.paletteEntryCountIn(model, &workspace);
    if (total > std.math.maxInt(u16)) return error.CatalogTooLarge;
    const first = std.mem.readInt(u16, request[10..12], .little);
    if (first > total) return error.InvalidRequest;
    if (out.len < request.len + 3) return error.BufferTooSmall;
    @memcpy(out[0..request.len], request);
    std.mem.writeInt(u16, out[request.len..][0..2], @intCast(total), .little);
    var entries: [page_size]Entry = undefined;
    const count = projection.paletteEntriesWindowIn(model, &workspace, .{ .first = first, .count = page_size }, &entries);
    out[request.len + 2] = @intCast(count);
    var written = request.len + 3;
    for (entries[0..count]) |entry| written = try encodeEntry(model, entry, out, written);
    return out[0..written];
}

test "navigation display elision preserves UTF-8 and signals omitted text" {
    var out: [8]u8 = undefined;
    try std.testing.expectEqualStrings("éé…", displayText("ééééé", &out));
    try std.testing.expectEqualStrings("short", displayText("short", &out));
}

test "navigation catalog pages include every window and resolve only the fenced inventory" {
    const engine_module = @import("ts_engine.zig");
    const protocol = @import("ts_protocol.zig");
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    for (0..7) |_| {
        const intent = protocol.encodeIntent(.{ .kind = .new_terminal, .expected_revision = engine.revision, .window = 0, .argument = 0 });
        try std.testing.expect(engine.applyIntent(&intent, &engine_module.NoShells{}));
    }
    const open_window = protocol.encodeIntent(.{ .kind = .new_window, .expected_revision = engine.revision, .argument = 0 });
    try std.testing.expect(engine.applyIntent(&open_window, &engine_module.NoShells{}));
    var request = [_]u8{0} ** 13;
    request[0] = 1;
    request[1] = 3;
    std.mem.writeInt(u64, request[2..10], engine.revision, .little);
    var buffer: [max_bytes]u8 = undefined;
    var reached: usize = 0;
    for (0..3) |page| {
        std.mem.writeInt(u16, request[10..12], @intCast(page * page_size), .little);
        const response = try encode(engine.model, engine.revision, &request, &buffer);
        try std.testing.expectEqual(@as(u16, 9), std.mem.readInt(u16, response[13..15], .little));
        const count = response[15];
        reached += count;
        var at: usize = 16;
        for (0..count) |_| {
            const index = std.mem.readInt(u16, response[at..][0..2], .little);
            const destination = resolve(engine.model, engine.revision, engine.revision, index).?;
            if (index == 8) try std.testing.expectEqual(@as(u8, 1), destination.placed_terminal.window);
            try std.testing.expect(resolve(engine.model, engine.revision + 1, engine.revision, index) == null);
            at += 5 + @as(usize, std.mem.readInt(u16, response[at + 3 ..][0..2], .little)) + @as(usize, response[at + 2]);
        }
        try std.testing.expectEqual(response.len, at);
    }
    try std.testing.expectEqual(@as(usize, 9), reached);
    try std.testing.expectError(error.StaleRevision, encode(engine.model, engine.revision + 1, &request, &buffer));
    try std.testing.expectError(error.BufferTooSmall, encode(engine.model, engine.revision, &request, buffer[0..15]));
}

test "navigation search maps filtered rows back to unfiltered indices" {
    const engine_module = @import("ts_engine.zig");
    const protocol = @import("ts_protocol.zig");
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const intent = protocol.encodeIntent(.{ .kind = .new_window, .expected_revision = engine.revision, .argument = 0 });
    try std.testing.expect(engine.applyIntent(&intent, &engine_module.NoShells{}));
    var request = [_]u8{0} ** 21;
    request[0] = 1;
    request[1] = 3;
    std.mem.writeInt(u64, request[2..10], engine.revision, .little);
    request[12] = 8;
    @memcpy(request[13..], "window 2");
    var buffer: [max_bytes]u8 = undefined;
    const response = try encode(engine.model, engine.revision, &request, &buffer);
    try std.testing.expectEqual(@as(u8, 1), response[23]);
    try std.testing.expectEqual(@as(u16, 1), std.mem.readInt(u16, response[24..26], .little));
}

test "navigation reaches available remote identities and the last session beyond byte indices" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine_module = @import("ts_engine.zig");
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const remote = try model_module.PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/navigation-test-unused" }, null, "navigation-test");
    engine.model.phux_provider = remote;
    try std.testing.expectEqual(Connection.connecting, connection(engine.model));
    engine.model.phux_connection_unavailable = true;
    try std.testing.expectEqual(Connection.offline, connection(engine.model));
    const ref: model_module.TerminalRef = .{
        .provider_id = .phux,
        .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, 900, "") },
    };
    engine.model.remote_inventory[0] = ref;
    engine.model.remote_inventory_count = 1;
    for (0..256) |index| {
        const name = try std.fmt.allocPrint(std.testing.allocator, "session-{d}", .{index});
        try remote.host.sessions.append(std.testing.allocator, .{
            .id = @intCast(index + 1),
            .name = name,
            .created_at_unix_secs = 0,
            .window_count = 1,
            .attached_client_count = 0,
            .focused = false,
        });
    }
    try std.testing.expect(resolve(engine.model, 7, 7, 1).?.available_terminal.eql(ref));
    try std.testing.expectEqual(@as(u32, 256), resolve(engine.model, 7, 7, 257).?.session);
    var request = [_]u8{0} ** 13;
    request[0] = 1;
    request[1] = 3;
    request[2] = 7;
    std.mem.writeInt(u16, request[10..12], 256, .little);
    var buffer: [max_bytes]u8 = undefined;
    const response = try encode(engine.model, 7, &request, &buffer);
    try std.testing.expectEqual(@as(u16, 258), std.mem.readInt(u16, response[13..15], .little));
    try std.testing.expectEqual(@as(u8, 2), response[15]);
    try std.testing.expect(std.mem.indexOf(u8, response, "session-255") != null);
}
