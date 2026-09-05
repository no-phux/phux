const std = @import("std");
const app = @import("../native_test_root.zig");
const support = @import("support.zig");

const testing = std.testing;

const createSession = support.createSession;
const remoteTerminalRef = support.remoteTerminalRef;

// Placement is a topology leaf, not provider ownership. These tests pin the
// split explicitly: inventory reconciliation is complete and bounded while
// presentation admission remains intentional.

test "a terminal occupies exactly one pane, and admitting it twice is a no-op" {
    const session = try createSession(80, 24);
    var model = app.initialModel(session);
    defer app.deinitModel(&model);

    const id = app.initialTerminalRef(0);
    const terminal = model.provider.terminal(id) orelse return error.TestExpectedTerminal;
    terminal.session.feed("durable state");

    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
    try testing.expect(model.admitTab(id));
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
    try testing.expectEqual(@as(usize, 0), model.tabOfTerminal(id).?);

    // Selecting is not creating: the emulator behind the pane is the same one.
    try testing.expect(model.selectTerminal(id));
    try testing.expect(model.provider.terminal(id) == terminal);
    const text = try terminal.session.plainText(testing.allocator);
    defer testing.allocator.free(text);
    try testing.expect(std.mem.indexOf(u8, text, "durable state") != null);
}

test "remote discovery updates bounded inventory without allocating tabs" {
    const session = try createSession(80, 24);
    var model = app.initialModel(session);
    defer app.deinitModel(&model);

    const local = app.initialTerminalRef(0);
    var published: [app.max_tabs + 3]app.TerminalRef = undefined;
    for (&published, 0..) |*terminal_ref, index| {
        terminal_ref.* = try remoteTerminalRef(@intCast(index + 11));
    }

    app.reconcileRemoteRefs(
        &model.remote_inventory,
        &model.remote_inventory_count,
        &published,
    );

    // Inventory exceeds one workspace's tab ceiling and remains complete.
    try testing.expect(published.len > app.max_tabs);
    try testing.expectEqual(published.len, model.remoteTerminalRefs().len);
    for (published, model.remoteTerminalRefs()) |expected, actual| {
        try testing.expect(expected.eql(actual));
    }

    // A later publication retires the missing identity instead of preserving
    // a stale inventory row.
    app.reconcileRemoteRefs(
        &model.remote_inventory,
        &model.remote_inventory_count,
        published[1..],
    );
    try testing.expectEqual(published.len - 1, model.remoteTerminalRefs().len);
    try testing.expect(model.remoteTerminalRefs()[0].eql(published[1]));
    // Discovery is not admission: the local working tab and focus do not move.
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
    try testing.expectEqual(@as(usize, 0), model.tabOfTerminal(local).?);
    try testing.expect(model.selectedTerminalRef().?.eql(local));
    for (published) |terminal_ref| try testing.expect(model.locateTerminal(terminal_ref) == null);
}

test "a terminal that disappears loses its pane and the local ones stay put" {
    const session = try createSession(80, 24);
    var model = app.initialModel(session);
    defer app.deinitModel(&model);

    const local = app.initialTerminalRef(0);
    const remote = try remoteTerminalRef(31);
    try testing.expect(model.admitTab(remote));
    try testing.expectEqual(@as(usize, 2), model.ws().tab_count);

    // No provider vouches for the remote terminal, so normalization takes
    // its pane, and with it the tab that held nothing else.
    model.normalizeTopology();
    try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
    try testing.expectEqual(@as(?usize, null), model.tabOfTerminal(remote));
    try testing.expectEqual(@as(usize, 0), model.tabOfTerminal(local).?);
    try testing.expect(model.selectedTerminalRef().?.eql(local));
}

test "provider dispatch refuses a provider-qualified remote identity at the local backend" {
    const session = try createSession(80, 24);
    var model = app.initialModel(session);
    defer app.deinitModel(&model);

    const local = app.initialTerminalRef(0);
    const remote = try remoteTerminalRef(1);
    try testing.expect(model.containsTerminal(local));
    try testing.expect(!model.containsTerminal(remote));
    try testing.expect(model.terminalOwner(local) != null);
    try testing.expect(model.terminalOwner(remote) == null);
    // A terminal nobody has cannot be selected into a pane.
    try testing.expect(!model.selectTerminal(remote));
}

