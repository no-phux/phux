//! Several coordinators at once: terminal identity qualified by the
//! coordinator that minted it (`TerminalRef.provider_id`), end to end. Two
//! servers both publish terminal 7; Cockpit holds them as two terminals,
//! projects both coordinators' workspaces into the same windows, and routes
//! every resize, paste and catalog target to the one that owns it. A peer
//! that is not showing a session never attaches.

const std = @import("std");
const native_sdk = @import("native_sdk");
const contract = @import("provider_contract");
const support = @import("../cockpit/phux_support.zig");
const model_module = @import("../cockpit/model.zig");
const ts_engine = @import("../cockpit/native/ts_engine.zig");
const navigation = @import("../cockpit/native/ts_navigation.zig");
const interaction = @import("../cockpit/terminal_interaction.zig");
const picker = @import("../cockpit/native/directory_picker.zig");
const remote_commands = @import("../cockpit/native/remote_presentation_commands.zig");
const shipping_pointer = @import("../cockpit/native/shipping_pointer.zig");
const update_module = @import("../cockpit/update.zig");
const protocol = @import("../cockpit/native/ts_protocol.zig");
const scene = @import("../cockpit/native/scene.zig");
const projection = @import("../cockpit/native/workspace_projection.zig");
const layout = @import("../cockpit/layout.zig");
const shared_workspace = @import("../cockpit/shared_workspace.zig");
const geometry = native_sdk.geometry;

const testing = std.testing;
const targets = navigation.targets;
const fixture = support.PhuxProvider.test_support;
const shared = contract.workspace;
const TerminalRef = support.TerminalRef;

/// Wire type bytes after the 4-byte length prefix (phux-protocol frame/mod.rs).
const type_attach: u8 = 0x02;
const type_spawn: u8 = 0x22;
const type_command: u8 = 0x31;
const type_subscribe_metadata: u8 = 0x54;

fn countFrames(provider: *support.PhuxProvider) struct { total: usize, attach: usize, command: usize, spawn: usize, subscribe: usize } {
    var total: usize = 0;
    var attach: usize = 0;
    var command: usize = 0;
    var spawn: usize = 0;
    var subscribe: usize = 0;
    while (provider.bridge.outgoing.take()) |frame| {
        defer provider.bridge.outgoing.release(frame);
        total += 1;
        if (frame[4] == type_attach) attach += 1;
        if (frame[4] == type_command) command += 1;
        if (frame[4] == type_spawn) spawn += 1;
        if (frame[4] == type_subscribe_metadata) subscribe += 1;
    }
    return .{ .total = total, .attach = attach, .command = command, .spawn = spawn, .subscribe = subscribe };
}

fn refOn(provider: *const support.PhuxProvider, id: u32) !TerminalRef {
    return .{ .provider_id = provider.providerId(), .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, id, "") } };
}

test "independent clients on one coordinator reject each others replica owners" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const first = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/same-coordinator" }, null, "first");
    defer first.destroy();
    const second = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/same-coordinator" }, null, "second");
    defer second.destroy();
    try fixture.attachHostWith(first.host, "hello.bin");
    try fixture.attachHostWith(second.host, "hello.bin");
    const ref = try refOn(first, 7);
    const first_owner = first.owner(ref).?;
    const second_owner = second.owner(ref).?;
    try testing.expect(first_owner.generation.sameReplica(second_owner.generation));
    try testing.expect(first_owner.terminal_ref.eql(second_owner.terminal_ref));
    try testing.expect(!first_owner.eql(second_owner));
    try testing.expect(first.ownerIsCurrent(first_owner));
    try testing.expect(second.ownerIsCurrent(second_owner));
    try testing.expect(!first.ownerIsCurrent(second_owner));
    try testing.expect(!second.ownerIsCurrent(first_owner));
    try testing.expectError(error.InvalidState, second.sendFocus(first_owner, true));
}

/// This Mac's coordinator active and the registered host `mini` beside it,
/// showing a session. Both are attached through real frames, so both
/// publish their own terminal 7.
const Pair = struct {
    engine: *ts_engine.Engine,
    here: *support.PhuxProvider,
    mini: *support.PhuxProvider,

    fn start(attached: bool) !Pair {
        return startWith(if (attached) "hello.bin" else null);
    }

    /// Both attached with `hello`'s feature bits, or neither when null.
    fn startWith(hello: ?[]const u8) !Pair {
        const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
        errdefer engine.destroy();
        const here = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/multi-coordinator-unused" }, null, "multi");
        engine.model.phux_provider = here;
        const mini = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "mini" } }, null, "multi");
        try engine.model.ensurePeerSlots(1);
        engine.model.peers.items[0].provider = mini;
        try mini.show(1);
        if (hello) |name| {
            try fixture.attachHostWith(here.host, name);
            try fixture.attachHostWith(mini.host, name);
        }
        return .{ .engine = engine, .here = here, .mini = mini };
    }

    /// This Mac's terminal 7 in one tab and mini's terminal 7 in the next,
    /// each projected from its own coordinator's workspace; mini's selected.
    fn projectBoth(self: *Pair) !void {
        const model = self.engine.model;
        model.shared_workspace.authority = self.here.providerId();
        model.shared_workspace.attachment_id = self.here.context_id;
        model.peers.items[0].workspace.authority = self.mini.providerId();
        model.peers.items[0].workspace.attachment_id = self.mini.context_id;
        const one = [_]shared.Window{.{ .id = @splat(1), .root = 0 }};
        const here_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = try refOn(self.here, 7) }};
        const mini_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = try refOn(self.mini, 7) }};
        _ = try model.shared_workspace.apply(model, leafSnapshot(1, 1, &one, &here_nodes), self.here.connectionEpoch());
        _ = try model.peers.items[0].workspace.apply(model, leafSnapshot(1, 1, &one, &mini_nodes), self.mini.connectionEpoch());
        try testing.expect(model.selectTerminal(try refOn(self.mini, 7)));
        _ = countFrames(self.here);
        _ = countFrames(self.mini);
    }
};

/// Channel, notification and peer-restart effects, counted; nothing real.
const PeerFx = struct {
    restarts: usize = 0,
    notifications: usize = 0,

    pub fn restartPeer(self: *@This(), _: *ts_engine.Engine, _: usize) bool {
        self.restarts += 1;
        return true;
    }
    pub fn openChannel(_: *@This(), _: anytype) native_sdk.ChannelHandle {
        return .{};
    }
    pub fn closeChannel(_: *@This(), _: u64) void {}
    pub fn showNotification(self: *@This(), _: anytype) void {
        self.notifications += 1;
    }
    pub fn ptyResize(_: *@This(), _: u64, _: u16, _: u16) void {}
};

fn leafSnapshot(session: u32, revision: u64, windows: []const shared.Window, nodes: []const shared.Node) shared.Snapshot {
    return .{ .session_id = session, .revision = revision, .state = .authoritative, .windows = windows, .nodes = nodes };
}

test "model resolves same coordinator attachments and held owners without ambiguous ref fallback" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const model = engine.model;
    const first = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/model-same-host" }, null, "first");
    model.phux_provider = first;
    try model.ensurePeerSlots(1);
    const second = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/model-same-host" }, null, "second");
    model.peers.items[0].provider = second;
    try fixture.attachHost(first.host);
    try fixture.attachHost(second.host);
    const ref = try refOn(first, 7);
    const windows = [_]shared.Window{.{ .id = @splat(1), .root = 0 }};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = ref }};
    model.shared_workspace.attachment_id = first.context_id;
    model.peers.items[0].workspace.attachment_id = second.context_id;
    _ = try model.shared_workspace.apply(model, leafSnapshot(1, 1, &windows, &nodes), first.connectionEpoch());
    try testing.expect(model.phuxForRef(ref) == first);
    _ = try model.peers.items[0].workspace.apply(model, leafSnapshot(1, 1, &windows, &nodes), second.connectionEpoch());
    try testing.expect(model.phuxForRef(ref) == null);
    for (model.primary.tabs[0..model.primary.tab_count]) |*tree| {
        const id = tree.attachment_id orelse continue;
        try testing.expect(model.phuxForTreeConst(tree) == model.phuxForAttachmentConst(id));
    }
    const first_owner = first.owner(ref).?;
    const second_owner = second.owner(ref).?;
    try testing.expect(model.phuxForOwner(first_owner) == first);
    try testing.expect(model.phuxForOwner(second_owner) == second);
    try testing.expect(model.ownerIsCurrent(first_owner));
    try testing.expect(model.ownerIsCurrent(second_owner));
    var stale = second_owner;
    stale.source_context = std.math.maxInt(u64);
    try testing.expect(model.phuxForOwner(stale) == null);
    var tree = model.primary.tabs[0];
    tree.attachment_id = std.math.maxInt(u64);
    try testing.expect(model.phuxForTreeConst(&tree) == null);
    _ = countFrames(first);
    _ = countFrames(second);
    try model.phuxForOwner(second_owner).?.sendFocus(second_owner, true);
    try testing.expectEqual(@as(usize, 0), countFrames(first).total);
    try testing.expect(countFrames(second).total != 0);
}

