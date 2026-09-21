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

/// Drain until the runtime gives up on a host that cannot be reached.
///
/// The socket worker used to post a disconnect through the bridge. The
/// runtime owns the dial now, so the session's own failed state is the
/// signal, and draining is what surfaces it.
fn awaitFailed(self: *PhuxProvider) !void {
    const started = std.Io.Clock.awake.now(std.testing.io);
    while (self.state() != .failed) {
        if (started.durationTo(std.Io.Clock.awake.now(std.testing.io)).toMilliseconds() >= 30000)
            return error.FailureTimedOut;
        // A drain refuses once the host retires the connection; that is the
        // road to the failure, not a reason to stop waiting for it.
        _ = self.drainReadiness() catch {};
        try std.Io.sleep(std.testing.io, .fromMilliseconds(10), .awake);
    }
    // The reason is recorded by a drain, and the loop above stops the moment
    // the state flips -- which can be between a drain and the check. One
    // more drain is what a running UI would do on its next wake.
    _ = self.drainReadiness() catch {};
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
    // The accept above is what proves which endpoint was dialed; the
    // runtime reads the capture's configuration and keeps no handle here.
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
    // The sibling dialed the same retained endpoint on its own connection,
    // which its own accept above establishes.
    const context = self.context_id;
    const stopping = std.Io.Clock.awake.now(io);
    self.stop();
    try std.testing.expect(stopping.durationTo(std.Io.Clock.awake.now(io)).toMilliseconds() < 5000);
    // Stopping one attachment leaves the other's connection alone.
    try std.testing.expectEqual(provider.Lane.connected, sibling.host.lane);
    // Ordinary reconnect also dials the retained A capability, even after B was
    // saved. It does not implicitly adopt an externally edited pin/token path.
    try self.reconnect(.{});
    try std.testing.expectEqual(@as(usize, 1), try std.posix.poll(&polls, 10000));
    const reconnected = try server.accept(io);
    defer reconnected.close(io);
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
    try awaitFailed(self);
    // A runtime start rejection must retire the newly supplied one-shot capture
    // even though the previous worker still awaits the ordinary stop/join path.
    try self.replaceCapturedTunnel(try remote.Tunnel.resolve("mini", registry.path), identity);
    try std.testing.expectError(error.InvalidState, self.open(.{}));
    try std.testing.expectEqual(null, self.capture.?.pending);
    self.stop();
    try self.replaceCapturedTunnel(try remote.Tunnel.resolve("mini", registry.path), identity);
    try self.reconnect(.{});
    try awaitFailed(self);
    try std.testing.expectEqual(context, self.context_id);
    try std.testing.expect(self.registryIdentity().?.matches(identity));
}

test "an unresolved capture is refused rather than falling back to the alias" {
    var registry = try remote.TestRegistry.init("mini", "ws://127.0.0.1:1");
    defer registry.deinit();
    // "missing" is not in the registry, so its tunnel is FAILED from birth.
    // A capture built from it must refuse: resolving "mini" instead would be
    // exactly the alias fallback a capture exists to prevent.
    const unresolved = try remote.Tunnel.resolve("missing", registry.path);
    try std.testing.expectError(error.CapturedTunnelUnavailable, PhuxProvider.createCaptured(
        std.testing.allocator,
        std.testing.io,
        unresolved,
        .{ .role = 1, .name = "mini", .endpoint = "ws://127.0.0.1:1" },
        "unresolved-test",
    ));
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
    try awaitFailed(self);
    // The runtime reports why the dial failed. The provenance that matters
    // here is structural: the rewritten registry never displaced the
    // captured identity, so a retry still carries the row the user chose.
    // Asserting the captured token PATH in the message needs an enrolled
    // routable fixture, because a loopback dial requires no bearer and so
    // never reads the file (phux-akpf).
    var buffer: [remote.max_text_bytes]u8 = undefined;
    try std.testing.expect(self.remote_status.failureInto(&buffer).len != 0);
    try std.testing.expectEqualStrings("mini", self.registryIdentity().?.name);
    try std.testing.expectEqualStrings("ws://127.0.0.1:1", self.registryIdentity().?.endpoint);
    self.stop();
    try self.replaceCapturedTunnel(try remote.Tunnel.resolve("mini", registry.path), self.registryIdentity().?);
    try self.reconnect(.{});
    try awaitFailed(self);
    try std.testing.expect(std.mem.indexOf(u8, self.remote_status.failureInto(&buffer), "captured-token-file") == null);
}

test "a captured connected provider dials the retained endpoint, not the moved alias" {
    const io = std.testing.io;
    // The listener is the evidence: whichever endpoint is dialed is the one
    // that accepts. It never completes a handshake, and does not need to.
    var listener = try std.Io.net.IpAddress.parse("127.0.0.1", 0);
    var server = try listener.listen(io, .{});
    defer server.deinit(io);
    listener = server.socket.address;
    const endpoint = try std.fmt.allocPrint(std.testing.allocator, "ws://127.0.0.1:{d}", .{listener.getPort()});
    defer std.testing.allocator.free(endpoint);
    var registry = try remote.TestRegistry.init("mini", endpoint);
    defer registry.deinit();

    const self = try fromRegistry(registry.path);
    defer self.destroy();

    // The alias moves after the capture was taken. The capture is authority.
    try rewrite(&registry, "ws://127.0.0.1:1");
    const fresh = try remote.Tunnel.resolve("mini", registry.path);
    defer fresh.close();
    try std.testing.expectEqualStrings("ws://127.0.0.1:1", fresh.describe().endpoint.slice());

    try self.open(.{});
    try std.testing.expectEqual(provider.Lane.connected, self.host.lane);

    // A bounded poll keeps a wrong-alias regression from blocking accept.
    var polls = [_]std.posix.pollfd{.{ .fd = server.socket.handle, .events = std.posix.POLL.IN, .revents = 0 }};
    try std.testing.expectEqual(@as(usize, 1), try std.posix.poll(&polls, 10000));
    const accepted = try server.accept(io);
    defer accepted.close(io);

    // Stopping joins the runtime's driver, so it is bounded, not the network's.
    const stopping = std.Io.Clock.awake.now(io);
    self.stop();
    try std.testing.expect(stopping.durationTo(std.Io.Clock.awake.now(io)).toMilliseconds() < 5000);
    try std.testing.expectEqual(provider.Lane.embedded, self.host.lane);
}
