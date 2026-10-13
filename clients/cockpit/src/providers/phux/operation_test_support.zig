//! Tests load canonical Rust-generated frames through the production queues.
const std = @import("std");
const provider = @import("provider_contract");
const agent_sessions = @import("agent_sessions.zig");

/// Rows one hand-built resource-catalog fixture may carry. Deliberately far
/// below the roster ceiling: this array is a local, and a 256-row one would be
/// 150 KB of stack. Thirty rows exercise overflow past the 24-row chrome cap.
pub const max_agent_fixture_rows: usize = 30;

/// One agent row of a hand-built resource catalog, in the terms the ABI
/// publishes them (`PhuxResourceInfo`): a local resource id, its parent's, and
/// the three UTF-8 spans.
pub const AgentSessionFixture = struct {
    id: u32,
    host: []const u8 = "",
    parent: ?u32 = null,
    provider_name: []const u8 = "",
    native_id: []const u8 = "",
    state: []const u8 = "",
};

/// A `phux.agent/v1` facet attached to a Terminal, not an AgentSession child.
pub const AgentIdentityFixture = struct {
    terminal: u32,
    provider_name: []const u8,
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
            .id = try provider.RemoteResourceId.fromPhux(0, row.id, row.host),
            .parent = if (row.parent) |parent| try provider.RemoteResourceId.fromPhux(0, parent, "") else null,
            .provider_name = row.provider_name,
            .native_id = row.native_id,
            .state = row.state,
        };
    }
    try host.agents.adopt(host.gpa, entries[0..rows.len]);
}