test "the same numeric terminal on two coordinators is two identities, and input reaches only its own" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    const model = pair.engine.model;
    try testing.expect(pair.here.providerId() != pair.mini.providerId());

    const here7 = try refOn(pair.here, 7);
    const mini7 = try refOn(pair.mini, 7);
    try testing.expect(!here7.eql(mini7));
    try testing.expect(model.containsTerminal(here7));
    try testing.expect(model.containsTerminal(mini7));
    try testing.expect(model.phuxForRef(here7) == pair.here);
    try testing.expect(model.phuxForRef(mini7) == pair.mini);
    const mini_owner = model.terminalOwner(mini7).?;
    try testing.expect(mini_owner.terminal_ref.eql(mini7));
    try testing.expect(!mini_owner.eql(model.terminalOwner(here7).?));
    // A coordinator no longer held owns nothing: its refs route nowhere.
    const studio7: TerminalRef = .{ .provider_id = contract.phuxCoordinatorId("studio"), .terminal_id = mini7.terminal_id };
    try testing.expect(!model.containsTerminal(studio7));
    try testing.expect(model.phuxForRef(studio7) == null);

    // A resize of mini's terminal 7 is framed on mini's connection only.
    const fx = ts_engine.NoShells{};
    interaction.resize(model, &fx, mini7, .{ .cols = 100, .rows = 30 });
    try testing.expectEqual(@as(usize, 1), countFrames(pair.mini).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expect(pair.mini.lastViewport(mini7).?.eql(.{ .cols = 100, .rows = 30 }));
    try testing.expect(pair.here.lastViewport(here7) == null);

    // A paste to this Mac's terminal 7 is framed on this Mac's only.
    model.paste_owner = model.terminalOwner(here7).?;
    model.paste_inflight = true;
    model.paste_target = .terminal;
    interaction.pasted(model, &fx, true, "echo here");
    try testing.expect(!model.paste_failed);
    try testing.expectEqual(@as(usize, 1), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);

    // Handed another coordinator's owner, a provider refuses and sends nothing.
    try testing.expectError(error.InvalidState, pair.here.sendPaste(mini_owner, "wrong machine", false));
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
}

test "two coordinators project side by side; each publication replaces only its own tabs" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(false);
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const here = &model.shared_workspace;
    const mini = &model.peers.items[0].workspace;
    here.authority = pair.here.providerId();
    mini.authority = pair.mini.providerId();
    const here11 = try refOn(pair.here, 11);
    const here12 = try refOn(pair.here, 12);
    const mini11 = try refOn(pair.mini, 11);

    // Both servers name their window 1 and terminal 11; neither collides.
    const one = [_]shared.Window{.{ .id = @splat(1), .root = 0 }};
    const here_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = here11 }};
    const mini_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = mini11 }};
    _ = try here.apply(model, leafSnapshot(1, 1, &one, &here_nodes), 1);
    _ = try mini.apply(model, leafSnapshot(1, 1, &one, &mini_nodes), 1);
    try testing.expectEqual(@as(usize, 2), model.primary.tab_count);
    try testing.expectEqual(@as(usize, 0), model.locateTerminal(here11).?.tab);
    try testing.expectEqual(@as(usize, 1), model.locateTerminal(mini11).?.tab);
    // Tab and split commands address the active coordinator: mini's tab is
    // foreign to them, this Mac's is not.
    try testing.expect(!model.foreignTab(&model.primary, 0));
    try testing.expect(model.foreignTab(&model.primary, 1));
    try testing.expect(model.activeOwnsRef(here11) and !model.activeOwnsRef(mini11));

    // mini's tab is selected; this Mac republishes with a second window.
    try testing.expect(model.selectTerminal(mini11));
    const two = [_]shared.Window{ .{ .id = @splat(1), .root = 0 }, .{ .id = @splat(2), .root = 1 } };
    const grown = [_]shared.Node{ .{ .kind = .leaf, .terminal_ref = here11 }, .{ .kind = .leaf, .terminal_ref = here12 } };
    _ = try here.apply(model, leafSnapshot(1, 2, &two, &grown), 1);
    try testing.expectEqual(@as(usize, 3), model.primary.tab_count);
    try testing.expectEqual(@as(usize, 2), model.locateTerminal(mini11).?.tab);
    try testing.expect(model.focusedTerminalRef().?.eql(mini11));
    // mini republishing keeps this Mac's tabs where they are.
    _ = try mini.apply(model, leafSnapshot(1, 2, &one, &mini_nodes), 1);
    try testing.expectEqual(@as(usize, 0), model.locateTerminal(here11).?.tab);
    try testing.expectEqual(@as(usize, 1), model.locateTerminal(here12).?.tab);
    try testing.expect(model.focusedTerminalRef().?.eql(mini11));

    // A coordinator's publication naming another's terminal is refused whole.
    const mixed = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = mini11 }};
    try testing.expectError(error.MixedAuthority, here.apply(model, leafSnapshot(1, 3, &one, &mixed), 1));
    try testing.expectEqual(@as(usize, 3), model.primary.tab_count);

    // mini leaves: only its tab goes.
    try mini.leaveSession(model);
    try testing.expectEqual(@as(usize, 2), model.primary.tab_count);
    try testing.expect(model.locateTerminal(mini11) == null);
    try testing.expect(model.locateTerminal(here12) != null);
}

test "a listing peer never attaches; shown, its first frame after HELLO_OK is ATTACH" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const peer = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "mini" } }, null, "multi");
    defer peer.destroy();
    peer.standBy();
    try testing.expect(!peer.showing());
    try peer.host.start("multi");
    try fixture.stageFixture(peer.bridge, "hello.bin");
    _ = try peer.drainReadiness();
    const listing = countFrames(peer);
    try testing.expectEqual(@as(usize, 0), listing.attach);
    try testing.expectEqual(@as(usize, 1), listing.command);
    try fixture.stageFixture(peer.bridge, "standby_state.bin");
    try testing.expect((try peer.drainReadiness()).sessions_listed);

    // Picking `deploy` (2): the peer shows it on its next connection.
    try peer.show(2);
    try testing.expect(peer.showing());
    peer.stop();
    try peer.host.reconnect("multi");
    try fixture.stageFixture(peer.bridge, "hello.bin");
    _ = try peer.drainReadiness();
    const shown = countFrames(peer);
    try testing.expectEqual(@as(usize, 1), shown.attach);
    try testing.expectEqual(@as(usize, 0), shown.command);
    try testing.expect(peer.attach_queued);

    // Standing by again stops attaching on the next connection.
    peer.standBy();
    peer.stop();
    try peer.host.reconnect("multi");
    try fixture.stageFixture(peer.bridge, "hello.bin");
    _ = try peer.drainReadiness();
    try testing.expectEqual(@as(usize, 0), countFrames(peer).attach);
}

test "keys, text, focus, pointer, selection, search and bells on a peer's pane reach only that coordinator" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    try pair.projectBoth();
    const engine = pair.engine;
    const model = engine.model;
    const mini7 = try refOn(pair.mini, 7);
    const here7 = try refOn(pair.here, 7);
    var fx: PeerFx = .{};

    // Keys and committed text.
    interaction.remoteKey(model, mini7, .{ .phase = .key_down, .key = "enter" });
    try testing.expect(countFrames(pair.mini).total >= 1);
    interaction.remoteText(model, mini7, .{ .phase = .text_input, .text = "ls" });
    try testing.expect(countFrames(pair.mini).total >= 1);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);

    // Focus: the app gaining focus tells the focused pane's coordinator.
    model.focused = false;
    engine.setFocused(&ts_engine.NoShells{}, true);
    try testing.expect(countFrames(pair.mini).total >= 1);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);

    // A pointer drop pastes into mini's pane only.
    try testing.expect(shipping_pointer.pasteDrop(model, mini7, "dropped"));
    try testing.expectEqual(@as(usize, 1), countFrames(pair.mini).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);

    // Keyboard selection and Select All take anchors on mini, and clearing
    // releases them there.
    const state = model.remoteUi(mini7).?;
    update_module.remote_selection.begin(model, mini7, state);
    try testing.expect(state.selecting);
    try testing.expect(state.start_anchor != 0);
    update_module.remote_selection.move(model, mini7, state, 1, 0);
    try testing.expect(state.end_anchor != 0);
    update_module.remote_selection.clear(model, state);
    try testing.expectEqual(@as(u64, 0), state.start_anchor);
    try testing.expect(remote_commands.command(model, mini7, .select_all));
    try testing.expect(state.start_anchor != 0);
    update_module.remote_selection.clear(model, state);

    // Search runs on mini's replica; this Mac's holds no search.
    try testing.expect(remote_commands.command(model, mini7, .find));
    _ = remote_commands.input(model, mini7, "x");
    try testing.expect(!state.search.failed);
    try testing.expect(pair.mini.host.search_owner != null);
    try testing.expect(pair.here.host.search_owner == null);
    remote_commands.close(model, state);

    // A bell on mini's terminal 7 rings mini's, never this Mac's.
    model.focused = false;
    try fixture.stageFixture(pair.mini.bridge, "remote-bell-1.bin");
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .data }, null);
    try testing.expect(pair.mini.bellRung(mini7));
    try testing.expect(!pair.here.bellRung(here7));
    try testing.expectEqual(@as(usize, 1), fx.notifications);
}

fn pickerRequest(kind: picker.Kind, request_id: u32, out: []u8) []const u8 {
    out[0] = picker.version;
    out[1] = @intFromEnum(kind);
    std.mem.writeInt(u32, out[2..6], request_id, .little);
    std.mem.writeInt(u16, out[6..8], 0, .little);
    std.mem.writeInt(u16, out[8..10], if (kind == .here) picker.here_index else 0, .little);
    out[10] = 0;
    return out[0..11];
}

fn contains(frames: *support.PhuxProvider, needle: []const u8) struct { total: usize, found: bool } {
    var total: usize = 0;
    var found = false;
    while (frames.bridge.outgoing.take()) |frame| {
        defer frames.bridge.outgoing.release(frame);
        total += 1;
        if (std.mem.indexOf(u8, frame, needle) != null) found = true;
    }
    return .{ .total = total, .found = found };
}

