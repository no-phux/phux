//! Per-window header context: which machine and session each open window
//! shows, and that window's own connection and Empty session state.
//!
//! Kind 4 (`empty_session`) and kind 3 (`navigation_context`) describe one
//! window: the first empty one, and the primary coordinator. With independent
//! same-machine sessions and several machines across windows, every header
//! rendered from them showed another window's labels. This record carries one
//! entry per OPEN window, primary included, resolved from that window's exact
//! source. It is display metadata only: no epoch or execution authority rides
//! here (the snapshot's sequence/revision owns freshness, and action targets
//! stay separate opaque bytes).
//!
//! Payload, after the shared `kind:u8, len:u16` extension framing:
//!   version:u8 = 1, count:u8, then `count` records of
//!   window:u8, flags:u8, connection:u8,
//!   session_len:u8, session UTF-8 (<= 64), host_len:u8, host UTF-8 (<= 64).
//! flags: bit0 empty, bit1 picked, bit2 opening, bit3 unavailable.
//! connection: `ts_navigation.Connection` values.
const std = @import("std");
const model_module = @import("../model.zig");
const support = @import("../phux_support.zig");
const shared_workspace = @import("../shared_workspace.zig");
const navigation = @import("ts_navigation.zig");
const empty_session = @import("empty_session.zig");
const Model = model_module.Model;
const Remote = support.PhuxProvider;

pub const version: u8 = 1;
pub const max_session_bytes: usize = 64;
pub const max_host_bytes: usize = 64;
const entry_bytes: usize = 5 + max_session_bytes + max_host_bytes;
/// Framing, version and count, then every window open at once.
pub const record_bytes: usize = 3 + 2 + model_module.max_windows * entry_bytes;

pub const Flags = packed struct(u8) {
    empty: bool = false,
    picked: bool = false,
    opening: bool = false,
    unavailable: bool = false,
    reserved: u4 = 0,
};

pub const Context = struct {
    flags: Flags = .{},
    connection: navigation.Connection = .local,
    session: []const u8,
    host: []const u8,
};

const scratch_session = "Local terminals";
const this_mac = "This Mac";

/// The header context window `window` shows, when it is open.
pub fn context(model: *const Model, window: usize, scratch: []u8) ?Context {
    if (!model.windowOpen(window)) return null;
    var value = sourceContext(model, window, scratch);
    const shown = empty_session.view(model, window) orelse return value;
    // The Empty session state names the session the window will open into,
    // which can differ from the attachment's selection until New Tab lands.
    value.session = shown.name;
    value.host = shown.host;
    value.flags = .{ .empty = true, .picked = shown.picked, .opening = shown.opening, .unavailable = shown.unavailable };
    return value;
}

fn sourceContext(model: *const Model, window: usize, scratch: []u8) Context {
    if (showsScratch(model, window)) return .{ .session = scratch_session, .host = this_mac };
    const remote = windowSource(model, window) orelse return .{ .session = scratch_session, .host = this_mac };
    return .{
        .connection = connection(model, remote),
        .session = sessionName(remote, scratch),
        .host = remote.remoteLabel() orelse this_mac,
    };
}

/// A selected local-PTY tab is Cockpit's ephemeral scratch terminal, even in
/// a window whose default provider would otherwise be the local coordinator.
fn showsScratch(model: *const Model, window: usize) bool {
    const workspace = model.wsAtConst(window) orelse return false;
    const tree = workspace.treeConst(workspace.selected_tab) orelse return false;
    return shared_workspace.tabAuthority(tree) == .local;
}

fn windowSource(model: *const Model, window: usize) ?*const Remote {
    if (comptime !support.phux_enabled) return null;
    return model.phuxForWindowConst(window);
}

/// `ts_navigation.connection`, for one exact source. The model-wide reconnect
/// and unavailability flags describe the primary coordinator only.
fn connection(model: *const Model, remote: *const Remote) navigation.Connection {
    // The disabled provider has no connection states; never analyze them there.
    if (comptime !support.phux_enabled) return .local;
    const primary = model.phuxConst() == remote;
    if (primary and model.phux_reconnect_after_close) return .connecting;
    if (primary and model.phux_connection_unavailable) return .offline;
    return switch (remote.state()) {
        .new, .hello_queued, .negotiated => .connecting,
        .attached => if (workspaceRefused(model, remote)) .workspace_unavailable else .connected,
        .detached, .failed => .offline,
    };
}

fn workspaceRefused(model: *const Model, remote: *const Remote) bool {
    const state = sourceWorkspace(model, remote) orelse return false;
    return state.refused or state.subscription_refused;
}

fn sourceWorkspace(model: *const Model, remote: *const Remote) ?*const shared_workspace.State {
    if (model.phuxConst() == remote) return &model.shared_workspace;
    const slot = model.peerSlotForAttachment(remote.context_id) orelse return null;
    return &model.peers.items[slot].workspace;
}

