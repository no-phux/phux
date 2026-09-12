//! Exact catalog targets across sibling Client attachments of one machine
//! (`catalog_targets.Exact`). Every fixture holds two real `PhuxProvider`s
//! dialing the SAME endpoint, so both publish one coordinator id, identical
//! terminal refs and identical session numbers. Only the attachment, the
//! session's creation and the native window/tab lifetime tell them apart,
//! which is exactly what a v2 target cannot carry.
const std = @import("std");
const contract = @import("provider_contract");
const support = @import("../phux_support.zig");
const model_module = @import("../model.zig");
const targets = @import("catalog_targets.zig");
const ts_engine = @import("ts_engine.zig");

const testing = std.testing;
const shared = contract.workspace;
const Model = model_module.Model;
const TerminalRef = support.TerminalRef;

/// The active attachment and one peer attachment of the same machine.
const Siblings = struct {
    engine: *ts_engine.Engine,
    first: *support.PhuxProvider,
    second: *support.PhuxProvider,

    fn start(endpoint: []const u8) !Siblings {
        const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
        errdefer engine.destroy();
        const first = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = endpoint }, null, "first");
        engine.model.phux_provider = first;
        try engine.model.ensurePeerSlots(1);
        const second = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = endpoint }, null, "second");
        engine.model.peers.items[0].provider = second;
        try testing.expectEqual(first.providerId(), second.providerId());
        try testing.expect(first.context_id != second.context_id);
        return .{ .engine = engine, .first = first, .second = second };
    }
};

fn refOn(remote: *const support.PhuxProvider, id: u32) !TerminalRef {
    return .{ .provider_id = remote.providerId(), .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, id, "") } };
}

fn placed(window: u8, tab: u8, ref: TerminalRef) model_module.PaletteDestination {
    return .{ .placed_terminal = .{ .window = window, .tab = tab, .terminal_ref = ref } };
}

/// Publish one leaf of `ref` from `remote`'s own workspace state, tagged with
/// its attachment the way `session_attachments.show` tags a real view.
fn project(model: *Model, remote: *const support.PhuxProvider, state: anytype, ref: TerminalRef) !void {
    const windows = [_]shared.Window{.{ .id = @splat(1), .root = 0 }};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = ref }};
    state.attachment_id = remote.context_id;
    const snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes };
    _ = try state.apply(model, snapshot, remote.connectionEpoch());
}

/// A negotiated, listing connection with the fixture's session catalog.
fn listSessions(remote: *support.PhuxProvider) !void {
    const fixture = support.PhuxProvider.test_support;
    remote.standBy();
    try remote.host.start("catalog-exact");
    try fixture.stageFixture(remote.bridge, "hello.bin");
    _ = try remote.drainReadiness();
    try fixture.stageFixture(remote.bridge, "standby_state.bin");
    _ = try remote.drainReadiness();
}

fn sourceOf(model: *const Model, resolved: targets.Resolved) ?*const support.PhuxProvider {
    return model.phuxForAttachmentConst(resolved.attachment);
}

test "exact placed targets name each sibling attachment's copy of one terminal ref" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const pair = try Siblings.start("/catalog-exact-placed");
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const fixture = support.PhuxProvider.test_support;
    try fixture.attachHost(pair.first.host);
    try fixture.attachHost(pair.second.host);
    const ref = try refOn(pair.first, 7);
    try project(model, pair.first, &model.shared_workspace, ref);
    try project(model, pair.second, &model.peers.items[0].workspace, ref);
    try testing.expectEqual(@as(usize, 2), model.primary.tab_count);

    // The v2 capture of the two rows is one byte string: it names the
    // coordinator, never the attachment or the tab, so either row would act
    // on whichever attachment `phuxForConst` answers first.
    var first_bytes: [targets.max_len]u8 = undefined;
    var second_bytes: [targets.max_len]u8 = undefined;
    try testing.expectEqualSlices(
        u8,
        targets.capture(model, placed(0, 0, ref)).?.encode(&first_bytes),
        targets.capture(model, placed(0, 1, ref)).?.encode(&second_bytes),
    );

    const sources = [_]*const support.PhuxProvider{ model.phuxForTreeConst(&model.primary.tabs[0]).?, model.phuxForTreeConst(&model.primary.tabs[1]).? };
    try testing.expect(sources[0] != sources[1]);
    var captured: [2]targets.Exact = undefined;
    for (&captured, 0..) |*target, tab| {
        target.* = targets.captureExact(model, placed(0, @intCast(tab), ref)).?;
        try testing.expectEqual(sources[tab].context_id, target.attachment);
        try testing.expectEqual(@as(u32, 1), target.session.?.id);
    }
    for (captured, 0..) |target, tab| {
        const resolved = target.resolve(model).?;
        try testing.expectEqual(@as(u8, @intCast(tab)), resolved.entry.placed_terminal.tab);
        try testing.expect(sourceOf(model, resolved) == sources[tab]);
    }

    // A replacement attachment re-publishing the same tab is a different
    // source even on the same endpoint: the old capture refuses, the
    // untouched sibling's still resolves, and restoring the original source
    // restores the target.
    const original = model.primary.tabs[1].attachment_id.?;
    const replacement = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/catalog-exact-placed" }, null, "replacement");
    defer replacement.destroy();
    try fixture.attachHost(replacement.host);
    const displaced = model.peers.items[0].provider;
    model.peers.items[0].provider = replacement;
    model.primary.tabs[1].attachment_id = replacement.context_id;
    try testing.expect(model.phuxForTreeConst(&model.primary.tabs[1]) == replacement);
    try testing.expect(captured[1].resolve(model) == null);
    try testing.expect(captured[0].resolve(model) != null);
    model.peers.items[0].provider = displaced;
    model.primary.tabs[1].attachment_id = original;
    try testing.expect(captured[1].resolve(model) != null);

    // A reconnect of one sibling retires only that sibling's replica.
    const second_source: *support.PhuxProvider = @constCast(sources[1]);
    second_source.host.client_generation += 1;
    try testing.expect(captured[1].resolve(model) == null);
    try testing.expect(captured[0].resolve(model) != null);
    second_source.host.client_generation -= 1;
    try testing.expect(captured[1].resolve(model) != null);
}

