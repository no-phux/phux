//! Agent sessions reach the chrome as ROWS under the terminal that owns them,
//! and never as a terminal surface.
//!
//! Two things are being pinned here that the provider-level suite cannot see.
//! First, the join: a row is projected under a parent the workspace actually
//! places, so a session whose parent is gone projects nowhere without anything
//! having to remember to remove it. Second, the attention path: a blocked
//! agent lights the SAME marker a bell does, through the same predicate, so
//! there is one answer to "does this terminal want me" rather than two.

const std = @import("std");
const native_sdk = @import("native_sdk");
const engine_module = @import("../cockpit/native/ts_engine.zig");
const model_module = @import("../cockpit/model.zig");
const projection = @import("../cockpit/native/workspace_projection.zig");
const ts_snapshot = @import("../cockpit/native/ts_snapshot.zig");
const ts_agents = @import("../cockpit/native/ts_agents.zig");
const support = @import("../cockpit/phux_support.zig");

const testing = std.testing;
const fixture = if (support.phux_enabled) support.PhuxProvider.test_support else struct {};

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

/// An engine with one attached remote terminal, from the canonical fixtures.
fn start() !*engine_module.Engine {
    const engine = try engine_module.Engine.create(testing.allocator, testing.io);
    errdefer engine.destroy();
    const remote = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture.sock" }, null, "agent-rows");
    model_module.attachPhuxProvider(engine.model, remote);
    try fixture.attachHost(remote.host);
    remote.attach_queued = true;
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
    remote.bridge.outgoing.reset();
    return engine;
}

/// The one published remote terminal, and the local resource number the
/// catalog fixture must name as a parent to hang under it.
fn remoteParent(engine: *engine_module.Engine) !struct { ref: support.TerminalRef, id: u32 } {
    const remote = engine.model.phux().?;
    var refs: [model_module.max_remote_terminals]support.TerminalRef = undefined;
    const count = remote.terminalRefs(&refs);
    try testing.expect(count >= 1);
    return .{ .ref = refs[0], .id = refs[0].terminal_id.phux.id };
}

test "agent sessions project as rows under their parent terminal" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const parent = try remoteParent(engine);

    try fixture.adoptAgentSessions(model.phux().?.host, &.{
        .{ .id = 9001, .parent = parent.id, .provider_name = "claude", .native_id = "sess-1", .state = "working" },
        .{ .id = 9002, .parent = parent.id, .provider_name = "codex", .native_id = "sess-2", .state = "done" },
        // Under a terminal this workspace does not place: never projected.
        .{ .id = 9003, .parent = 9999, .provider_name = "claude", .state = "blocked" },
    });

    var rows: [model_module.max_agent_sessions]projection.AgentRow = undefined;
    const count = projection.agentRowsUnder(model, parent.ref, &rows);
    try testing.expectEqual(@as(usize, 2), count);
    try testing.expect(rows[0].parent.eql(parent.ref));
    try testing.expectEqualStrings("claude", rows[0].provider_name);
    try testing.expectEqualStrings("sess-1", rows[0].native_id);
    try testing.expectEqualStrings("working", rows[0].state.word());
    try testing.expectEqualStrings("codex", rows[1].provider_name);
    try testing.expectEqualStrings("done", rows[1].state.word());
    try testing.expect(!rows[0].needsAttention());
    try testing.expect(!rows[1].needsAttention());

    // The tab-level projection is the same rows, reached through the pane tree.
    var tab_rows: [model_module.max_agent_sessions]projection.AgentRow = undefined;
    const tab_index = model.wsConst().tabOfTerminal(parent.ref).?;
    try testing.expectEqual(@as(usize, 2), projection.tabAgentRows(model, model.wsConst(), tab_index, &tab_rows));
    try testing.expect(tab_rows[0].resource.eql(rows[0].resource));

    // The caller's buffer bounds the answer; it is never overrun.
    var one: [1]projection.AgentRow = undefined;
    try testing.expectEqual(@as(usize, 1), projection.agentRowsUnder(model, parent.ref, &one));
    var none: [0]projection.AgentRow = undefined;
    try testing.expectEqual(@as(usize, 0), projection.agentRowsUnder(model, parent.ref, &none));
}

