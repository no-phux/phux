//! Hermetic contracts for adopting the actual checked FFI handle.
const std = @import("std");
const extension = @import("phux_extension");
const remote = extension.remote;
const provider = @import("provider.zig");
const PhuxProvider = provider.PhuxProvider;
const transport = @import("phux_transport");

fn rewrite(registry: *remote.TestRegistry, endpoint: []const u8) !void {
    const body = try std.fmt.allocPrint(std.testing.allocator, "[[remote]]\nname = \"mini\"\nendpoint = \"{s}\"\nsession = \"work\"\n", .{endpoint});
    defer std.testing.allocator.free(body);
    try registry.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config.toml", .data = body });
}

fn awaitDisconnect(bridge: *transport.Bridge) !void {
    const started = std.Io.Clock.awake.now(std.testing.io);
    while (bridge.incoming.takeDisconnect() == null) {
        if (started.durationTo(std.Io.Clock.awake.now(std.testing.io)).toMilliseconds() >= 10000)
            return error.DisconnectTimedOut;
        try std.Io.sleep(std.testing.io, .fromMilliseconds(5), .awake);
    }
}

fn fromRegistry(path: []const u8) !*PhuxProvider {
    const snapshot = try provider.machines.Registry.open(path, 10, 65536);
    defer snapshot.close();
    const row = try snapshot.get(0);
    const tunnel = try snapshot.resolve(0);
    return PhuxProvider.createCaptured(std.testing.allocator, std.testing.io, tunnel, .{
        .role = row.role,
        .name = row.name,
        .endpoint = row.endpoint,
        .session = row.session,
    }, "capture-test");
}

test "captured provider dials endpoint A after registry alias changes to B" {
    const io = std.testing.io;
    // The listener proves which endpoint was actually dialed. It need not run
    // Phux or complete a WebSocket handshake: stop cancels that pending handshake.
    var listener = try std.Io.net.IpAddress.parse("127.0.0.1", 0);
    var server = try listener.listen(io, .{});
    defer server.deinit(io);
    listener = server.socket.address;
    const endpoint = try std.fmt.allocPrint(std.testing.allocator, "ws://127.0.0.1:{d}", .{listener.getPort()});
    defer std.testing.allocator.free(endpoint);
    var registry = try remote.TestRegistry.init("mini", endpoint);
    defer registry.deinit();
    const identity: provider.RegistryIdentity = .{ .role = 1, .name = "mini", .endpoint = endpoint, .session = "work" };
    // The FFI registry is closed before open; both provider strings and the
    // resolved Tunnel must outlive its borrowed row storage.
    const self = try fromRegistry(registry.path);
    defer self.destroy();
    const owned = self.capture.?.pending.?.handle;
    try rewrite(&registry, "ws://127.0.0.1:1");
    const fresh = try remote.Tunnel.resolve("mini", registry.path);
    defer fresh.close();
    try std.testing.expectEqualStrings("ws://127.0.0.1:1", fresh.describe().endpoint.slice());
    try self.open(.{});
    // A bounded OS poll keeps a wrong-alias regression from blocking accept.
    var polls = [_]std.posix.pollfd{.{ .fd = server.socket.handle, .events = std.posix.POLL.IN, .revents = 0 }};
    try std.testing.expectEqual(@as(usize, 1), try std.posix.poll(&polls, 10000));
    const accepted = try server.accept(io);
    defer accepted.close(io);
    try std.testing.expectEqual(owned, self.worker.?.tunnel.?.handle);
    try std.testing.expectEqualStrings(endpoint, self.worker.?.tunnel.?.describe().endpoint.slice());
    try std.testing.expect(self.registryIdentity().?.matches(identity));
    try std.testing.expect(!self.registryIdentity().?.matches(.{ .role = 1, .name = "mini", .endpoint = "ws://127.0.0.1:1" }));
    try std.testing.expectError(error.CapturedTunnelRequired, self.copyTarget(std.testing.allocator));
    const sibling = try self.createSiblingAttachment(std.testing.allocator, io, "sibling-test");
    defer sibling.destroy();
    try std.testing.expect(self.context_id != sibling.context_id);
    try sibling.open(.{});
    try std.testing.expectEqual(@as(usize, 1), try std.posix.poll(&polls, 10000));
    const sibling_socket = try server.accept(io);
    defer sibling_socket.close(io);
    try std.testing.expect(owned != sibling.worker.?.tunnel.?.handle);
    try std.testing.expectEqualStrings(endpoint, sibling.worker.?.tunnel.?.describe().endpoint.slice());
    const context = self.context_id;
    const stopping = std.Io.Clock.awake.now(io);
    self.stop();
    try std.testing.expect(stopping.durationTo(std.Io.Clock.awake.now(io)).toMilliseconds() < 5000);
    try std.testing.expectEqual(remote.State.connecting, sibling.worker.?.tunnel.?.describe().state);
    // Ordinary reconnect also dials the retained A capability, even after B was
    // saved. It does not implicitly adopt an externally edited pin/token path.
    try self.reconnect(.{});
    try std.testing.expectEqual(@as(usize, 1), try std.posix.poll(&polls, 10000));
    const reconnected = try server.accept(io);
    defer reconnected.close(io);
    try std.testing.expectEqualStrings(endpoint, self.worker.?.tunnel.?.describe().endpoint.slice());
    try std.testing.expectEqual(context, self.context_id);
}