test "exact placed targets refuse a reopened window slot with an identical tab" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const pair = try Siblings.start("/catalog-exact-window");
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const fixture = support.PhuxProvider.test_support;
    try fixture.attachHost(pair.first.host);
    try fixture.attachHost(pair.second.host);
    const ref = try refOn(pair.second, 7);
    const window = model.openWindow(1) orelse return error.NoWindow;
    try testing.expect(window.admitTab(ref));
    window.tabs[0].attachment_id = pair.second.context_id;
    const target = targets.captureExact(model, placed(1, 0, ref)).?;
    try testing.expectEqual(pair.second.context_id, target.attachment);
    try testing.expect(target.resolve(model) != null);

    model.closeWindow(1);
    try testing.expect(target.resolve(model) == null);
    const reopened = model.openWindow(1) orelse return error.NoWindow;
    try testing.expect(reopened.admitTab(ref));
    reopened.tabs[0].attachment_id = pair.second.context_id;
    // Same slot, ref, attachment, tab id and tab generation: only the window
    // lifetime moved, and that alone must refuse.
    try testing.expectEqual(target.placement.?.tab_id, reopened.tabId(0).?);
    try testing.expectEqual(target.placement.?.tab_generation, reopened.tab_generation);
    try testing.expect(target.resolve(model) == null);
    const fresh = targets.captureExact(model, placed(1, 0, ref)).?;
    try testing.expect(fresh.resolve(model) != null);

    // The sibling that never published this tree cannot answer for it.
    reopened.tabs[0].attachment_id = pair.first.context_id;
    try testing.expect(fresh.resolve(model) == null);
}

test "exact session targets keep sibling listings apart and refuse a recreated session number" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const pair = try Siblings.start("/catalog-exact-sessions");
    defer pair.engine.destroy();
    const model = pair.engine.model;
    try listSessions(pair.first);
    try listSessions(pair.second);
    const listed = pair.second.standbyCatalog();
    try testing.expect(listed.len != 0);
    const id = listed[0].id;
    try testing.expectEqual(id, pair.first.sessionCatalog()[0].id);
    const coordinator = pair.second.providerId();

    const active = targets.captureExact(model, .{ .session = id }).?;
    const peer = targets.captureExact(model, .{ .peer_session = .{ .coordinator = coordinator, .id = id, .attachment_id = pair.second.context_id } }).?;
    try testing.expectEqual(pair.first.context_id, active.attachment);
    try testing.expectEqual(pair.second.context_id, peer.attachment);
    try testing.expectEqual(@as(?i64, listed[0].created_at_unix_secs), peer.session.?.created_at);

    const on_active = active.resolve(model).?;
    try testing.expectEqual(id, on_active.entry.session);
    try testing.expect(sourceOf(model, on_active) == pair.first);
    const on_peer = peer.resolve(model).?;
    try testing.expectEqual(@as(?u64, pair.second.context_id), on_peer.entry.peer_session.attachment_id);
    try testing.expect(sourceOf(model, on_peer) == pair.second);

    // A peer row must name its peer attachment: the active sibling is not a
    // peer, and a bare coordinator is never enough.
    try testing.expect(targets.captureExact(model, .{ .peer_session = .{ .coordinator = coordinator, .id = id, .attachment_id = pair.first.context_id } }) == null);
    try testing.expect(targets.captureExact(model, .{ .peer_session = .{ .coordinator = coordinator, .id = id } }) == null);

    // The same number recreated on the peer's server within this connection:
    // only the creation time differs, and only the peer's row refuses.
    const sessions = pair.second.host.sessions.items;
    const index = for (sessions, 0..) |session, at| {
        if (session.id == id) break at;
    } else unreachable;
    sessions[index].created_at_unix_secs +%= 1;
    try testing.expect(peer.resolve(model) == null);
    try testing.expect(active.resolve(model) != null);
    sessions[index].created_at_unix_secs -%= 1;
    try testing.expect(peer.resolve(model) != null);

    // A newer listing connection of the peer retires the captured catalog.
    pair.second.host.sessions_generation += 1;
    try testing.expect(peer.resolve(model) == null);
    try testing.expect(active.resolve(model) != null);
}