test "a blocked agent raises its terminal's attention and a close lowers it" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const host = model.phux().?.host;
    const parent = try remoteParent(engine);

    try fixture.adoptAgentSessions(host, &.{
        .{ .id = 9001, .parent = parent.id, .provider_name = "claude", .state = "working" },
    });
    try testing.expect(!projection.terminalNeedsAttention(model, parent.ref));

    // An `ask` on the stream is the signal; nothing else about the terminal
    // changed, so the marker is entirely stream-sourced.
    try testing.expect(try fixture.feedAgentRecords(host, 9001, .live, "{\"seq\":7,\"ts_ms\":70,\"type\":\"ask\",\"data\":{\"question\":\"run it?\"}}\n"));
    try testing.expect(projection.terminalNeedsAttention(model, parent.ref));
    var rows: [model_module.max_agent_sessions]projection.AgentRow = undefined;
    try testing.expectEqual(@as(usize, 1), projection.agentRowsUnder(model, parent.ref, &rows));
    try testing.expectEqualStrings("blocked", rows[0].state.word());
    try testing.expect(rows[0].needsAttention());

    // Answered: the agent goes back to work and the marker goes quiet again.
    try testing.expect(try fixture.feedAgentRecords(host, 9001, .live, "{\"seq\":8,\"ts_ms\":80,\"type\":\"prompt\",\"data\":{}}\n"));
    try testing.expect(!projection.terminalNeedsAttention(model, parent.ref));

    // A close retires the row, and the terminal keeps its own quiet state.
    try testing.expect(try fixture.feedAgentRecords(host, 9001, .live, "{\"seq\":9,\"ts_ms\":90,\"type\":\"ask\",\"data\":{}}\n"));
    try testing.expect(projection.terminalNeedsAttention(model, parent.ref));
    try testing.expect(try fixture.feedAgentRecords(host, 9001, .closed, ""));
    try testing.expectEqual(@as(usize, 0), projection.agentRowsUnder(model, parent.ref, &rows));
    try testing.expect(!projection.terminalNeedsAttention(model, parent.ref));
}

test "an agent session is never a terminal surface" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const parent = try remoteParent(engine);

    try fixture.adoptAgentSessions(model.phux().?.host, &.{
        .{ .id = 9001, .parent = parent.id, .provider_name = "claude", .state = "working" },
    });
    var rows: [model_module.max_agent_sessions]projection.AgentRow = undefined;
    try testing.expectEqual(@as(usize, 1), projection.agentRowsUnder(model, parent.ref, &rows));
    const agent_ref = rows[0].resource;

    try testing.expect(model.isAgentSession(agent_ref));
    try testing.expect(!model.containsTerminal(agent_ref));
    try testing.expectEqual(@as(?model_module.Presentation, null), model.remotePresentation(agent_ref));
    try testing.expectEqual(@as(?model_module.ReplicaOwner, null), model.terminalOwner(agent_ref));
    try testing.expectEqual(@as(?model_module.TerminalLocation, null), model.locateTerminal(agent_ref));
    // The parent stays an ordinary terminal alongside it.
    try testing.expect(model.containsTerminal(parent.ref));
    try testing.expect(!model.isAgentSession(parent.ref));
}

