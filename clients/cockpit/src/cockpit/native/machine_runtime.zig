//! Machines inventory joins actual captured attachments, never registry aliases.
//! Lifecycle writes belong to the owning Engine hooks. All callbacks are
//! synchronous; queued continuations must copy their borrowed identity/targets.
const std = @import("std");
const machines = @import("machines.zig");
const support = @import("../phux_support.zig");
const model_module = @import("../model.zig");
const windows = @import("ts_window_navigation.zig");
const Model = model_module.Model;
const Provider = support.PhuxProvider;
pub const RegistryIdentity = if (support.phux_enabled) @import("phux_provider").RegistryIdentity else struct {};

pub const Target = struct {
    attachment_id: u64,
    source_context: u64,
    connection_epoch: u64,
    identity: machines.Identity,
    origin: windows.Target,

    /// Slot/alias reuse and reconnect invalidate an already captured action.
    pub fn matches(self: Target, model: *const Model) bool {
        if (comptime !support.phux_enabled) return false;
        if (!self.origin.validWindow(model)) return false;
        var iterator: Iterator = .{ .model = model };
        while (iterator.next()) |entry| {
            if (entry.provider.context_id != self.attachment_id) continue;
            if (entry.provider.host.context_id != self.source_context) return false;
            if (entry.provider.connectionEpoch() != self.connection_epoch) return false;
            return matchesIdentity(model, entry.provider, self.identity);
        }
        return false;
    }
};

pub const Browse = struct {
    identity: machines.Identity,
    targets: []const Target,
    origin: windows.Target,

    pub fn valid(self: Browse, model: *const Model) bool {
        if (comptime !support.phux_enabled) return false;
        if (!self.origin.validWindow(model)) return false;
        if (self.targets.len == 0) return false;
        for (self.targets) |target| {
            if (!registryIdentity(self.identity).matches(registryIdentity(target.identity))) return false;
            if (!target.matches(model)) return false;
        }
        return true;
    }
};

pub const Hooks = struct {
    userdata: ?*anyopaque = null,
    /// Ownership of provider transfers on EVERY return, including rejection.
    adoptCaptured: ?*const fn (?*anyopaque, *Provider) anyerror!void = null,
    /// Ownership of tunnel transfers on EVERY return. Revalidate target before
    /// replacing its exact provider; never recover it by alias or array slot.
    retryCaptured: ?*const fn (?*anyopaque, Target, machines.Tunnel, RegistryIdentity) anyerror!void = null,
    /// Preflight ALL targets (including origin/window lifetime) before any
    /// provider/window mutation. Stop all matching independent attachments only.
    disconnectCaptured: ?*const fn (?*anyopaque, []const Target) anyerror!void = null,
    /// Opens Sessions for these exact attachments. Display names are not filters
    /// with authority; retain/revalidate Browse if navigation completes later.
    browse: ?*const fn (?*anyopaque, Browse) anyerror!void = null,
};

pub const Adapter = AdapterWithRelease(closeOwnedTunnel);