fn sessionName(remote: *const Remote, scratch: []u8) []const u8 {
    const id = remote.selectedSessionId() orelse return "";
    for (remote.sessionCatalog()) |session| {
        if (session.id == id and session.name.len > 0) return session.name;
    }
    return std.fmt.bufPrint(scratch, "Session #{d}", .{id}) catch "Session";
}

/// Append the record after `start`. Always written, so a decoder can tell a
/// current snapshot with no open window apart from an old one without kind 5.
pub fn encode(model: *const Model, kind: u8, out: []u8, start: usize) error{BufferTooSmall}!usize {
    if (start + 5 > out.len) return error.BufferTooSmall;
    var at = start + 5;
    var count: u8 = 0;
    for (0..model_module.max_windows) |window| {
        var scratch: [32]u8 = undefined;
        const value = context(model, window, &scratch) orelse continue;
        at = try encodeEntry(@intCast(window), value, out, at);
        count += 1;
    }
    out[start] = kind;
    std.mem.writeInt(u16, out[start + 1 ..][0..2], @intCast(at - start - 3), .little);
    out[start + 3] = version;
    out[start + 4] = count;
    return at;
}

fn encodeEntry(window: u8, value: Context, out: []u8, start: usize) error{BufferTooSmall}!usize {
    if (start + 3 > out.len) return error.BufferTooSmall;
    out[start] = window;
    out[start + 1] = @bitCast(value.flags);
    out[start + 2] = @intFromEnum(value.connection);
    var session: [max_session_bytes]u8 = undefined;
    var host: [max_host_bytes]u8 = undefined;
    const at = try encodeText(navigation.displayText(value.session, &session), out, start + 3);
    return encodeText(navigation.displayText(value.host, &host), out, at);
}

fn encodeText(text: []const u8, out: []u8, start: usize) error{BufferTooSmall}!usize {
    if (start + 1 + text.len > out.len) return error.BufferTooSmall;
    out[start] = @intCast(text.len);
    @memcpy(out[start + 1 ..][0..text.len], text);
    return start + 1 + text.len;
}

/// Test and fixture reader: the same strictness the TypeScript decoder applies.
pub const Decoded = struct {
    count: usize = 0,
    windows: [model_module.max_windows]u8 = undefined,
    contexts: [model_module.max_windows]Context = undefined,

    pub fn find(self: *const Decoded, window: u8) ?Context {
        for (self.windows[0..self.count], self.contexts[0..self.count]) |index, value| {
            if (index == window) return value;
        }
        return null;
    }
};

pub fn decode(payload: []const u8) error{Invalid}!Decoded {
    if (payload.len < 2 or payload[0] != version) return error.Invalid;
    var result: Decoded = .{};
    var at: usize = 2;
    for (0..payload[1]) |_| at = try decodeEntry(payload, at, &result);
    if (at != payload.len) return error.Invalid;
    return result;
}

fn decodeEntry(payload: []const u8, start: usize, result: *Decoded) error{Invalid}!usize {
    if (start + 3 > payload.len or result.count == result.windows.len) return error.Invalid;
    const window = payload[start];
    if (window >= model_module.max_windows or result.find(window) != null) return error.Invalid;
    const flags: Flags = @bitCast(payload[start + 1]);
    if (flags.reserved != 0) return error.Invalid;
    const state = std.enums.fromInt(navigation.Connection, payload[start + 2]) orelse return error.Invalid;
    const session = try decodeText(payload, start + 3, max_session_bytes);
    const host = try decodeText(payload, start + 4 + session.len, max_host_bytes);
    result.windows[result.count] = window;
    result.contexts[result.count] = .{ .flags = flags, .connection = state, .session = session, .host = host };
    result.count += 1;
    return start + 5 + session.len + host.len;
}

fn decodeText(payload: []const u8, start: usize, limit: usize) error{Invalid}![]const u8 {
    if (start >= payload.len) return error.Invalid;
    const len = payload[start];
    if (len > limit or start + 1 + len > payload.len) return error.Invalid;
    const text = payload[start + 1 ..][0..len];
    if (!std.unicode.utf8ValidateSlice(text)) return error.Invalid;
    return text;
}

fn payloadOf(record: []const u8) []const u8 {
    return record[3..][0..std.mem.readInt(u16, record[1..3], .little)];
}

test "window contexts encode every open window from its own source" {
    const Engine = @import("ts_engine.zig").Engine;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    _ = engine.model.openWindow(2) orelse return error.NoWindow;
    var out: [record_bytes]u8 = undefined;
    const end = try encode(engine.model, 5, &out, 0);
    try std.testing.expectEqual(@as(u8, 5), out[0]);
    const decoded = try decode(payloadOf(out[0..end]));
    try std.testing.expectEqual(@as(usize, 2), decoded.count);
    // A closed slot between open ones is absent, never a stale entry.
    try std.testing.expect(decoded.find(1) == null);
    for ([_]u8{ 0, 2 }) |window| {
        const value = decoded.find(window).?;
        try std.testing.expectEqual(navigation.Connection.local, value.connection);
        try std.testing.expectEqualStrings(this_mac, value.host);
    }
}