test "the snapshot carries agent rows as an extension record the TS core decodes" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const parent = try remoteParent(engine);
    const tab: u8 = @intCast(model.wsConst().tabOfTerminal(parent.ref).?);

    // A workspace with no agent sessions writes no record at all, so this
    // length is the whole snapshot the core parsed before the kind existed.
    var quiet_buffer: [ts_snapshot.max_bytes]u8 = undefined;
    const quiet = try engine.snapshot(&quiet_buffer);
    const quiet_len = quiet.len;

    try fixture.adoptAgentSessions(model.phux().?.host, &.{
        .{ .id = 9001, .parent = parent.id, .provider_name = "claude", .native_id = "sess-1", .state = "working" },
        .{ .id = 9002, .parent = parent.id, .provider_name = "codex", .native_id = "sess-2", .state = "blocked" },
    });

    var buffer: [ts_snapshot.max_bytes]u8 = undefined;
    const bytes = try engine.snapshot(&buffer);

    // The payload the TS decoder reads: a row count, then window, tab, state
    // ordinal, attention flag, and the provider slug per row. The ordinals are
    // `agent_sessions.State`, which `AGENT_STATE_WORDS` in core.ts indexes.
    const payload = [_]u8{ 2, 0, tab, 1, 0, 6 } ++ "claude".* ++ [_]u8{ 0, tab, 2, 1, 5 } ++ "codex".*;
    const record = [_]u8{ @intFromEnum(ts_snapshot.ExtensionKind.agent_rows), payload.len, 0 } ++ payload;
    // Later extension records (navigation context, identity rows) follow the
    // agent extension, so the payload is found by content, not by position.
    try testing.expect(std.mem.indexOf(u8, bytes, &record) != null);
    // The identity-bound parent rows travel as their own kind-5 record after
    // the shared sections. Walk the trailing records by their framed lengths
    // instead of assuming the parent rows close the snapshot.
    var identity: ?[]const u8 = null;
    var at = quiet_len;
    while (at + 3 <= bytes.len) {
        const kind = bytes[at];
        const len = std.mem.readInt(u16, bytes[at + 1 ..][0..2], .little);
        if (at + 3 + len > bytes.len) return error.TestTruncatedExtension;
        if (kind == @intFromEnum(ts_snapshot.ExtensionKind.parent_agent_rows)) {
            identity = bytes[at..][0 .. 3 + @as(usize, len)];
            break;
        }
        at += 3 + @as(usize, len);
    }
    const rows = identity orelse return error.TestExpectedParentAgentRows;
    try testing.expectEqual(@as(u16, 2), std.mem.readInt(u16, rows[3..5], .little));
    try testing.expectEqual(@as(u8, 2), rows[5]);
    try testing.expectEqual(tab, rows[7]);
    try testing.expect(std.mem.indexOf(u8, rows, "phux:0:9001@") != null);
    try testing.expect(std.mem.indexOf(u8, rows, "phux:0:9002@") != null);
    const target = std.mem.readInt(u16, rows[8..10], .little);
    const resolved = engine_module.navigation.resolve(model, engine.revision, engine.revision, target).?;
    try testing.expect(resolved.placed_terminal.terminal_ref.eql(parent.ref));

    // The parent terminal asks for attention because one of its rows does.
    try testing.expect(projection.terminalNeedsAttention(model, parent.ref));

    // The row goes away with the session, and so does the whole record.
    try testing.expect(try fixture.feedAgentRecords(model.phux().?.host, 9002, .closed, ""));
    try testing.expect(try fixture.feedAgentRecords(model.phux().?.host, 9001, .closed, ""));
    const closed = try engine.snapshot(&buffer);
    try testing.expectEqual(quiet_len, closed.len);
}

fn agentRequest(revision: u64, offset: u16) [13]u8 {
    var request = [_]u8{0} ** 13;
    request[0] = 1;
    request[1] = 5;
    std.mem.writeInt(u64, request[2..10], revision, .little);
    std.mem.writeInt(u16, request[10..12], offset, .little);
    return request;
}

fn snapshotTabAttention(bytes: []const u8, tab: usize) u8 {
    var at: usize = ts_snapshot.header_len;
    for (0..tab) |_| at += 7 + @as(usize, bytes[at + 5]) + bytes[at + 6];
    return bytes[at + 4];
}

test "snapshot attention includes a blocked agent under a nonfocused split" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const parent = try remoteParent(engine);
    var local_refs: [model_module.max_tabs]support.TerminalRef = undefined;
    try testing.expect(model.provider.terminalRefs(&local_refs) > 0);
    const local = local_refs[0];
    const tab = model.ws().tabOfTerminal(parent.ref).?;
    const tree = model.ws().tree(tab).?;
    _ = try tree.split(tree.focus, .horizontal, local);
    try testing.expect(model.focusedTerminalRef().?.eql(local));
    var buffer: [ts_snapshot.max_bytes]u8 = undefined;
    try testing.expectEqual(@as(u8, 0), snapshotTabAttention(try engine.snapshot(&buffer), tab));
    try fixture.adoptAgentSessions(model.phux().?.host, &.{
        .{ .id = 9100, .parent = parent.id, .provider_name = "claude", .state = "blocked" },
    });
    // The selected local split is quiet; the remote sibling owns the agent.
    // The tab marker must summarize the whole tree, not just its focused leaf.
    try testing.expect(!projection.terminalNeedsAttention(model, local));
    try testing.expectEqual(@as(u8, 1), snapshotTabAttention(try engine.snapshot(&buffer), tab));
    try testing.expect(try fixture.feedAgentRecords(model.phux().?.host, 9100, .closed, ""));
    try testing.expectEqual(@as(u8, 0), snapshotTabAttention(try engine.snapshot(&buffer), tab));
}