test "Go to Directory over a peer's satellite pane asks that peer, even when both hubs federate the same name" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    // Both coordinators relay a satellite called devbox.
    var pair = try Pair.startWith("hello_directory_host.bin");
    defer pair.engine.destroy();
    const engine = pair.engine;
    const tree = engine.model.selectedTree().?;
    const replaced = tree.nodes[tree.focus].terminal;
    defer tree.nodes[tree.focus].terminal = replaced;
    const devbox: TerminalRef = .{ .provider_id = pair.mini.providerId(), .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(1, 9, "devbox") } };
    tree.nodes[tree.focus].terminal = devbox;
    _ = countFrames(pair.here);
    _ = countFrames(pair.mini);

    var request: [11]u8 = undefined;
    var out: [picker.max_bytes]u8 = undefined;
    _ = try picker.handle(engine, pickerRequest(.open, 0, &request), &out);
    // LIST_DIRECTORY{host: devbox} went to mini's hub; This Mac's heard nothing.
    try testing.expect(contains(pair.mini, "devbox").found);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(pair.mini.providerId(), engine.directory_origin.coordinator);
    try testing.expectEqual(picker.Scope.satellite, engine.directory_origin.scope);

    // mini's answer settles and announces through mini's own channel.
    try fixture.stageFixture(pair.mini.bridge, "directory_listing.bin");
    var fx: PeerFx = .{};
    try testing.expect(engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .data }, null));
    const page = try picker.handle(engine, pickerRequest(.page, 1, &request), &out);
    try testing.expect(std.mem.indexOf(u8, page, "devbox") != null);
    try testing.expect(std.mem.endsWith(u8, page, "\x04mini"));
    // mini shows a projected session, so its listing offers Open Here: the
    // tab would open on mini.
    try testing.expect(std.mem.indexOf(u8, page, "\xff\xff") != null);
    // The drain's own subscription frames are not the question here.
    _ = countFrames(pair.mini);
    _ = countFrames(pair.here);
    // Its satellite pane is not attached, so Open Here is refused rather
    // than opened ownerless, and nothing reaches either coordinator.
    try testing.expectError(error.Refused, picker.handle(engine, pickerRequest(.here, 1, &request), &out));
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expectEqual(@as(usize, 0), engine.peer_edits.pendingCreations(0));
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);

    // A peer pane of mini's own lists mini's home, never This Mac's.
    tree.nodes[tree.focus].terminal = try refOn(pair.mini, 7);
    _ = try picker.handle(engine, pickerRequest(.open, 0, &request), &out);
    const own = contains(pair.mini, "devbox");
    try testing.expectEqual(@as(usize, 1), own.total);
    try testing.expect(!own.found);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(picker.Scope.coordinator, engine.directory_origin.scope);
}

test "a shown peer none of whose tabs is on screen returns to listing and holds no attach" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    try pair.projectBoth();
    const engine = pair.engine;
    const model = engine.model;
    var fx: PeerFx = .{};

    // Its tab is selected: it stays shown.
    try testing.expect(!engine.settlePeers(&fx));
    try testing.expect(pair.mini.showing());
    // Switching away to This Mac's session tab leaves mini with nothing on
    // screen: it goes back to listing, its tabs leave, and it reconnects.
    try testing.expect(model.selectTerminal(try refOn(pair.here, 7)));
    try testing.expect(engine.settlePeers(&fx));
    try testing.expect(!pair.mini.showing());
    try testing.expectEqual(@as(usize, 1), fx.restarts);
    try testing.expect(model.locateTerminal(try refOn(pair.mini, 7)) == null);
    try testing.expectEqual(@as(u32, 0), model.peers.items[0].workspace.session);
    try testing.expect(model.locateTerminal(try refOn(pair.here, 7)) != null);
    // The reconnection asks GET_STATE and never ATTACH: no viewport is held.
    pair.mini.stop();
    try pair.mini.host.reconnect("multi");
    try fixture.stageFixture(pair.mini.bridge, "hello.bin");
    _ = try pair.mini.drainReadiness();
    const listing = countFrames(pair.mini);
    try testing.expectEqual(@as(usize, 0), listing.attach);
    try testing.expectEqual(@as(usize, 1), listing.command);
    try testing.expect(!engine.settlePeers(&fx));
}

test "closing a peer's tab closes it on that coordinator, and its last tab closing un-shows it" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    const engine = pair.engine;
    const model = engine.model;
    var fx: PeerFx = .{};
    // mini's own published workspace, so the window it is asked to remove is
    // one it knows.
    const published = pair.mini.workspaceSnapshot();
    try testing.expect(published.windows.len != 0);
    model.peers.items[0].workspace.authority = pair.mini.providerId();
    _ = try model.peers.items[0].workspace.apply(model, published, pair.mini.connectionEpoch());
    var mini_tab: ?u8 = null;
    for (0..model.primary.tab_count) |tab| {
        if (@import("../cockpit/shared_workspace.zig").tabAuthority(&model.primary.tabs[tab]) == pair.mini.providerId()) mini_tab = @intCast(tab);
    }
    model.primary.selected_tab = mini_tab.?;
    try testing.expect(!engine.settlePeers(&fx));
    _ = countFrames(pair.mini);
    _ = countFrames(pair.here);

    // The window removal goes to mini, not This Mac, and marks nothing refused.
    try testing.expect(engine.closeTab(mini_tab.?, &ts_engine.NoShells{}));
    // The mutation, plus the workspace read an accepted mutation queues.
    try testing.expect(countFrames(pair.mini).total >= 1);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expect(!model.shared_workspace.refused);
    // mini republishes without the window: its last tab is gone.
    _ = try model.peers.items[0].workspace.apply(model, leafSnapshot(published.session_id, published.revision + 1, &.{}, &.{}), pair.mini.connectionEpoch());
    try testing.expect(model.locateTerminal(try refOn(pair.mini, 7)) == null);
    try testing.expect(engine.settlePeers(&fx));
    try testing.expect(!pair.mini.showing());
    try testing.expectEqual(@as(usize, 1), fx.restarts);
}

test "a catalog target resolves only against the coordinator that minted it" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    const model = pair.engine.model;
    model.shared_workspace.authority = pair.here.providerId();
    model.peers.items[0].workspace.authority = pair.mini.providerId();
    const here7 = try refOn(pair.here, 7);
    const mini7 = try refOn(pair.mini, 7);
    const one = [_]shared.Window{.{ .id = @splat(1), .root = 0 }};
    const here_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = here7 }};
    const mini_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = mini7 }};
    _ = try model.shared_workspace.apply(model, leafSnapshot(1, 1, &one, &here_nodes), pair.here.connectionEpoch());
    _ = try model.peers.items[0].workspace.apply(model, leafSnapshot(1, 1, &one, &mini_nodes), pair.mini.connectionEpoch());

    const here_place = model.locateTerminal(here7).?;
    const mini_place = model.locateTerminal(mini7).?;
    const here_target = targets.capture(model, .{ .placed_terminal = .{ .window = @intCast(here_place.window), .tab = @intCast(here_place.tab), .terminal_ref = here7 } }).?;
    const mini_target = targets.capture(model, .{ .placed_terminal = .{ .window = @intCast(mini_place.window), .tab = @intCast(mini_place.tab), .terminal_ref = mini7 } }).?;
    var bytes: [targets.max_len]u8 = undefined;
    const decoded = targets.decode(mini_target.encode(&bytes)).?;
    try testing.expectEqual(pair.mini.providerId(), decoded.provider_id);
    try testing.expect(decoded.resolve(model).?.placed_terminal.terminal_ref.eql(mini7));
    try testing.expect(here_target.resolve(model).?.placed_terminal.terminal_ref.eql(here7));
    // mini's context never authorizes this Mac's terminal 7, or the reverse.
    var forged = mini_target;
    forged.resource = .{ .terminal = here7 };
    try testing.expect(forged.resolve(model) == null);

    // mini goes: its targets stop resolving; this Mac's still do.
    pair.engine.dropPeer(&ts_engine.NoShells{}, 0);
    try testing.expect(decoded.resolve(model) == null);
    try testing.expect(model.locateTerminal(mini7) == null);
    try testing.expect(here_target.resolve(model) != null);
}

// ------------------------------------------ edits of a peer's tabs (peer_edits)

fn intent(engine: *ts_engine.Engine, kind: protocol.IntentKind, argument: u8) bool {
    const bytes = protocol.encodeIntent(.{ .kind = kind, .expected_revision = engine.revision, .argument = argument, .window = 255 });
    return engine.applyIntent(&bytes, &ts_engine.NoShells{});
}

/// mini's own published workspace projected beside the others and its tab
/// selected, so mini's terminal 7 is the focused pane.
fn showMini(pair: *Pair) !void {
    const model = pair.engine.model;
    model.peers.items[0].workspace.authority = pair.mini.providerId();
    _ = try model.peers.items[0].workspace.apply(model, pair.mini.workspaceSnapshot(), pair.mini.connectionEpoch());
    try testing.expect(model.selectTerminal(try refOn(pair.mini, 7)));
    _ = countFrames(pair.here);
    _ = countFrames(pair.mini);
}

fn drainMini(pair: *Pair, fx: *PeerFx) void {
    _ = pair.engine.onPeerChannel(fx, .{ .key = pair.engine.peerChannelKey(0), .kind = .data }, null);
}

