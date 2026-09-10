//! Connect to Host: the engine half of Cockpit's remote-host flow.
//!
//! One bounded request, `cockpit.remote`, carries both the action and its
//! answer (docs/REMOTE_HOSTS.md has the table):
//!
//!   request  version=1, kind:u8, target_len:u8, target UTF-8
//!            kind 1 status, 2 connect to target, 3 return to this Mac
//!   reply    version=1, phase:u8, host_len:u8, host, reason_len:u8, reason
//!            phase 0 local, 1 connecting, 2 connected, 3 failed,
//!            4 reconnecting
//!
//! Connecting never builds a second provider or a second lifecycle. The
//! host is resolved in the phux CLI's own registry first (files only, no
//! network), so an unregistered host fails here with the command that pairs
//! it and nothing about the current connection changes. A resolved host
//! retargets the one Phux provider and restarts it through the same
//! close-then-reopen path Reconnect uses, so frozen canvases, session
//! handoff and command fencing behave exactly as for a local coordinator.

const std = @import("std");
const support = @import("../phux_support.zig");
const model_module = @import("../model.zig");
const navigation = @import("ts_navigation.zig");
const remote_memory = @import("../remote_memory.zig");
const config_module = @import("../../config/config.zig");

const Model = model_module.Model;

pub const request_name = "cockpit.remote";
pub const version: u8 = 1;
/// Per-field display bound; the same one the tunnel copies with.
pub const max_text_bytes: usize = 240;
pub const max_bytes: usize = 4 + 2 * max_text_bytes;

pub const Kind = enum(u8) { status = 1, connect = 2, local = 3 };
pub const Phase = enum(u8) { local = 0, connecting = 1, connected = 2, failed = 3, reconnecting = 4 };
pub const Error = error{ InvalidRequest, BufferTooSmall };

pub const Request = struct { kind: Kind, target: []const u8 = "" };

pub fn decode(bytes: []const u8) Error!Request {
    if (bytes.len < 3 or bytes[0] != version) return error.InvalidRequest;
    const kind: Kind = switch (bytes[1]) {
        1 => .status,
        2 => .connect,
        3 => .local,
        else => return error.InvalidRequest,
    };
    if (@as(usize, bytes[2]) != bytes.len - 3) return error.InvalidRequest;
    const target = bytes[3..];
    switch (kind) {
        .connect => if (target.len == 0 or !config_module.validPhuxRemote(target)) return error.InvalidRequest,
        .status, .local => if (target.len != 0) return error.InvalidRequest,
    }
    return .{ .kind = kind, .target = target };
}

pub const Reply = struct { phase: Phase, host: []const u8 = "", reason: []const u8 = "" };

pub fn encode(reply: Reply, out: []u8) Error![]const u8 {
    var host_buffer: [max_text_bytes]u8 = undefined;
    var reason_buffer: [max_text_bytes]u8 = undefined;
    const host = navigation.displayText(reply.host, &host_buffer);
    const reason = navigation.displayText(reply.reason, &reason_buffer);
    const len = 4 + host.len + reason.len;
    if (out.len < len) return error.BufferTooSmall;
    out[0] = version;
    out[1] = @intFromEnum(reply.phase);
    out[2] = @intCast(host.len);
    @memcpy(out[3..][0..host.len], host);
    out[3 + host.len] = @intCast(reason.len);
    @memcpy(out[4 + host.len ..][0..reason.len], reason);
    return out[0..len];
}

/// Storage a reply's slices borrow until it is encoded.
pub const Scratch = struct {
    host: [max_text_bytes]u8 = undefined,
    reason: [max_text_bytes]u8 = undefined,
};

/// Apply one request on the owning thread and encode the resulting status.
pub fn handle(engine: anytype, fx: anytype, payload: []const u8, out: []u8) Error![]const u8 {
    const request = try decode(payload);
    var scratch: Scratch = .{};
    const reply = switch (request.kind) {
        .status => status(engine.model, &scratch),
        .connect => connect(engine, fx, request.target, &scratch),
        .local => returnLocal(engine, fx),
    };
    return encode(reply, out);
}

/// Where the one Phux provider stands, named by the host it dials.
pub fn status(model: *Model, scratch: *Scratch) Reply {
    if (comptime !support.phux_enabled) return .{ .phase = .local };
    const remote = model.phux() orelse return .{ .phase = .local };
    const host = copy(&scratch.host, remote.remoteLabel() orelse return .{ .phase = .local });
    return switch (navigation.connection(model)) {
        .local => .{ .phase = .local },
        .connected, .workspace_unavailable => blk: {
            remote.noteRemoteConnected();
            rememberIfChosen(model, remote.remoteTarget());
            break :blk .{ .phase = .connected, .host = host };
        },
        .connecting => .{ .phase = if (remote.remoteConnectedOnce()) .reconnecting else .connecting, .host = host },
        .offline => .{ .phase = .failed, .host = host, .reason = failureReason(remote, scratch) },
    };
}

fn failureReason(remote: anytype, scratch: *Scratch) []const u8 {
    const recorded = remote.remoteFailure(&scratch.reason);
    return if (recorded.len != 0) recorded else "the connection was lost";
}

