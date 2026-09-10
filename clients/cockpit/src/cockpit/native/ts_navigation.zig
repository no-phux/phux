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
// At the 420pt minimum: 32 outer + 32 panel + 32 heading + 32 scopes +
// 40 input + 4*40 rows + 20 notice + 32 paging + 5*8 gaps = 420pt.
pub const page_size = 4;
pub const max_label_bytes = 240;
pub const max_detail_bytes = 160;
pub const max_host_bytes = support.RemoteResourceId.max_host_bytes;
pub const Scope = enum(u8) { all = 0, sessions = 1, known_hosts = 2, exact_host = 3 };
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

/// Known-host rows filter the catalog; they never carry activation authority.
/// Their target slot holds `host_filter_tag, host_len:u8, raw host` instead of
/// a catalog target (tag 2), so no catalog decoder or TS enqueue accepts it.
pub const host_filter_tag: u8 = 3;

comptime {
    std.debug.assert(host_filter_tag != 2);
    std.debug.assert(2 + max_host_bytes <= targets.max_len);
    // Longest scoped request echo (13 + query + scope/host) + total/count +
    // metadata marker, then four records of the longest target and label plus
    // their metadata detail, bounded by the host limit.
    std.debug.assert(19 + model_module.max_palette_query_bytes + max_host_bytes + page_size * (8 + targets.max_len + max_label_bytes + max_detail_bytes) <= max_bytes);
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

fn entryLabel(model: *const Model, entry: Entry, out: []u8) []const u8 {
    return switch (entry) {
        .placed_terminal => |placed| projection.terminalTitleInto(model, placed.terminal_ref, out),
        .available_terminal => |ref| projection.terminalTitleInto(model, ref, out),
        .session => |id| sessionLabel(model, id, out),
    };
}

fn sessionLabel(model: *const Model, id: u32, out: []u8) []const u8 {
    const remote = model.phuxConst() orelse return "Session";
    for (remote.sessionCatalog()) |session| {
        if (session.id != id) continue;
        if (session.name.len > 0) return session.name;
        break;
    }
    return std.fmt.bufPrint(out, "Session #{d}", .{id}) catch "Session";
}

const Row = struct { entry: Entry, index: u16, is_host: bool };

fn terminalRef(entry: *const Entry) ?*const model_module.TerminalRef {
    return switch (entry.*) {
        .placed_terminal => &entry.placed_terminal.terminal_ref,
        .available_terminal => &entry.available_terminal,
        .session => null,
    };
}

/// Borrow only from the caller's stable entry, never from a copied union payload.
fn entryHost(entry: *const Entry) ?[]const u8 {
    const ref = terminalRef(entry) orelse return null;
    if (support.providerKind(ref.*) == .local) return null;
    return ref.terminal_id.phux.host();
}

fn selectable(model: *const Model, row: *const Row) bool {
    if (row.is_host) return true;
    if (row.entry != .available_terminal) return true;
    if (entryHost(&row.entry) == null) return true;
    const remote = model.phuxConst() orelse return false;
    return remote.terminalSession(row.entry.available_terminal) != null;
}

fn rowKind(row: *const Row) u8 {
    if (row.is_host) return 3;
    return switch (row.entry) {
        .placed_terminal => 0,
        .available_terminal => 1,
        .session => 2,
    };
}

fn hostLabel(host: []const u8) []const u8 {
    return if (host.len == 0) "Coordinator" else host;
}

fn terminalDirectory(model: *const Model, ref: model_module.TerminalRef) []const u8 {
    if (model.provider.terminalConst(ref)) |pane| return pane.pwd();
    const remote = model.phuxConst() orelse return "";
    for (remote.catalogTerminals()) |*entry| {
        if (entry.terminal_ref.eql(ref)) return entry.cwd.slice();
    }
    return "";
}

fn rowDetail(model: *const Model, row: *const Row, out: []u8) []const u8 {
    if (row.is_host) return "Known terminal host";
    if (!selectable(model, row)) return "Ownership unavailable";
    const ref = terminalRef(&row.entry) orelse return "Phux session";
    const host = if (entryHost(&row.entry)) |value| hostLabel(value) else "Local PTY";
    if (row.entry != .placed_terminal) return locationDetail(host, terminalDirectory(model, ref.*), out);
    var location_buffer: [512]u8 = undefined;
    const location = locationDetail(host, terminalDirectory(model, ref.*), &location_buffer);
    const placed = row.entry.placed_terminal;
    return std.fmt.bufPrint(out, "Window {d} · Tab {d} · {s}", .{ placed.window + 1, placed.tab + 1, location }) catch "Open terminal";
}

fn locationDetail(host: []const u8, cwd: []const u8, out: []u8) []const u8 {
    if (cwd.len == 0) return displayText(host, out);
    var directory: [max_detail_bytes]u8 = undefined;
    const bounded = displayText(cwd, &directory);
    return std.fmt.bufPrint(out, "{s} · {s}", .{ host, bounded }) catch "Terminal location";
}

fn encodeEntry(model: *const Model, row: *const Row, out: []u8, start: usize) Error!usize {
    var full: [1024]u8 = undefined;
    var bounded: [max_label_bytes]u8 = undefined;
    const text = if (row.is_host) hostLabel(entryHost(&row.entry).?) else entryLabel(model, row.entry, &full);
    const label = displayText(text, &bounded);
    var target_buffer: [targets.max_len]u8 = undefined;
    const bytes = try rowTarget(model, row, &target_buffer);
    if (start + 5 + label.len + bytes.len > out.len) return error.BufferTooSmall;
    std.mem.writeInt(u16, out[start..][0..2], row.index, .little);
    out[start + 2] = @intCast(label.len);
    std.mem.writeInt(u16, out[start + 3 ..][0..2], @intCast(bytes.len), .little);
    @memcpy(out[start + 5 ..][0..bytes.len], bytes);
    @memcpy(out[start + 5 + bytes.len ..][0..label.len], label);
    return start + 5 + bytes.len + label.len;
}

/// Terminal and session rows capture provider-qualified catalog authority.
/// Host rows carry only their raw host as a filter token.
fn rowTarget(model: *const Model, row: *const Row, out: *[targets.max_len]u8) Error![]const u8 {
    if (row.is_host) return encodeHostFilter(entryHost(&row.entry).?, out);
    const target = targets.capture(model, row.entry) orelse return error.UnavailableContext;
    return target.encode(out);
}

fn encodeHostFilter(host: []const u8, out: *[targets.max_len]u8) []const u8 {
    out[0] = host_filter_tag;
    out[1] = @intCast(host.len);
    @memcpy(out[2..][0..host.len], host);
    return out[0 .. 2 + host.len];
}

fn encodeMetadata(model: *const Model, row: *const Row, out: []u8, start: usize) Error!usize {
    var full: [1024]u8 = undefined;
    var bounded: [max_detail_bytes]u8 = undefined;
    const detail = displayText(rowDetail(model, row, &full), &bounded);
    const end = start + 3 + detail.len;
    if (end > out.len) return error.BufferTooSmall;
    out[start] = rowKind(row);
    out[start + 1] = @intFromBool(selectable(model, row));
    out[start + 2] = @intCast(detail.len);
    @memcpy(out[start + 3 ..][0..detail.len], detail);
    return end;
}

const Request = struct { query: []const u8, scope: Scope = .all, host: []const u8 = "", offset: u16 };

/// Request: version=1, kind=3, revision:u64, offset:u16, query_len:u8, query
/// UTF-8 (<=64). Kind 4 appends scope:u8, host_len:u8, raw host. Both replies
/// echo the complete request, then total:u16, count:u8, records (index:u16,
/// label_len:u8, target_len:u16, target bytes, label), then 0x4e and count
/// metadata records (kind:u8, selectable:u8, detail_len:u8, detail). Only a
/// catalog target authorizes activation; a host row's target is a filter token.
fn validateRequest(revision: u64, request: []const u8) Error!Request {
    if (request.len < 13 or request[0] != 1) return error.InvalidRequest;
    if (request[1] != 3 and request[1] != 4) return error.InvalidRequest;
    const query_len = request[12];
    const end = 13 + @as(usize, query_len);
    if (query_len > model_module.max_palette_query_bytes or request.len < end) return error.InvalidRequest;
    if (std.mem.readInt(u64, request[2..10], .little) != revision) return error.StaleRevision;
    var result: Request = .{ .query = request[13..end], .offset = std.mem.readInt(u16, request[10..12], .little) };
    try readScope(request, end, &result);
    return result;
}

fn readScope(bytes: []const u8, at: usize, request: *Request) Error!void {
    if (bytes[1] == 3) {
        if (bytes.len != at) return error.InvalidRequest;
        return;
    }
    if (bytes.len < at + 2) return error.InvalidRequest;
    request.scope = std.enums.fromInt(Scope, bytes[at]) orelse return error.InvalidRequest;
    if (bytes.len != at + 2 + @as(usize, bytes[at + 1])) return error.InvalidRequest;
    request.host = bytes[at + 2 ..];
    if (request.scope != .exact_host and request.host.len != 0) return error.InvalidRequest;
}

fn firstHostOccurrence(model: *const Model, host: []const u8, index: usize) bool {
    var iterator = projection.PaletteIterator.init(model, &empty_workspace);
    for (0..index) |_| {
        const previous = iterator.next() orelse return true;
        const other = entryHost(&previous) orelse continue;
        if (std.mem.eql(u8, host, other)) return false;
    }
    return true;
}

fn matches(model: *const Model, entry: *const Entry, index: usize, request: Request) bool {
    switch (request.scope) {
        .all => {},
        .sessions => if (entry.* != .session) return false,
        .known_hosts => {
            const host = entryHost(entry) orelse return false;
            if (!firstHostOccurrence(model, host, index)) return false;
            return projection.containsIgnoreCase(hostLabel(host), request.query);
        },
        .exact_host => {
            const host = entryHost(entry) orelse return false;
            if (!std.mem.eql(u8, host, request.host)) return false;
        },
    }
    return projection.paletteDestinationMatches(model, entry.*, request.query);
}

const Page = struct { rows: [page_size]Row = undefined, count: usize = 0, total: u16 = 0 };

fn collectPage(model: *const Model, request: Request) Error!Page {
    var result: Page = .{};
    var iterator = projection.PaletteIterator.init(model, &empty_workspace);
    var index: usize = 0;
    while (iterator.next()) |entry| : (index += 1) {
        if (index > std.math.maxInt(u16)) return error.CatalogTooLarge;
        if (!matches(model, &entry, index, request)) continue;
        if (result.total == std.math.maxInt(u16)) return error.CatalogTooLarge;
        if (result.total >= request.offset and result.count < page_size) {
            result.rows[result.count] = .{ .entry = entry, .index = @intCast(index), .is_host = request.scope == .known_hosts };
            result.count += 1;
        }
        result.total += 1;
    }
    if (request.offset > result.total) return error.InvalidRequest;
    return result;
}

pub fn encode(model: *const Model, revision: u64, request: []const u8, out: []u8) Error![]const u8 {
    const parsed = try validateRequest(revision, request);
    const page = try collectPage(model, parsed);
    if (out.len < request.len + 3) return error.BufferTooSmall;
    @memcpy(out[0..request.len], request);
    std.mem.writeInt(u16, out[request.len..][0..2], page.total, .little);
    out[request.len + 2] = @intCast(page.count);
    var written = request.len + 3;
    for (page.rows[0..page.count]) |*row| written = try encodeEntry(model, row, out, written);
    if (written == out.len) return error.BufferTooSmall;
    out[written] = 0x4e;
    written += 1;
    for (page.rows[0..page.count]) |*row| written = try encodeMetadata(model, row, out, written);
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
        try std.testing.expectEqual(@as(u8, 0x4e), response[at]);
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
    const sessions = try collectPage(engine.model, .{ .scope = .sessions, .query = "", .offset = 252 });
    try std.testing.expectEqual(@as(u16, 256), sessions.total);
    try std.testing.expectEqual(@as(usize, 4), sessions.count);
    try std.testing.expectEqual(@as(u16, 257), sessions.rows[3].index);
    const filtered = try collectPage(engine.model, .{ .scope = .sessions, .query = "session-255", .offset = 0 });
    try std.testing.expectEqual(@as(u16, 1), filtered.total);
    try std.testing.expectEqual(@as(u16, 257), filtered.rows[0].index);
}

fn testRemoteRef(id: u32, host: []const u8) !model_module.TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, id, host) } };
}