fn feedMini(pair: *Pair, fx: *PeerFx, name: []const u8) !void {
    try fixture.stageFixture(pair.mini.bridge, name);
    drainMini(pair, fx);
}

/// A canonical workspace reply with only its request correlation changed,
/// staged on mini, as durable_creation_tests stages them for the active
/// coordinator: mini's host issued the same request sequence.
fn miniWorkspaceReply(pair: *Pair, name: []const u8, expected: u32, request: u32) !void {
    const path = try std.fmt.allocPrint(testing.allocator, "src/providers/phux/fixtures/{s}", .{name});
    defer testing.allocator.free(path);
    const bytes = try std.Io.Dir.cwd().readFileAlloc(testing.io, path, testing.allocator, .limited(64 * 1024));
    defer testing.allocator.free(bytes);
    try testing.expectEqualSlices(u8, &.{ 1, 4, 4 }, bytes[5..8]);
    try testing.expectEqual(0x8000_0000 + expected, std.mem.readInt(u32, bytes[8..12], .big));
    std.mem.writeInt(u32, bytes[8..12], 0x8000_0000 + request, .big);
    try testing.expect(pair.mini.bridge.incoming.stage(bytes));
}

/// Whether mini's own edit queue holds a mutation of `kind` it has sent.
/// Frame totals are not the measure: focus follows the focused pane.
fn miniSent(engine: *ts_engine.Engine, kind: @FieldType(shared.Mutation, "kind")) bool {
    if (engine.peer_edits.states.items.len == 0) return false;
    for (engine.peer_edits.states.items[0].mutations.pending) |slot| {
        const entry = slot orelse continue;
        if (entry.mutation.kind == kind and entry.mutation_request != 0) return true;
    }
    return false;
}

/// The coordinator of the tab holding `ref`.
fn tabOwner(model: *model_module.Model, ref: TerminalRef) ?contract.ProviderId {
    const place = model.locateTerminal(ref) orelse return null;
    return shared_workspace.tabAuthority(&model.wsAt(place.window).?.tabs[place.tab]);
}

test "New Tab, reorder and split with a peer's pane focused go to that peer and to no other" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    const engine = pair.engine;
    const model = engine.model;
    try showMini(&pair);
    var fx: PeerFx = .{};

    // New Tab: a SPAWN on mini. This Mac's coordinator hears nothing and
    // holds no creation of its own.
    try testing.expect(intent(engine, .new_terminal, 0));
    try testing.expectEqual(@as(usize, 1), countFrames(pair.mini).spawn);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expectEqual(@as(usize, 1), engine.peer_edits.pendingCreations(0));

    // mini spawns terminal 8 and publishes it live; its placement is asked
    // of mini alone, refresh then add, as the active coordinator's is.
    try feedMini(&pair, &fx, "spawn-local.bin");
    try feedMini(&pair, &fx, "local-ready.bin");
    try testing.expect(countFrames(pair.mini).total >= 1);
    try miniWorkspaceReply(&pair, "workspace_refresh_metadata.bin", 3, 3);
    try miniWorkspaceReply(&pair, "workspace_refresh_state.bin", 2, 2);
    drainMini(&pair, &fx);
    try miniWorkspaceReply(&pair, "workspace_add_metadata.bin", 14, 5);
    try miniWorkspaceReply(&pair, "workspace_add_state.bin", 13, 4);
    drainMini(&pair, &fx);
    try testing.expectEqual(@as(usize, 0), engine.peer_edits.pendingCreations(0));
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    // The new tab is mini's and has focus.
    const mini7 = try refOn(pair.mini, 7);
    const mini8 = try refOn(pair.mini, 8);
    try testing.expectEqual(pair.mini.providerId(), tabOwner(model, mini8).?);
    try testing.expect(model.focusedTerminalRef().?.eql(mini8));
    try testing.expect(!model.shared_workspace.refused);
    try testing.expect(!model.peers.items[0].workspace.refused);
    _ = countFrames(pair.mini);

    // Reorder: mini's first window moves right in mini's own order.
    try testing.expect(model.selectTerminal(mini7));
    try testing.expect(!miniSent(engine, .reorder));
    try testing.expect(intent(engine, .native_command, @intFromEnum(protocol.NativeCommand.move_tab_right)));
    try testing.expect(miniSent(engine, .reorder));
    try testing.expect(countFrames(pair.mini).command >= 1);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expect(!model.shared_workspace.refused);

    // Split of mini's pane: a SPAWN on mini, owned by that pane.
    try testing.expect(intent(engine, .native_command, @intFromEnum(protocol.NativeCommand.split_right)));
    try testing.expectEqual(@as(usize, 1), countFrames(pair.mini).spawn);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expectEqual(@as(usize, 1), engine.peer_edits.pendingCreations(0));
}

test "a split of a peer's pane lands on that peer, and dragging its divider resizes it there" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    const engine = pair.engine;
    const model = engine.model;
    try showMini(&pair);
    var fx: PeerFx = .{};

    try testing.expect(intent(engine, .native_command, @intFromEnum(protocol.NativeCommand.split_right)));
    try testing.expectEqual(@as(usize, 1), countFrames(pair.mini).spawn);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try feedMini(&pair, &fx, "spawn-local.bin");
    try feedMini(&pair, &fx, "local-ready.bin");
    try miniWorkspaceReply(&pair, "workspace_rename_metadata.bin", 5, 3);
    try miniWorkspaceReply(&pair, "workspace_refresh_state.bin", 2, 2);
    drainMini(&pair, &fx);
    try miniWorkspaceReply(&pair, "workspace_split_metadata.bin", 8, 5);
    try miniWorkspaceReply(&pair, "workspace_split_state.bin", 7, 4);
    drainMini(&pair, &fx);
    const tree = model.selectedTree().?;
    try testing.expectEqual(@as(usize, 2), tree.paneCount());
    try testing.expectEqual(pair.mini.providerId(), shared_workspace.tabAuthority(tree).?);
    try testing.expect(model.focusedTerminalRef().?.eql(try refOn(pair.mini, 8)));
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);

    // Size the window, then drag the divider between mini's two panes.
    engine.pumpViewports(&fx, .{ .label = scene.canvas_label, .size = geometry.SizeF.init(980, 640) });
    _ = countFrames(pair.mini);
    _ = countFrames(pair.here);
    const workspace = model.ws();
    const chrome = projection.workspaceChromeIn(model, workspace, workspace.surface_size);
    var dividers: [layout.max_panes - 1]layout.Divider = undefined;
    const count = tree.dividers(chrome.content, projection.split_divider_width, projection.split_pane_min_width, projection.split_pane_min_height, &dividers);
    try testing.expectEqual(@as(usize, 1), count);
    const x = dividers[0].rect.x + dividers[0].rect.width / 2;
    const y = dividers[0].rect.y + dividers[0].rect.height / 2;
    const none = ts_engine.NoShells{};
    try testing.expect(!miniSent(engine, .resize));
    _ = engine.onPointer(&none, .{ .label = scene.canvas_label, .kind = .pointer_down, .pointer_id = 1, .x = x, .y = y });
    try testing.expect(engine.split_drag != null);
    _ = engine.onPointer(&none, .{ .label = scene.canvas_label, .kind = .pointer_drag, .pointer_id = 1, .x = x - 120, .y = y });
    _ = engine.onPointer(&none, .{ .label = scene.canvas_label, .kind = .pointer_up, .pointer_id = 1, .x = x - 120, .y = y });
    try testing.expect(engine.split_drag == null);
    // The resize went to mini; This Mac heard nothing, and neither
    // coordinator's workspace is marked refused.
    try testing.expect(miniSent(engine, .resize));
    try testing.expect(countFrames(pair.mini).command >= 1);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expect(!model.shared_workspace.refused);
    try testing.expect(!model.peers.items[0].workspace.refused);
}

test "Open Here on a peer's listing opens the tab on that peer" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.startWith("hello_directory_host.bin");
    defer pair.engine.destroy();
    const engine = pair.engine;
    try showMini(&pair);
    var request: [11]u8 = undefined;
    var out: [picker.max_bytes]u8 = undefined;
    _ = try picker.handle(engine, pickerRequest(.open, 0, &request), &out);
    try testing.expectEqual(pair.mini.providerId(), engine.directory_origin.coordinator);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try fixture.stageFixture(pair.mini.bridge, "directory_listing.bin");
    var fx: PeerFx = .{};
    try testing.expect(engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .data }, null));
    const page = try picker.handle(engine, pickerRequest(.page, 1, &request), &out);
    try testing.expect(std.mem.indexOf(u8, page, "\xff\xff") != null);
    try testing.expectEqual(@as(u8, 0), page[6] & picker.flag_open_here_unavailable);
    _ = countFrames(pair.mini);
    _ = countFrames(pair.here);

    _ = try picker.handle(engine, pickerRequest(.here, 1, &request), &out);
    // One SPAWN, on mini; This Mac's coordinator hears nothing.
    try testing.expectEqual(@as(usize, 1), countFrames(pair.mini).spawn);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expectEqual(@as(usize, 1), engine.peer_edits.pendingCreations(0));
}

// ------------------------------------------------ Rename Session (session_commands)

const session_commands = @import("../cockpit/native/session_commands.zig");

const SessionAnswer = struct {
    phase: session_commands.Phase,
    name: []const u8,
    host: []const u8,
    reason: []const u8,
};

/// One `cockpit.session` request through the handler the bridge calls.
fn sessionCommand(engine: *ts_engine.Engine, kind: u8, name: []const u8, out: *[session_commands.max_bytes]u8) !SessionAnswer {
    var fx: PeerFx = .{};
    return sessionCommandWith(engine, &fx, kind, name, out);
}

