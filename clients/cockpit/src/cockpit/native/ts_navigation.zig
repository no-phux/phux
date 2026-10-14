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
/// 0 every open tab and session, 1 sessions only, 5 one captured machine's
/// sessions. 2, 3, 6 and 7 are retired and never reused.
pub const Scope = enum(u8) { all = 0, sessions = 1, captured_machine = 5 };
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
        .peer_session => |target| peerSessionLabel(model, target, out),
        .peer_unavailable => |coordinator| projection.peerHostLabel(model, coordinator),
    };
}

fn sessionLabel(model: *const Model, id: u32, out: []u8) []const u8 {
    const remote = model.phuxConst() orelse return "Session";
    for (remote.sessionCatalog()) |session| {
        if (session.id != id) continue;
        if (session.name.len > 0) return session.name;
        break;
    }
    _ = out;
    return "Untitled session";
}

const Row = struct { entry: Entry, index: u16 };

fn terminalRef(entry: *const Entry) ?*const model_module.TerminalRef {
    return switch (entry.*) {
        .placed_terminal => &entry.placed_terminal.terminal_ref,
        .available_terminal => &entry.available_terminal,
        .session, .peer_session, .peer_unavailable => null,
    };
}

/// Borrow only from the caller's stable entry, never from a copied union payload.
fn entryHost(entry: *const Entry) ?[]const u8 {
    const ref = terminalRef(entry) orelse return null;
    if (support.providerKind(ref.*) == .local) return null;
    return ref.terminal_id.phux.host();
}

fn selectable(model: *const Model, row: *const Row) bool {
    if (row.entry == .peer_session) return peerSessionListed(model, row.entry.peer_session);
    if (row.entry == .peer_unavailable) return peerRetryable(model, row.entry.peer_unavailable);
    if (row.entry != .available_terminal) return true;
    if (entryHost(&row.entry) == null) return true;
    const remote = model.phuxForRefConst(row.entry.available_terminal) orelse return false;
    return remote.terminalSession(row.entry.available_terminal) != null;
}

fn rowKind(row: *const Row) u8 {
    return switch (row.entry) {
        .placed_terminal => 0,
        .available_terminal => 1,
        .session, .peer_session, .peer_unavailable => 2,
    };
}

/// The coordinator's own rows (empty host) name the registered remote host
/// when the provider dials one, else This Mac; satellite hosts keep their
/// own names.
fn coordinatorLabel(model: *const Model, host: []const u8) []const u8 {
    if (host.len != 0) return host;
    return remoteHost(model) orelse "This Mac";
}

/// A session row names only the machine it runs on, and says Empty when the
/// persistent session has no members (ADR-0160).
fn sessionDetail(model: *const Model, id: u32, out: []u8) []const u8 {
    const host = remoteHost(model) orelse "This Mac";
    if (!activeSessionEmpty(model, id)) return host;
    return std.fmt.bufPrint(out, "Empty · {s}", .{host}) catch host;
}

/// The Phux session the focused window is showing: the selected tab's session
/// on the coordinator that minted its terminal, or the window's picked empty
/// session. Null for a local PTY, web content, or an unknown session.
pub const ShownSession = struct { coordinator: support.ProviderId, id: u32 };

pub fn shownSession(model: *const Model) ?ShownSession {
    if (comptime !support.phux_enabled) return null;
    const window = model.active_window;
    const ws = model.wsAtConst(window) orelse return null;
    if (ws.tab_count == 0) {
        const pick = model.empty_pick orelse return null;
        if (pick.window != window) return null;
        return .{ .coordinator = pick.coordinator, .id = pick.session };
    }
    const ref = ws.focusedTerminalRef() orelse return null;
    const remote = model.phuxForRefConst(ref) orelse return null;
    const id = remote.terminalSession(ref) orelse return null;
    return .{ .coordinator = remote.providerId(), .id = id };
}

/// Whether a session row names the session the focused window is showing.
fn currentSession(model: *const Model, row: *const Row) bool {
    const shown = shownSession(model) orelse return false;
    return switch (row.entry) {
        .session => |id| id == shown.id and activeCoordinator(model, shown.coordinator),
        .peer_session => |target| target.id == shown.id and target.coordinator == shown.coordinator,
        else => false,
    };
}

fn activeCoordinator(model: *const Model, coordinator: support.ProviderId) bool {
    if (comptime !support.phux_enabled) return false;
    const remote = model.phuxConst() orelse return false;
    return remote.providerId() == coordinator;
}

