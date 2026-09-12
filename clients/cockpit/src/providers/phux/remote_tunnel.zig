//! Zig face of phux-client-ffi's remote-host tunnel (`phux/client.h`,
//! "remote hosts").
//!
//! A registered remote host reaches the socket worker as one end of a
//! Unix-domain socket pair whose other end belongs to a tunnel. The tunnel
//! resolves the host in the phux CLI's own `[[remote]]` registry, dials it
//! over QUIC or TLS WebSocket with the pinned certificate and bearer token,
//! and relays frames byte-for-byte. Everything above the socket is therefore
//! the ordinary framed worker; nothing here decodes a frame.
//!
//! The socket worker owns one `Tunnel` per connection generation. The UI
//! thread uses `describe` for immediate feedback before any worker exists.
//! `Status` is the one record both threads touch.

const std = @import("std");
const posix = std.posix;
const c = @cImport({
    @cInclude("phux/client.h");
});
/// Canonical ABI types for provider-side registry wrappers. Only this extension
/// module owns the FFI include path and Tunnel type; do not re-import this file.
pub fn registryAbi() type {
    return c;
}

/// Display bound for every copied field: a host label, an endpoint URI, or a
/// one-line reason. Longer text is elided on a UTF-8 boundary, never refused,
/// because a reason that lost its tail is still a reason.
pub const max_text_bytes = 240;

pub const State = enum { resolved, connecting, connected, failed, closed };

comptime {
    // The wire values are the header's; `stateFrom` switches on them.
    std.debug.assert(c.PHUX_REMOTE_TUNNEL_RESOLVED == 0);
    std.debug.assert(c.PHUX_REMOTE_TUNNEL_CONNECTING == 1);
    std.debug.assert(c.PHUX_REMOTE_TUNNEL_CONNECTED == 2);
    std.debug.assert(c.PHUX_REMOTE_TUNNEL_FAILED == 3);
    std.debug.assert(c.PHUX_REMOTE_TUNNEL_CLOSED == 4);
}

fn stateFrom(raw: u32) State {
    return switch (raw) {
        0 => .resolved,
        1 => .connecting,
        2 => .connected,
        4 => .closed,
        // FAILED, and any state a newer library adds: never mistake an
        // unknown answer for a usable connection.
        else => .failed,
    };
}

/// A bounded owned copy of borrowed FFI text.
pub const Text = struct {
    bytes: [max_text_bytes]u8 = undefined,
    len: u8 = 0,

    pub fn init(value: []const u8) Text {
        var text: Text = .{};
        text.set(value);
        return text;
    }

    pub fn set(self: *Text, value: []const u8) void {
        var end = @min(value.len, max_text_bytes);
        if (end < value.len) {
            while (end > 0 and (value[end] & 0xc0) == 0x80) end -= 1;
        }
        @memcpy(self.bytes[0..end], value[0..end]);
        self.len = @intCast(end);
    }

    pub fn slice(self: *const Text) []const u8 {
        return self.bytes[0..self.len];
    }
};

/// A tunnel's displayable state, copied out so it outlives the tunnel.
pub const Description = struct {
    state: State = .failed,
    /// The registry entry's name; the typed target when resolution failed.
    name: Text = .{},
    /// Effective endpoint URI. Never a token.
    endpoint: Text = .{},
    /// The entry's pinned session, or empty.
    session: Text = .{},
    /// Why the tunnel failed; empty unless `state == .failed`.
    message: Text = .{},
};

fn span(bytes: []const u8) c.PhuxBytes {
    return .{ .data = bytes.ptr, .len = bytes.len };
}

fn borrowed(bytes: c.PhuxBytes) []const u8 {
    if (bytes.len == 0) return "";
    return bytes.data[0..bytes.len];
}