test "navigation exact hosts deduplicate known terminals and omit ephemeral local PTYs" {
    const engine_module = @import("ts_engine.zig");
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const hosts = [_][]const u8{ "worker", "worker", "worker-extra", "", "Worker", "h" ** 255 };
    for (hosts, 0..) |host, index| engine.model.remote_inventory[index] = try testRemoteRef(@intCast(index + 1), host);
    engine.model.remote_inventory_count = hosts.len;
    const known = try collectPage(engine.model, .{ .scope = .known_hosts, .query = "", .offset = 0 });
    try std.testing.expectEqual(@as(u16, 5), known.total);
    try std.testing.expectEqual(@as(usize, 4), known.count);
    try std.testing.expectEqual(@as(u16, 1), known.rows[0].index);
    try std.testing.expectEqualStrings("", entryHost(&known.rows[2].entry).?);
    const last = try collectPage(engine.model, .{ .scope = .known_hosts, .query = "", .offset = 4 });
    try std.testing.expectEqualStrings("h" ** 255, entryHost(&last.rows[0].entry).?);
    const exact = try collectPage(engine.model, .{ .scope = .exact_host, .host = "worker", .query = "", .offset = 0 });
    try std.testing.expectEqual(@as(u16, 2), exact.total);
    try std.testing.expect(!selectable(engine.model, &exact.rows[0]));
    const coordinator = try collectPage(engine.model, .{ .scope = .exact_host, .host = "", .query = "", .offset = 0 });
    try std.testing.expectEqual(@as(u16, 1), coordinator.total);
    try std.testing.expect(!selectable(engine.model, &coordinator.rows[0]));
    const independent_query = try collectPage(engine.model, .{ .scope = .exact_host, .host = "worker", .query = "worker-extra", .offset = 0 });
    try std.testing.expectEqual(@as(u16, 0), independent_query.total);
    const placed: Row = .{ .entry = .{ .placed_terminal = .{ .window = 0, .tab = 0, .terminal_ref = engine.model.remote_inventory[0] } }, .index = 0, .is_host = false };
    try std.testing.expect(selectable(engine.model, &placed));
}

