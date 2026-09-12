//! Real Model and FFI attachment fixtures; no process launch or user registry.
const std = @import("std");
const runtime = @import("cockpit/native/machine_runtime.zig");
const machines = @import("cockpit/native/machines.zig");
const Engine = @import("cockpit/native/ts_engine.zig").Engine;
const Provider = @import("cockpit/phux_support.zig").PhuxProvider;
const api = @import("phux_provider");
const remote = api.remote_api;
const gpa = std.testing.allocator;
const identity: machines.Identity = .{ .role = .remote, .name = "mini", .endpoint = "ws://127.0.0.1:1", .session = "work" };
const local_identity: machines.Identity = .{ .role = .local, .name = "This Mac", .endpoint = "", .session = "" };

const Tracked = struct {
    var closed: usize = 0;
    fn release(tunnel: machines.Tunnel) void {
        closed += 1;
        tunnel.close();
    }
};
const Adapter = runtime.AdapterWithRelease(Tracked.release);

const Fixture = struct {
    engine: *Engine,
    registry: remote.TestRegistry,
    adopted: usize = 0,
    retried: usize = 0,
    disconnected: usize = 0,
    browsed: usize = 0,
    reject_adoption: bool = false,
    retire_window_before_disconnect: bool = false,
    targets: [16]runtime.Target = undefined,
    target_count: usize = 0,

    fn create() !*Fixture {
        const self = try gpa.create(Fixture);
        errdefer gpa.destroy(self);
        const engine = try Engine.create(gpa, std.testing.io);
        errdefer engine.destroy();
        self.* = .{ .engine = engine, .registry = try remote.TestRegistry.init("mini", identity.endpoint) };
        Tracked.closed = 0;
        return self;
    }

    fn destroy(self: *Fixture) void {
        self.engine.destroy();
        self.registry.deinit();
        gpa.destroy(self);
    }

    fn adapter(self: *Fixture) Adapter {
        return .{
            .model = self.engine.model,
            .gpa = gpa,
            .io = std.testing.io,
            .origin = .{ .window = 0, .epoch = self.engine.model.window_epochs[0] },
            .hooks = .{ .userdata = self, .adoptCaptured = adopt, .retryCaptured = retry, .disconnectCaptured = disconnect, .browse = browse },
        };
    }

    fn tunnel(self: *Fixture) !machines.Tunnel {
        return self.tunnelFor(identity);
    }

    fn tunnelFor(self: *Fixture, chosen: machines.Identity) !machines.Tunnel {
        const body = try std.fmt.allocPrint(gpa, "[[remote]]\nname = \"{s}\"\nendpoint = \"{s}\"\nsession = \"{s}\"\n", .{ chosen.name, chosen.endpoint, chosen.session });
        defer gpa.free(body);
        try self.registry.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config.toml", .data = body });
        const registry = try api.machines.Registry.open(self.registry.path, 100, 65536);
        defer registry.close();
        return registry.resolve(0);
    }

    fn add(self: *Fixture, chosen: machines.Identity) !*Provider {
        const provider = try Provider.createCaptured(gpa, std.testing.io, try self.tunnelFor(chosen), runtime.registryIdentity(chosen), "fixture");
        errdefer provider.destroy();
        provider.standBy();
        const slot = try self.engine.model.freePeerSlot();
        self.engine.model.peers.items[slot].provider = provider;
        return provider;
    }

    fn negotiate(provider: *Provider) !void {
        try provider.host.start("fixture");
        try std.testing.expect(provider.bridge.incoming.stage(@embedFile("tests/fixtures/hello.bin")));
        _ = try provider.host.drainReadiness();
        try std.testing.expectEqual(api.State.negotiated, provider.state());
    }

    fn from(raw: ?*anyopaque) *Fixture {
        return @ptrCast(@alignCast(raw.?));
    }

    fn adopt(raw: ?*anyopaque, provider: *Provider) !void {
        const self = from(raw);
        errdefer provider.destroy();
        self.adopted += 1;
        if (self.reject_adoption) return error.Refused;
        const slot = try self.engine.model.freePeerSlot();
        self.engine.model.peers.items[slot].provider = provider;
    }

    fn retry(raw: ?*anyopaque, target: runtime.Target, tunnel_value: machines.Tunnel, chosen: runtime.RegistryIdentity) !void {
        const self = from(raw);
        if (!target.matches(self.engine.model)) {
            tunnel_value.close();
            return error.Stale;
        }
        const provider = self.find(target.attachment_id).?;
        provider.stop();
        try provider.replaceCapturedTunnel(tunnel_value, chosen);
        self.retried += 1;
    }

    fn find(self: *Fixture, context: u64) ?*Provider {
        if (self.engine.model.phux()) |active| if (active.context_id == context) return active;
        for (self.engine.model.peers.items) |peer| {
            const provider = peer.provider orelse continue;
            if (provider.context_id == context) return provider;
        }
        return null;
    }

    fn copyTargets(self: *Fixture, targets: []const runtime.Target) !void {
        try std.testing.expect(targets.len <= self.targets.len);
        @memcpy(self.targets[0..targets.len], targets);
        self.target_count = targets.len;
    }

    fn disconnect(raw: ?*anyopaque, targets: []const runtime.Target) !void {
        const self = from(raw);
        try self.copyTargets(targets);
        if (self.retire_window_before_disconnect) {
            self.engine.model.closeWindow(1);
            _ = self.engine.model.openWindow(1) orelse return error.OutOfMemory;
        }
        // This is the runtime hook contract: the entire batch is checked before
        // any provider is stopped, including a reused originating window slot.
        for (targets) |target| if (!target.matches(self.engine.model)) return error.Stale;
        for (targets) |target| self.drop(target.attachment_id);
    }

    fn drop(self: *Fixture, context: u64) void {
        if (self.engine.model.phux()) |active| {
            if (active.context_id == context) {
                active.destroy();
                self.engine.model.phux_provider = null;
                self.disconnected += 1;
                return;
            }
        }
        for (self.engine.model.peers.items) |peer| {
            const provider = peer.provider orelse continue;
            if (provider.context_id != context) continue;
            provider.destroy();
            peer.provider = null;
            self.disconnected += 1;
            return;
        }
    }

    fn browse(raw: ?*anyopaque, selection: runtime.Browse) !void {
        const self = from(raw);
        if (!selection.valid(self.engine.model)) return error.Stale;
        try self.copyTargets(selection.targets);
        self.browsed += 1;
    }
};