fn activeSessionEmpty(model: *const Model, id: u32) bool {
    const remote = model.phuxConst() orelse return false;
    for (remote.sessionCatalog()) |session| if (session.id == id) return session.empty;
    return false;
}

fn peerSessionDetail(model: *const Model, target: model_module.PeerSession, out: []u8) []const u8 {
    const host = projection.peerHostLabel(model, target.coordinator);
    const empty = @import("empty_session.zig").peerSessionEmpty(model, target.coordinator, target.id);
    const prefix = if (empty) "Empty · " else "";
    // A showing peer whose workspace could not be projected says so here,
    // since the chrome's connection state is the active coordinator's.
    if (peerProjectionRefused(model, target.coordinator))
        return std.fmt.bufPrint(out, "{s}{s} · not syncing", .{ prefix, host }) catch host;
    return std.fmt.bufPrint(out, "{s}{s}", .{ prefix, host }) catch host;
}

fn peerProjectionRefused(model: *const Model, coordinator: support.ProviderId) bool {
    const slot = model.peerSlot(coordinator) orelse return false;
    return model.peers.items[slot].workspace.refused;
}

/// A failed peer's row retries it; a connecting one has nothing to do.
fn peerRetryable(model: *const Model, coordinator: support.ProviderId) bool {
    const slot = model.peerSlot(coordinator) orelse return false;
    return model.peers.items[slot].failed;
}

/// A peer that cannot list says why: the recorded failure (a remote host's
/// dial or connection reason), else that its connection was lost, else that
/// it is still connecting.
fn peerUnavailableDetail(model: *const Model, coordinator: support.ProviderId, out: []u8) []const u8 {
    if (comptime !support.phux_enabled) return "Unavailable";
    const slot = model.peerSlot(coordinator) orelse return "Unavailable";
    const peer = model.phuxPeerAtConst(slot).?;
    if (!model.peers.items[slot].failed) return "Connecting…";
    var reason_buffer: [max_detail_bytes]u8 = undefined;
    const recorded = peer.remoteFailure(&reason_buffer);
    const reason = if (recorded.len != 0) recorded else "the connection was lost";
    return std.fmt.bufPrint(out, "Unavailable · {s}", .{reason}) catch "Unavailable";
}

/// A peer session is shown by id, so any listed one is selectable.
fn peerSessionListed(model: *const Model, target: model_module.PeerSession) bool {
    const slot = model.peerSlot(target.coordinator) orelse return false;
    const peer = model.phuxPeerAtConst(slot).?;
    for (peer.standbyCatalog()) |session| {
        if (session.id == target.id) return true;
    }
    return false;
}

fn peerSessionLabel(model: *const Model, target: model_module.PeerSession, out: []u8) []const u8 {
    const slot = model.peerSlot(target.coordinator) orelse return "Session";
    const peer = model.phuxPeerAtConst(slot).?;
    for (peer.standbyCatalog()) |session| {
        if (session.id != target.id) continue;
        if (session.name.len > 0) return session.name;
        break;
    }
    _ = out;
    return "Untitled session";
}

/// A terminal's coordinator label: a satellite by its own name, else the
/// coordinator that minted the ref, so two coordinators' terminals never
/// read as one host.
fn coordinatorLabelFor(model: *const Model, ref: model_module.TerminalRef, host: []const u8) []const u8 {
    if (host.len != 0) return host;
    if (comptime support.phux_enabled) {
        if (model.phuxConst()) |active| {
            if (active.providerId() != ref.provider_id) return projection.peerHostLabel(model, ref.provider_id);
        }
    }
    return coordinatorLabel(model, host);
}

fn terminalDirectory(model: *const Model, ref: model_module.TerminalRef) []const u8 {
    if (model.provider.terminalConst(ref)) |pane| return pane.pwd();
    const remote = model.phuxForRefConst(ref) orelse return "";
    for (remote.catalogTerminals()) |*entry| {
        if (entry.terminal_ref.eql(ref)) return entry.cwd.slice();
    }
    return "";
}

/// A tab row says where it runs and in which directory; never which window
/// or tab number holds it.
fn rowDetail(model: *const Model, row: *const Row, out: []u8) []const u8 {
    if (row.entry == .peer_unavailable) return peerUnavailableDetail(model, row.entry.peer_unavailable, out);
    if (!selectable(model, row)) return "Unavailable";
    if (row.entry == .peer_session) return peerSessionDetail(model, row.entry.peer_session, out);
    const ref = terminalRef(&row.entry) orelse return sessionDetail(model, row.entry.session, out);
    const host = if (entryHost(&row.entry)) |value| coordinatorLabelFor(model, ref.*, value) else "This Mac";
    return locationDetail(host, terminalDirectory(model, ref.*), out);
}

