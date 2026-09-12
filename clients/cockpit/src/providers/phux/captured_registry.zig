//! Stable, credential-free identity and retained immutable dial capability.
const std = @import("std");
const remote = @import("phux_extension").remote;

pub const Identity = struct {
    role: u32,
    name: []const u8,
    endpoint: []const u8,
    session: []const u8 = "",

    /// A retargeted alias or changed default session is a different capture.
    pub fn matches(self: Identity, other: Identity) bool {
        if (self.role != other.role) return false;
        if (!std.mem.eql(u8, self.name, other.name)) return false;
        if (!std.mem.eql(u8, self.endpoint, other.endpoint)) return false;
        return std.mem.eql(u8, self.session, other.session);
    }

    fn copy(self: Identity, gpa: std.mem.Allocator) !Identity {
        const name = try gpa.dupe(u8, self.name);
        errdefer gpa.free(name);
        const endpoint = try gpa.dupe(u8, self.endpoint);
        errdefer gpa.free(endpoint);
        const session = try gpa.dupe(u8, self.session);
        return .{ .role = self.role, .name = name, .endpoint = endpoint, .session = session };
    }

    fn deinit(self: Identity, gpa: std.mem.Allocator) void {
        gpa.free(self.name);
        gpa.free(self.endpoint);
        gpa.free(self.session);
    }
};

pub const Capture = OwnedCapture(remote.Tunnel);

/// The type parameter lets ownership tests count closes without dialing or
/// dereferencing an already-freed FFI handle. Production uses only remote.Tunnel.
fn OwnedCapture(comptime Tunnel: type) type {
    return struct {
        const Self = @This();
        identity: Identity,
        template: Tunnel,
        pending: ?Tunnel,

        /// Consumes the tunnel on every return. Identity is copied, while the
        /// opaque tunnel retains endpoint, certificate pin and token provenance.
        pub fn init(gpa: std.mem.Allocator, tunnel: Tunnel, identity: Identity) !Self {
            errdefer tunnel.close();
            if (identity.role != 1) return error.InvalidRegistryIdentity;
            if (identity.name.len == 0) return error.InvalidRegistryIdentity;
            if (identity.endpoint.len == 0) return error.InvalidRegistryIdentity;
            const template = try tunnel.cloneResolved();
            errdefer template.close();
            return .{ .identity = try identity.copy(gpa), .template = template, .pending = tunnel };
        }

        pub fn deinit(self: *Self, gpa: std.mem.Allocator) void {
            self.cancel();
            self.template.close();
            self.identity.deinit(gpa);
        }

        pub fn cancel(self: *Self) void {
            const tunnel = self.pending orelse return;
            self.pending = null;
            tunnel.close();
        }

        /// First start adopts the original checked handle. Subsequent generations
        /// get independent tunnels from its retained immutable dial configuration.
        /// Machines Retry explicitly replaces that configuration after validation.
        pub fn take(self: *Self) !Tunnel {
            const tunnel = self.pending orelse return self.cloneTunnel();
            self.pending = null;
            return tunnel;
        }

        pub fn cloneTunnel(self: *const Self) !Tunnel {
            return self.template.cloneResolved();
        }

        /// Consumes on every return, including a changed alias, role or session.
        pub fn replace(self: *Self, tunnel: Tunnel, identity: Identity) !void {
            errdefer tunnel.close();
            if (!self.identity.matches(identity)) return error.InvalidRegistryIdentity;
            const template = try tunnel.cloneResolved();
            self.cancel();
            self.template.close();
            self.template = template;
            self.pending = tunnel;
        }
    };
}

const FakeState = struct {
    created: usize = 0,
    closed: [16]bool = @splat(false),

    fn make(self: *FakeState) FakeTunnel {
        const id = self.created;
        self.created += 1;
        return .{ .state = self, .id = id };
    }

    fn expectAllClosed(self: FakeState) !void {
        for (self.closed[0..self.created]) |closed| try std.testing.expect(closed);
    }
};

const FakeTunnel = struct {
    state: *FakeState,
    id: usize,

    pub fn cloneResolved(self: FakeTunnel) error{}!FakeTunnel {
        std.debug.assert(!self.state.closed[self.id]);
        return self.state.make();
    }

    pub fn close(self: FakeTunnel) void {
        std.debug.assert(!self.state.closed[self.id]);
        self.state.closed[self.id] = true;
    }
};