/// The release operation is injectable so tests count real FFI closes while
/// exercising the same adapter/Model path. Shipping always uses closeOwnedTunnel.
pub fn AdapterWithRelease(comptime release: fn (machines.Tunnel) void) type {
    return struct {
        const Self = @This();
        model: *const Model,
        gpa: std.mem.Allocator,
        io: std.Io,
        origin: windows.Target,
        hooks: Hooks = .{},
        // Status and action strings must survive separate callbacks during encoding.
        status_message: [240]u8 = undefined,

        pub fn context(self: *Self) machines.Context {
            return .{ .userdata = self, .status = statusCallback, .action = actionCallback };
        }

        pub fn status(self: *Self, identity: machines.Identity) machines.Status {
            if (comptime !support.phux_enabled) return .{};
            const summary = self.summarize(identity);
            if (summary.failed != 0 and summary.state != .failed)
                return .{ .state = summary.state, .message = "Some attachments are disconnected" };
            if (summary.state != .failed) return .{ .state = summary.state };
            const entry = summary.best orelse return .{};
            const message = entry.provider.remoteFailure(&self.status_message);
            return .{ .state = .failed, .message = if (message.len == 0) "The machine connection was lost" else message };
        }

        /// Even unsupported, stale, deduplicated and null-hook paths consume Tunnel.
        pub fn action(self: *Self, action_kind: machines.Action, identity: machines.Identity, supplied: ?machines.Tunnel) machines.ActionResult {
            var tunnel = supplied;
            defer releaseIfPresent(tunnel);
            if (comptime !support.phux_enabled) return unavailable;
            if (!self.origin.validWindow(self.model)) return stale;
            switch (action_kind) {
                .connect, .retry => return self.connect(identity, &tunnel),
                .disconnect => return self.disconnect(identity),
                .browse => return self.browse(identity),
            }
        }

        fn connect(self: *Self, identity: machines.Identity, tunnel: *?machines.Tunnel) machines.ActionResult {
            if (identity.role != .remote) return unavailable;
            const summary = self.summarize(identity);
            if (isLive(summary.state)) return .{};
            if (summary.best) |entry| return self.retry(entry, identity, tunnel);
            const callback = self.hooks.adoptCaptured orelse return unavailable;
            const owned = takeTunnel(tunnel) orelse return stale;
            const provider = Provider.createCaptured(self.gpa, self.io, owned, registryIdentity(identity), "Phux Cockpit") catch return failed;
            provider.standBy();
            callback(self.hooks.userdata, provider) catch return failed;
            return .{};
        }

        fn retry(self: *Self, entry: Entry, identity: machines.Identity, tunnel: *?machines.Tunnel) machines.ActionResult {
            const callback = self.hooks.retryCaptured orelse return unavailable;
            const target = self.captureTarget(entry, identity);
            if (!target.matches(self.model)) return stale;
            const owned = takeTunnel(tunnel) orelse return stale;
            callback(self.hooks.userdata, target, owned, registryIdentity(identity)) catch return failed;
            return .{};
        }

        fn disconnect(self: *Self, identity: machines.Identity) machines.ActionResult {
            if (identity.role != .remote) return unavailable;
            const callback = self.hooks.disconnectCaptured orelse return unavailable;
            const targets = self.collect(identity) catch return failed;
            defer self.gpa.free(targets);
            if (targets.len == 0) return stale;
            callback(self.hooks.userdata, targets) catch return failed;
            return .{};
        }

        fn browse(self: *Self, identity: machines.Identity) machines.ActionResult {
            const callback = self.hooks.browse orelse return unavailable;
            const targets = self.collect(identity) catch return failed;
            defer self.gpa.free(targets);
            const selection: Browse = .{ .identity = identity, .targets = targets, .origin = self.origin };
            if (!selection.valid(self.model)) return stale;
            callback(self.hooks.userdata, selection) catch return failed;
            return .{};
        }

        fn captureTarget(self: *const Self, entry: Entry, identity: machines.Identity) Target {
            return .{ .attachment_id = entry.provider.context_id, .source_context = entry.provider.host.context_id, .connection_epoch = entry.provider.connectionEpoch(), .identity = identity, .origin = self.origin };
        }

        fn collect(self: *const Self, identity: machines.Identity) ![]Target {
            var result: std.ArrayList(Target) = .empty;
            errdefer result.deinit(self.gpa);
            var iterator: Iterator = .{ .model = self.model };
            while (iterator.next()) |entry| {
                if (!matchesIdentity(self.model, entry.provider, identity)) continue;
                try result.append(self.gpa, self.captureTarget(entry, identity));
            }
            return result.toOwnedSlice(self.gpa);
        }

        fn summarize(self: *const Self, identity: machines.Identity) Summary {
            var result: Summary = .{};
            var iterator: Iterator = .{ .model = self.model };
            while (iterator.next()) |entry| {
                if (!matchesIdentity(self.model, entry.provider, identity)) continue;
                const current = connection(entry);
                if (current == .failed) result.failed += 1;
                if (rank(current) <= rank(result.state)) continue;
                result.state = current;
                result.best = entry;
            }
            return result;
        }

        fn releaseIfPresent(tunnel: ?machines.Tunnel) void {
            if (comptime support.phux_enabled) if (tunnel) |owned| release(owned);
        }

        fn statusCallback(userdata: ?*anyopaque, identity: machines.Identity) machines.Status {
            const self: *Self = @ptrCast(@alignCast(userdata orelse return .{}));
            return self.status(identity);
        }

        fn actionCallback(userdata: ?*anyopaque, action_kind: machines.Action, identity: machines.Identity, tunnel: ?machines.Tunnel) machines.ActionResult {
            const raw = userdata orelse {
                releaseIfPresent(tunnel);
                return unavailable;
            };
            const self: *Self = @ptrCast(@alignCast(raw));
            return self.action(action_kind, identity, tunnel);
        }
    };
}