fn sessionCommandWith(engine: *ts_engine.Engine, fx: *PeerFx, kind: u8, name: []const u8, out: *[session_commands.max_bytes]u8) !SessionAnswer {
    var request: [3 + 255]u8 = undefined;
    request[0] = session_commands.version;
    request[1] = kind;
    request[2] = @intCast(name.len);
    @memcpy(request[3..][0..name.len], name);
    const reply = try session_commands.handle(engine, fx, request[0 .. 3 + name.len], out);
    const host_at = 3 + @as(usize, reply[2]);
    const reason_at = host_at + 1 + @as(usize, reply[host_at]);
    return .{
        .phase = @enumFromInt(reply[1]),
        .name = reply[3..host_at],
        .host = reply[host_at + 1 .. reason_at],
        .reason = reply[reason_at + 1 ..],
    };
}

/// The sessions scope of the switcher, as the core pages it.
fn sessionsPage(engine: *ts_engine.Engine, out: []u8) ![]const u8 {
    return scopePage(engine, .sessions, out);
}

fn scopePage(engine: *ts_engine.Engine, scope: navigation.Scope, out: []u8) ![]const u8 {
    var request: [15]u8 = undefined;
    request[0] = 1;
    request[1] = 4;
    std.mem.writeInt(u64, request[2..10], engine.revision, .little);
    std.mem.writeInt(u16, request[10..12], 0, .little);
    request[12] = 0;
    request[13] = @intFromEnum(scope);
    request[14] = 0;
    return engine.navigationSnapshot(&request, out);
}

test "Rename Session writes to the coordinator that owns the session on screen, and to no other" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    try pair.projectBoth();
    const engine = pair.engine;
    const model = engine.model;
    var fx: PeerFx = .{};
    var out: [session_commands.max_bytes]u8 = undefined;

    // mini's tab is on screen: the rename names mini's session, on mini.
    const described = try sessionCommand(engine, 1, "", &out);
    try testing.expectEqual(session_commands.Phase.ready, described.phase);
    try testing.expectEqualStrings("fixture", described.name);
    try testing.expectEqualStrings("mini", described.host);
    const sent = try sessionCommand(engine, 2, "renamed", &out);
    try testing.expectEqual(session_commands.Phase.pending, sent.phase);
    try testing.expect(contains(pair.mini, "fixture\x00renamed").found);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);

    // mini's server applies it: mini's switcher row follows METADATA_CHANGED
    // and the rename settles; This Mac's session keeps its name.
    try feedMini(&pair, &fx, "session_renamed.bin");
    try testing.expectEqualStrings("renamed", pair.mini.sessionCatalog()[0].name);
    try testing.expectEqualStrings("fixture", pair.here.sessionCatalog()[0].name);
    try testing.expectEqual(session_commands.Phase.renamed, (try sessionCommand(engine, 3, "", &out)).phase);
    var page_out: [navigation.max_bytes]u8 = undefined;
    const page = try sessionsPage(engine, &page_out);
    try testing.expect(std.mem.indexOf(u8, page, "renamed") != null);
    try testing.expect(std.mem.indexOf(u8, page, "fixture") != null);
    _ = countFrames(pair.mini);

    // This Mac's tab: the rename is This Mac's, and mini hears nothing.
    try testing.expect(model.selectTerminal(try refOn(pair.here, 7)));
    const here_session = try sessionCommand(engine, 1, "", &out);
    try testing.expectEqualStrings("fixture", here_session.name);
    try testing.expectEqualStrings("This Mac", here_session.host);
    try testing.expectEqual(session_commands.Phase.pending, (try sessionCommand(engine, 2, "renamed", &out)).phase);
    try testing.expect(contains(pair.here, "fixture\x00renamed").found);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);

    // The header (the active coordinator's session name) follows This Mac's
    // METADATA_CHANGED.
    var before_out: [@import("../cockpit/native/ts_snapshot.zig").max_bytes]u8 = undefined;
    try testing.expect(std.mem.indexOf(u8, try engine.snapshot(&before_out), "renamed") == null);
    try fixture.stageFixture(pair.here.bridge, "session_renamed.bin");
    _ = engine.onPhuxChannel(&fx, .{ .key = support.phux_channel_key, .kind = .data }, null);
    try testing.expectEqualStrings("renamed", pair.here.sessionCatalog()[0].name);
    var after_out: [@import("../cockpit/native/ts_snapshot.zig").max_bytes]u8 = undefined;
    try testing.expect(std.mem.indexOf(u8, try engine.snapshot(&after_out), "renamed") != null);
    try testing.expectEqual(session_commands.Phase.renamed, (try sessionCommand(engine, 3, "", &out)).phase);
}

test "a pending rename refuses a second rename on its own coordinator only" {
    // phux-c2td.31: the pending rename was one engine-wide slot, so a rename
    // still pending on mini refused a rename of This Mac's session as well.
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    try pair.projectBoth();
    const engine = pair.engine;
    const model = engine.model;
    var fx: PeerFx = .{};
    var out: [session_commands.max_bytes]u8 = undefined;

    // mini's tab is on screen: its rename is sent to mini and stays pending.
    try testing.expectEqual(session_commands.Phase.pending, (try sessionCommand(engine, 2, "renamed", &out)).phase);
    try testing.expect(contains(pair.mini, "fixture\x00renamed").found);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);

    // This Mac's tab: its rename goes out while mini's is still pending.
    try testing.expect(model.selectTerminal(try refOn(pair.here, 7)));
    try testing.expectEqual(session_commands.Phase.pending, (try sessionCommand(engine, 2, "renamed", &out)).phase);
    try testing.expect(contains(pair.here, "fixture\x00renamed").found);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);

    // A second rename on This Mac, whose rename is still pending, is refused
    // and nothing is sent to either coordinator.
    const again = try sessionCommand(engine, 2, "other", &out);
    try testing.expectEqual(session_commands.Phase.refused, again.phase);
    try testing.expectEqualStrings("A rename is already in progress.", again.reason);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);
    // So is a second rename on mini, once its tab is on screen again.
    try testing.expect(model.selectTerminal(try refOn(pair.mini, 7)));
    const mini_again = try sessionCommand(engine, 2, "other", &out);
    try testing.expectEqual(session_commands.Phase.refused, mini_again.phase);
    try testing.expectEqualStrings("A rename is already in progress.", mini_again.reason);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);

    // Each settles on its own coordinator; status reports the latest rename,
    // This Mac's, which is still pending until This Mac's server applies it.
    try feedMini(&pair, &fx, "session_renamed.bin");
    try testing.expectEqualStrings("renamed", pair.mini.sessionCatalog()[0].name);
    try testing.expectEqual(session_commands.Phase.pending, (try sessionCommand(engine, 3, "", &out)).phase);
    try fixture.stageFixture(pair.here.bridge, "session_renamed.bin");
    _ = engine.onPhuxChannel(&fx, .{ .key = support.phux_channel_key, .kind = .data }, null);
    try testing.expectEqual(session_commands.Phase.renamed, (try sessionCommand(engine, 3, "", &out)).phase);
    // mini's rename settled, so mini takes another.
    try testing.expectEqual(session_commands.Phase.pending, (try sessionCommand(engine, 2, "third", &out)).phase);
    try testing.expect(contains(pair.mini, "renamed\x00third").found);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
}

test "a refused rename says why and writes nothing; a pane of a coordinator no longer held names nothing" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    try pair.projectBoth();
    const engine = pair.engine;
    const model = engine.model;
    var out: [session_commands.max_bytes]u8 = undefined;
    try testing.expect(model.selectTerminal(try refOn(pair.here, 7)));
    // This Mac also holds `deploy`.
    try pair.here.host.sessions.append(testing.allocator, .{
        .id = 2,
        .name = try testing.allocator.dupe(u8, "deploy"),
        .created_at_unix_secs = 0,
        .window_count = 1,
        .attached_client_count = 0,
        .focused = false,
    });

    const duplicate = try sessionCommand(engine, 2, "deploy", &out);
    try testing.expectEqual(session_commands.Phase.refused, duplicate.phase);
    try testing.expectEqualStrings("\"deploy\" already exists on This Mac.", duplicate.reason);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);
    // A control character is refused before anything is sent.
    try testing.expectEqual(session_commands.Phase.refused, (try sessionCommand(engine, 2, "a\tb", &out)).phase);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);

    // A focused pane minted by a coordinator Cockpit no longer holds: the
    // rename is refused, never sent to This Mac or to mini instead.
    const tree = model.selectedTree().?;
    const replaced = tree.nodes[tree.focus].terminal;
    defer tree.nodes[tree.focus].terminal = replaced;
    tree.nodes[tree.focus].terminal = .{ .provider_id = contract.phuxCoordinatorId("studio"), .terminal_id = (try refOn(pair.here, 7)).terminal_id };
    try testing.expectEqual(session_commands.Phase.unavailable, (try sessionCommand(engine, 1, "", &out)).phase);
    try testing.expectEqual(session_commands.Phase.unavailable, (try sessionCommand(engine, 2, "gone", &out)).phase);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);
}

// ------------------------------- renames followed live, and Rename on a row