fn locationDetail(host: []const u8, cwd: []const u8, out: []u8) []const u8 {
    if (cwd.len == 0) return displayText(host, out);
    var directory: [max_detail_bytes]u8 = undefined;
    const bounded = displayText(cwd, &directory);
    return std.fmt.bufPrint(out, "{s} · {s}", .{ host, bounded }) catch "Terminal location";
}

/// The registered host the Phux catalog belongs to, or null for the local
/// coordinator. A remote host's rows name it where a local coordinator's
/// read This Mac, so its terminals and sessions read as one group.
fn remoteHost(model: *const Model) ?[]const u8 {
    if (comptime !support.phux_enabled) return null;
    const remote = model.phuxConst() orelse return null;
    return remote.remoteLabel();
}

fn encodeEntry(model: *const Model, row: *const Row, out: []u8, start: usize) Error!usize {
    var full: [1024]u8 = undefined;
    var bounded: [max_label_bytes]u8 = undefined;
    const text = entryLabel(model, row.entry, &full);
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

/// Every row captures provider-qualified catalog authority.
fn rowTarget(model: *const Model, row: *const Row, out: *[targets.max_len]u8) Error![]const u8 {
    const target = targets.capture(model, row.entry) orelse return error.UnavailableContext;
    return target.encode(out);
}

fn encodeMetadata(model: *const Model, row: *const Row, out: []u8, start: usize) Error!usize {
    var full: [1024]u8 = undefined;
    var bounded: [max_detail_bytes]u8 = undefined;
    const detail = displayText(rowDetail(model, row, &full), &bounded);
    const end = start + 3 + detail.len;
    if (end > out.len) return error.BufferTooSmall;
    out[start] = rowKind(row);
    // Bit 1 marks the session the focused window is showing ("Current").
    out[start + 1] = @as(u8, @intFromBool(selectable(model, row))) | (@as(u8, @intFromBool(currentSession(model, row))) << 1);
    out[start + 2] = @intCast(detail.len);
    @memcpy(out[start + 3 ..][0..detail.len], detail);
    return end;
}

const Request = struct { query: []const u8, scope: Scope = .all, host: []const u8 = "", offset: u16, attachments: []const u64 = &.{} };

/// Request: version=1, kind=3, revision:u64, offset:u16, query_len:u8, query
/// UTF-8 (<=64). Kind 4 appends scope:u8, host_len:u8, raw host. Both replies
/// echo the complete request, then total:u16, count:u8, records (index:u16,
/// label_len:u8, target_len:u16, target bytes, label), then 0x4e and count
/// metadata records (kind:u8, selectable:u8, detail_len:u8, detail). Only a
/// catalog target authorizes activation.
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
    if (request.scope == .captured_machine) {
        if (request.host.len != 12) return error.InvalidRequest;
    } else if (request.host.len != 0) return error.InvalidRequest;
}

fn matches(model: *const Model, entry: *const Entry, request: Request) bool {
    if (!scopeIncludes(model, entry.*, request)) return false;
    return projection.paletteDestinationMatches(model, entry.*, request.query);
}

fn scopeIncludes(model: *const Model, entry: Entry, request: Request) bool {
    switch (request.scope) {
        .all => return true,
        // Every host group: the active coordinator's and each peer's.
        .sessions => return isSessionEntry(entry),
        .captured_machine => return capturedSessionMatches(model, entry, request.attachments),
    }
}

fn isSessionEntry(entry: Entry) bool {
    return switch (entry) {
        .session, .peer_session, .peer_unavailable => true,
        else => false,
    };
}

fn capturedSessionMatches(model: *const Model, entry: Entry, attachments: []const u64) bool {
    if (entry != .session and entry != .peer_session) return false;
    const target = targets.capture(model, entry) orelse return false;
    return std.mem.indexOfScalar(u64, attachments, target.context.provider) != null;
}

const Page = struct { rows: [page_size]Row = undefined, count: usize = 0, total: u16 = 0 };