pub const Tunnel = struct {
    handle: *c.PhuxRemoteTunnel,

    /// New independent tunnel from immutable captured dial configuration only.
    /// No registry lookup, token read, or active socket/worker ownership sharing.
    pub fn cloneResolved(self: Tunnel) error{CapturedTunnelUnavailable}!Tunnel {
        var out: ?*c.PhuxRemoteTunnel = null;
        if (c.phux_remote_tunnel_clone_resolved(self.handle, &out) != c.PHUX_CLIENT_OK)
            return error.CapturedTunnelUnavailable;
        return .{ .handle = out orelse return error.CapturedTunnelUnavailable };
    }

    /// Resolve `target` in the registry at `config_path` (empty: the CLI's
    /// own path). Touches no network. An unregistered host still yields a
    /// tunnel, in the failed state with a reason; only a malformed argument
    /// is an error here.
    pub fn resolve(target: []const u8, config_path: []const u8) error{ InvalidTarget, OutOfMemory }!Tunnel {
        var request = std.mem.zeroes(c.PhuxRemoteTarget);
        request.size = @sizeOf(c.PhuxRemoteTarget);
        request.version = c.PHUX_CLIENT_ABI_VERSION;
        request.target = span(target);
        request.config_path = span(config_path);
        var out: ?*c.PhuxRemoteTunnel = null;
        const result = c.phux_remote_tunnel_resolve(&request, &out);
        if (result == c.PHUX_CLIENT_OUT_OF_MEMORY) return error.OutOfMemory;
        if (result != c.PHUX_CLIENT_OK) return error.InvalidTarget;
        return .{ .handle = out orelse return error.InvalidTarget };
    }

    pub fn describe(self: Tunnel) Description {
        var info = std.mem.zeroes(c.PhuxRemoteTunnelInfo);
        info.size = @sizeOf(c.PhuxRemoteTunnelInfo);
        info.version = c.PHUX_CLIENT_ABI_VERSION;
        if (c.phux_remote_tunnel_info(self.handle, &info) != c.PHUX_CLIENT_OK)
            return .{ .message = Text.init("the remote tunnel's state could not be read") };
        return .{
            .state = stateFrom(info.state),
            .name = Text.init(borrowed(info.name)),
            .endpoint = Text.init(borrowed(info.endpoint)),
            .session = Text.init(borrowed(info.session)),
            .message = Text.init(borrowed(info.message)),
        };
    }

    /// Start dialing through `fd`, one end of a connected Unix-domain socket
    /// pair. Ownership of `fd` transfers on every path, including failure.
    pub fn start(self: Tunnel, fd: posix.fd_t) error{TunnelStartFailed}!void {
        if (c.phux_remote_tunnel_start(self.handle, fd) != c.PHUX_CLIENT_OK) return error.TunnelStartFailed;
    }

    /// Cancel any dial, close the connection, join the tunnel's thread.
    pub fn close(self: Tunnel) void {
        c.phux_remote_tunnel_free(self.handle);
    }
};

/// Resolve, copy, free: immediate feedback for the UI thread, before any
/// worker exists. Reads config.toml only; the token file is read by the
/// tunnel thread when a worker dials.
pub fn describe(target: []const u8, config_path: []const u8) Description {
    const tunnel = Tunnel.resolve(target, config_path) catch |err| return .{
        .name = Text.init(target),
        .message = Text.init(switch (err) {
            error.OutOfMemory => "out of memory",
            error.InvalidTarget => "that is not a host name Cockpit can look up",
        }),
    };
    defer tunnel.close();
    return tunnel.describe();
}

/// The last connection failure for one provider, written by its socket worker
/// and read by the UI. Provider-owned, so it outlives every worker; guarded by
/// a spin lock because both sides hold it only for a bounded copy.
pub const Status = struct {
    mutex: std.atomic.Mutex = .unlocked,
    failure: Text = .{},
    connected_once: bool = false,

    fn lock(self: *const Status) *Status {
        // Interior mutability: readers are logically const, like the
        // provider's paint cache. The lock is the only thing they write.
        const mutable: *Status = @constCast(self);
        while (!mutable.mutex.tryLock()) std.atomic.spinLoopHint();
        return mutable;
    }

    pub fn recordFailure(self: *Status, message: []const u8) void {
        const locked = self.lock();
        defer locked.mutex.unlock();
        locked.failure.set(message);
    }

    pub fn noteConnected(self: *Status) void {
        const locked = self.lock();
        defer locked.mutex.unlock();
        locked.connected_once = true;
        locked.failure.len = 0;
    }

    /// A new host starts with no history: neither a stale reason nor a
    /// "reconnecting" label inherited from the previous host.
    pub fn reset(self: *Status) void {
        const locked = self.lock();
        defer locked.mutex.unlock();
        locked.failure.len = 0;
        locked.connected_once = false;
    }

    pub fn failureInto(self: *const Status, out: []u8) []const u8 {
        const locked = self.lock();
        defer locked.mutex.unlock();
        const text = locked.failure.slice();
        const len = @min(text.len, out.len);
        @memcpy(out[0..len], text[0..len]);
        return out[0..len];
    }

    pub fn connectedOnce(self: *const Status) bool {
        const locked = self.lock();
        defer locked.mutex.unlock();
        return locked.connected_once;
    }
};

