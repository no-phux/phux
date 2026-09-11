//! Go to Directory: the engine seam (`cockpit.directory`) over the one Phux
//! provider's retained LIST_DIRECTORY listing. The fixtures come from
//! phux-client-ffi's cockpit_fixture example, which validates them through
//! the C ABI: `hello_directory.bin` advertises LIST_DIRECTORY and
//! `directory_listing.bin` answers host request 1 with `/work` (parent `/`)
//! holding `.config`, `cockpit` and the symlink `phux`.

const std = @import("std");
const native_sdk = @import("native_sdk");
const support = @import("../cockpit/phux_support.zig");
const model_module = @import("../cockpit/model.zig");
const ts_engine = @import("../cockpit/native/ts_engine.zig");
const picker = @import("../cockpit/native/directory_picker.zig");

const testing = std.testing;

test {
    _ = picker;
}

/// The channel effects a drain may touch; none of them may fire here.
const ChannelFx = struct {
    pub fn phuxChannelLive(_: *const @This()) bool {
        return false;
    }
    pub fn openChannel(_: *const @This(), _: anytype) native_sdk.ChannelHandle {
        return .{};
    }
    pub fn closeChannel(_: *const @This(), _: u64) void {}
    pub fn showNotification(_: *const @This(), _: anytype) void {}
};

fn requestBytes(kind: picker.Kind, request_id: u32, offset: u16, index: u16, query: []const u8, out: []u8) []const u8 {
    out[0] = picker.version;
    out[1] = @intFromEnum(kind);
    std.mem.writeInt(u32, out[2..6], request_id, .little);
    std.mem.writeInt(u16, out[6..8], offset, .little);
    std.mem.writeInt(u16, out[8..10], index, .little);
    out[10] = @intCast(query.len);
    @memcpy(out[11..][0..query.len], query);
    return out[0 .. 11 + query.len];
}

const Reply = struct {
    status: picker.Status,
    request_id: u32,
    total: u16,
    count: u8,
    first: ?u16,
    bytes: []const u8,
};

fn parse(bytes: []const u8) Reply {
    const path_end = 13 + @as(usize, bytes[12]);
    const query_end = path_end + 1 + @as(usize, bytes[path_end]);
    const count = bytes[query_end];
    return .{
        .status = @enumFromInt(bytes[1]),
        .request_id = std.mem.readInt(u32, bytes[2..6], .little),
        .total = std.mem.readInt(u16, bytes[8..10], .little),
        .count = count,
        .first = if (count == 0) null else std.mem.readInt(u16, bytes[query_end + 1 ..][0..2], .little),
        .bytes = bytes,
    };
}

/// An engine attached to a Phux provider the way durable creation's tests
/// attach one, so Open Here can spawn through the real creation path.
const Rig = struct {
    engine: *ts_engine.Engine,
    provider: *support.PhuxProvider,
    out: [picker.max_bytes]u8 = undefined,
    request: [11 + picker.max_query_bytes]u8 = undefined,

    fn start(hello: []const u8) !Rig {
        const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
        errdefer engine.destroy();
        const provider = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/unused" }, null, "directory-test");
        // The engine owns the provider from here and destroys it with the model.
        model_module.attachPhuxProvider(engine.model, provider);
        try support.PhuxProvider.test_support.attachHostWith(provider.host, hello);
        provider.attach_queued = true;
        _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
        try testing.expect(!engine.model.phux_connection_unavailable);
        provider.bridge.outgoing.reset();
        return .{ .engine = engine, .provider = provider };
    }

    fn send(self: *Rig, kind: picker.Kind, request_id: u32, offset: u16, index: u16, query: []const u8) !Reply {
        const payload = requestBytes(kind, request_id, offset, index, query, &self.request);
        return parse(try picker.handle(self.engine, payload, &self.out));
    }

    /// Deliver `directory_listing.bin` through the engine's own wake path;
    /// true when the engine announced an invalidation for it.
    fn deliverListing(self: *Rig) !bool {
        try support.PhuxProvider.test_support.stageFixture(self.provider.bridge, "directory_listing.bin");
        return self.engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
    }

    fn outgoingContains(self: *Rig, needle: []const u8) bool {
        var found = false;
        while (self.provider.bridge.outgoing.take()) |frame| {
            defer self.provider.bridge.outgoing.release(frame);
            if (std.mem.indexOf(u8, frame, needle) != null) found = true;
        }
        return found;
    }
};