test "navigation searches host catalog title and directory without a presentation" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine_module = @import("ts_engine.zig");
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const remote = try model_module.PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/navigation-unused" }, null, "navigation");
    engine.model.phux_provider = remote;
    const ref = try testRemoteRef(900, "build-host");
    engine.model.remote_inventory[0] = ref;
    engine.model.remote_inventory_count = 1;
    const workspace = @import("provider_contract").workspace;
    remote.host.workspace_store.catalog = try std.testing.allocator.alloc(workspace.CatalogTerminal, 1);
    remote.host.workspace_store.catalog[0] = .{ .terminal_ref = ref, .session_id = 0, .title = try workspace.Text.init("Compile cockpit"), .cwd = try workspace.Text.init("/work/vertical-slice") };
    try std.testing.expect(engine.model.remotePresentation(ref) == null);
    for ([_][]const u8{ "BUILD-HOST", "compile cockpit", "/work/vertical" }) |query| {
        const page = try collectPage(engine.model, .{ .query = query, .offset = 0 });
        try std.testing.expectEqual(@as(u16, 1), page.total);
        try std.testing.expectEqual(@as(u16, 1), page.rows[0].index);
        try std.testing.expect(!selectable(engine.model, &page.rows[0]));
    }
    remote.host.workspace_store.catalog[0].session_id = 42;
    const owned = try collectPage(engine.model, .{ .query = "compile", .scope = .exact_host, .host = "build-host", .offset = 0 });
    try std.testing.expect(selectable(engine.model, &owned.rows[0]));
    var label: [1024]u8 = undefined;
    try std.testing.expectEqualStrings("Compile cockpit", entryLabel(engine.model, owned.rows[0].entry, &label));
    try std.testing.expectEqualStrings("build-host · /work/vertical-slice", rowDetail(engine.model, &owned.rows[0], &label));
}