test "exact retry rows name the failed sibling attachment, never a shared coordinator" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const pair = try Siblings.start("/catalog-exact-unavailable");
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const coordinator = pair.second.providerId();
    model.peers.items[0].failed = true;
    const retry = targets.captureExact(model, .{ .peer_unavailable = coordinator }).?;
    try testing.expectEqual(pair.second.context_id, retry.attachment);
    const resolved = retry.resolve(model).?;
    try testing.expectEqual(coordinator, resolved.entry.peer_unavailable);
    try testing.expect(sourceOf(model, resolved) == pair.second);
    model.peers.items[0].failed = false;
    try testing.expect(retry.resolve(model) == null);
    model.peers.items[0].failed = true;

    // Two failed siblings of one machine: the bare coordinator names neither,
    // while each attachment's own row resolves to that attachment alone.
    try model.ensurePeerSlots(2);
    const third = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/catalog-exact-unavailable" }, null, "third");
    model.peers.items[1].provider = third;
    model.peers.items[1].failed = true;
    try testing.expect(targets.captureExact(model, .{ .peer_unavailable = coordinator }) == null);
    const third_retry = targets.captureUnavailablePeer(model, third.context_id).?;
    try testing.expect(sourceOf(model, third_retry.resolve(model).?) == third);
    try testing.expect(sourceOf(model, retry.resolve(model).?) == pair.second);

    // Retiring that sibling refuses its row; the other remains.
    model.peers.items[1].provider = null;
    third.destroy();
    try testing.expect(third_retry.resolve(model) == null);
    try testing.expect(retry.resolve(model) != null);
}

test "exact available targets follow the listing attachment's connection" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const pair = try Siblings.start("/catalog-exact-available");
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const ref = try refOn(pair.first, 900);
    pair.first.host.workspace_store.catalog = try testing.allocator.alloc(shared.CatalogTerminal, 1);
    pair.first.host.workspace_store.catalog[0] = .{ .terminal_ref = ref, .session_id = 42, .title = try shared.Text.init("build"), .cwd = try shared.Text.init("/work") };
    const target = targets.captureExact(model, .{ .available_terminal = ref }).?;
    try testing.expectEqual(pair.first.context_id, target.attachment);
    try testing.expectEqual(@as(u32, 42), target.session.?.id);
    try testing.expect(target.resolve(model).?.entry.available_terminal.eql(ref));
    // The sibling lists nothing; handing it the capture's attachment slot
    // would be the name-based guess this type exists to prevent.
    var forged = target;
    forged.attachment = pair.second.context_id;
    forged.host = pair.second.host.context_id;
    try testing.expect(forged.resolve(model) == null);
    pair.first.host.client_generation += 1;
    try testing.expect(target.resolve(model) == null);
}

// Self-contained: no Phux fixtures. Names the local provider's context and
// refuses a reopened window the same way remote Exact targets do, so the
// no-phux gate still exercises capture/resolve.
test "exact local placed targets name the provider context and refuse a reopened window" {
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const model = engine.model;
    const ref = @import("../../providers/local/provider.zig").initialTerminalRef(1);
    const window = model.openWindow(1) orelse return error.NoWindow;
    try testing.expect(window.admitTab(ref));
    const target = targets.captureExact(model, placed(1, 0, ref)).?;
    try testing.expectEqual(model.provider.context_id, target.attachment);
    try testing.expect(target.session == null);
    try testing.expect(target.host == 0);
    try testing.expect(target.resolve(model) != null);

    model.closeWindow(1);
    try testing.expect(target.resolve(model) == null);
    const reopened = model.openWindow(1) orelse return error.NoWindow;
    try testing.expect(reopened.admitTab(ref));
    try testing.expectEqual(target.placement.?.tab_id, reopened.tabId(0).?);
    try testing.expectEqual(target.placement.?.tab_generation, reopened.tab_generation);
    try testing.expect(target.resolve(model) == null);
    try testing.expect(targets.captureExact(model, placed(1, 0, ref)).?.resolve(model) != null);
}