/// A disposable phux `config.toml` holding one `[[remote]]` entry, for tests
/// here and in the socket worker. Nothing touches the developer's registry.
pub const TestRegistry = struct {
    tmp: std.testing.TmpDir,
    /// Sentinel-terminated as `realPathFileAlloc` allocates it, so the free
    /// matches the allocation.
    path: [:0]u8,

    pub fn init(name: []const u8, endpoint: []const u8) !TestRegistry {
        var tmp = std.testing.tmpDir(.{});
        errdefer tmp.cleanup();
        const body = try std.fmt.allocPrint(
            std.testing.allocator,
            "[[remote]]\nname = \"{s}\"\nendpoint = \"{s}\"\nsession = \"work\"\n",
            .{ name, endpoint },
        );
        defer std.testing.allocator.free(body);
        try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "config.toml", .data = body });
        const path = try tmp.dir.realPathFileAlloc(std.testing.io, "config.toml", std.testing.allocator);
        return .{ .tmp = tmp, .path = path };
    }

    pub fn deinit(self: *TestRegistry) void {
        std.testing.allocator.free(self.path);
        self.tmp.cleanup();
    }
};

test "an unregistered host is described with the CLI command that pairs it" {
    var registry = try TestRegistry.init("mini", "ws://127.0.0.1:1");
    defer registry.deinit();
    const described = describe("me@studio", registry.path);
    try std.testing.expectEqual(State.failed, described.state);
    try std.testing.expectEqualStrings("me@studio", described.name.slice());
    try std.testing.expect(std.mem.indexOf(u8, described.message.slice(), "phux --remote me@studio") != null);
}

test "a registered host resolves its display fields without dialing" {
    var registry = try TestRegistry.init("mini", "quic://127.0.0.1:8788");
    defer registry.deinit();
    const described = describe("me@mini", registry.path);
    try std.testing.expectEqual(State.resolved, described.state);
    try std.testing.expectEqualStrings("mini", described.name.slice());
    try std.testing.expectEqualStrings("quic://127.0.0.1:8788", described.endpoint.slice());
    try std.testing.expectEqualStrings("work", described.session.slice());
    try std.testing.expectEqual(@as(usize, 0), described.message.slice().len);
}

test "a malformed target is refused at the ABI, not described" {
    try std.testing.expectError(error.InvalidTarget, Tunnel.resolve("mi\x00ni", ""));
    try std.testing.expectError(error.InvalidTarget, Tunnel.resolve("mini", "relative/config.toml"));
    const described = describe("mi\x00ni", "");
    try std.testing.expectEqual(State.failed, described.state);
}

test "copied text is elided on a UTF-8 boundary" {
    var long: [max_text_bytes + 8]u8 = undefined;
    @memset(&long, 'a');
    // A three-byte scalar straddling the bound must not be cut in half.
    @memcpy(long[max_text_bytes - 1 ..][0..3], "\xe2\x80\xa6");
    const text = Text.init(&long);
    try std.testing.expectEqual(@as(usize, max_text_bytes - 1), text.slice().len);
    try std.testing.expect(std.unicode.utf8ValidateSlice(text.slice()));
}

test "status keeps the last failure until a connection or a new host clears it" {
    var status: Status = .{};
    var out: [max_text_bytes]u8 = undefined;
    try std.testing.expectEqual(@as(usize, 0), status.failureInto(&out).len);
    status.recordFailure("mini did not answer");
    try std.testing.expectEqualStrings("mini did not answer", status.failureInto(&out));
    try std.testing.expect(!status.connectedOnce());
    status.noteConnected();
    try std.testing.expect(status.connectedOnce());
    try std.testing.expectEqual(@as(usize, 0), status.failureInto(&out).len);
    status.recordFailure("lost");
    status.reset();
    try std.testing.expect(!status.connectedOnce());
    try std.testing.expectEqual(@as(usize, 0), status.failureInto(&out).len);
}