test "a listing is requested on the connected server, then paged and filtered with its synthetic rows" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start("hello_directory.bin");
    defer rig.engine.destroy();

    // The focused terminal is a local PTY, whose directory is on this Mac:
    // the server lists its own user's home instead.
    const opened = try rig.send(.open, 0, 0, 0, "");
    try testing.expectEqual(picker.Status.pending, opened.status);
    try testing.expectEqual(@as(u32, 1), opened.request_id);
    try testing.expectEqualStrings("", rig.provider.directoryInfo().path);

    // The settled listing announces, so the waiting core asks again.
    try testing.expect(try rig.deliverListing());
    const first = try rig.send(.page, 1, 0, 0, "");
    try testing.expectEqual(picker.Status.listed, first.status);
    // Open here, `..`, then `.config`, `cockpit`, `phux`.
    try testing.expectEqual(@as(u16, 5), first.total);
    try testing.expectEqual(@as(u8, 4), first.count);
    try testing.expectEqual(@as(?u16, picker.here_index), first.first);
    try testing.expect(std.mem.indexOf(u8, first.bytes, "/work") != null);
    const second = try rig.send(.page, 1, 4, 0, "");
    try testing.expectEqual(@as(u8, 1), second.count);
    try testing.expectEqual(@as(?u16, 2), second.first);
    const filtered = try rig.send(.page, 1, 0, 0, "COCK");
    try testing.expectEqual(@as(u16, 1), filtered.total);
    try testing.expectEqual(@as(?u16, 1), filtered.first);

    // Enter on `cockpit` lists `/work/cockpit` under a new request ID.
    const descended = try rig.send(.descend, 1, 0, 1, "");
    try testing.expectEqual(picker.Status.pending, descended.status);
    try testing.expectEqual(@as(u32, 2), descended.request_id);
    try testing.expectEqualStrings("/work/cockpit", rig.provider.directoryInfo().path);

    // An action against the listing the user has left is refused.
    try testing.expectError(error.StaleListing, rig.send(.descend, 1, 0, 1, ""));
    // A late answer to the superseded request never replaces the new one.
    _ = try rig.deliverListing();
    try testing.expectEqual(picker.Status.pending, (try rig.send(.page, 2, 0, 0, "")).status);
}

test "the parent row lists the server's lexical parent" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start("hello_directory.bin");
    defer rig.engine.destroy();
    _ = try rig.send(.open, 0, 0, 0, "");
    _ = try rig.deliverListing();
    const up = try rig.send(.parent, 1, 0, 0, "");
    try testing.expectEqual(picker.Status.pending, up.status);
    try testing.expectEqualStrings("/", rig.provider.directoryInfo().path);
}

test "Open Here spawns one new tab whose shell starts in the listed directory" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start("hello_directory.bin");
    defer rig.engine.destroy();
    _ = try rig.send(.open, 0, 0, 0, "");
    _ = try rig.deliverListing();
    try testing.expect(!rig.outgoingContains("/work"));
    try testing.expectEqual(@as(usize, 0), rig.engine.creation.count());

    _ = try rig.send(.here, 1, 0, picker.here_index, "");
    try testing.expectEqual(@as(usize, 1), rig.engine.creation.count());
    try testing.expect(rig.outgoingContains("/work"));

    // An entry's own tab: `/work/cockpit`.
    _ = try rig.send(.here, 1, 0, 1, "");
    try testing.expectEqual(@as(usize, 2), rig.engine.creation.count());
    try testing.expect(rig.outgoingContains("/work/cockpit"));
}

test "a server without LIST_DIRECTORY is named, not waited on" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start("hello.bin");
    defer rig.engine.destroy();
    const refused = try rig.send(.open, 0, 0, 0, "");
    try testing.expectEqual(picker.Status.unsupported, refused.status);
    try testing.expectEqual(@as(u8, 0), refused.count);
    try testing.expect(std.mem.indexOf(u8, refused.bytes, "cannot list directories") != null);
}

test "without a Phux provider the picker says so instead of failing" {
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    var out: [picker.max_bytes]u8 = undefined;
    var request: [11]u8 = undefined;
    const reply = parse(try picker.handle(engine, requestBytes(.open, 0, 0, 0, "", &request), &out));
    try testing.expectEqual(picker.Status.unavailable, reply.status);
}