test "agent inspection reaches every overflow row with catalog and producer evidence" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const parent = try remoteParent(engine);
    var entries: [30]fixture.AgentSessionFixture = undefined;
    for (&entries, 0..) |*entry, i| entry.* = .{ .id = @intCast(9000 + i), .parent = parent.id, .provider_name = "claude", .native_id = "producer-session", .state = "working" };
    try fixture.adoptAgentSessions(model.phux().?.host, &entries);
    try testing.expect(try fixture.feedAgentRecords(model.phux().?.host, 9029, .live, "{\"seq\":1,\"ts_ms\":70,\"type\":\"ask\",\"data\":{\"question\":\"run it?\"}}\n"));
    var compact: [6]u8 = undefined;
    try testing.expectEqual(@as(usize, 6), try ts_agents.snapshot(model, &compact, 0));
    try testing.expectEqual(@as(u16, 30), std.mem.readInt(u16, compact[3..5], .little));
    try testing.expectEqual(@as(u8, 0), compact[5]);
    var buffer: [4096]u8 = undefined;
    for (0..30) |i| {
        const request = agentRequest(engine.revision, @intCast(i));
        const page = try engine.navigationSnapshot(&request, &buffer);
        try testing.expectEqual(@as(u16, 30), std.mem.readInt(u16, page[13..15], .little));
        try testing.expectEqual(@as(u8, 1), page[15]);
        var expected: [64]u8 = undefined;
        const resource = try std.fmt.bufPrint(&expected, "phux:0:{d}@", .{9000 + i});
        try testing.expect(std.mem.indexOf(u8, page, resource) != null);
        try testing.expect(std.mem.indexOf(u8, page, "producer-session") != null);
        if (i == 29) try testing.expect(std.mem.indexOf(u8, page, "Catalog: working; records: blocked") != null);
    }
    const stale = agentRequest(engine.revision - 1, 29);
    try testing.expectError(error.StaleRevision, engine.navigationSnapshot(&stale, &buffer));
    try testing.expect(try fixture.feedAgentRecords(model.phux().?.host, 9029, .closed, ""));
    const request = agentRequest(engine.revision, 29);
    const empty = try engine.navigationSnapshot(&request, &buffer);
    try testing.expectEqual(@as(u8, 0), empty[15]);
}

test "agent parent navigation distinguishes a nonfocused split and survives placement changes" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const model = engine.model;
    const parent = try remoteParent(engine);
    const other: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, 4242, "") } };
    const workspace = model.ws();
    const tab = workspace.tabOfTerminal(parent.ref).?;
    const tree = workspace.tree(tab).?;
    _ = try tree.split(tree.focus, .horizontal, other);
    try fixture.adoptAgentSessions(model.phux().?.host, &.{
        .{ .id = 9001, .parent = parent.id, .provider_name = "claude", .state = "blocked" },
        .{ .id = 9002, .parent = 4242, .provider_name = "codex", .state = "working" },
    });
    const first = ts_agents.parentTarget(model, parent.ref);
    const second = ts_agents.parentTarget(model, other);
    try testing.expect(first.index != second.index);
    try testing.expectEqual(first.tab, second.tab);
    try testing.expect(projection.terminalNeedsAttention(model, parent.ref));
    const resolved = engine_module.navigation.resolve(model, engine.revision, engine.revision, first.index).?;
    try testing.expect(resolved.placed_terminal.terminal_ref.eql(parent.ref));
    try testing.expect(engine_module.navigation.resolve(model, engine.revision + 1, engine.revision, first.index) == null);
    // Move presentation to another window without changing either resource.
    model.openWindow(1).?.* = model.primary;
    model.primary = .{};
    const moved = ts_agents.parentTarget(model, parent.ref);
    try testing.expectEqual(@as(u8, 1), moved.window);
    try testing.expect(model.phux().?.agentSessions()[0].parentRef().?.eql(parent.ref));
}
