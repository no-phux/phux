//! Connect to Host: the engine half of Cockpit's remote-host flow.
//!
//! One bounded request, `cockpit.remote`, carries both the action and its
//! answer (docs/REMOTE_HOSTS.md has the table):
//!
//!   request  version=1, kind:u8, target_len:u8, target UTF-8
//!            kind 1 status, 2 connect to target, 3 return to this Mac,
//!            4 disconnect target (every host when target is empty)
//!   reply    version=1, phase:u8, host_len:u8, host, reason_len:u8, reason
//!            phase 0 local, 1 connecting, 2 connected, 3 failed,
//!            4 reconnecting, 5 refused (nothing changed; reason says why)
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

/// 4 removes a remote host entirely: its switcher group and tabs go, and it
/// is no longer reattached at launch. With a target it removes that host
/// alone; with none, every host. 3 (Use this Mac) only makes this Mac active
/// and keeps the remote hosts listed beside it.
pub const Kind = enum(u8) { status = 1, connect = 2, local = 3, disconnect = 4 };
/// `refused`: the request changed nothing (a host Cockpit does not hold, or
/// no room for another coordinator); the reason says why, and the
/// connection status stays whatever it was.
pub const Phase = enum(u8) { local = 0, connecting = 1, connected = 2, failed = 3, reconnecting = 4, refused = 5 };
pub const Error = error{ InvalidRequest, BufferTooSmall };

pub const Request = struct { kind: Kind, target: []const u8 = "" };

