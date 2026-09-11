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

const testing = std.testing;
const targets = navigation.targets;
const fixture = support.PhuxProvider.test_support;
const shared = contract.workspace;
const TerminalRef = support.TerminalRef;

/// Wire type bytes after the 4-byte length prefix (phux-protocol frame/mod.rs).
const type_attach: u8 = 0x02;
const type_command: u8 = 0x31;

fn countFrames(provider: *support.PhuxProvider) struct { total: usize, attach: usize, command: usize } {
    var total: usize = 0;
    var attach: usize = 0;
    var command: usize = 0;
    while (provider.bridge.outgoing.take()) |frame| {
        defer provider.bridge.outgoing.release(frame);
        total += 1;
        if (frame[4] == type_attach) attach += 1;
        if (frame[4] == type_command) command += 1;
    }
    return .{ .total = total, .attach = attach, .command = command };
}

fn refOn(provider: *const support.PhuxProvider, id: u32) !TerminalRef {
    return .{ .provider_id = provider.providerId(), .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, id, "") } };
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
        engine.model.phux_peers[0] = mini;
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
        model.peer_workspaces[0].authority = self.mini.providerId();
        const one = [_]shared.Window{.{ .id = @splat(1), .root = 0 }};
        const here_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = try refOn(self.here, 7) }};
        const mini_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = try refOn(self.mini, 7) }};
        _ = try model.shared_workspace.apply(model, leafSnapshot(1, 1, &one, &here_nodes), self.here.connectionEpoch());
        _ = try model.peer_workspaces[0].apply(model, leafSnapshot(1, 1, &one, &mini_nodes), self.mini.connectionEpoch());
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
    const mini = &model.peer_workspaces[0];
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
    _ = engine.onPeerChannel(&fx, .{ .key = support.phuxPeerChannelKey(0), .kind = .data }, null);
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
    try testing.expect(engine.onPeerChannel(&fx, .{ .key = support.phuxPeerChannelKey(0), .kind = .data }, null));
    const page = try picker.handle(engine, pickerRequest(.page, 1, &request), &out);
    try testing.expect(std.mem.indexOf(u8, page, "devbox") != null);
    try testing.expect(std.mem.endsWith(u8, page, "\x04mini"));
    // No Open Here row on another coordinator's listing, and Open Here is
    // refused without spawning anywhere.
    try testing.expect(std.mem.indexOf(u8, page, "\xff\xff") == null);
    // The drain's own subscription frames are not the question here.
    _ = countFrames(pair.mini);
    _ = countFrames(pair.here);
    try testing.expectError(error.OtherCoordinator, picker.handle(engine, pickerRequest(.here, 1, &request), &out));
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
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
    try testing.expectEqual(@as(u32, 0), model.peer_workspaces[0].session);
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
    model.peer_workspaces[0].authority = pair.mini.providerId();
    _ = try model.peer_workspaces[0].apply(model, published, pair.mini.connectionEpoch());
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
    _ = try model.peer_workspaces[0].apply(model, leafSnapshot(published.session_id, published.revision + 1, &.{}, &.{}), pair.mini.connectionEpoch());
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
    model.peer_workspaces[0].authority = pair.mini.providerId();
    const here7 = try refOn(pair.here, 7);
    const mini7 = try refOn(pair.mini, 7);
    const one = [_]shared.Window{.{ .id = @splat(1), .root = 0 }};
    const here_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = here7 }};
    const mini_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = mini7 }};
    _ = try model.shared_workspace.apply(model, leafSnapshot(1, 1, &one, &here_nodes), pair.here.connectionEpoch());
    _ = try model.peer_workspaces[0].apply(model, leafSnapshot(1, 1, &one, &mini_nodes), pair.mini.connectionEpoch());

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