pub fn adoptAgentIdentities(host: anytype, rows: []const AgentIdentityFixture) !void {
    std.debug.assert(rows.len <= max_agent_fixture_rows);
    var entries: [max_agent_fixture_rows]agent_sessions.IdentityEntry = undefined;
    for (rows, entries[0..rows.len]) |row, *entry| {
        entry.* = .{
            .terminal = try provider.RemoteResourceId.fromPhux(0, row.terminal, ""),
            .provider_name = row.provider_name,
            .native_id = row.native_id,
            .state = row.state,
        };
    }
    try host.agents.adoptIdentities(host.gpa, entries[0..rows.len]);
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

/// Attach the canonical independent session used by same-coordinator tests.
pub fn attachSiblingHost(host: anytype) !void {
    try host.start("operations-test-sibling");
    try stageFixture(host.bridge, "hello.bin");
    _ = try host.drainReadiness();
    try host.attachSessionId(2, .{ .cols = 80, .rows = 24 });
    try stageFixture(host.bridge, "attached_session_b.bin");
    const delta = try host.drainReadiness();
    try std.testing.expect(delta.ready_published);
    try std.testing.expectEqual(.unavailable, host.workspaceSnapshot().state);
    try stageWorkspaceFixture(host.bridge, "workspace_session2_initial_metadata.bin");
    try stageWorkspaceFixture(host.bridge, "workspace_session2_initial_state.bin");
    _ = try host.drainReadiness();
    try std.testing.expectEqual(.confirmed, host.workspaceSnapshot().status);
    try std.testing.expectEqual(@as(?u32, 2), host.selectedSessionId());
    host.bridge.outgoing.reset();
}

/// `hello_directory.bin` also advertises LIST_DIRECTORY; `hello.bin` does not.
/// The attached session's stored layout shows terminal 7 in one group.
pub fn attachHostWith(host: anytype, hello: []const u8) !void {
    try attachHostReading(host, hello, "workspace_arranged", .authoritative);
}

/// The attached session has no stored layout: its arrangement publishes no
/// groups and terminal 7 is unarranged.
pub fn attachHostUnarranged(host: anytype) !void {
    try attachHostReading(host, "hello.bin", "workspace_initial", .unarranged);
}

fn attachHostReading(host: anytype, hello: []const u8, comptime reply: []const u8, state: @import("provider_contract").workspace.State) !void {
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
    try stageWorkspaceFixture(host.bridge, reply ++ "_metadata.bin");
    try stageWorkspaceFixture(host.bridge, reply ++ "_state.bin");
    _ = try host.drainReadiness();
    try std.testing.expectEqual(.confirmed, host.workspaceSnapshot().status);
    try std.testing.expectEqual(state, host.workspaceSnapshot().state);
    try std.testing.expectEqual(@as(?u32, 1), host.selectedSessionId());
    host.bridge.outgoing.reset();
}

/// The layout document of the newest whole-value layout write still queued
/// outgoing (the SET of `phux.tui.layout/v1/<session>`), copied; free it with
/// the testing allocator. Group ids are random, so no canned reply can confirm
/// a write that adds a group: the confirming read echoes what was written, as
/// the server does. The queue is left as it was.
pub fn queuedLayoutWrite(bridge: anytype) ![]u8 {
    const queue = &bridge.outgoing;
    var found: ?[]const u8 = null;
    for (queue.frames.items[queue.read_index..]) |frame| {
        // The document is the frame's last field, a CBOR map whose first key
        // is "version".
        const at = std.mem.indexOf(u8, frame, "\x67version") orelse continue;
        if (at == 0 or frame[at - 1] & 0xe0 != 0xa0) continue;
        found = frame[at - 1 ..];
    }
    return std.testing.allocator.dupe(u8, found orelse return error.NoQueuedLayoutWrite);
}

/// Stage the layout read that answers internal request `request` with
/// `value`, a document `queuedLayoutWrite` copied.
pub fn stageLayoutValue(bridge: anytype, value: []const u8, request: u32) !void {
    var length: [5]u8 = undefined;
    var length_len: usize = 0;
    var rest = value.len;
    while (true) {
        const low: u8 = @intCast(rest & 0x7f);
        rest >>= 7;
        length[length_len] = if (rest == 0) low else low | 0x80;
        length_len += 1;
        if (rest == 0) break;
    }
    // METADATA_VALUE: kind, field 1 the request id, field 2 the value.
    const body = 1 + 3 + 4 + 2 + length_len + value.len;
    const frame = try std.testing.allocator.alloc(u8, 4 + body);
    defer std.testing.allocator.free(frame);
    std.mem.writeInt(u32, frame[0..4], @intCast(body), .big);
    frame[4] = 0xd1;
    frame[5..8].* = .{ 1, 4, 4 };
    std.mem.writeInt(u32, frame[8..12], 0x8000_0000 + request, .big);
    frame[12] = 2;
    frame[13] = 4;
    @memcpy(frame[14..][0..length_len], length[0..length_len]);
    @memcpy(frame[14 + length_len ..], value);
    try std.testing.expect(bridge.incoming.stage(frame));
}

/// Confirm the queued layout write: its read (internal `metadata`) echoes
/// the written document, and `state` re-correlated to internal `state_request`
/// is the registry beside it.
pub fn confirmLayoutWrite(bridge: anytype, metadata: u32, state: []const u8, state_from: u32, state_request: u32) !void {
    const written = try queuedLayoutWrite(bridge);
    defer std.testing.allocator.free(written);
    try stageLayoutValue(bridge, written, metadata);
    try stageMappedWorkspaceFixture(bridge, state, state_from, state_request);
}

/// A canonical workspace reply with only its internal request correlation
/// changed from `from` to `request`.
pub fn stageMappedWorkspaceFixture(bridge: anytype, name: []const u8, from: u32, request: u32) !void {
    const path = try std.fmt.allocPrint(std.testing.allocator, "src/providers/phux/fixtures/{s}", .{name});
    defer std.testing.allocator.free(path);
    const bytes = try std.Io.Dir.cwd().readFileAlloc(std.testing.io, path, std.testing.allocator, .limited(64 * 1024));
    defer std.testing.allocator.free(bytes);
    try std.testing.expectEqualSlices(u8, &.{ 1, 4, 4 }, bytes[5..8]);
    try std.testing.expectEqual(0x8000_0000 + from, std.mem.readInt(u32, bytes[8..12], .big));
    std.mem.writeInt(u32, bytes[8..12], 0x8000_0000 + request, .big);
    try std.testing.expect(bridge.incoming.stage(bytes));
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