test "explicit captured retry replaces checked tunnel and preserves provider context" {
    const endpoint = "ws://127.0.0.1:1";
    var registry = try remote.TestRegistry.init("mini", endpoint);
    defer registry.deinit();
    const identity: provider.RegistryIdentity = .{ .role = 1, .name = "mini", .endpoint = endpoint };
    const self = try PhuxProvider.createCaptured(std.testing.allocator, std.testing.io, try remote.Tunnel.resolve("mini", registry.path), identity, "retry-test");
    defer self.destroy();
    const context = self.context_id;
    try self.open(.{});
    try awaitDisconnect(self.bridge);
    // A runtime start rejection must retire the newly supplied one-shot capture
    // even though the previous worker still awaits the ordinary stop/join path.
    try self.replaceCapturedTunnel(try remote.Tunnel.resolve("mini", registry.path), identity);
    try std.testing.expectError(error.InvalidState, self.open(.{}));
    try std.testing.expectEqual(null, self.capture.?.pending);
    self.stop();
    try self.replaceCapturedTunnel(try remote.Tunnel.resolve("mini", registry.path), identity);
    try self.reconnect(.{});
    try awaitDisconnect(self.bridge);
    try std.testing.expectEqual(context, self.context_id);
    try std.testing.expect(self.registryIdentity().?.matches(identity));
}

test "captured worker start rejects nonremote and allocation failure without a worker" {
    var registry = try remote.TestRegistry.init("mini", "ws://127.0.0.1:1");
    defer registry.deinit();
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    try std.testing.expectError(error.InvalidCapturedEndpoint, extension.Worker.startCaptured(std.testing.io, std.testing.allocator, &bridge, .{}, .{ .unix = "/unused" }, try remote.Tunnel.resolve("mini", registry.path)));
    var failing = std.testing.FailingAllocator.init(std.testing.allocator, .{ .fail_index = 0 });
    try std.testing.expectError(error.OutOfMemory, extension.Worker.startCaptured(std.testing.io, failing.allocator(), &bridge, .{}, .{ .remote = .{ .target = "mini" } }, try remote.Tunnel.resolve("mini", registry.path)));
}

test "captured unresolved tunnel fails through its own reason with no alias fallback" {
    var registry = try remote.TestRegistry.init("mini", "ws://127.0.0.1:1");
    defer registry.deinit();
    var bridge = transport.Bridge.init(std.testing.allocator);
    defer bridge.deinit();
    var status: remote.Status = .{};
    const tunnel = try remote.Tunnel.resolve("missing", registry.path);
    const worker = try extension.Worker.startCaptured(std.testing.io, std.testing.allocator, &bridge, .{}, .{ .remote = .{ .target = "mini", .config_path = registry.path, .status = &status } }, tunnel);
    defer worker.stop();
    try awaitDisconnect(&bridge);
    var buffer: [remote.max_text_bytes]u8 = undefined;
    try std.testing.expect(std.mem.indexOf(u8, status.failureInto(&buffer), "missing") != null);
}

test "captured token file provenance survives registry rewrite" {
    var registry = try remote.TestRegistry.init("mini", "ws://127.0.0.1:1");
    defer registry.deinit();
    const token_path = try std.fmt.allocPrint(std.testing.allocator, "{s}.captured-token-file", .{registry.path});
    defer std.testing.allocator.free(token_path);
    const body = try std.fmt.allocPrint(std.testing.allocator, "[[remote]]\nname = \"mini\"\nendpoint = \"ws://127.0.0.1:1\"\ntoken-file = \"{s}\"\n", .{token_path});
    defer std.testing.allocator.free(body);
    try registry.tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config.toml", .data = body });
    const self = try fromRegistry(registry.path);
    defer self.destroy();
    // The new registry no longer names a token file. Starting the old capture
    // must still fail on its captured file path, before any network dial.
    try rewrite(&registry, "ws://127.0.0.1:1");
    try self.open(.{});
    try awaitDisconnect(self.bridge);
    var buffer: [remote.max_text_bytes]u8 = undefined;
    const message = self.remote_status.failureInto(&buffer);
    try std.testing.expect(std.mem.indexOf(u8, message, "captured-token-file") != null);
    try std.testing.expectEqualStrings("mini", self.registryIdentity().?.name);
    try std.testing.expectEqualStrings("ws://127.0.0.1:1", self.registryIdentity().?.endpoint);
    self.stop();
    try self.replaceCapturedTunnel(try remote.Tunnel.resolve("mini", registry.path), self.registryIdentity().?);
    try self.reconnect(.{});
    try awaitDisconnect(self.bridge);
    try std.testing.expect(std.mem.indexOf(u8, self.remote_status.failureInto(&buffer), "captured-token-file") == null);
}