/// This Mac attached to `fixture`, and mini beside it LISTING through real
/// frames: negotiated, its GET_STATE queued, never attached.
fn listingPair() !Pair {
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    errdefer engine.destroy();
    const here = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/multi-coordinator-unused" }, null, "multi");
    engine.model.phux_provider = here;
    try fixture.attachHostWith(here.host, "hello.bin");
    const mini = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "mini" } }, null, "multi");
    try engine.model.ensurePeerSlots(1);
    engine.model.peers.items[0].provider = mini;
    mini.standBy();
    try mini.host.start("multi");
    try fixture.stageFixture(mini.bridge, "hello.bin");
    _ = try mini.drainReadiness();
    return .{ .engine = engine, .here = here, .mini = mini };
}

/// The captured target of the sessions-page row labelled `label`.
fn pageTarget(page: []const u8, label: []const u8, out: *[targets.max_len]u8) ![]const u8 {
    // The 15-byte scoped request echo, then total:u16 and count:u8.
    var at: usize = 18;
    for (0..page[17]) |_| {
        const label_len = page[at + 2];
        const target_len = std.mem.readInt(u16, page[at + 3 ..][0..2], .little);
        const target = page[at + 5 ..][0..target_len];
        if (std.mem.eql(u8, page[at + 5 + target_len ..][0..label_len], label)) {
            @memcpy(out[0..target_len], target);
            return out[0..target_len];
        }
        at += 5 + target_len + label_len;
    }
    return error.RowNotFound;
}

/// One `cockpit.session` row request (kind 6 or 7) through the bridge's handler.
fn rowCommand(engine: *ts_engine.Engine, kind: u8, name: []const u8, target: []const u8, out: *[session_commands.max_bytes]u8) !SessionAnswer {
    var request: [4 + 255 + 255]u8 = undefined;
    request[0] = session_commands.version;
    request[1] = kind;
    request[2] = @intCast(name.len);
    @memcpy(request[3..][0..name.len], name);
    request[3 + name.len] = @intCast(target.len);
    @memcpy(request[4 + name.len ..][0..target.len], target);
    var fx: PeerFx = .{};
    const reply = try session_commands.handle(engine, &fx, request[0 .. 4 + name.len + target.len], out);
    const host_at = 3 + @as(usize, reply[2]);
    const reason_at = host_at + 1 + @as(usize, reply[host_at]);
    return .{
        .phase = @enumFromInt(reply[1]),
        .name = reply[3..host_at],
        .host = reply[host_at + 1 .. reason_at],
        .reason = reply[reason_at + 1 ..],
    };
}

test "every coordinator connection follows renames from its handshake, and a listing peer never attaches for it" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    // phux-c2td.33: the key used to be subscribed only at this Cockpit's own
    // first rename, so another client's rename reached a list only on the
    // next refresh. Now every connection subscribes right after HELLO_OK.
    const active = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/multi-coordinator-unused" }, null, "multi");
    defer active.destroy();
    try active.host.start("multi");
    _ = countFrames(active);
    try fixture.stageFixture(active.bridge, "hello.bin");
    _ = try active.host.drainReadiness();
    const negotiated = countFrames(active);
    try testing.expectEqual(@as(usize, 1), negotiated.subscribe);
    try testing.expectEqual(@as(usize, 1), negotiated.total);

    var pair = try listingPair();
    defer pair.engine.destroy();
    // The listing peer: one read-only subscription beside its GET_STATE.
    const handshake = countFrames(pair.mini);
    try testing.expectEqual(@as(usize, 1), handshake.subscribe);
    try testing.expectEqual(@as(usize, 1), handshake.command);
    try testing.expectEqual(@as(usize, 0), handshake.attach);
    try testing.expect(!pair.mini.showing());

    // Each connection subscribes again: the next one is a new client.
    pair.mini.stop();
    try pair.mini.host.reconnect("multi");
    _ = countFrames(pair.mini);
    try fixture.stageFixture(pair.mini.bridge, "hello.bin");
    _ = try pair.mini.drainReadiness();
    const again = countFrames(pair.mini);
    try testing.expectEqual(@as(usize, 1), again.subscribe);
    try testing.expectEqual(@as(usize, 0), again.attach);
}

test "another client's rename moves the switcher row and the header live, on the active coordinator and on a listing peer" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try listingPair();
    defer pair.engine.destroy();
    const engine = pair.engine;
    var fx: PeerFx = .{};
    try fixture.stageFixture(pair.mini.bridge, "standby_state.bin");
    try testing.expect((try pair.mini.drainReadiness()).sessions_listed);
    _ = countFrames(pair.mini);
    _ = countFrames(pair.here);

    // Someone else renames mini's `build` to `ship`: the listing peer's row
    // follows at once, and mini still attaches nothing.
    var page_out: [navigation.max_bytes]u8 = undefined;
    try testing.expect(std.mem.indexOf(u8, try sessionsPage(engine, &page_out), "build") != null);
    try fixture.stageFixture(pair.mini.bridge, "standby_session_renamed.bin");
    wakeMini(engine, &fx);
    try testing.expectEqualStrings("ship", pair.mini.standbyCatalog()[0].name);
    const page = try sessionsPage(engine, &page_out);
    try testing.expect(std.mem.indexOf(u8, page, "ship") != null);
    try testing.expect(std.mem.indexOf(u8, page, "build") == null);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).attach);
    try testing.expect(!pair.mini.showing());

    // Someone else renames This Mac's attached `fixture`: the header and its
    // row follow, though this Cockpit renamed nothing.
    var before: [ts_snapshot.max_bytes]u8 = undefined;
    try testing.expect(std.mem.indexOf(u8, try engine.snapshot(&before), "renamed") == null);
    try fixture.stageFixture(pair.here.bridge, "session_renamed.bin");
    _ = engine.onPhuxChannel(&fx, .{ .key = support.phux_channel_key, .kind = .data }, null);
    var after: [ts_snapshot.max_bytes]u8 = undefined;
    try testing.expect(std.mem.indexOf(u8, try engine.snapshot(&after), "renamed") != null);
    try testing.expect(std.mem.indexOf(u8, try sessionsPage(engine, &page_out), "renamed") != null);
    try testing.expect(pair.here.renameInfo().status == .none);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).attach);
}

test "Rename on a switcher row goes to the coordinator that listed it and to no other, and a listing peer never attaches" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try listingPair();
    defer pair.engine.destroy();
    const engine = pair.engine;
    var fx: PeerFx = .{};
    try fixture.stageFixture(pair.mini.bridge, "standby_state.bin");
    try testing.expect((try pair.mini.drainReadiness()).sessions_listed);
    var page_out: [navigation.max_bytes]u8 = undefined;
    const page = try sessionsPage(engine, &page_out);
    var build_buffer: [targets.max_len]u8 = undefined;
    var fixture_buffer: [targets.max_len]u8 = undefined;
    const build = try pageTarget(page, "build", &build_buffer);
    const here_row = try pageTarget(page, "fixture", &fixture_buffer);
    _ = countFrames(pair.mini);
    _ = countFrames(pair.here);
    var out: [session_commands.max_bytes]u8 = undefined;

    // mini's `build` row names mini's `build`, though mini is only listing.
    const described = try rowCommand(engine, 6, "", build, &out);
    try testing.expectEqual(session_commands.Phase.ready, described.phase);
    try testing.expectEqualStrings("build", described.name);
    try testing.expectEqualStrings("mini", described.host);
    // Renaming it writes on mini's listing connection: the write and its
    // confirmation read, no ATTACH; This Mac hears nothing.
    try testing.expectEqual(session_commands.Phase.pending, (try rowCommand(engine, 7, "ship", build, &out)).phase);
    const written = contains(pair.mini, "build\x00ship");
    try testing.expect(written.found);
    try testing.expectEqual(@as(usize, 2), written.total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expect(!pair.mini.showing());
    // mini's server applies it; the rename settles and the row follows.
    try fixture.stageFixture(pair.mini.bridge, "standby_session_renamed.bin");
    wakeMini(engine, &fx);
    try testing.expectEqual(session_commands.Phase.renamed, (try sessionCommand(engine, 3, "", &out)).phase);
    try testing.expect(std.mem.indexOf(u8, try sessionsPage(engine, &page_out), "ship") != null);
    _ = countFrames(pair.mini);

    // This Mac's `fixture` row renames on This Mac alone.
    try testing.expectEqual(session_commands.Phase.pending, (try rowCommand(engine, 7, "renamed", here_row, &out)).phase);
    try testing.expect(contains(pair.here, "fixture\x00renamed").found);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);

    // A name mini's other session holds is refused with the reason, and
    // nothing is sent anywhere.
    const refused = try rowCommand(engine, 7, "deploy", build, &out);
    try testing.expectEqual(session_commands.Phase.refused, refused.phase);
    try testing.expectEqualStrings("\"deploy\" already exists on mini.", refused.reason);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);

    // Fail closed: mini's row claimed by This Mac's id, or held against a
    // context mini has left, names nothing and reaches no coordinator.
    var forged = build_buffer;
    std.mem.writeInt(u64, forged[2..10], @intFromEnum(pair.here.providerId()), .little);
    try testing.expectEqual(session_commands.Phase.unavailable, (try rowCommand(engine, 7, "x", forged[0..build.len], &out)).phase);
    var stale = build_buffer;
    stale[26] +%= 1;
    const gone = try rowCommand(engine, 7, "x", stale[0..build.len], &out);
    try testing.expectEqual(session_commands.Phase.unavailable, gone.phase);
    try testing.expect(gone.reason.len != 0);
    try testing.expectEqual(session_commands.Phase.unavailable, (try rowCommand(engine, 6, "", stale[0..build.len], &out)).phase);
    // This Mac's session row claimed by mini's id: a session by number is
    // only the active coordinator's, so it names nothing on mini.
    var claimed = fixture_buffer;
    std.mem.writeInt(u64, claimed[2..10], @intFromEnum(pair.mini.providerId()), .little);
    try testing.expectEqual(session_commands.Phase.unavailable, (try rowCommand(engine, 7, "x", claimed[0..here_row.len], &out)).phase);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expect(!pair.mini.showing());

    // mini's connection ends: its row, captured on that connection, names
    // nothing, and nothing reaches either coordinator.
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null);
    try testing.expectEqual(session_commands.Phase.unavailable, (try rowCommand(engine, 7, "x", build, &out)).phase);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.mini).total);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
}

