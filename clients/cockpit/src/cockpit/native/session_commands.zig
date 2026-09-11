//! Rename Session: the engine half (docs/REMOTE_HOSTS.md, "Renaming a
//! session").
//!
//! One bounded request, `cockpit.session`, carries the action and its answer:
//!
//!   request  version=1, kind:u8, name_len:u8, name UTF-8
//!            kind 1 describe the session a rename would name, 2 rename it
//!            to `name`, 3 the last rename's outcome
//!   reply    version=1, phase:u8, name_len:u8, name, host_len:u8, host,
//!            reason_len:u8, reason
//!            phase 0 ready, 1 pending, 2 renamed, 3 refused, 4 unavailable
//!
//! The session is the one on screen: the session whose tab holds the focused
//! pane, on the coordinator that minted that pane's ref, and on no other.
//! With no Phux pane focused it is the active coordinator's attached session.
//! A pane whose coordinator is no longer held names nothing, so the rename is
//! refused rather than sent anywhere else. The write is
//! `phux.session.name/v1` on that coordinator's own connection; the server's
//! METADATA_CHANGED renames the session in that coordinator's list, which the
//! header and the switcher read.

const std = @import("std");
const support = @import("../phux_support.zig");
const navigation = @import("ts_navigation.zig");
const projection = @import("workspace_projection.zig");

pub const request_name = "cockpit.session";
pub const version: u8 = 1;
/// Per-field display bound, as for `cockpit.remote`.
pub const max_text_bytes: usize = 240;
pub const max_bytes: usize = 5 + 3 * max_text_bytes;

pub const Kind = enum(u8) { describe = 1, rename = 2, status = 3 };
/// `unavailable`: nothing on screen can be renamed. `refused`: this rename
/// changed nothing; the reason says why.
pub const Phase = enum(u8) { ready = 0, pending = 1, renamed = 2, refused = 3, unavailable = 4 };
pub const Error = error{ InvalidRequest, BufferTooSmall };

/// The rename sent to one coordinator, on one of its connections.
pub const Flight = struct { coordinator: support.ProviderId, epoch: u64, request_id: u32 };

pub const Request = struct { kind: Kind, name: []const u8 = "" };

pub fn decode(bytes: []const u8) Error!Request {
    if (bytes.len < 3 or bytes[0] != version) return error.InvalidRequest;
    const kind: Kind = switch (bytes[1]) {
        1 => .describe,
        2 => .rename,
        3 => .status,
        else => return error.InvalidRequest,
    };
    if (@as(usize, bytes[2]) != bytes.len - 3) return error.InvalidRequest;
    const name = bytes[3..];
    switch (kind) {
        .rename => if (name.len == 0 or !std.unicode.utf8ValidateSlice(name)) return error.InvalidRequest,
        .describe, .status => if (name.len != 0) return error.InvalidRequest,
    }
    return .{ .kind = kind, .name = name };
}

pub const Reply = struct { phase: Phase, name: []const u8 = "", host: []const u8 = "", reason: []const u8 = "" };

pub fn encode(reply: Reply, out: []u8) Error![]const u8 {
    var name_buffer: [max_text_bytes]u8 = undefined;
    var host_buffer: [max_text_bytes]u8 = undefined;
    var reason_buffer: [max_text_bytes]u8 = undefined;
    const fields = [_][]const u8{
        navigation.displayText(reply.name, &name_buffer),
        navigation.displayText(reply.host, &host_buffer),
        navigation.displayText(reply.reason, &reason_buffer),
    };
    var len: usize = 2;
    for (fields) |field| len += 1 + field.len;
    if (out.len < len) return error.BufferTooSmall;
    out[0] = version;
    out[1] = @intFromEnum(reply.phase);
    var at: usize = 2;
    for (fields) |field| {
        out[at] = @intCast(field.len);
        @memcpy(out[at + 1 ..][0..field.len], field);
        at += 1 + field.len;
    }
    return out[0..len];
}

/// Storage a reply's slices borrow until it is encoded.
const Scratch = struct {
    reason: [max_text_bytes]u8 = undefined,
};

/// Apply one request on the owning thread and encode the answer.
pub fn handle(engine: anytype, payload: []const u8, out: []u8) Error![]const u8 {
    const request = try decode(payload);
    var scratch: Scratch = .{};
    const reply = switch (request.kind) {
        .describe => describe(engine),
        .rename => rename(engine, request.name, &scratch),
        .status => status(engine, &scratch),
    };
    return encode(reply, out);
}

const nothing_on_screen: Reply = .{ .phase = .unavailable, .reason = "No Phux session is on screen to rename." };

/// The session a rename would name, and whose host it is on.
pub fn describe(engine: anytype) Reply {
    const target = engine.renameTarget() orelse return nothing_on_screen;
    return .{ .phase = .ready, .name = target.name, .host = hostLabel(engine.model, target.provider) };
}