const Entry = struct { provider: *const Provider, failed: bool, reopening: bool };
const Summary = struct { state: machines.Connection = .not_connected, best: ?Entry = null, failed: usize = 0 };

const Iterator = struct {
    model: *const Model,
    position: usize = 0,

    fn next(self: *Iterator) ?Entry {
        if (self.position == 0) {
            self.position = 1;
            if (self.model.phuxConst()) |provider| return .{ .provider = provider, .failed = self.model.phux_connection_unavailable, .reopening = self.model.phux_reconnect_after_close };
        }
        while (self.position <= self.model.peers.items.len) {
            const slot = self.position - 1;
            self.position += 1;
            const peer = self.model.peers.items[slot];
            if (peer.provider) |provider| return .{ .provider = provider, .failed = peer.failed, .reopening = peer.reopen };
        }
        return null;
    }
};

pub fn registryIdentity(identity: machines.Identity) RegistryIdentity {
    if (comptime !support.phux_enabled) return .{};
    return .{ .role = @intFromEnum(identity.role), .name = identity.name, .endpoint = identity.endpoint, .session = identity.session };
}

fn matchesIdentity(model: *const Model, provider: *const Provider, identity: machines.Identity) bool {
    if (comptime !support.phux_enabled) return false;
    if (identity.role == .local) return localIdentity(model, provider, identity);
    if (identity.role != .remote) return false;
    const captured = provider.registryIdentity() orelse return false;
    return captured.matches(registryIdentity(identity));
}

fn localIdentity(model: *const Model, provider: *const Provider, identity: machines.Identity) bool {
    if (!std.mem.eql(u8, identity.name, "This Mac")) return false;
    if (identity.endpoint.len != 0 or identity.session.len != 0) return false;
    if (provider.pending_retarget != null or provider.endpoint != .unix) return false;
    const configured = model.config.phux_socket.slice();
    return configured.len == 0 or std.mem.eql(u8, configured, provider.endpoint.unix);
}

fn connection(entry: Entry) machines.Connection {
    if (entry.reopening) return .reconnecting;
    if (entry.failed) return .failed;
    return switch (entry.provider.state()) {
        .attached, .negotiated => .connected,
        .new, .hello_queued => if (entry.provider.remoteConnectedOnce()) .reconnecting else .connecting,
        .detached, .failed => .failed,
    };
}

fn isLive(state: machines.Connection) bool {
    return switch (state) {
        .connected, .connecting, .reconnecting => true,
        else => false,
    };
}

fn rank(state: machines.Connection) u8 {
    return switch (state) {
        .not_connected => 0,
        .failed => 1,
        .connecting => 2,
        .reconnecting => 3,
        .connected => 4,
    };
}

fn takeTunnel(tunnel: *?machines.Tunnel) ?machines.Tunnel {
    const result = tunnel.*;
    tunnel.* = null;
    return result;
}

fn closeOwnedTunnel(tunnel: machines.Tunnel) void {
    if (comptime support.phux_enabled) tunnel.close();
}

const unavailable: machines.ActionResult = .{ .status = .unsupported, .message = "This machine action is unavailable" };
const stale: machines.ActionResult = .{ .status = .stale, .message = "Machine or window changed; refresh and select again" };
const failed: machines.ActionResult = .{ .status = .failed, .message = "Machine action failed; refresh Machines for current state" };