test "capture identity survives registry edits and joins exact role name endpoint" {
    var name = "mini".*;
    var endpoint = "wss://endpointA".*;
    var state: FakeState = .{};
    var capture = try OwnedCapture(FakeTunnel).init(std.testing.allocator, state.make(), .{ .role = 1, .name = &name, .endpoint = &endpoint });
    defer capture.deinit(std.testing.allocator);
    name[0] = 'x';
    endpoint[endpoint.len - 1] = 'B';
    try std.testing.expect(capture.identity.matches(.{ .role = 1, .name = "mini", .endpoint = "wss://endpointA" }));
    try std.testing.expect(!capture.identity.matches(.{ .role = 1, .name = "mini", .endpoint = &endpoint }));
    try std.testing.expect(!capture.identity.matches(.{ .role = 2, .name = "mini", .endpoint = "wss://endpointA" }));
}

test "capture rejection cancellation retry and sibling transfer close each owned tunnel once" {
    const gpa = std.testing.allocator;
    const identity: Identity = .{ .role = 1, .name = "mini", .endpoint = "wss://endpointA" };
    var state: FakeState = .{};
    const first = state.make();
    var capture = try OwnedCapture(FakeTunnel).init(gpa, first, identity);
    const rejected = state.make();
    try std.testing.expectError(error.InvalidRegistryIdentity, capture.replace(rejected, .{ .role = 1, .name = "mini", .endpoint = "wss://endpointB" }));
    try std.testing.expect(state.closed[rejected.id]);
    const replacement = state.make();
    try capture.replace(replacement, identity);
    try std.testing.expect(state.closed[first.id]);
    const transferred = try capture.take();
    try std.testing.expectEqual(replacement.id, transferred.id);
    const reconnected = try capture.take();
    try std.testing.expect(reconnected.id != transferred.id);
    capture.cancel();
    capture.deinit(gpa);
    try std.testing.expect(!state.closed[transferred.id]);
    try std.testing.expect(!state.closed[reconnected.id]);
    transferred.close();
    reconnected.close();
    try state.expectAllClosed();

    var unopened = try OwnedCapture(FakeTunnel).init(gpa, state.make(), identity);
    unopened.cancel();
    unopened.cancel();
    unopened.deinit(gpa);
    try state.expectAllClosed();
}

test "capture initialization consumes tunnel on invalid identity and allocation failure" {
    const CaptureTest = OwnedCapture(FakeTunnel);
    var state: FakeState = .{};
    try std.testing.expectError(error.InvalidRegistryIdentity, CaptureTest.init(std.testing.allocator, state.make(), .{ .role = 2, .name = "mini", .endpoint = "endpoint" }));
    try state.expectAllClosed();
    var failing = std.testing.FailingAllocator.init(std.testing.allocator, .{ .fail_index = 1 });
    try std.testing.expectError(error.OutOfMemory, CaptureTest.init(failing.allocator(), state.make(), .{ .role = 1, .name = "mini", .endpoint = "endpoint" }));
    try state.expectAllClosed();
}

test "capture replacement rejects changed session without replacing retained capability" {
    const gpa = std.testing.allocator;
    const identity: Identity = .{ .role = 1, .name = "mini", .endpoint = "wss://endpointA", .session = "work" };
    var state: FakeState = .{};
    {
        const original = state.make();
        var capture = try OwnedCapture(FakeTunnel).init(gpa, original, identity);
        defer capture.deinit(gpa);
        const template = capture.template.id;
        var changed = identity;
        changed.session = "other";
        const rejected = state.make();
        const created = state.created;

        // Previously replace accepted the new tunnel but retained identity.session
        // and the provider's selected default from "work", yielding a mixed capture.
        try std.testing.expectError(error.InvalidRegistryIdentity, capture.replace(rejected, changed));
        try std.testing.expect(state.closed[rejected.id]);
        try std.testing.expectEqual(created, state.created);
        try std.testing.expectEqual(template, capture.template.id);
        try std.testing.expectEqual(original.id, capture.pending.?.id);
        try std.testing.expect(!state.closed[template]);
        try std.testing.expect(!state.closed[original.id]);
        try std.testing.expectEqualStrings("work", capture.identity.session);

        const replacement = state.make();
        try capture.replace(replacement, identity);
        try std.testing.expect(state.closed[template]);
        try std.testing.expect(state.closed[original.id]);
        try std.testing.expectEqual(replacement.id, capture.pending.?.id);
        try std.testing.expectEqualStrings("work", capture.identity.session);
    }
    // FakeTunnel.close rejects double-free, and every original/clone must close.
    try state.expectAllClosed();
}