fn connect(engine: anytype, fx: anytype, target: []const u8, scratch: *Scratch) Reply {
    if (comptime !support.phux_enabled)
        return .{ .phase = .failed, .host = target, .reason = "this build of Cockpit has no Phux provider" };
    const remote = engine.model.phux() orelse
        return .{ .phase = .failed, .host = target, .reason = "Phux is not configured" };
    const described = support.PhuxProvider.describeRemote(target);
    if (described.state != .resolved) return .{
        .phase = .failed,
        .host = copy(&scratch.host, target),
        .reason = copy(&scratch.reason, described.message.slice()),
    };
    const session = described.session.slice();
    remote.requestRetarget(
        .{ .remote = .{ .target = target } },
        if (session.len == 0) null else session,
        described.name.slice(),
    ) catch return .{ .phase = .failed, .host = target, .reason = "out of memory" };
    choose(target);
    restart(engine, fx);
    return .{ .phase = .connecting, .host = copy(&scratch.host, described.name.slice()) };
}

/// Back to the configured local coordinator, and forget the remembered host.
fn returnLocal(engine: anytype, fx: anytype) Reply {
    if (comptime !support.phux_enabled) return .{ .phase = .local };
    const model = engine.model;
    const remote = model.phux() orelse return .{ .phase = .local };
    if (remote.remoteTarget() == null) return .{ .phase = .local };
    const socket = model.config.phux_socket.slice();
    const session = model.config.phux_session.slice();
    remote.requestRetarget(.{ .unix = socket }, if (session.len == 0) null else session, null) catch
        return .{ .phase = .failed, .reason = "out of memory" };
    choose(null);
    remember(model, null);
    restart(engine, fx);
    return .{ .phase = .local };
}

/// The live effects restart through the engine's Reconnect path. Test fakes
/// without a channel leave the retarget pending, which is still observable.
fn restart(engine: anytype, fx: anytype) void {
    const Fx = switch (@typeInfo(@TypeOf(fx))) {
        .pointer => |pointer| pointer.child,
        else => @TypeOf(fx),
    };
    if (comptime !@hasDecl(Fx, "restartPhux")) return;
    _ = fx.restartPhux(engine);
}

fn copy(out: *[max_text_bytes]u8, text: []const u8) []const u8 {
    return navigation.displayText(text, out);
}

/// Last value written, so a status poll does not rewrite an unchanged file.
var remembered_buffer: [config_module.max_phux_remote_bytes]u8 = undefined;
var remembered_len: usize = 0;
var remembered_known = false;

/// The host the user picked in Connect to Host this run. Only that host is
/// ever remembered: one selected by `PHUX_REMOTE` or `phux-remote` is the
/// environment's or the config's choice, and writing it down would keep
/// reattaching it after the variable or the line is gone.
var chosen_buffer: [config_module.max_phux_remote_bytes]u8 = undefined;
var chosen_len: usize = 0;

fn choose(target: ?[]const u8) void {
    const value = target orelse "";
    if (value.len > chosen_buffer.len) {
        chosen_len = 0;
        return;
    }
    @memcpy(chosen_buffer[0..value.len], value);
    chosen_len = value.len;
}

fn rememberIfChosen(model: *Model, target: ?[]const u8) void {
    const value = target orelse return;
    if (chosen_len == 0 or !std.mem.eql(u8, chosen_buffer[0..chosen_len], value)) return;
    remember(model, value);
}

/// Tests share this module's process state; each starts from nothing.
pub fn forgetForTests() void {
    chosen_len = 0;
    remembered_len = 0;
    remembered_known = false;
}

fn remember(model: *Model, target: ?[]const u8) void {
    const path = remote_memory.path() orelse return;
    const value = target orelse "";
    if (value.len > remembered_buffer.len) return;
    if (remembered_known and std.mem.eql(u8, remembered_buffer[0..remembered_len], value)) return;
    remote_memory.store(model.provider.io, path, target);
    @memcpy(remembered_buffer[0..value.len], value);
    remembered_len = value.len;
    remembered_known = true;
}

test "requests are exact: a connect names a host, status and local carry none" {
    try std.testing.expectEqualDeep(Request{ .kind = .connect, .target = "me@mini" }, try decode("\x01\x02\x07me@mini"));
    try std.testing.expectEqual(Kind.status, (try decode("\x01\x01\x00")).kind);
    try std.testing.expectEqual(Kind.local, (try decode("\x01\x03\x00")).kind);
    for ([_][]const u8{
        "",                "\x02\x01\x00",           "\x01\x09\x00",
        "\x01\x02\x00",    "\x01\x02\x05mini",       "\x01\x01\x04mini",
        "\x01\x02\x03a b", "\x01\x02\x0aquic://x:1",
    }) |bytes| try std.testing.expectError(error.InvalidRequest, decode(bytes));
}

test "replies carry the phase, the host, and an elided reason" {
    var out: [max_bytes]u8 = undefined;
    const bytes = try encode(.{ .phase = .failed, .host = "mini", .reason = "did not answer" }, &out);
    try std.testing.expectEqualSlices(u8, "\x01\x03\x04mini\x0edid not answer", bytes);
    var long: [max_text_bytes * 2]u8 = undefined;
    @memset(&long, 'x');
    const elided = try encode(.{ .phase = .connecting, .host = &long, .reason = &long }, &out);
    try std.testing.expectEqual(max_bytes, elided.len);
    try std.testing.expectError(error.BufferTooSmall, encode(.{ .phase = .local, .host = "mini" }, out[0..4]));
}