test "machine runtime aggregates actual captured connections without joining edited aliases" {
    if (comptime !@import("cockpit/phux_support.zig").phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.create();
    defer fixture.destroy();
    var adapter = fixture.adapter();
    try std.testing.expectEqual(machines.Connection.not_connected, adapter.status(identity).state);
    const failed = try fixture.add(identity);
    failed.stop();
    try std.testing.expectEqual(machines.Connection.failed, adapter.status(identity).state);
    const healthy = try fixture.add(identity);
    try Fixture.negotiate(healthy);
    const status = adapter.status(identity);
    try std.testing.expectEqual(machines.Connection.connected, status.state);
    try std.testing.expectEqualStrings("Some attachments are disconnected", status.message);
    var edited = identity;
    edited.endpoint = "ws://127.0.0.1:2";
    try std.testing.expectEqual(machines.Connection.not_connected, adapter.status(edited).state);
    edited = identity;
    edited.session = "other";
    try std.testing.expectEqual(machines.Connection.not_connected, adapter.status(edited).state);
    edited = identity;
    edited.role = .satellite;
    try std.testing.expectEqual(machines.Connection.not_connected, adapter.status(edited).state);
    // An ordinary alias-backed provider cannot prove registry identity.
    fixture.engine.model.phux_provider = try Provider.create(gpa, std.testing.io, .{ .remote = .{ .target = "mini" } }, null, "legacy");
    try Fixture.negotiate(fixture.engine.model.phux_provider.?);
    healthy.stop();
    try std.testing.expectEqual(machines.Connection.failed, adapter.status(identity).state);
}

test "machine runtime consumes rejected and deduplicated real FFI tunnels exactly once" {
    if (comptime !@import("cockpit/phux_support.zig").phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.create();
    defer fixture.destroy();
    var adapter = fixture.adapter();
    adapter.hooks = .{};
    try std.testing.expectEqual(machines.ReplyStatus.unsupported, adapter.action(.connect, identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 1), Tracked.closed);
    adapter = fixture.adapter();
    adapter.origin.window = 1;
    try std.testing.expectEqual(machines.ReplyStatus.stale, adapter.action(.connect, identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 2), Tracked.closed);
    adapter = fixture.adapter();
    const context = adapter.context();
    try std.testing.expectEqual(machines.ReplyStatus.unsupported, context.action.?(null, .connect, identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 3), Tracked.closed);
    var unsupported = identity;
    unsupported.role = .satellite;
    try std.testing.expectEqual(machines.ReplyStatus.unsupported, adapter.action(.connect, unsupported, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 4), Tracked.closed);
    try std.testing.expectEqual(machines.ReplyStatus.ok, adapter.action(.connect, identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 1), fixture.adopted);
    try std.testing.expectEqual(@as(usize, 4), Tracked.closed);
    try std.testing.expectEqual(machines.Connection.connecting, context.status.?(context.userdata, identity).state);
    try std.testing.expectEqual(machines.ReplyStatus.ok, adapter.action(.connect, identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(machines.ReplyStatus.ok, adapter.action(.retry, identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 6), Tracked.closed);
    try std.testing.expectEqual(@as(usize, 1), fixture.adopted);
}

test "machine runtime adoption failure and retry preserve ownership and exact attachment" {
    if (comptime !@import("cockpit/phux_support.zig").phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.create();
    defer fixture.destroy();
    var adapter = fixture.adapter();
    fixture.reject_adoption = true;
    try std.testing.expectEqual(machines.ReplyStatus.failed, adapter.action(.connect, identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 0), Tracked.closed); // consuming adoption hook destroyed it
    try std.testing.expectEqual(@as(usize, 0), fixture.engine.model.peers.items.len);
    const provider = try fixture.add(identity);
    const context = provider.context_id;
    provider.stop();
    try std.testing.expectEqual(machines.ReplyStatus.ok, adapter.action(.retry, identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 1), fixture.retried);
    try std.testing.expectEqual(context, fixture.engine.model.phuxPeer().?.context_id);
    try std.testing.expect(provider.registryIdentity().?.matches(runtime.registryIdentity(identity)));
    try std.testing.expectEqual(@as(usize, 0), Tracked.closed); // retry hook adopted the supplied handle
}

test "machine runtime disconnect fences every matching attachment and the originating window" {
    if (comptime !@import("cockpit/phux_support.zig").phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.create();
    defer fixture.destroy();
    const first = try fixture.add(identity);
    _ = try fixture.add(identity);
    fixture.engine.model.phux_provider = try Provider.createCaptured(gpa, std.testing.io, try fixture.tunnel(), runtime.registryIdentity(identity), "active");
    var other_identity = identity;
    other_identity.session = "other";
    // The row payload itself is a separate capture; comparison never uses alias.
    const other = try fixture.add(other_identity);
    const other_context = other.context_id;
    _ = fixture.engine.model.openWindow(1) orelse return error.OutOfMemory;
    var adapter = fixture.adapter();
    adapter.origin = .{ .window = 1, .epoch = fixture.engine.model.window_epochs[1] };
    fixture.retire_window_before_disconnect = true;
    try std.testing.expectEqual(machines.ReplyStatus.failed, adapter.action(.disconnect, identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 1), Tracked.closed);
    try std.testing.expectEqual(@as(usize, 3), fixture.target_count);
    try std.testing.expectEqual(@as(usize, 0), fixture.disconnected);
    try std.testing.expect(fixture.find(first.context_id) != null);
    fixture.retire_window_before_disconnect = false;
    adapter.origin.epoch = fixture.engine.model.window_epochs[1];
    try std.testing.expectEqual(machines.ReplyStatus.ok, adapter.action(.disconnect, identity, null).status);
    try std.testing.expectEqual(@as(usize, 3), fixture.disconnected);
    try std.testing.expectEqual(other_context, fixture.engine.model.phuxPeer().?.context_id);
    try std.testing.expectEqual(machines.Connection.not_connected, adapter.status(identity).state);
}

test "machine runtime browse retains exact identity and refuses replaced source or window" {
    if (comptime !@import("cockpit/phux_support.zig").phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.create();
    defer fixture.destroy();
    const provider = try fixture.add(identity);
    try Fixture.negotiate(provider);
    var adapter = fixture.adapter();
    try std.testing.expectEqual(machines.ReplyStatus.ok, adapter.action(.browse, identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 1), Tracked.closed);
    try std.testing.expectEqual(@as(usize, 1), fixture.target_count);
    const selected: runtime.Browse = .{ .identity = identity, .targets = fixture.targets[0..fixture.target_count], .origin = adapter.origin };
    try std.testing.expect(selected.valid(fixture.engine.model));
    var changed = identity;
    changed.endpoint = "ws://127.0.0.1:2";
    try std.testing.expectEqual(machines.ReplyStatus.stale, adapter.action(.browse, changed, null).status);
    try std.testing.expectEqual(@as(usize, 1), fixture.browsed);
    try provider.host.reconnect("fixture");
    try std.testing.expect(!selected.valid(fixture.engine.model));
}

test "machine runtime This Mac requires an actual local Phux provider" {
    if (comptime !@import("cockpit/phux_support.zig").phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.create();
    defer fixture.destroy();
    var adapter = fixture.adapter();
    try std.testing.expectEqual(machines.Connection.not_connected, adapter.status(local_identity).state);
    fixture.engine.model.phux_provider = try Provider.create(gpa, std.testing.io, .{ .remote = .{ .target = "ambient-remote" } }, null, "fixture");
    try Fixture.negotiate(fixture.engine.model.phux_provider.?);
    try std.testing.expectEqual(machines.Connection.not_connected, adapter.status(local_identity).state);
    const local = try Provider.create(gpa, std.testing.io, .{ .unix = "/fixture-local.sock" }, null, "fixture");
    const slot = try fixture.engine.model.freePeerSlot();
    fixture.engine.model.peers.items[slot].provider = local;
    try Fixture.negotiate(local);
    try std.testing.expectEqual(machines.Connection.connected, adapter.status(local_identity).state);
    try std.testing.expectEqual(machines.ReplyStatus.ok, adapter.action(.browse, local_identity, null).status);
    try std.testing.expectEqual(local.context_id, fixture.targets[0].attachment_id);
    try std.testing.expectEqual(machines.ReplyStatus.unsupported, adapter.action(.disconnect, local_identity, try fixture.tunnel()).status);
    try std.testing.expectEqual(@as(usize, 1), Tracked.closed);
}

test "machine runtime changed alias connects separately and cannot disconnect old authority" {
    if (comptime !@import("cockpit/phux_support.zig").phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.create();
    defer fixture.destroy();
    const old = try fixture.add(identity);
    try Fixture.negotiate(old);
    const old_context = old.context_id;
    var changed = identity;
    changed.endpoint = "ws://127.0.0.1:2";
    var adapter = fixture.adapter();
    try std.testing.expectEqual(machines.ReplyStatus.stale, adapter.action(.disconnect, changed, null).status);
    try std.testing.expectEqual(@as(usize, 0), fixture.disconnected);
    try std.testing.expectEqual(machines.ReplyStatus.ok, adapter.action(.connect, changed, try fixture.tunnelFor(changed)).status);
    try std.testing.expectEqual(@as(usize, 1), fixture.adopted);
    try std.testing.expectEqual(@as(usize, 2), fixture.engine.model.peers.items.len);
    try std.testing.expectEqual(machines.Connection.connected, adapter.status(identity).state);
    try std.testing.expectEqual(machines.ReplyStatus.ok, adapter.action(.disconnect, changed, null).status);
    try std.testing.expectEqual(@as(usize, 1), fixture.disconnected);
    try std.testing.expectEqual(old_context, fixture.engine.model.phuxPeer().?.context_id);
    try std.testing.expectEqual(api.State.negotiated, old.state());
}

test "machine runtime disabled context compiles and refuses actions" {
    if (comptime @import("cockpit/phux_support.zig").phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(gpa, std.testing.io);
    defer engine.destroy();
    var adapter: runtime.Adapter = .{
        .model = engine.model,
        .gpa = gpa,
        .io = std.testing.io,
        .origin = .{ .window = 0, .epoch = engine.model.window_epochs[0] },
    };
    const context = adapter.context();
    try std.testing.expectEqual(machines.Connection.not_connected, context.status.?(context.userdata, identity).state);
    try std.testing.expectEqual(machines.ReplyStatus.unsupported, context.action.?(context.userdata, .connect, identity, null).status);
}