test "window contexts elide on UTF-8 boundaries and reject malformed records" {
    var out: [64 + entry_bytes]u8 = undefined;
    const long = "\xc3\xa9" ** 40; // 80 bytes of two-byte scalars
    const end = try encodeEntry(3, .{ .flags = .{ .empty = true, .opening = true }, .connection = .offline, .session = long, .host = "mini" }, &out, 2);
    out[0] = version;
    out[1] = 1;
    const decoded = try decode(out[0..end]);
    const value = decoded.find(3).?;
    try std.testing.expect(value.session.len <= max_session_bytes);
    // displayText elides with one U+2026 on a scalar boundary.
    try std.testing.expect(std.mem.endsWith(u8, value.session, "\xe2\x80\xa6"));
    try std.testing.expect(std.unicode.utf8ValidateSlice(value.session));
    try std.testing.expect(value.flags.empty and value.flags.opening and !value.flags.picked);
    try std.testing.expectEqual(navigation.Connection.offline, value.connection);

    var bad = out;
    bad[3] |= 0x10; // reserved flag bit
    try std.testing.expectError(error.Invalid, decode(bad[0..end]));
    bad = out;
    bad[4] = 9; // unknown connection
    try std.testing.expectError(error.Invalid, decode(bad[0..end]));
    bad = out;
    bad[1] = 2; // count exceeds the payload
    try std.testing.expectError(error.Invalid, decode(bad[0..end]));
    try std.testing.expectError(error.Invalid, decode(out[0 .. end - 1]));
    bad = out;
    bad[0] = 2; // unknown version
    try std.testing.expectError(error.Invalid, decode(bad[0..end]));
}

test "same-machine windows on independent sessions carry their own session labels" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const Engine = @import("ts_engine.zig").Engine;
    const fixture = Remote.test_support;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const remote = try Remote.create(std.testing.allocator, std.testing.io, .{ .unix = "/window-contexts-unused" }, null, "contexts");
    model.phux_provider = remote;
    model.primary = .{};
    remote.standBy();
    try remote.host.start("contexts");
    try fixture.stageFixture(remote.bridge, "hello.bin");
    _ = try remote.drainReadiness();
    try fixture.stageFixture(remote.bridge, "standby_state.bin");
    _ = try remote.drainReadiness();
    try remote.show(1);
    _ = try remote.drainReadiness();
    try fixture.stageFixture(remote.bridge, "attached.bin");
    _ = try remote.drainReadiness();
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_metadata.bin");
    try fixture.stageFixture(remote.bridge, "workspace_sessions_a_state.bin");
    _ = try remote.drainReadiness();
    model.shared_workspace.attachment_id = remote.context_id;
    model.shared_workspace.showInWindow(0, model.window_epochs[0]);
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    model.bindWindowAttachment(0, remote.context_id);
    _ = model.openWindow(1) orelse return error.NoWindow;
    const Fx = struct {
        restarted: ?usize = null,
        pub fn restartPeer(self: *@This(), _: *Engine, slot: usize) bool {
            self.restarted = slot;
            return true;
        }
        pub fn restartPhux(_: *@This(), _: *Engine) bool {
            return false;
        }
        pub fn openChannel(_: *@This(), _: anytype) @import("native_sdk").ChannelHandle {
            return .{};
        }
        pub fn closeChannel(_: *@This(), _: u64) void {}
        pub fn showNotification(_: *@This(), _: anytype) void {}
    };
    var fx: Fx = .{};
    try engine.showSessionFromInWindow(remote, 2, 1, model.window_epochs[1], &fx);
    const second = model.phuxPeerAt(fx.restarted.?).?;
    try std.testing.expect(model.phuxForWindowConst(1) == second);

    var scratch: [2][32]u8 = undefined;
    const first = context(model, 0, &scratch[0]).?;
    const other = context(model, 1, &scratch[1]).?;
    try std.testing.expectEqual(navigation.Connection.connected, first.connection);
    try std.testing.expectEqualStrings(sessionName(remote, &scratch[0]), first.session);
    // The second attachment has not attached yet: its own state, not the
    // primary's, decides the header, and its label is not window 0's.
    try std.testing.expectEqual(navigation.Connection.connecting, other.connection);
    try std.testing.expect(!std.mem.eql(u8, first.session, other.session));

    var out: [record_bytes]u8 = undefined;
    const end = try encode(model, 5, &out, 0);
    const decoded = try decode(payloadOf(out[0..end]));
    try std.testing.expectEqual(@as(usize, 2), decoded.count);
    try std.testing.expectEqual(navigation.Connection.connected, decoded.find(0).?.connection);
    try std.testing.expectEqual(navigation.Connection.connecting, decoded.find(1).?.connection);
}
