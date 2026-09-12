//! New Session's captured destination and correlated asynchronous result.
//! The provider hook performs the real coordinator operation with keep_empty.
const std = @import("std");
const windows = @import("ts_window_navigation.zig");

pub const request_name = "cockpit.new-session";
pub const max_text_bytes = 240;
pub const max_bytes = 16 + 2 * max_text_bytes;
pub const Error = error{ InvalidRequest, BufferTooSmall, OutOfMemory, TokenExhausted };
pub const Kind = enum(u8) { describe = 1, create = 2, status = 3, cancel = 4 };
pub const Phase = enum(u8) { ready = 0, pending = 1, created = 2, refused = 3, unavailable = 4 };
pub const Outcome = union(enum) { pending, created: u32, refused: []const u8, unknown: []const u8 };

/// `host` is borrowed only during capture; Controller owns its display copy.
/// Identity is the exact provider lifetime plus host and connection epochs.
pub const Destination = struct {
    window: u8,
    window_epoch: u64,
    provider: u64,
    provider_context: u64,
    host_context: u64,
    connection_epoch: u64,
    selection_epoch: u64,
    host: []const u8 = "",
};

pub const Request = struct { kind: Kind, token: u64, name: []const u8 };

pub fn decode(bytes: []const u8) Error!Request {
    if (bytes.len < 11 or bytes[0] != 1) return error.InvalidRequest;
    const kind = std.enums.fromInt(Kind, bytes[1]) orelse return error.InvalidRequest;
    if (bytes.len != 11 + @as(usize, bytes[10])) return error.InvalidRequest;
    const token = std.mem.readInt(u64, bytes[2..10], .little);
    const name = bytes[11..];
    if ((kind == .describe) != (token == 0)) return error.InvalidRequest;
    if (kind == .create) {
        if (!validName(name)) return error.InvalidRequest;
    } else if (name.len != 0) return error.InvalidRequest;
    return .{ .kind = kind, .token = token, .name = name };
}

fn validName(name: []const u8) bool {
    if (name.len == 0 or name.len > max_text_bytes) return false;
    if (!std.unicode.utf8ValidateSlice(name)) return false;
    for (name) |byte| if (byte < 0x20 or byte == 0x7f) return false;
    return true;
}

pub const Reply = struct { phase: Phase, token: u64 = 0, session_id: u32 = 0, host: []const u8 = "", reason: []const u8 = "" };

pub fn encode(reply: Reply, out: []u8) Error![]const u8 {
    var host_buffer: [max_text_bytes]u8 = undefined;
    var reason_buffer: [max_text_bytes]u8 = undefined;
    const host = windows.display(reply.host, &host_buffer);
    const reason = windows.display(reply.reason, &reason_buffer);
    const len = 16 + host.len + reason.len;
    if (out.len < len) return error.BufferTooSmall;
    out[0] = 1;
    out[1] = @intFromEnum(reply.phase);
    std.mem.writeInt(u64, out[2..10], reply.token, .little);
    std.mem.writeInt(u32, out[10..14], reply.session_id, .little);
    out[14] = @intCast(host.len);
    @memcpy(out[15..][0..host.len], host);
    out[15 + host.len] = @intCast(reason.len);
    @memcpy(out[16 + host.len ..][0..reason.len], reason);
    return out[0..len];
}

const Entry = struct {
    token: u64,
    destination: Destination,
    host_buffer: [max_text_bytes]u8 = undefined,
    host_len: usize = 0,
    reason_buffer: [max_text_bytes]u8 = undefined,
    reason_len: usize = 0,
    request_id: u32 = 0,
    session_id: u32 = 0,
    phase: Phase = .ready,
    cancelled: bool = false,

    fn reply(self: *const Entry) Reply {
        return .{ .phase = self.phase, .token = self.token, .session_id = self.session_id, .host = self.host_buffer[0..self.host_len], .reason = self.reason_buffer[0..self.reason_len] };
    }

    fn refuse(self: *Entry, reason: []const u8) void {
        self.phase = .refused;
        self.reason_len = windows.display(reason, &self.reason_buffer).len;
    }

    fn windowCurrent(self: *const Entry, engine: anytype) bool {
        const target: windows.Target = .{ .window = self.destination.window, .epoch = self.destination.window_epoch };
        return target.validWindow(engine.model);
    }
};

