//! Tests load canonical Rust-generated frames through the production queues.
const std = @import("std");
const provider = @import("provider_contract");
const agent_sessions = @import("agent_sessions.zig");

/// Rows one hand-built resource-catalog fixture may carry. Deliberately far
/// below the roster ceiling: this array is a local, and a 256-row one would be
/// 150 KB of stack for a fixture that never needs more than a handful.
pub const max_agent_fixture_rows: usize = 16;

/// One agent row of a hand-built resource catalog, in the terms the ABI
/// publishes them (`PhuxResourceInfo`): a local resource id, its parent's, and
/// the three UTF-8 spans.
pub const AgentSessionFixture = struct {
    id: u32,
    parent: ?u32 = null,
    provider_name: []const u8 = "",
    native_id: []const u8 = "",
    state: []const u8 = "",
};

/// Adopt a hand-built resource catalog, exactly as an attach snapshot does.
///
/// There is no wire fixture to stage and none to regenerate: the resource
/// catalog is an ABI READ (`phux_client_resource_count` / `_get`) rather than a
/// frame, so the fixture IS the record array. The decode from
/// `c.PhuxResourceInfo` is covered by host.zig's own catalog test.
pub fn adoptAgentSessions(host: anytype, rows: []const AgentSessionFixture) !void {
    std.debug.assert(rows.len <= max_agent_fixture_rows);
    var entries: [max_agent_fixture_rows]agent_sessions.Entry = undefined;
    for (rows, entries[0..rows.len]) |row, *entry| {
        entry.* = .{
            .id = try provider.RemoteResourceId.fromPhux(0, row.id, ""),
            .parent = if (row.parent) |parent| try provider.RemoteResourceId.fromPhux(0, parent, "") else null,
            .provider_name = row.provider_name,
            .native_id = row.native_id,
            .state = row.state,
        };
    }
    try host.agents.adopt(host.gpa, entries[0..rows.len]);
}

/// Fold one hand-built AGENT_RECORDS batch, exactly as `captureEffects` does.
pub fn feedAgentRecords(host: anytype, id: u32, kind: agent_sessions.RecordsKind, payload: []const u8) !bool {
    const resource = try provider.RemoteResourceId.fromPhux(0, id, "");
    return host.agents.applyRecords(host.gpa, resource, kind, payload);
}

pub fn readFixture(name: []const u8) ![]u8 {
    const path = try std.fmt.allocPrint(std.testing.allocator, "src/tests/fixtures/{s}", .{name});
    defer std.testing.allocator.free(path);
    return std.Io.Dir.cwd().readFileAlloc(std.testing.io, path, std.testing.allocator, .limited(64 * 1024));
}

pub fn expectOutgoing(bridge: anytype, name: []const u8) !void {
    const expected = try readFixture(name);
    defer std.testing.allocator.free(expected);
    const actual = bridge.outgoing.take() orelse return error.MissingFrame;
    defer bridge.outgoing.release(actual);
    try std.testing.expectEqualSlices(u8, expected, actual);
}

pub fn stageFixture(bridge: anytype, name: []const u8) !void {
    const encoded = try readFixture(name);
    defer std.testing.allocator.free(encoded);
    var offset: usize = 0;
    while (offset < encoded.len) {
        const len = 4 + std.mem.readInt(u32, encoded[offset..][0..4], .big);
        try std.testing.expect(bridge.incoming.stage(encoded[offset..][0..len]));
        offset += len;
    }
}

pub fn attachHost(host: anytype) !void {
    return attachHostWith(host, "hello.bin");
}

/// `hello_directory.bin` also advertises LIST_DIRECTORY; `hello.bin` does not.
pub fn attachHostWith(host: anytype, hello: []const u8) !void {
    try host.start("operations-test");
    try stageFixture(host.bridge, hello);
    _ = try host.drainReadiness();
    try host.attachSessionId(1, .{ .cols = 80, .rows = 24 });
    try stageFixture(host.bridge, "attached.bin");
    const delta = try host.drainReadiness();
    try std.testing.expect(delta.ready_published);
    try std.testing.expectEqual(.unavailable, host.workspaceSnapshot().state);
    try std.testing.expectEqual(@as(usize, 0), host.workspaceSnapshot().windows.len);
    // Complete the automatic workspace read deliberately. Terminal-operation
    // fixtures retain their low request IDs; workspace correlation is internal.
    try stageWorkspaceFixture(host.bridge, "workspace_initial_metadata.bin");
    try stageWorkspaceFixture(host.bridge, "workspace_initial_state.bin");
    _ = try host.drainReadiness();
    try std.testing.expectEqual(.confirmed, host.workspaceSnapshot().status);
    try std.testing.expectEqual(.fallback, host.workspaceSnapshot().state);
    try std.testing.expectEqual(@as(?u32, 1), host.selectedSessionId());
    host.bridge.outgoing.reset();
}

pub fn stageWorkspaceFixture(bridge: anytype, name: []const u8) !void {
    // Regenerate with phux-client-ffi's workspace_fixtures example, passing
    // clients/cockpit/src/providers/phux/fixtures as its output directory.
    const path = try std.fmt.allocPrint(std.testing.allocator, "src/providers/phux/fixtures/{s}", .{name});
    defer std.testing.allocator.free(path);
    const encoded = try std.Io.Dir.cwd().readFileAlloc(std.testing.io, path, std.testing.allocator, .limited(64 * 1024));
    defer std.testing.allocator.free(encoded);
    try std.testing.expect(bridge.incoming.stage(encoded));
}

pub fn expectOutgoingCount(bridge: anytype, expected: usize) !void {
    var count: usize = 0;
    while (bridge.outgoing.take()) |frame| {
        bridge.outgoing.release(frame);
        count += 1;
    }
    try std.testing.expectEqual(expected, count);
}

pub fn stageFrames(bridge: anytype, encoded: []const u8, offset: *usize, count: usize) !void {
    for (0..count) |_| {
        const len = 4 + std.mem.readInt(u32, encoded[offset.*..][0..4], .big);
        try std.testing.expect(bridge.incoming.stage(encoded[offset.*..][0..len]));
        offset.* += len;
    }
}