pub fn decode(bytes: []const u8) Error!Request {
    if (bytes.len < 3 or bytes[0] != version) return error.InvalidRequest;
    const kind: Kind = switch (bytes[1]) {
        1 => .status,
        2 => .connect,
        3 => .local,
        4 => .disconnect,
        else => return error.InvalidRequest,
    };
    if (@as(usize, bytes[2]) != bytes.len - 3) return error.InvalidRequest;
    const target = bytes[3..];
    switch (kind) {
        .connect => if (target.len == 0 or !config_module.validPhuxRemote(target)) return error.InvalidRequest,
        .disconnect => if (target.len != 0 and !config_module.validPhuxRemote(target)) return error.InvalidRequest,
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
        .disconnect => disconnect(engine, fx, request.target, &scratch),
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
    _ = engine.model.phux() orelse
        return .{ .phase = .failed, .host = target, .reason = "Phux is not configured" };
    const described = support.PhuxProvider.describeRemote(target);
    if (described.state != .resolved) return .{
        .phase = .failed,
        .host = copy(&scratch.host, target),
        .reason = copy(&scratch.reason, described.message.slice()),
    };
    const session = described.session.slice();
    const pinned: ?[]const u8 = if (session.len == 0) null else session;
    const endpoint: support.PhuxEndpoint = .{ .remote = .{ .target = target } };
    // The host becomes active and the coordinator it leaves stays listed
    // beside it: this Mac, or a host connected before. A host already listed
    // trades places with the active one; nothing else moves.
    engine.exchangeCoordinators(fx, endpoint, pinned, described.name.slice()) catch |err| return switch (err) {
        // Nothing changed: the connections held stay as they are.
        error.PeerCapacity => .{ .phase = .refused, .host = target, .reason = capacity_reason },
        else => .{ .phase = .failed, .host = target, .reason = "out of memory" },
    };
    choose(target);
    return .{ .phase = .connecting, .host = copy(&scratch.host, described.name.slice()) };
}

/// Use this Mac: make the local coordinator the active one. The remote host
/// stays listed beside it as the peer and stays remembered, so this is
/// simply selecting this Mac's group; Disconnect is what removes the host.
fn returnLocal(engine: anytype, fx: anytype) Reply {
    if (comptime !support.phux_enabled) return .{ .phase = .local };
    const model = engine.model;
    const remote = model.phux() orelse return .{ .phase = .local };
    if (remote.remoteTarget() == null) return .{ .phase = .local };
    const socket = model.config.phux_socket.slice();
    const session = model.config.phux_session.slice();
    engine.exchangeCoordinators(fx, .{ .unix = socket }, if (session.len == 0) null else session, null) catch
        return .{ .phase = .failed, .reason = "out of memory" };
    choose(null);
    return .{ .phase = .local };
}

/// The refusal for a fifth coordinator (docs/REMOTE_HOSTS.md, Known limits).
pub const capacity_reason = "Cockpit holds at most four coordinators: this Mac and three hosts. Disconnect a host first.";
const unknown_host_reason = "Cockpit is not connected to that host";

/// Disconnect `target` alone, or every remote host when it is empty.
fn disconnect(engine: anytype, fx: anytype, target: []const u8, scratch: *Scratch) Reply {
    if (comptime !support.phux_enabled) return .{ .phase = .local };
    if (target.len == 0) return disconnectAll(engine, fx);
    return disconnectHost(engine, fx, target, scratch);
}

/// Remove the remote hosts: this Mac becomes active if it was not, every
/// peer goes (their groups and tabs leave), and no host is reattached at
/// launch.
fn disconnectAll(engine: anytype, fx: anytype) Reply {
    const model = engine.model;
    const remote = model.phux() orelse return .{ .phase = .local };
    if (remote.remoteTarget() != null) {
        const socket = model.config.phux_socket.slice();
        const session = model.config.phux_session.slice();
        remote.requestRetarget(.{ .unix = socket }, if (session.len == 0) null else session, null) catch
            return .{ .phase = .failed, .reason = "out of memory" };
        restart(engine, fx);
    }
    for (0..model.phux_peers.len) |slot| engine.dropPeer(fx, slot);
    choose(null);
    forgetEveryHost(model);
    return .{ .phase = .local };
}

/// Remove one registered host, named by its target or its registry name,
/// and nothing else: every other coordinator keeps its connection, its tabs
/// and its slot. A listed host's slot goes. The active host hands over to
/// this Mac, whose standby slot is freed rather than listing it twice. A
/// host Cockpit does not hold is refused and nothing changes.
fn disconnectHost(engine: anytype, fx: anytype, target: []const u8, scratch: *Scratch) Reply {
    const model = engine.model;
    const active = model.phux() orelse return .{ .phase = .local };
    var removed_buffer: [config_module.max_phux_remote_bytes]u8 = undefined;
    const slot = hostSlot(model, target);
    const holder = if (slot) |index| model.phux_peers[index].? else if (namesHost(active, target)) active else return .{
        .phase = .refused,
        .host = copy(&scratch.host, target),
        .reason = unknown_host_reason,
    };
    // Copied before the provider it borrows from is retargeted or destroyed.
    const removed = copyTarget(holder.remoteTarget().?, &removed_buffer);
    if (slot) |index| {
        engine.dropPeer(fx, index);
    } else if (!handOverToThisMac(engine, fx)) {
        return .{ .phase = .failed, .host = copy(&scratch.host, target), .reason = "out of memory" };
    }
    forgetHost(model, removed);
    return status(model, scratch);
}

/// Whether `provider` dials the registered host `target` names, by target
/// or by registry name.
fn namesHost(provider: anytype, target: []const u8) bool {
    const host = provider.remoteTarget() orelse return false;
    if (std.mem.eql(u8, host, target)) return true;
    const label = provider.remoteLabel() orelse return false;
    return std.mem.eql(u8, label, target);
}

/// The peer slot holding the host `target` names.
fn hostSlot(model: *Model, target: []const u8) ?usize {
    for (model.phux_peers, 0..) |value, slot| {
        const peer = value orelse continue;
        if (namesHost(peer, target)) return slot;
    }
    return null;
}

/// The active host goes: this Mac becomes active on its configured socket
/// and session, and its own standby slot, if it has one, is dropped first.
fn handOverToThisMac(engine: anytype, fx: anytype) bool {
    const model = engine.model;
    const active = model.phux().?;
    const socket = model.config.phux_socket.slice();
    const session = model.config.phux_session.slice();
    const next = active.prepareRetarget(.{ .unix = socket }, if (session.len == 0) null else session, null) catch return false;
    for (model.phux_peers, 0..) |value, slot| {
        const peer = value orelse continue;
        if (peer.remoteTarget() == null) engine.dropPeer(fx, slot);
    }
    active.commitRetarget(next);
    restart(engine, fx);
    return true;
}

fn copyTarget(target: []const u8, out: *[config_module.max_phux_remote_bytes]u8) []const u8 {
    const len = @min(target.len, out.len);
    @memcpy(out[0..len], target[0..len]);
    return out[0..len];
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

/// The hosts reattached at launch, read from the file once and kept here,
/// so a status poll does not rewrite an unchanged file.
var remembered: remote_memory.Hosts = .{};
var remembered_loaded = false;

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

/// The remembered hosts, read from the file on first use.
fn rememberedHosts(model: *Model) ?*remote_memory.Hosts {
    const path = remote_memory.path() orelse return null;
    if (!remembered_loaded) {
        remote_memory.loadAll(model.provider.io, path, &remembered);
        remembered_loaded = true;
    }
    return &remembered;
}

fn saveRemembered(model: *Model) void {
    const path = remote_memory.path() orelse return;
    remote_memory.storeAll(model.provider.io, path, &remembered);
}

/// A host removed by name is no longer chosen, and no longer reattached at
/// launch. The other remembered hosts stay.
fn forgetHost(model: *Model, target: []const u8) void {
    if (chosen_len != 0 and std.mem.eql(u8, chosen_buffer[0..chosen_len], target)) chosen_len = 0;
    const hosts = rememberedHosts(model) orelse return;
    if (hosts.remove(target)) saveRemembered(model);
}

/// Disconnect All: no host is reattached at launch.
fn forgetEveryHost(model: *Model) void {
    const hosts = rememberedHosts(model) orelse return;
    hosts.* = .{};
    saveRemembered(model);
}

/// The chosen host, once seen connected, joins the hosts reattached at
/// launch; the ones remembered before stay.
fn rememberIfChosen(model: *Model, target: ?[]const u8) void {
    const value = target orelse return;
    if (chosen_len == 0 or !std.mem.eql(u8, chosen_buffer[0..chosen_len], value)) return;
    const hosts = rememberedHosts(model) orelse return;
    if (hosts.add(value)) saveRemembered(model);
}

/// Tests share this module's process state; each starts from nothing.
pub fn forgetForTests() void {
    chosen_len = 0;
    remembered = .{};
    remembered_loaded = false;
}

test "requests are exact: a connect names a host, a disconnect may, status and local carry none" {
    try std.testing.expectEqualDeep(Request{ .kind = .connect, .target = "me@mini" }, try decode("\x01\x02\x07me@mini"));
    try std.testing.expectEqual(Kind.status, (try decode("\x01\x01\x00")).kind);
    try std.testing.expectEqual(Kind.local, (try decode("\x01\x03\x00")).kind);
    try std.testing.expectEqualDeep(Request{ .kind = .disconnect, .target = "mini" }, try decode("\x01\x04\x04mini"));
    try std.testing.expectEqualDeep(Request{ .kind = .disconnect }, try decode("\x01\x04\x00"));
    for ([_][]const u8{
        "",                 "\x02\x01\x00",           "\x01\x09\x00",
        "\x01\x02\x00",     "\x01\x02\x05mini",       "\x01\x01\x04mini",
        "\x01\x02\x03a b",  "\x01\x02\x0aquic://x:1", "\x01\x04\x03a b",
        "\x01\x03\x04mini",
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