// GUARD: switcher-stable-id
test "keyboard session activation keeps the highlighted id across catalog rebuilds" {
    if (comptime app.phux_enabled) {
        const remote = try app.PhuxProvider.create(
            testing.allocator,
            testing.io,
            .{ .unix = "/unused" },
            "session",
            "cockpit",
        );
        const session = try createSession(80, 24);
        var state = support.TerminalApp.init(
            std.heap.page_allocator,
            app.initialModelWithPhux(session, remote),
            app.appOptions(),
        );
        defer app.deinitModel(&state.model);
        defer state.deinit();
        state.effects.executor = .fake;

        try remote.host.sessions.append(testing.allocator, .{
            .id = 71,
            .name = try testing.allocator.dupe(u8, "build"),
            .created_at_unix_secs = 1,
            .window_count = 2,
            .attached_client_count = 1,
            .focused = false,
        });
        try remote.host.sessions.append(testing.allocator, .{
            .id = 72,
            .name = try testing.allocator.dupe(u8, "tests"),
            .created_at_unix_secs = 2,
            .window_count = 1,
            .attached_client_count = 3,
            .focused = false,
        });

        // One placed local terminal precedes the two sessions, so two steps
        // highlight session 72. This is the identity the painted row carries.
        app.update(&state.model, .palette_open, &state.effects);
        app.update(&state.model, .{ .palette_step = 2 }, &state.effects);
        switch (state.model.ws().palette.highlighted orelse return error.TestExpectedSession) {
            .session => |id| try testing.expectEqual(@as(u32, 72), id),
            else => return error.TestExpectedSession,
        }

        // Removal leaves session 71 occupying the last live catalog slot. An
        // index-based Enter clamps to that row and switches to the wrong ID;
        // the fenced payload instead reports 72 unavailable by selecting none.
        const removed = remote.host.sessions.orderedRemove(1);
        app.update(&state.model, .{ .key = .{
            .key = "enter",
            .phase = .key_down,
        } }, &state.effects);
        try testing.expectEqual(@as(?u32, null), remote.session_id);

        // Restore the catalog, highlight 72 again, then reorder after the
        // highlight was projected. Enter must still activate 72, not the 71
        // that moved into its old index.
        try remote.host.sessions.append(testing.allocator, removed);
        app.update(&state.model, .palette_open, &state.effects);
        app.update(&state.model, .{ .palette_step = 2 }, &state.effects);
        std.mem.swap(
            @TypeOf(remote.host.sessions.items[0]),
            &remote.host.sessions.items[0],
            &remote.host.sessions.items[1],
        );
        app.update(&state.model, .{ .key = .{
            .key = "enter",
            .phase = .key_down,
        } }, &state.effects);
        try testing.expectEqual(@as(?u32, 72), remote.session_id);
    } else {
        return error.SkipZigTest;
    }
}

test "attach-ready admission selects exactly one current remote terminal" {
    if (comptime app.phux_enabled) {
        const remote = try app.PhuxProvider.create(
            testing.allocator,
            testing.io,
            .{ .unix = "/unused" },
            "session",
            "cockpit",
        );
        const session = try createSession(80, 24);
        var model = app.initialModelWithPhux(session, remote);
        defer app.deinitModel(&model);

        var expected: [3]app.TerminalRef = undefined;
        for (&expected, 0..) |*terminal_ref, index| {
            const remote_id = try app.RemoteTerminalId.fromPhux(
                @intCast(index),
                51,
                if (index == 0) "local" else "satellite",
            );
            try remote.host.terminals.append(testing.allocator, .{
                .id = remote_id,
                .phase = .live,
                .published = true,
            });
            terminal_ref.* = .{
                .provider_id = .phux,
                .terminal_id = .{ .phux = remote_id },
            };
        }

        model.reconcileRemoteTerminals();
        try testing.expectEqual(@as(usize, 3), model.remoteTerminalRefs().len);
        try testing.expectEqual(@as(usize, 1), model.ws().tab_count);
        for (expected) |terminal_ref| try testing.expect(model.locateTerminal(terminal_ref) == null);
        for (expected) |terminal_ref| try testing.expect(model.remoteUiConst(terminal_ref) != null);

        try testing.expect(model.admitAndSelectCurrentRemoteTerminal());
        try testing.expectEqual(@as(usize, 2), model.ws().tab_count);
        try testing.expect(model.selectedTerminalRef().?.eql(expected[0]));
        try testing.expect(model.locateTerminal(expected[1]) == null);
        try testing.expect(model.locateTerminal(expected[2]) == null);
    } else {
        return error.SkipZigTest;
    }
}