pub const Controller = struct {
    entries: std.ArrayList(Entry) = .empty,
    next_token: u64 = 1,

    pub fn deinit(self: *Controller, allocator: std.mem.Allocator) void {
        self.entries.deinit(allocator);
        self.* = .{};
    }

    /// Engine owns allocator and the provider hooks documented by Destination.
    /// Encoding capacity is checked before a request can mutate anything.
    pub fn handle(self: *Controller, engine: anytype, fx: anytype, bytes: []const u8, out: []u8) Error![]const u8 {
        const request = try decode(bytes);
        if (out.len < max_bytes) return error.BufferTooSmall;
        if (request.kind == .cancel) {
            if (self.find(request.token)) |index| self.cancel(index);
            self.poll(engine, fx);
            return encode(.{ .phase = .ready, .token = request.token }, out);
        }
        self.poll(engine, fx);
        if (request.kind == .describe) return encode(try self.describe(engine), out);
        const index = self.find(request.token) orelse return encode(.{ .phase = .unavailable, .token = request.token, .reason = "This New Session request is no longer available. Open New Session again." }, out);
        const entry = &self.entries.items[index];
        if (request.kind == .create) self.create(engine, entry, request.name);
        return encode(entry.reply(), out);
    }

    fn find(self: *const Controller, token: u64) ?usize {
        for (self.entries.items, 0..) |entry, index| if (entry.token == token) return index;
        return null;
    }

    fn describe(self: *Controller, engine: anytype) Error!Reply {
        var destination = engine.captureNewSessionDestination() orelse return .{ .phase = .unavailable, .reason = "Connect to a Phux machine before creating a session." };
        if (self.next_token == std.math.maxInt(u64)) return error.TokenExhausted;
        var entry: Entry = .{ .token = self.next_token, .destination = destination };
        entry.host_len = windows.display(destination.host, &entry.host_buffer).len;
        // A borrowed provider label must never survive the synchronous capture.
        destination.host = "";
        entry.destination = destination;
        if (!entry.windowCurrent(engine)) return .{ .phase = .unavailable, .reason = "The invoking window is no longer open." };
        try self.entries.append(engine.allocator, entry);
        self.next_token += 1;
        return self.entries.items[self.entries.items.len - 1].reply();
    }

    fn create(_: *Controller, engine: anytype, entry: *Entry, name: []const u8) void {
        // The same token is idempotent: neither double-click nor retry sends a
        // second operation. A fresh describe is needed after a refusal.
        if (entry.phase != .ready or entry.cancelled) return;
        if (!entry.windowCurrent(engine)) return entry.refuse("The invoking window was closed. Open New Session again.");
        if (!engine.newSessionDestinationCurrent(entry.destination)) return entry.refuse("The destination connection changed. Open New Session again.");
        const id = engine.sendNewSession(entry.destination, name, true) catch |err| {
            var reason: [max_text_bytes]u8 = undefined;
            entry.refuse(std.fmt.bufPrint(&reason, "Could not create the session ({s}). Check the machine connection and try again.", .{@errorName(err)}) catch "Could not create the session. Check the machine connection and try again.");
            return;
        };
        if (id == 0) return entry.refuse("The coordinator did not accept this request.");
        entry.request_id = id;
        entry.phase = .pending;
    }

    fn cancel(self: *Controller, index: usize) void {
        if (self.entries.items[index].phase == .pending) {
            self.entries.items[index].cancelled = true;
        } else {
            _ = self.entries.swapRemove(index);
        }
    }

    /// Run each engine tick, not only while a modal remains visible. Canceling
    /// withdraws focus authority; it cannot undo an already-sent server write.
    pub fn poll(self: *Controller, engine: anytype, fx: anytype) void {
        var index: usize = 0;
        while (index < self.entries.items.len) {
            const entry = &self.entries.items[index];
            if (entry.phase == .pending) settle(engine, entry, fx);
            if (entry.cancelled and entry.phase != .pending) {
                _ = self.entries.swapRemove(index);
            } else index += 1;
        }
    }
};