fn rename(engine: anytype, name: []const u8, scratch: *Scratch) Reply {
    if (comptime !support.phux_enabled) return nothing_on_screen;
    const target = engine.renameTarget() orelse return nothing_on_screen;
    const host = hostLabel(engine.model, target.provider);
    var reply: Reply = .{ .phase = .refused, .name = target.name, .host = host };
    if (engine.rename_flight) |flight| if (flightPending(engine, flight)) {
        reply.reason = "A rename is already in progress.";
        return reply;
    };
    for (name) |byte| if (byte < 0x20 or byte == 0x7f) {
        reply.reason = "A session name cannot contain control characters.";
        return reply;
    };
    // Judged first against the list Cockpit shows for that coordinator; the
    // client judges it again against its own, and the server has the last word.
    for (target.provider.sessionCatalog()) |entry| {
        if (entry.id == target.session or !std.mem.eql(u8, entry.name, name)) continue;
        reply.reason = std.fmt.bufPrint(&scratch.reason, "\"{s}\" already exists on {s}.", .{ name, host }) catch "That name already exists.";
        return reply;
    }
    const request_id = target.provider.requestRename(target.name, name) catch {
        reply.reason = std.fmt.bufPrint(&scratch.reason, "Could not send the rename to {s}.", .{host}) catch "Could not send the rename.";
        return reply;
    };
    engine.rename_flight = .{ .coordinator = target.provider.providerId(), .epoch = target.provider.connectionEpoch(), .request_id = request_id };
    return outcome(engine, target.provider, request_id, scratch);
}

/// Whether the flight's coordinator still holds it pending on the same
/// connection. A flight whose connection moved on can never settle.
fn flightPending(engine: anytype, flight: Flight) bool {
    const owner = engine.model.phuxFor(flight.coordinator) orelse return false;
    if (owner.connectionEpoch() != flight.epoch) return false;
    const info = owner.renameInfo();
    return info.request_id == flight.request_id and info.status == .pending;
}

/// The last rename's outcome, read from the coordinator it was sent to.
fn status(engine: anytype, scratch: *Scratch) Reply {
    const flight = engine.rename_flight orelse return describe(engine);
    const unknown: Reply = .{ .phase = .refused, .reason = "The connection ended before the rename was confirmed." };
    const owner = engine.model.phuxFor(flight.coordinator) orelse return unknown;
    if (owner.connectionEpoch() != flight.epoch) return unknown;
    return outcome(engine, owner, flight.request_id, scratch);
}

fn outcome(engine: anytype, owner: anytype, request_id: u32, scratch: *Scratch) Reply {
    const info = owner.renameInfo();
    var reply: Reply = .{ .phase = .refused, .host = hostLabel(engine.model, owner), .name = sessionName(owner, info.session_id) };
    if (info.request_id != request_id) {
        reply.reason = "The rename's outcome is unknown.";
        return reply;
    }
    switch (info.status) {
        .pending => reply.phase = .pending,
        .renamed => reply.phase = .renamed,
        .none => reply.reason = "The rename's outcome is unknown.",
        else => {
            // Borrowed from the client: copied before anything else runs.
            const message = if (info.message.len != 0) info.message else "the server refused the rename";
            const length = @min(message.len, scratch.reason.len);
            @memcpy(scratch.reason[0..length], message[0..length]);
            reply.reason = scratch.reason[0..length];
        },
    }
    return reply;
}

fn sessionName(owner: anytype, id: u32) []const u8 {
    for (owner.sessionCatalog()) |entry| if (entry.id == id) return entry.name;
    return "";
}

/// The host a coordinator's sessions are listed under: this Mac, or the
/// registered host's label, as the switcher's groups name them.
fn hostLabel(model: anytype, owner: anytype) []const u8 {
    if (model.phuxConst()) |active| if (active.providerId() == owner.providerId()) return active.remoteLabel() orelse "This Mac";
    return projection.peerHostLabel(model, owner.providerId());
}

test "a rename request names its session; malformed requests are refused" {
    try std.testing.expectEqual(Kind.describe, (try decode(&.{ 1, 1, 0 })).kind);
    const renamed = try decode(&.{ 1, 2, 4, 's', 'h', 'i', 'p' });
    try std.testing.expectEqualStrings("ship", renamed.name);
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 2, 0 }));
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 2, 2, 0xff, 0xfe }));
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 1, 3, 1, 'x' }));
    try std.testing.expectError(error.InvalidRequest, decode(&.{ 2, 1, 0 }));
    var out: [max_bytes]u8 = undefined;
    const bytes = try encode(.{ .phase = .refused, .name = "a", .host = "mini", .reason = "why" }, &out);
    try std.testing.expectEqualSlices(u8, &.{ 1, 3, 1, 'a', 4, 'm', 'i', 'n', 'i', 3, 'w', 'h', 'y' }, bytes);
}