// ------------------------------------ keep-empty sessions (empty_session.zig)

const empty_session = @import("../cockpit/native/empty_session.zig");
const ts_snapshot = @import("../cockpit/native/ts_snapshot.zig");

fn wakeMini(engine: *ts_engine.Engine, fx: *PeerFx) void {
    _ = engine.onPeerChannel(fx, .{ .key = engine.peerChannelKey(0), .kind = .data }, null);
}

test "a peer's empty session is listed as empty, and picking it attaches nothing until New Tab shows it there" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const model = engine.model;
    const here = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/multi-coordinator-unused" }, null, "multi");
    model.phux_provider = here;
    try fixture.attachHostWith(here.host, "hello.bin");
    // mini lists without attaching: `build` and the keep-empty `scratch` (3).
    const mini = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "mini" } }, null, "multi");
    try model.ensurePeerSlots(1);
    model.peers.items[0].provider = mini;
    mini.standBy();
    try mini.host.start("multi");
    try fixture.stageFixture(mini.bridge, "hello_keep_empty.bin");
    _ = try mini.drainReadiness();
    try fixture.stageFixture(mini.bridge, "standby_keep_empty_state.bin");
    try testing.expect((try mini.drainReadiness()).sessions_listed);
    const listed = mini.standbyCatalog();
    try testing.expect(!listed[0].empty);
    try testing.expect(listed[1].empty and listed[1].keep_empty);
    _ = countFrames(mini);
    _ = countFrames(here);

    // The switcher offers it as an empty session, not as a broken one.
    var page_out: [navigation.max_bytes]u8 = undefined;
    try testing.expect(std.mem.indexOf(u8, try sessionsPage(engine, &page_out), "Empty session · mini") != null);
    // Paging the switcher refreshes a listing peer's list (GET_STATE) and the
    // active coordinator's workspace; neither is an attach.
    try testing.expectEqual(@as(usize, 0), countFrames(mini).attach);
    try testing.expectEqual(@as(usize, 0), countFrames(here).attach);

    // Picking it shows the Empty session state; nothing attaches, nothing
    // restarts, and settling leaves mini listing.
    var fx: PeerFx = .{};
    try testing.expect(engine.showPeerSession(mini.providerId(), 3, &fx));
    try testing.expectEqual(@as(usize, 0), fx.restarts);
    try testing.expect(!mini.showing());
    try testing.expectEqual(@as(usize, 0), countFrames(mini).total);
    try testing.expect(!engine.settlePeers(&fx));
    try testing.expectEqual(@as(usize, 0), fx.restarts);
    const shown = empty_session.view(model, 0).?;
    try testing.expect(shown.picked);
    try testing.expectEqualStrings("scratch", shown.name);
    var snap_out: [ts_snapshot.max_bytes]u8 = undefined;
    try testing.expect(std.mem.indexOf(u8, try engine.snapshot(&snap_out), "scratch") != null);

    // New Tab shows it on mini alone: only mini restarts, to attach it.
    var out: [session_commands.max_bytes]u8 = undefined;
    try testing.expectEqual(session_commands.Phase.pending, (try sessionCommandWith(engine, &fx, 4, "", &out)).phase);
    try testing.expectEqual(@as(usize, 1), fx.restarts);
    try testing.expect(mini.showing());
    try testing.expectEqual(@as(usize, 0), countFrames(here).total);

    // mini's next connection attaches `scratch` by id; This Mac hears nothing.
    mini.stop();
    try mini.host.reconnect("multi");
    try fixture.stageFixture(mini.bridge, "hello_keep_empty.bin");
    wakeMini(engine, &fx);
    try testing.expectEqual(@as(usize, 1), countFrames(mini).attach);
    try fixture.stageFixture(mini.bridge, "attached_empty.bin");
    wakeMini(engine, &fx);
    _ = countFrames(mini);
    // Its empty workspace projects, and its first tab spawns there.
    try fixture.stageFixture(mini.bridge, "workspace_empty.bin");
    wakeMini(engine, &fx);
    try testing.expectEqual(@as(usize, 1), engine.peer_edits.pendingCreations(0));
    try testing.expectEqual(@as(usize, 1), countFrames(mini).spawn);
    try testing.expectEqual(@as(usize, 0), countFrames(here).total);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    // Held shown while that tab is on its way, not returned to listing.
    try testing.expect(!engine.settlePeers(&fx));
    try testing.expect(mini.showing());

    // mini's connection fails before the tab lands: the hold ends, and a
    // hidden mini goes back to listing.
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null);
    try testing.expect(model.empty_pick == null);
}

test "New Tab in the active coordinator's empty session opens there, and on no peer" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const model = engine.model;
    // This Mac is attached to its keep-empty `scratch` (3), which has no
    // windows: ATTACHED with nothing to bootstrap, then an empty workspace.
    const here = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/multi-coordinator-unused" }, null, "multi");
    model.phux_provider = here;
    try here.host.start("multi");
    try fixture.stageFixture(here.bridge, "hello_keep_empty.bin");
    _ = try here.host.drainReadiness();
    try here.host.attachSessionId(3, .{ .cols = 80, .rows = 24 });
    try fixture.stageFixture(here.bridge, "attached_empty.bin");
    try testing.expect((try here.host.drainReadiness()).ready_published);
    try fixture.stageFixture(here.bridge, "workspace_empty.bin");
    _ = try here.host.drainReadiness();
    here.bridge.outgoing.reset();
    // mini is shown beside it, attached to its own session.
    const mini = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "mini" } }, null, "multi");
    try model.ensurePeerSlots(1);
    model.peers.items[0].provider = mini;
    try mini.show(1);
    try fixture.attachHostWith(mini.host, "hello.bin");
    // The engine projects This Mac's empty workspace: no tab anywhere.
    var fx: PeerFx = .{};
    _ = engine.onPhuxChannel(&fx, .{ .key = support.phux_channel_key, .kind = .data }, null);
    try testing.expectEqual(@as(usize, 0), model.primary.tab_count);
    const shown = empty_session.view(model, 0).?;
    try testing.expect(!shown.picked);
    try testing.expectEqualStrings("scratch", shown.name);
    try testing.expectEqualStrings("This Mac", shown.host);
    var snap_out: [ts_snapshot.max_bytes]u8 = undefined;
    try testing.expect(std.mem.indexOf(u8, try engine.snapshot(&snap_out), "scratch") != null);
    _ = countFrames(here);
    _ = countFrames(mini);

    // New Tab spawns in `scratch` on This Mac, and mini hears nothing.
    var out: [session_commands.max_bytes]u8 = undefined;
    try testing.expectEqual(session_commands.Phase.pending, (try sessionCommandWith(engine, &fx, 4, "", &out)).phase);
    try testing.expectEqual(@as(usize, 1), countFrames(here).spawn);
    try testing.expectEqual(@as(usize, 0), countFrames(mini).total);
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try testing.expectEqual(@as(usize, 0), fx.restarts);
}

// ------------------------------ New Window and the inventory follow the focus

test "New Window with a peer's pane focused opens its tab on that peer, in the new window" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    const engine = pair.engine;
    const model = engine.model;
    try showMini(&pair);
    var fx: PeerFx = .{};
    const windows = model.openWindowCount();

    // A SPAWN on mini alone, and a native window opened for its tab.
    try testing.expect(intent(engine, .new_window, 0));
    try testing.expectEqual(@as(usize, 1), countFrames(pair.mini).spawn);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expectEqual(@as(usize, 1), engine.peer_edits.pendingCreations(0));
    try testing.expectEqual(windows + 1, model.openWindowCount());
    const opened = model.active_window;
    try testing.expect(opened != 0);

    // mini spawns terminal 8 and places it; it lands in that window, as
    // mini's tab, focused, and This Mac hears nothing.
    try feedMini(&pair, &fx, "spawn-local.bin");
    try feedMini(&pair, &fx, "local-ready.bin");
    try miniWorkspaceReply(&pair, "workspace_refresh_metadata.bin", 3, 3);
    try miniWorkspaceReply(&pair, "workspace_refresh_state.bin", 2, 2);
    drainMini(&pair, &fx);
    try miniWorkspaceReply(&pair, "workspace_add_metadata.bin", 14, 5);
    try miniWorkspaceReply(&pair, "workspace_add_state.bin", 13, 4);
    drainMini(&pair, &fx);
    const mini8 = try refOn(pair.mini, 8);
    try testing.expectEqual(@as(usize, 0), engine.peer_edits.pendingCreations(0));
    try testing.expectEqual(opened, model.locateTerminal(mini8).?.window);
    try testing.expectEqual(pair.mini.providerId(), tabOwner(model, mini8).?);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expect(!engine.settlePeers(&fx));
    try testing.expectEqual(windows + 1, model.openWindowCount());
}