fn settle(engine: anytype, entry: *Entry, fx: anytype) void {
    if (!engine.newSessionDestinationCurrent(entry.destination)) {
        entry.refuse("The connection changed before creation was confirmed. Refresh Sessions to check the outcome.");
        return;
    }
    const outcome = engine.pollNewSession(entry.destination, entry.request_id);
    switch (outcome) {
        .pending => return,
        .refused, .unknown => |reason| entry.refuse(reason),
        .created => |id| {
            if (id == 0) {
                entry.refuse("The coordinator returned an invalid session identity.");
            } else {
                entry.session_id = id;
                entry.phase = .created;
                if (!entry.cancelled and entry.windowCurrent(engine)) engine.didCreateSession(entry.destination, id, fx);
            }
        },
    }
    engine.releaseNewSession(entry.destination, entry.request_id);
}

const Fixture = struct {
    const Model = struct {
        window_epochs: [2]u64 = .{ 10, 20 },
        pub fn windowOpen(_: *const @This(), window: usize) bool {
            return window < 2;
        }
    };
    allocator: std.mem.Allocator = std.testing.allocator,
    model: *Model,
    window: u8 = 0,
    provider: u64 = 7,
    current: bool = true,
    fail_send: bool = false,
    sent: u32 = 0,
    keep: bool = false,
    outcomes: [2]Outcome = .{ .pending, .pending },
    released: u32 = 0,
    selected: u32 = 0,
    pub fn captureNewSessionDestination(self: *@This()) ?Destination {
        return .{ .window = self.window, .window_epoch = self.model.window_epochs[self.window], .provider = self.provider, .provider_context = 12, .host_context = 13, .connection_epoch = 14, .selection_epoch = 15, .host = "This Mac" };
    }
    pub fn newSessionDestinationCurrent(self: *@This(), _: Destination) bool {
        return self.current;
    }
    pub fn sendNewSession(self: *@This(), _: Destination, _: []const u8, keep: bool) !u32 {
        if (self.fail_send) return error.ConnectionUnavailable;
        self.sent += 1;
        self.keep = keep;
        return self.sent;
    }
    pub fn pollNewSession(self: *@This(), _: Destination, id: u32) Outcome {
        return self.outcomes[id - 1];
    }
    pub fn releaseNewSession(self: *@This(), _: Destination, _: u32) void {
        self.released += 1;
    }
    pub fn didCreateSession(self: *@This(), _: Destination, id: u32, _: anytype) void {
        self.selected = id;
    }
};

fn testRequest(kind: Kind, token: u64, name: []const u8, out: []u8) []const u8 {
    out[0] = 1;
    out[1] = @intFromEnum(kind);
    std.mem.writeInt(u64, out[2..10], token, .little);
    out[10] = @intCast(name.len);
    @memcpy(out[11..][0..name.len], name);
    return out[0 .. 11 + name.len];
}

test "new session keeps empty and correlates concurrent out of order results" {
    var model: Fixture.Model = .{};
    var engine: Fixture = .{ .model = &model };
    var controller: Controller = .{};
    defer controller.deinit(std.testing.allocator);
    var request: [256]u8 = undefined;
    var response: [max_bytes]u8 = undefined;
    _ = try controller.handle(&engine, .{}, testRequest(.describe, 0, "", &request), &response);
    _ = try controller.handle(&engine, .{}, testRequest(.create, 1, "Build", &request), &response);
    _ = try controller.handle(&engine, .{}, testRequest(.create, 1, "Build", &request), &response);
    try std.testing.expectEqual(@as(u32, 1), engine.sent);
    try std.testing.expect(engine.keep);
    engine.window = 1;
    engine.provider = 8;
    _ = try controller.handle(&engine, .{}, testRequest(.describe, 0, "", &request), &response);
    _ = try controller.handle(&engine, .{}, testRequest(.create, 2, "Deploy", &request), &response);
    engine.outcomes[1] = .{ .created = 52 };
    const second = try controller.handle(&engine, .{}, testRequest(.status, 2, "", &request), &response);
    try std.testing.expectEqual(@as(u8, 2), second[1]);
    try std.testing.expectEqual(@as(u32, 52), std.mem.readInt(u32, second[10..14], .little));
    engine.outcomes[0] = .{ .refused = "name already exists" };
    const first = try controller.handle(&engine, .{}, testRequest(.status, 1, "", &request), &response);
    try std.testing.expectEqual(@as(u8, 3), first[1]);
    try std.testing.expectEqual(@as(u32, 52), engine.selected);
    try std.testing.expectEqual(@as(u32, 2), engine.released);
}