fn collectPage(model: *const Model, request: Request) Error!Page {
    var result: Page = .{};
    var iterator = projection.PaletteIterator.init(model, &empty_workspace);
    var index: usize = 0;
    while (iterator.next()) |entry| : (index += 1) {
        if (index > std.math.maxInt(u16)) return error.CatalogTooLarge;
        if (!matches(model, &entry, request)) continue;
        if (result.total == std.math.maxInt(u16)) return error.CatalogTooLarge;
        if (result.total >= request.offset and result.count < page_size) {
            result.rows[result.count] = .{ .entry = entry, .index = @intCast(index) };
            result.count += 1;
        }
        result.total += 1;
    }
    if (request.offset > result.total) return error.InvalidRequest;
    return result;
}

pub fn encode(model: *const Model, revision: u64, request: []const u8, out: []u8) Error![]const u8 {
    const parsed = try validateRequest(revision, request);
    if (parsed.scope == .captured_machine) return error.UnavailableContext;
    return encodePage(model, request, parsed, out);
}

/// The composition root validates the opaque Browse token and its complete
/// captured Target list before supplying these exact attachment identities.
pub fn encodeForAttachments(model: *const Model, revision: u64, request: []const u8, out: []u8, attachment_ids: []const u64) Error![]const u8 {
    var parsed = try validateRequest(revision, request);
    if (parsed.scope != .captured_machine) return error.InvalidRequest;
    if (attachment_ids.len == 0) return error.UnavailableContext;
    for (attachment_ids) |id| {
        if (model.phuxForAttachmentConst(id) == null) return error.UnavailableContext;
    }
    parsed.attachments = attachment_ids;
    return encodePage(model, request, parsed, out);
}

fn encodePage(model: *const Model, request: []const u8, parsed: Request, out: []u8) Error![]const u8 {
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
    const owned = try collectPage(engine.model, .{ .query = "compile", .offset = 0 });
    try std.testing.expect(selectable(engine.model, &owned.rows[0]));
    var label: [1024]u8 = undefined;
    try std.testing.expectEqualStrings("Compile cockpit", entryLabel(engine.model, owned.rows[0].entry, &label));
    try std.testing.expectEqualStrings("build-host · /work/vertical-slice", rowDetail(engine.model, &owned.rows[0], &label));
}

test "session rows name their machine and mark the session the focused window shows" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine_module = @import("ts_engine.zig");
    const engine = try engine_module.Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const remote = try model_module.PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/navigation-current-unused" }, null, "navigation-current");
    engine.model.phux_provider = remote;
    for ([_]u32{ 41, 42 }) |id| {
        try remote.host.sessions.append(std.testing.allocator, .{
            .id = id,
            .name = try std.fmt.allocPrint(std.testing.allocator, "work-{d}", .{id}),
            .created_at_unix_secs = 0,
            .window_count = 1,
            .attached_client_count = 0,
            .focused = false,
        });
    }
    var detail: [1024]u8 = undefined;
    const sessions = try collectPage(engine.model, .{ .scope = .sessions, .query = "", .offset = 0 });
    try std.testing.expectEqual(@as(usize, 2), sessions.count);
    // Assert absent first: the primary window shows a local PTY, no session.
    try std.testing.expect(shownSession(engine.model) == null);
    for (sessions.rows[0..sessions.count]) |*row| try std.testing.expect(!currentSession(engine.model, row));
    try std.testing.expectEqualStrings("This Mac", rowDetail(engine.model, &sessions.rows[1], &detail));

    // Show session 42's terminal in the primary window's selected tab.
    const ref = try testRemoteRef(900, "");
    const workspace = @import("provider_contract").workspace;
    remote.host.workspace_store.catalog = try std.testing.allocator.alloc(workspace.CatalogTerminal, 1);
    remote.host.workspace_store.catalog[0] = .{ .terminal_ref = ref, .session_id = 42, .title = try workspace.Text.init("shell"), .cwd = try workspace.Text.init("/work") };
    const ws = engine.model.wsAt(0).?;
    const saved = ws.tabs[ws.selected_tab];
    defer ws.tabs[ws.selected_tab] = saved;
    ws.tabs[ws.selected_tab] = @import("../layout.zig").Tree.initLeaf(ref);
    try std.testing.expectEqual(@as(u32, 42), (shownSession(engine.model) orelse return error.NoShownSession).id);
    var marked: usize = 0;
    for (sessions.rows[0..sessions.count]) |*row| {
        const current = currentSession(engine.model, row);
        try std.testing.expectEqual(row.entry.session == 42, current);
        marked += @intFromBool(current);
    }
    try std.testing.expectEqual(@as(usize, 1), marked);
}