test "a peer's New Window that never lands leaves no empty window behind" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    const engine = pair.engine;
    const model = engine.model;
    try showMini(&pair);
    var fx: PeerFx = .{};
    const windows = model.openWindowCount();
    try testing.expect(intent(engine, .new_window, 0));
    try testing.expectEqual(windows + 1, model.openWindowCount());
    // mini's connection closes before the tab is placed: the spawn is
    // forgotten, and settling closes the window it would have landed in.
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null);
    try testing.expectEqual(@as(usize, 0), engine.peer_edits.pendingCreations(0));
    _ = engine.settlePeers(&fx);
    try testing.expectEqual(windows, model.openWindowCount());
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
}

test "the available inventory follows the focused pane's coordinator, and a peer's terminal is placed on that peer" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    const engine = pair.engine;
    const model = engine.model;
    try showMini(&pair);
    // This Mac's own tab is on screen too, beside mini's.
    model.shared_workspace.authority = pair.here.providerId();
    _ = try model.shared_workspace.apply(model, pair.here.workspaceSnapshot(), pair.here.connectionEpoch());
    var fx: PeerFx = .{};
    // mini's registry now holds terminal 8 in its shown session, in no tab.
    _ = try pair.mini.requestWorkspaceRefresh();
    try miniWorkspaceReply(&pair, "workspace_refresh_metadata.bin", 3, 3);
    try miniWorkspaceReply(&pair, "workspace_refresh_state.bin", 2, 2);
    drainMini(&pair, &fx);
    const mini7 = try refOn(pair.mini, 7);
    const mini8 = try refOn(pair.mini, 8);
    try testing.expect(model.locateTerminal(mini8) == null);

    // With mini's pane focused, the switcher offers mini's terminal 8; with
    // This Mac's, it does not.
    var page_out: [navigation.max_bytes]u8 = undefined;
    try testing.expect(model.selectTerminal(mini7));
    try testing.expect(std.mem.indexOf(u8, try scopePage(engine, .all, &page_out), "unplaced terminal") != null);
    try testing.expect(model.selectTerminal(try refOn(pair.here, 7)));
    try testing.expect(std.mem.indexOf(u8, try scopePage(engine, .all, &page_out), "unplaced terminal") == null);
    try testing.expect(model.selectTerminal(mini7));
    _ = countFrames(pair.mini);
    _ = countFrames(pair.here);

    // Picking it attaches and places it on mini alone.
    const tab_commands = @import("../cockpit/native/tab_commands.zig");
    const target = targets.capture(model, .{ .available_terminal = mini8 }).?;
    var target_bytes: [targets.max_len]u8 = undefined;
    const encoded = target.encode(&target_bytes);
    var command: [targets.max_len + 10]u8 = undefined;
    command[0] = 1;
    command[1] = 2;
    std.mem.writeInt(u64, command[2..10], 7, .little);
    @memcpy(command[10..][0..encoded.len], encoded);
    const receipt = engine.applySelectionCommand(command[0 .. 10 + encoded.len], &ts_engine.NoShells{});
    try testing.expectEqual(tab_commands.Status.accepted_pending, receipt.status);
    try testing.expectEqual(@as(usize, 1), engine.peer_edits.pendingCreations(0));
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expect(countFrames(pair.mini).total >= 1);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    // A second pick of the same terminal is refused while it is pending.
    std.mem.writeInt(u64, command[2..10], 8, .little);
    try testing.expectEqual(tab_commands.Status.rejected, engine.applySelectionCommand(command[0 .. 10 + encoded.len], &ts_engine.NoShells{}).status);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
}

// ------------------------------------- unplaced spawns (ADR-0109, strays)

/// The instance token spawn-bound.bin binds terminal 8 to.
const fixture_instance = [_]u8{0xa5} ** 16;

/// mini, listing again on a fresh connection: HELLO_OK, then its list. Its
/// wakes arrive on the slot's current channel: the close that ended the
/// last connection moved the slot to its next channel generation.
fn relistMini(pair: *Pair, fx: *PeerFx, hello: []const u8) !void {
    pair.mini.standBy();
    pair.mini.stop();
    try pair.mini.host.reconnect("multi");
    const key = pair.engine.peerChannelKey(0);
    try fixture.stageFixture(pair.mini.bridge, hello);
    _ = pair.engine.onPeerChannel(fx, .{ .key = key, .kind = .data }, null);
    try fixture.stageFixture(pair.mini.bridge, "standby_state.bin");
    _ = pair.engine.onPeerChannel(fx, .{ .key = key, .kind = .data }, null);
}

test "a peer's spawn whose placement was never sent is killed there, conditionally, once it lists again" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.startWith("hello_conditional_kill.bin");
    defer pair.engine.destroy();
    const engine = pair.engine;
    try showMini(&pair);
    var fx: PeerFx = .{};
    try testing.expect(pair.mini.conditionalKillSupported());

    // New Tab spawns terminal 8 on mini, bound to mini's instance token;
    // mini's connection ends before its placement is sent.
    try testing.expect(intent(engine, .new_terminal, 0));
    try feedMini(&pair, &fx, "spawn-bound.bin");
    try testing.expectEqual(@as(usize, 1), engine.peer_edits.pendingCreations(0));
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null);
    try testing.expectEqual(@as(usize, 0), engine.peer_edits.pendingCreations(0));
    try testing.expectEqual(@as(usize, 1), engine.peer_edits.strayCount(0));
    _ = countFrames(pair.here);

    // Listing again, mini alone is asked to kill it, bound to that token
    // and to nobody having attached it; This Mac hears nothing.
    try relistMini(&pair, &fx, "hello_conditional_kill.bin");
    try testing.expectEqual(@as(usize, 0), engine.peer_edits.strayCount(0));
    const sent = contains(pair.mini, &fixture_instance);
    try testing.expect(sent.found);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
}

test "without CONDITIONAL_KILL a peer's unplaced spawn is left running and nothing is killed" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    const engine = pair.engine;
    try showMini(&pair);
    var fx: PeerFx = .{};
    try testing.expect(!pair.mini.conditionalKillSupported());
    try testing.expect(intent(engine, .new_terminal, 0));
    try feedMini(&pair, &fx, "spawn-local.bin");
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null);
    // Unbound, it is no stray: nothing could kill it safely.
    try testing.expectEqual(@as(usize, 0), engine.peer_edits.strayCount(0));
    _ = countFrames(pair.here);
    try relistMini(&pair, &fx, "hello.bin");
    // The new connection's HELLO, its read-only rename subscription, then
    // GET_STATE and no other command: no kill of any kind.
    const frames = countFrames(pair.mini);
    try testing.expectEqual(@as(usize, 3), frames.total);
    try testing.expectEqual(@as(usize, 1), frames.subscribe);
    try testing.expectEqual(@as(usize, 1), frames.command);
    try testing.expectEqual(@as(usize, 0), frames.attach);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
}

test "a peer's spawn whose placement was already sent is never killed" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.startWith("hello_conditional_kill.bin");
    defer pair.engine.destroy();
    const engine = pair.engine;
    try showMini(&pair);
    var fx: PeerFx = .{};
    try testing.expect(intent(engine, .new_terminal, 0));
    try feedMini(&pair, &fx, "spawn-bound.bin");
    try feedMini(&pair, &fx, "local-ready.bin");
    // Its placement is on the wire: the refresh settles and the add is sent.
    try miniWorkspaceReply(&pair, "workspace_refresh_metadata.bin", 3, 3);
    try miniWorkspaceReply(&pair, "workspace_refresh_state.bin", 2, 2);
    drainMini(&pair, &fx);
    try testing.expect(miniSent(engine, .add));
    // The connection ends before the add is confirmed: the add may still
    // land there, so the terminal is no stray, and nothing is killed.
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null);
    try testing.expectEqual(@as(usize, 0), engine.peer_edits.strayCount(0));
    try relistMini(&pair, &fx, "hello_conditional_kill.bin");
    try testing.expect(!contains(pair.mini, &fixture_instance).found);
}

test "an edit of a peer that cannot take one is refused and reaches no coordinator" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(true);
    defer pair.engine.destroy();
    const engine = pair.engine;
    // mini's pane is focused, but mini's workspace is not projected for this
    // connection: New Tab and a split are refused, never sent to This Mac.
    const tree = engine.model.selectedTree().?;
    const replaced = tree.nodes[tree.focus].terminal;
    defer tree.nodes[tree.focus].terminal = replaced;
    tree.nodes[tree.focus].terminal = try refOn(pair.mini, 7);
    _ = countFrames(pair.here);
    _ = countFrames(pair.mini);
    // A refused peer edit leaves the active coordinator's pending focus alone.
    const pending = try refOn(pair.here, 7);
    engine.model.shared_workspace.desired_terminal = pending;
    try testing.expect(!intent(engine, .new_terminal, 0));
    try testing.expect(!intent(engine, .native_command, @intFromEnum(protocol.NativeCommand.split_right)));
    try testing.expect(engine.model.shared_workspace.desired_terminal.?.eql(pending));
    // No SPAWN or request reached mini (its focus event is input, not an
    // edit), and nothing at all reached This Mac.
    const refused = countFrames(pair.mini);
    try testing.expectEqual(@as(usize, 0), refused.spawn);
    try testing.expectEqual(@as(usize, 0), refused.command);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    try testing.expectEqual(@as(usize, 0), engine.peer_edits.pendingCreations(0));
    // Standing by, it takes no edit either.
    pair.mini.standBy();
    try testing.expect(!intent(engine, .new_terminal, 0));
    const standing = countFrames(pair.mini);
    try testing.expectEqual(@as(usize, 0), standing.spawn);
    try testing.expectEqual(@as(usize, 0), standing.command);
    try testing.expectEqual(@as(usize, 0), countFrames(pair.here).total);
}