test "navigation scoped requests preserve every raw host byte and reject malformed framing" {
    const engine_module = @import("ts_engine.zig");
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    engine.model.remote_inventory[0] = try testRemoteRef(99, "h" ** 255);
    engine.model.remote_inventory_count = 1;
    var buffer: [max_bytes]u8 = undefined;
    // A known-host row carries its complete raw host as a filter token, never
    // as catalog authority, so it needs no provider context to encode.
    var hosts = [_]u8{0} ** 15;
    hosts[0] = 1;
    hosts[1] = 4;
    hosts[2] = 7;
    hosts[13] = 2;
    const listed = try encode(engine.model, 7, &hosts, &buffer);
    try std.testing.expectEqual(@as(u16, 1), std.mem.readInt(u16, listed[15..17], .little));
    const token_len = std.mem.readInt(u16, listed[21..23], .little);
    try std.testing.expectEqual(@as(u16, 2 + 255), token_len);
    try std.testing.expectEqual(host_filter_tag, listed[23]);
    try std.testing.expectEqual(@as(u8, 255), listed[24]);
    try std.testing.expectEqualStrings("h" ** 255, listed[25..][0..255]);
    try std.testing.expect(targets.decode(listed[23..][0..token_len]) == null);
    var request = [_]u8{0} ** 270;
    request[0] = 1;
    request[1] = 4;
    request[2] = 7;
    request[13] = 3;
    request[14] = 255;
    @memcpy(request[15..], "h" ** 255);
    try std.testing.expectError(error.InvalidRequest, encode(engine.model, 7, request[0..269], &buffer));
    try std.testing.expectError(error.StaleRevision, encode(engine.model, 8, &request, &buffer));
    request[13] = 4;
    try std.testing.expectError(error.InvalidRequest, encode(engine.model, 7, &request, &buffer));
    request[13] = 2;
    try std.testing.expectError(error.InvalidRequest, encode(engine.model, 7, &request, &buffer));
}