test "new session rejects stale invoking window and changed coordinator before sending" {
    var model: Fixture.Model = .{};
    var engine: Fixture = .{ .model = &model };
    var controller: Controller = .{};
    defer controller.deinit(std.testing.allocator);
    var request: [256]u8 = undefined;
    var response: [max_bytes]u8 = undefined;
    _ = try controller.handle(&engine, .{}, testRequest(.describe, 0, "", &request), &response);
    model.window_epochs[0] += 1;
    _ = try controller.handle(&engine, .{}, testRequest(.create, 1, "Build", &request), &response);
    try std.testing.expectEqual(@as(u32, 0), engine.sent);
    try std.testing.expectEqual(@as(u8, 3), response[1]);
    _ = try controller.handle(&engine, .{}, testRequest(.describe, 0, "", &request), &response);
    engine.current = false;
    _ = try controller.handle(&engine, .{}, testRequest(.create, 2, "Build", &request), &response);
    try std.testing.expectEqual(@as(u32, 0), engine.sent);
}

test "cancelled creation settles and releases without stealing focus" {
    var model: Fixture.Model = .{};
    var engine: Fixture = .{ .model = &model };
    var controller: Controller = .{};
    defer controller.deinit(std.testing.allocator);
    var request: [256]u8 = undefined;
    var response: [max_bytes]u8 = undefined;
    _ = try controller.handle(&engine, .{}, testRequest(.describe, 0, "", &request), &response);
    _ = try controller.handle(&engine, .{}, testRequest(.create, 1, "Build", &request), &response);
    engine.outcomes[0] = .{ .created = 41 };
    // Cancel wins even when the success is ready in this same event turn.
    _ = try controller.handle(&engine, .{}, testRequest(.cancel, 1, "", &request), &response);
    controller.poll(&engine, .{});
    try std.testing.expectEqual(@as(u32, 0), engine.selected);
    try std.testing.expectEqual(@as(u32, 1), engine.released);
    try std.testing.expectEqual(@as(usize, 0), controller.entries.items.len);
}

test "invalid names and small reply buffer never send a session operation" {
    var model: Fixture.Model = .{};
    var engine: Fixture = .{ .model = &model };
    var controller: Controller = .{};
    defer controller.deinit(std.testing.allocator);
    var request: [256]u8 = undefined;
    var response: [max_bytes]u8 = undefined;
    try std.testing.expectError(error.InvalidRequest, controller.handle(&engine, .{}, testRequest(.create, 1, "a\nb", &request), &response));
    try std.testing.expectError(error.BufferTooSmall, controller.handle(&engine, .{}, testRequest(.describe, 0, "", &request), response[0..16]));
    try std.testing.expectEqual(@as(usize, 0), controller.entries.items.len);
    try std.testing.expectEqual(@as(u32, 0), engine.sent);
}

test "a failed create is an actionable refusal and cannot masquerade as pending" {
    var model: Fixture.Model = .{};
    var engine: Fixture = .{ .model = &model, .fail_send = true };
    var controller: Controller = .{};
    defer controller.deinit(std.testing.allocator);
    var request: [256]u8 = undefined;
    var response: [max_bytes]u8 = undefined;
    _ = try controller.handle(&engine, .{}, testRequest(.describe, 0, "", &request), &response);
    const reply = try controller.handle(&engine, .{}, testRequest(.create, 1, "Build", &request), &response);
    try std.testing.expectEqual(@as(u8, 3), reply[1]);
    try std.testing.expect(std.mem.indexOf(u8, reply, "Check the machine connection") != null);
    try std.testing.expectEqual(@as(u32, 0), engine.sent);
}
