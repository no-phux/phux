//! A Machines action lends identity bytes only for its callback. Retain an
//! owned capture for subsequent Sessions pages without retargeting by alias.
const std = @import("std");
const runtime = @import("machine_runtime.zig");
const Identity = @import("machines.zig").Identity;
const Model = @import("../model.zig").Model;

pub const token_len = 12;
pub const scope = 5;

/// The request ID, registry generation and row are already echoed in the
/// successful op-7 receipt. No display string becomes navigation authority.
pub fn actionToken(request: []const u8) ?[token_len]u8 {
    if (request.len != 16 or request[0] != 1 or request[1] != 7) return null;
    return request[2..14].*;
}

pub fn navigationToken(request: []const u8) ?[token_len]u8 {
    if (request.len < 15 or request[0] != 1 or request[1] != 4) return null;
    const offset = 13 + @as(usize, request[12]);
    if (request.len != offset + 2 + token_len) return null;
    if (request[offset] != scope or request[offset + 1] != token_len) return null;
    return request[offset + 2 ..][0..token_len].*;
}

pub const Capture = struct {
    arena: ?std.heap.ArenaAllocator = null,
    token: [token_len]u8 = @splat(0),
    selection: ?runtime.Browse = null,
    attachments: []const u64 = &.{},

    pub fn deinit(self: *Capture) void {
        if (self.arena) |*arena| arena.deinit();
        self.* = .{};
    }

    /// Allocate before retiring the old capture. A failed browse cannot destroy
    /// a still-valid previous continuation or publish partly borrowed strings.
    pub fn replace(self: *Capture, allocator: std.mem.Allocator, token: [token_len]u8, selection: runtime.Browse) !void {
        var arena = std.heap.ArenaAllocator.init(allocator);
        errdefer arena.deinit();
        const gpa = arena.allocator();
        var owned = selection;
        owned.identity = try copyIdentity(gpa, selection.identity);
        const targets = try gpa.dupe(runtime.Target, selection.targets);
        const attachments = try gpa.alloc(u64, targets.len);
        for (targets, attachments) |*target, *attachment| {
            target.identity = try copyIdentity(gpa, target.identity);
            attachment.* = target.attachment_id;
        }
        owned.targets = targets;
        self.deinit();
        self.* = .{ .arena = arena, .token = token, .selection = owned, .attachments = attachments };
    }

    pub fn resolve(self: *const Capture, model: *const Model, token: [token_len]u8) ?[]const u64 {
        if (!std.mem.eql(u8, &self.token, &token)) return null;
        const selection = self.selection orelse return null;
        if (!selection.valid(model)) return null;
        return self.attachments;
    }
};

fn copyIdentity(arena: std.mem.Allocator, identity: Identity) !Identity {
    return .{
        .role = identity.role,
        .name = try arena.dupe(u8, identity.name),
        .endpoint = try arena.dupe(u8, identity.endpoint),
        .session = try arena.dupe(u8, identity.session),
    };
}

test "machine browse retains borrowed identity and replaces captures atomically" {
    var name = [_]u8{ 'h', 'o', 's', 't' };
    const identity: Identity = .{ .role = .remote, .name = &name, .endpoint = "ws://host:1", .session = "session" };
    var targets = [_]runtime.Target{.{ .attachment_id = 51, .source_context = 52, .connection_epoch = 53, .identity = identity, .origin = .{ .window = 0, .epoch = 0 } }};
    const selection: runtime.Browse = .{ .identity = identity, .targets = &targets, .origin = targets[0].origin };
    var capture: Capture = .{};
    defer capture.deinit();
    const token: [token_len]u8 = @splat(1);
    try capture.replace(std.testing.allocator, token, selection);
    name[0] = 'X';
    targets[0].attachment_id = 99;
    try std.testing.expectEqualStrings("host", capture.selection.?.identity.name);
    try std.testing.expectEqualStrings("host", capture.selection.?.targets[0].identity.name);
    try std.testing.expectEqual(@as(u64, 51), capture.attachments[0]);
    var failing = std.testing.FailingAllocator.init(std.testing.allocator, .{ .fail_index = 0 });
    try std.testing.expectError(error.OutOfMemory, capture.replace(failing.allocator(), @splat(2), selection));
    try std.testing.expectEqualSlices(u8, &token, &capture.token);
    try std.testing.expectEqualStrings("host", capture.selection.?.identity.name);
}

test "browse scope accepts only the exact opaque correlated token envelope" {
    var action: [16]u8 = @splat(0);
    action[0] = 1;
    action[1] = 7;
    for (action[2..14], 1..) |*byte, index| byte.* = @intCast(index);
    const token = actionToken(&action).?;
    var navigation: [29]u8 = @splat(0);
    navigation[0] = 1;
    navigation[1] = 4;
    navigation[12] = 2;
    @memcpy(navigation[13..15], "ab");
    navigation[15] = scope;
    navigation[16] = token_len;
    @memcpy(navigation[17..], &token);
    try std.testing.expectEqualSlices(u8, &token, &navigationToken(&navigation).?);
    try std.testing.expect(navigationToken(navigation[0..28]) == null);
    navigation[15] = 3;
    try std.testing.expect(navigationToken(&navigation) == null);
    action[1] = 2;
    try std.testing.expect(actionToken(&action) == null);
}
