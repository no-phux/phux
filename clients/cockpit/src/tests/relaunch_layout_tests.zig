//! A showing peer's session across relaunch (ADR-0110,
//! cockpit/native/peer_restore.zig). Every remembered host comes back
//! listing. Only the record whose tab was the front window's selected tab
//! is shown again, once that host's own list still carries the session and
//! the front window has a real size; the attach carries that size. Any other
//! record attaches nothing, a stale one is dropped, and nothing is ever sent
//! to a coordinator other than the one a record names.

const std = @import("std");
const native_sdk = @import("native_sdk");
const contract = @import("provider_contract");
const support = @import("../cockpit/phux_support.zig");
const model_module = @import("../cockpit/model.zig");
const ts_engine = @import("../cockpit/native/ts_engine.zig");
const peer_restore = @import("../cockpit/native/peer_restore.zig");
const remote_memory = @import("../cockpit/remote_memory.zig");
const scene = @import("../cockpit/native/scene.zig");
const shared_workspace = @import("../cockpit/shared_workspace.zig");

const testing = std.testing;
const fixture = support.PhuxProvider.test_support;
const shared = contract.workspace;
const TerminalRef = support.TerminalRef;

/// Wire type byte after the 4-byte length prefix (phux-protocol frame/mod.rs).
const type_attach: u8 = 0x02;

const Frames = struct { total: usize, attach: usize, viewport: bool };

/// Every frame staged since the last call; `viewport` is whether an ATTACH
/// carried `cols` by `rows` (ViewportInfo: two big-endian u16).
fn takeFrames(provider: *support.PhuxProvider, cols: u16, rows: u16) Frames {
    var needle: [4]u8 = undefined;
    std.mem.writeInt(u16, needle[0..2], cols, .big);
    std.mem.writeInt(u16, needle[2..4], rows, .big);
    var frames: Frames = .{ .total = 0, .attach = 0, .viewport = false };
    while (provider.bridge.outgoing.take()) |frame| {
        defer provider.bridge.outgoing.release(frame);
        frames.total += 1;
        if (frame[4] != type_attach) continue;
        frames.attach += 1;
        if (std.mem.indexOf(u8, frame, &needle) != null) frames.viewport = true;
    }
    return frames;
}

fn countFrames(provider: *support.PhuxProvider) Frames {
    return takeFrames(provider, 0, 0);
}

/// A workspace reply fixture staged on `provider`, its request id restamped
/// to the read it answers (the fixtures carry the ids of the sequence they
/// were generated for).
fn workspaceReply(provider: *support.PhuxProvider, name: []const u8, request: u32) !void {
    const path = try std.fmt.allocPrint(testing.allocator, "src/providers/phux/fixtures/{s}", .{name});
    defer testing.allocator.free(path);
    const bytes = try std.Io.Dir.cwd().readFileAlloc(testing.io, path, testing.allocator, .limited(64 * 1024));
    defer testing.allocator.free(bytes);
    std.mem.writeInt(u32, bytes[8..12], 0x8000_0000 + request, .big);
    try testing.expect(provider.bridge.incoming.stage(bytes));
}

/// `session`'s creation time in a session list.
fn createdOf(catalog: anytype, session: u32) ?i64 {
    for (catalog) |entry| if (entry.id == session) return entry.created_at_unix_secs;
    return null;
}

/// The creation time `standby_state.bin` lists for `session`, read through a
/// listing provider of its own so no launch under test is touched.
fn listedCreated(session: u32) !i64 {
    const peer = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "probe" } }, null, "relaunch");
    defer peer.destroy();
    peer.standBy();
    try peer.host.start("relaunch");
    try fixture.stageFixture(peer.bridge, "hello.bin");
    _ = try peer.drainReadiness();
    try fixture.stageFixture(peer.bridge, "standby_state.bin");
    _ = try peer.drainReadiness();
    return createdOf(peer.standbyCatalog(), session) orelse error.NotListed;
}

const Front = struct { authority: ?contract.ProviderId, window: ?[16]u8 };

/// The selected tab of the front window: which coordinator's, and its
/// shared window.
fn frontTab(model: *const model_module.Model) Front {
    const workspace = model.wsAtConst(model.active_window) orelse return .{ .authority = null, .window = null };
    const tree = workspace.treeConst(workspace.selected_tab) orelse return .{ .authority = null, .window = null };
    return .{ .authority = shared_workspace.tabAuthority(tree), .window = workspace.shared_ids[workspace.selected_tab] };
}

/// Channel, peer-restart and retry-timer effects, counted per slot; nothing
/// real.
const PeerFx = struct {
    restarts: [3]usize = @splat(0),
    /// The last retry timer armed, if any.
    retry_key: ?u64 = null,

    pub fn restartPeer(self: *@This(), _: *ts_engine.Engine, slot: usize) bool {
        self.restarts[slot] += 1;
        return true;
    }
    pub fn schedulePeerRetry(self: *@This(), key: u64, _: u64) void {
        self.retry_key = key;
    }
    pub fn openChannel(_: *@This(), _: anytype) native_sdk.ChannelHandle {
        return .{};
    }
    pub fn closeChannel(_: *@This(), _: u64) void {}
    pub fn showNotification(_: *@This(), _: anytype) void {}
    pub fn ptyResize(_: *@This(), _: u64, _: u16, _: u16) void {}

    fn total(self: *const @This()) usize {
        var sum: usize = 0;
        for (self.restarts) |count| sum += count;
        return sum;
    }
};

/// A relaunch: this Mac active and attached, and the remembered hosts `mini`
/// (slot 0) and `studio` (slot 1) beside it, each listing on its own
/// connection, as startup.attachRememberedPeers leaves them.
const Launch = struct {
    engine: *ts_engine.Engine,
    here: *support.PhuxProvider,
    mini: *support.PhuxProvider,
    studio: *support.PhuxProvider,

    fn start() !Launch {
        const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
        errdefer engine.destroy();
        const model = engine.model;
        const here = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/relaunch-unused" }, null, "relaunch");
        model.phux_provider = here;
        try fixture.attachHostWith(here.host, "hello.bin");
        const mini = try listingPeer(model, 0, "mini");
        const studio = try listingPeer(model, 1, "studio");
        _ = countFrames(here);
        return .{ .engine = engine, .here = here, .mini = mini, .studio = studio };
    }

    fn listingPeer(model: *model_module.Model, slot: usize, target: []const u8) !*support.PhuxProvider {
        const peer = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = target } }, null, "relaunch");
        try model.ensurePeerSlots(slot + 1);
        model.peers.items[slot].provider = peer;
        peer.standBy();
        try peer.host.start("relaunch");
        return peer;
    }

    /// A wake on the slot's current channel (its key moves on each close).
    fn wake(self: *Launch, fx: *PeerFx, slot: usize) void {
        _ = self.engine.onPeerChannel(fx, .{ .key = self.engine.peerChannelKey(slot), .kind = .data }, null);
    }

    /// The slot's current connection closes without Cockpit asking.
    fn fail(self: *Launch, fx: *PeerFx, slot: usize) void {
        _ = self.engine.onPeerChannel(fx, .{ .key = self.engine.peerChannelKey(slot), .kind = .closed }, null);
    }

    /// The restart a show asked for, as the runtime runs it: the peer's next
    /// connection negotiates, its first frame is ATTACH, the server answers
    /// ATTACHED, and its workspace read is answered with `metadata` and
    /// `state` (their request ids stamped as that read's, 1 and 0).
    fn attachShown(self: *Launch, fx: *PeerFx, slot: usize, metadata: []const u8, state: []const u8) !void {
        const peer = self.engine.model.peers.items[slot].provider.?;
        peer.stop();
        try peer.host.reconnect("relaunch");
        try self.hello(fx, slot);
        try fixture.stageFixture(peer.bridge, "attached.bin");
        self.wake(fx, slot);
        try workspaceReply(peer, metadata, 1);
        try workspaceReply(peer, state, 0);
        self.wake(fx, slot);
    }

    /// This Mac's workspace, already read, projects now (a wake on its
    /// channel).
    fn projectHere(self: *Launch, fx: *PeerFx) void {
        _ = self.engine.onPhuxChannel(fx, .{ .key = support.phux_channel_key, .kind = .data }, null);
    }

    /// HELLO_OK on the peer's connection: it asks GET_STATE and lists nothing yet.
    fn hello(self: *Launch, fx: *PeerFx, slot: usize) !void {
        try fixture.stageFixture(self.engine.model.peers.items[slot].provider.?.bridge, "hello.bin");
        self.wake(fx, slot);
    }

    /// HELLO_OK from the same host's server after a graceful upgrade: every
    /// fixture names server `cockpit-fixture`; this one names another id of
    /// the same length, so the frame is otherwise unchanged.
    fn helloUpgraded(self: *Launch, fx: *PeerFx, slot: usize) !void {
        const bytes = try fixture.readFixture("hello.bin");
        defer testing.allocator.free(bytes);
        const at = std.mem.indexOf(u8, bytes, "cockpit-fixture").?;
        @memcpy(bytes[at..][0.."cockpit-upgrade".len], "cockpit-upgrade");
        try testing.expect(self.engine.model.peers.items[slot].provider.?.bridge.incoming.stage(bytes));
        self.wake(fx, slot);
    }

    /// Its list arrives: `build` (1) and `deploy` (2).
    fn list(self: *Launch, fx: *PeerFx, slot: usize) !void {
        try fixture.stageFixture(self.engine.model.peers.items[slot].provider.?.bridge, "standby_state.bin");
        self.wake(fx, slot);
    }

    /// The record startup would hand this peer, for its server as HELLO_OK
    /// named it.
    fn remember(self: *Launch, slot: usize, shown: remote_memory.Shown) void {
        const peer = self.engine.model.peers.items[slot].provider.?;
        self.engine.model.peers.items[slot].restore = .{ .coordinator = peer.providerId(), .shown = shown, .pending = shown.front };
    }

    fn server(self: *Launch, slot: usize) u64 {
        return remote_memory.serverHash(self.engine.model.peers.items[slot].provider.?.serverId().?);
    }

    /// The server every fixture's HELLO_OK names, for a peer whose own
    /// connection has not negotiated yet.
    fn fixtureServer(self: *Launch) u64 {
        return remote_memory.serverHash(self.here.serverId().?);
    }

    /// The front window as its first frame measures it.
    fn measure(self: *Launch) void {
        const workspace = self.engine.model.wsAt(0).?;
        workspace.surface_size = .{ .width = 1200, .height = 800 };
        workspace.surface_measured = true;
    }
};

test "relaunch with a peer's tab not selected: that peer only lists and sends no ATTACH" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var launch = try Launch.start();
    defer launch.engine.destroy();
    var fx: PeerFx = .{};
    launch.measure();
    try launch.hello(&fx, 0);
    // mini was showing `build`, but its tab was not the front window's
    // selected tab when Cockpit quit.
    launch.remember(0, .{ .session = 1, .server = launch.server(0), .window = @splat(1), .front = false });
    try launch.list(&fx, 0);

    try testing.expectEqual(@as(usize, 0), fx.total());
    try testing.expect(!launch.mini.showing());
    // Listing only: GET_STATE and the like, never ATTACH.
    try testing.expectEqual(@as(usize, 0), countFrames(launch.mini).attach);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.here).total);
    // Its record stays only as the tab to select should the user pick it.
    try testing.expect(launch.engine.model.peers.items[0].restore != null);
    try testing.expect(!launch.engine.model.peers.items[0].restore.?.pending);

    // Later frames do not show it either.
    var frame_fx: PeerFx = .{};
    launch.engine.pumpViewports(&frame_fx, .{ .label = scene.canvas_label, .size = .{ .width = 1200, .height = 800 }, .scale_factor = 1, .frame_index = 1, .timestamp_ns = 1 });
    try testing.expectEqual(@as(usize, 0), frame_fx.total());
    try testing.expect(!launch.mini.showing());
}

test "relaunch with a peer's tab selected: exactly that peer attaches, with the front window's real size" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const model = launch.engine.model;
    var fx: PeerFx = .{};
    launch.measure();
    try launch.hello(&fx, 0);
    try launch.hello(&fx, 1);
    // mini's tab was the front window's selected tab; studio was showing
    // `deploy` in a window behind it.
    launch.remember(0, .{ .session = 1, .server = launch.server(0), .front = true });
    launch.remember(1, .{ .session = 2, .server = launch.server(1), .front = false });
    _ = countFrames(launch.mini);
    _ = countFrames(launch.studio);

    try launch.list(&fx, 1);
    try testing.expectEqual(@as(usize, 0), fx.total());
    try testing.expect(!launch.studio.showing());
    try launch.list(&fx, 0);
    // Only mini restarts, to show `build`; studio and This Mac are untouched.
    try testing.expectEqual(@as(usize, 1), fx.restarts[0]);
    try testing.expectEqual(@as(usize, 0), fx.restarts[1]);
    try testing.expect(launch.mini.showing());
    try testing.expectEqual(@as(?u32, 1), launch.mini.session_id);
    try testing.expect(!launch.studio.showing());
    try testing.expect(!model.peers.items[0].restore.?.pending);

    // The attach carries the front window's grid, not the 80 by 24 default.
    const viewport = peer_restore.frontViewport(model).?;
    try testing.expect(viewport.cols > 80 and viewport.rows > 24);
    try testing.expectEqual(viewport.cols, launch.mini.attach_viewport.cols);
    try testing.expectEqual(viewport.rows, launch.mini.attach_viewport.rows);
    launch.mini.stop();
    try launch.mini.host.reconnect("relaunch");
    try launch.hello(&fx, 0);
    const frames = takeFrames(launch.mini, viewport.cols, viewport.rows);
    try testing.expectEqual(@as(usize, 1), frames.attach);
    try testing.expect(frames.viewport);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.studio).attach);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.here).total);
}

test "a front record waits for the front window's first frame, and a choice made first cancels it" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const model = launch.engine.model;
    var fx: PeerFx = .{};
    try launch.hello(&fx, 0);
    launch.remember(0, .{ .session = 1, .server = launch.server(0), .front = true });
    try launch.list(&fx, 0);
    // Listed and reconciled, but no frame has measured a window (its size
    // is still the placeholder default): nothing to attach with yet.
    try testing.expect(!model.wsAt(0).?.surface_measured);
    try testing.expectEqual(@as(usize, 0), fx.total());
    try testing.expect(!launch.mini.showing());
    try testing.expect(model.peers.items[0].restore.?.pending and model.peers.items[0].restore.?.listed);
    _ = countFrames(launch.mini);

    // The first frame measures it: mini shows now, at that size.
    launch.engine.pumpViewports(&fx, .{ .label = scene.canvas_label, .size = .{ .width = 1200, .height = 800 }, .scale_factor = 1, .frame_index = 1, .timestamp_ns = 1 });
    try testing.expectEqual(@as(usize, 1), fx.restarts[0]);
    try testing.expect(launch.mini.showing());
    try testing.expect(launch.mini.attach_viewport.cols > 80);

    // A second launch where the user chooses a tab before mini lists: the
    // record is not shown, and mini keeps listing.
    var second = try Launch.start();
    defer second.engine.destroy();
    var second_fx: PeerFx = .{};
    second.measure();
    try second.hello(&second_fx, 0);
    second.remember(0, .{ .session = 1, .server = second.server(0), .front = true });
    peer_restore.cancelFront(second.engine.model);
    try second.list(&second_fx, 0);
    try testing.expectEqual(@as(usize, 0), second_fx.total());
    try testing.expect(!second.mini.showing());
    try testing.expectEqual(@as(usize, 0), countFrames(second.mini).attach);
}

test "a stale restored record is dropped quietly, and nothing is sent to another coordinator" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const model = launch.engine.model;
    var fx: PeerFx = .{};
    launch.measure();
    try launch.hello(&fx, 0);
    try launch.hello(&fx, 1);
    // mini's front session 9 is gone from its server; studio's record is of
    // an earlier server incarnation, whose session ids may name others now.
    launch.remember(0, .{ .session = 9, .server = launch.server(0), .front = true });
    launch.remember(1, .{ .session = 1, .server = remote_memory.serverHash("an earlier server"), .front = false });
    _ = countFrames(launch.mini);
    _ = countFrames(launch.studio);

    try launch.list(&fx, 0);
    try launch.list(&fx, 1);
    try testing.expectEqual(@as(usize, 0), fx.total());
    try testing.expect(model.peers.items[0].restore == null);
    try testing.expect(model.peers.items[1].restore == null);
    try testing.expect(!launch.mini.showing());
    try testing.expect(!launch.studio.showing());
    // Neither record reaches another coordinator: no ATTACH anywhere, and
    // This Mac hears nothing at all.
    try testing.expectEqual(@as(usize, 0), countFrames(launch.mini).attach);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.studio).attach);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.here).total);
}

test "a graceful upgrade keeps the relaunch restore: another server id, the same session id and creation time" {
    // phux-c2td.32: the record named the server's HELLO_OK.server_id, which
    // an upgrade changes, so the restore was lost although the session lived.
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const created = try listedCreated(1);
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const model = launch.engine.model;
    var fx: PeerFx = .{};
    launch.measure();
    try launch.helloUpgraded(&fx, 0);
    // mini's server is not the one This Mac's fixture names any more.
    try testing.expect(!std.mem.eql(u8, launch.here.serverId().?, launch.mini.serverId().?));
    launch.remember(0, .{ .session = 1, .created = created, .front = true });
    _ = countFrames(launch.mini);
    try launch.list(&fx, 0);
    try testing.expectEqual(@as(usize, 1), fx.restarts[0]);
    try testing.expectEqual(@as(usize, 1), fx.total());
    try testing.expect(launch.mini.showing());
    try testing.expectEqual(@as(?u32, 1), launch.mini.session_id);
    try testing.expect(!model.peers.items[0].restore.?.pending);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.here).total);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.studio).attach);
}

test "a cold restart that reissues the id drops the record, and so does an old record of another server" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const created = try listedCreated(1);
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const model = launch.engine.model;
    var fx: PeerFx = .{};
    launch.measure();
    try launch.hello(&fx, 0);
    try launch.hello(&fx, 1);
    // mini lists a session 1 created a second after the one on screen at
    // the quit: a cold restart gave the id to a new session.
    launch.remember(0, .{ .session = 1, .created = created + 1, .front = true });
    // studio's record was written before creation times were kept: read
    // from the file as it was, it names an earlier server.
    var file: [remote_memory.max_list_file_bytes]u8 = undefined;
    const bytes = try std.fmt.bufPrint(&file, "phux-cockpit-remote v3\ntarget=studio\nshown=2,{x:0>16},-,0\n", .{remote_memory.serverHash("an earlier server")});
    var hosts: remote_memory.Hosts = .{};
    defer hosts.deinit();
    try testing.expect(remote_memory.parseAll(bytes, &hosts));
    try testing.expect(hosts.shown[0].?.created == null);
    launch.remember(1, hosts.shown[0].?);
    _ = countFrames(launch.mini);
    _ = countFrames(launch.studio);

    try launch.list(&fx, 0);
    try launch.list(&fx, 1);
    try testing.expectEqual(@as(usize, 0), fx.total());
    try testing.expect(model.peers.items[0].restore == null);
    try testing.expect(model.peers.items[1].restore == null);
    try testing.expect(!launch.mini.showing());
    try testing.expect(!launch.studio.showing());
    try testing.expectEqual(@as(usize, 0), countFrames(launch.mini).attach);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.studio).attach);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.here).total);
}

test "a restored record that names another coordinator than its slot's peer is dropped, and shows nothing there" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const model = launch.engine.model;
    var fx: PeerFx = .{};
    launch.measure();
    try launch.hello(&fx, 0);
    // studio's front record in mini's slot (the slot went to another host):
    // mini lists `build` of the same server id, yet the record is not mini's.
    model.peers.items[0].restore = .{ .coordinator = launch.studio.providerId(), .shown = .{ .session = 1, .server = launch.server(0), .front = true }, .pending = true };
    _ = countFrames(launch.mini);
    // studio's own HELLO, from its connection's start.
    _ = countFrames(launch.studio);
    try launch.list(&fx, 0);
    try testing.expectEqual(@as(usize, 0), fx.total());
    try testing.expect(model.peers.items[0].restore == null);
    try testing.expect(!launch.mini.showing());
    try testing.expect(!launch.studio.showing());
    try testing.expectEqual(@as(usize, 0), countFrames(launch.mini).attach);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.studio).total);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.here).total);
}

test "a front record is kept through one failed connection and dropped by a second; a hint is consumed once, by its own coordinator and session" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const model = engine.model;
    const mini = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "mini" } }, null, "relaunch");
    try model.ensurePeerSlots(1);
    model.peers.items[0].provider = mini;
    mini.standBy();
    const shown: remote_memory.Shown = .{ .session = 4, .server = 9, .window = @splat(7), .front = true };
    model.peers.items[0].restore = .{ .coordinator = mini.providerId(), .shown = shown, .pending = true, .listed = true };
    peer_restore.failed(model, 0);
    // Kept for the redial, but what the failed connection listed is not.
    try testing.expect(model.peers.items[0].restore.?.pending and model.peers.items[0].restore.?.retried);
    try testing.expect(!model.peers.items[0].restore.?.listed);
    peer_restore.failed(model, 0);
    try testing.expect(model.peers.items[0].restore == null);

    // Not pending: a failure keeps the hint.
    model.peers.items[0].restore = .{ .coordinator = mini.providerId(), .shown = shown, .pending = false };
    peer_restore.failed(model, 0);
    try testing.expect(model.peers.items[0].restore != null);
    // Another coordinator's id or another session never takes it.
    try testing.expect(peer_restore.takeHint(model, contract.phuxCoordinatorId("studio"), 4) == null);
    try testing.expect(peer_restore.takeHint(model, mini.providerId(), 5) == null);
    const hint = peer_restore.takeHint(model, mini.providerId(), 4).?;
    try testing.expectEqualSlices(u8, &shown.window.?, &hint);
    try testing.expect(peer_restore.takeHint(model, mini.providerId(), 4) == null);
}

fn refOn(provider: *const support.PhuxProvider, id: u32) !TerminalRef {
    return .{ .provider_id = provider.providerId(), .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, id, "") } };
}

fn leafSnapshot(session: u32, windows: []const shared.Window, nodes: []const shared.Node) shared.Snapshot {
    return .{ .session_id = session, .revision = 1, .state = .authoritative, .windows = windows, .nodes = nodes };
}

test "what is kept is the session on screen, its tab, and whether that tab is in front" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const model = engine.model;
    // This Mac active, mini showing its session 1 beside it; both attached.
    const here = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/relaunch-unused" }, null, "relaunch");
    model.phux_provider = here;
    const mini = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "mini" } }, null, "relaunch");
    try model.ensurePeerSlots(1);
    model.peers.items[0].provider = mini;
    try mini.show(1);
    try fixture.attachHostWith(here.host, "hello.bin");
    try fixture.attachHostWith(mini.host, "hello.bin");
    model.shared_workspace.authority = here.providerId();
    model.peers.items[0].workspace.authority = mini.providerId();
    const one = [_]shared.Window{.{ .id = @splat(1), .root = 0 }};
    const mini_window = [_]shared.Window{.{ .id = @splat(5), .root = 0 }};
    const here_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = try refOn(here, 7) }};
    const mini_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = try refOn(mini, 7) }};
    _ = try model.shared_workspace.apply(model, leafSnapshot(1, &one, &here_nodes), here.connectionEpoch());
    _ = try model.peers.items[0].workspace.apply(model, leafSnapshot(1, &mini_window, &mini_nodes), mini.connectionEpoch());

    var hosts: remote_memory.Hosts = .{};
    try testing.expect(hosts.add("mini"));
    defer hosts.deinit();
    try testing.expect(hosts.add("studio"));
    // mini's tab in front: its session, its tab's shared window, front.
    try testing.expect(model.selectTerminal(try refOn(mini, 7)));
    try testing.expect(peer_restore.capture(model, &hosts));
    const kept = hosts.shown[0].?;
    try testing.expectEqual(@as(u32, 1), kept.session);
    // Named by its creation time in mini's own list, not by mini's server.
    try testing.expect(kept.created != null);
    try testing.expectEqual(createdOf(mini.sessionCatalog(), 1), kept.created);
    try testing.expectEqualSlices(u8, &mini_window[0].id, &kept.window.?);
    try testing.expect(kept.front);
    // studio is not held, so it shows nothing.
    try testing.expect(hosts.shown[1] == null);
    try testing.expect(!peer_restore.capture(model, &hosts));

    // This Mac's tab chosen: mini is not on screen, so nothing of it is kept.
    try testing.expect(model.selectTerminal(try refOn(here, 7)));
    try testing.expect(peer_restore.capture(model, &hosts));
    try testing.expect(hosts.shown[0] == null);

    // A launch record not yet judged is kept as it was loaded, so quitting
    // before its host lists loses nothing.
    const loaded: remote_memory.Shown = .{ .session = 3, .server = 11, .front = true };
    try testing.expect(hosts.setShown(0, loaded));
    model.peers.items[0].restore = .{ .coordinator = mini.providerId(), .shown = loaded, .pending = true };
    try testing.expect(!peer_restore.capture(model, &hosts));
    try testing.expect(hosts.shown[0].?.eql(loaded));
}

fn fronts(hosts: *const remote_memory.Hosts) usize {
    var count: usize = 0;
    for (hosts.shown[0..hosts.count]) |value| {
        const record = value orelse continue;
        if (record.front) count += 1;
    }
    return count;
}

test "Connect to Host during a pending front restore cancels it, and the file keeps one front" {
    // Review of phux-c2td.30: only a selection cancelled a pending front
    // record, so a host that listed after Connect to Host took the front
    // window from the host the user had just chosen.
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const engine = launch.engine;
    const model = engine.model;
    var fx: PeerFx = .{};
    launch.measure();
    try launch.hello(&fx, 0);
    // mini holds the front record and has not listed yet.
    const front: remote_memory.Shown = .{ .session = 1, .server = launch.server(0), .front = true };
    launch.remember(0, front);
    // The user connects to studio: it becomes active, trading places with
    // This Mac, and its slot keeps no restore state of the host it held.
    launch.remember(1, .{ .session = 2, .server = 4 });
    try engine.exchangeCoordinators(&fx, .{ .remote = .{ .target = "studio" } }, null, "studio");
    try testing.expect(!model.peers.items[0].restore.?.pending);
    try testing.expect(model.peers.items[1].restore == null);
    // mini lists: it stays listing and takes nothing from studio.
    const restarts = fx.total();
    _ = countFrames(launch.mini);
    try launch.list(&fx, 0);
    try testing.expectEqual(restarts, fx.total());
    try testing.expect(!launch.mini.showing());
    try testing.expectEqual(@as(usize, 0), countFrames(launch.mini).attach);
    // Its cancelled record is not front in the file any more.
    var hosts: remote_memory.Hosts = .{};
    defer hosts.deinit();
    try testing.expect(hosts.add("mini"));
    try testing.expect(hosts.add("studio"));
    try testing.expect(hosts.setShown(0, front));
    _ = peer_restore.capture(model, &hosts);
    try testing.expect(fronts(&hosts) <= 1);
}

test "a remembered host made active by the config, in front, outranks a loaded front: the file keeps one front" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const model = engine.model;
    // studio is active (PHUX_REMOTE names it) and attached, its tab in front.
    const studio = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "studio" } }, null, "relaunch");
    model.phux_provider = studio;
    try fixture.attachHostWith(studio.host, "hello.bin");
    model.shared_workspace.authority = studio.providerId();
    const window = [_]shared.Window{.{ .id = @splat(3), .root = 0 }};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = try refOn(studio, 7) }};
    _ = try model.shared_workspace.apply(model, leafSnapshot(1, &window, &nodes), studio.connectionEpoch());
    try testing.expect(model.selectTerminal(try refOn(studio, 7)));
    // mini holds the front record of the last quit and has not listed yet.
    const mini = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "mini" } }, null, "relaunch");
    try model.ensurePeerSlots(1);
    model.peers.items[0].provider = mini;
    mini.standBy();
    const loaded: remote_memory.Shown = .{ .session = 1, .server = 5, .front = true };
    model.peers.items[0].restore = .{ .coordinator = mini.providerId(), .shown = loaded, .pending = true };

    var hosts: remote_memory.Hosts = .{};
    defer hosts.deinit();
    try testing.expect(hosts.add("mini"));
    try testing.expect(hosts.add("studio"));
    try testing.expect(hosts.setShown(0, loaded));
    try testing.expect(peer_restore.capture(model, &hosts));
    try testing.expectEqual(@as(usize, 1), fronts(&hosts));
    try testing.expect(hosts.shown[0] == null);
    try testing.expect(hosts.shown[1].?.front);
    // Written and read back: both hosts, one front.
    var out: [remote_memory.max_list_file_bytes]u8 = undefined;
    var parsed: remote_memory.Hosts = .{};
    defer parsed.deinit();
    try testing.expect(remote_memory.parseAll(remote_memory.encodeAll(&hosts, &out).?, &parsed));
    try testing.expectEqual(@as(usize, 2), parsed.count);
    try testing.expectEqual(@as(usize, 1), fronts(&parsed));
}

test "Disconnect clears the removed host's restore state and leaves the others" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const model = launch.engine.model;
    var fx: PeerFx = .{};
    try launch.hello(&fx, 0);
    try launch.hello(&fx, 1);
    launch.remember(0, .{ .session = 1, .server = launch.server(0), .front = true });
    launch.remember(1, .{ .session = 2, .server = launch.server(1) });
    launch.engine.dropPeer(&fx, 0);
    try testing.expect(model.peers.items[0].restore == null);
    try testing.expect(model.peers.items[1].restore != null);
    try testing.expectEqual(launch.studio.providerId(), model.peers.items[1].restore.?.coordinator);
}

/// A relaunch whose front record for mini has been shown: mini restarted,
/// attached `build` (1) at the front window's size, and its first projection
/// (the workspace `metadata` and `state` replies) has landed. This Mac is
/// attached and its workspace read, but not projected yet.
fn restoreMini(launch: *Launch, fx: *PeerFx, window: ?[16]u8, metadata: []const u8, state: []const u8) !void {
    launch.measure();
    try launch.hello(fx, 0);
    launch.remember(0, .{ .session = 1, .server = launch.server(0), .window = window, .front = true });
    try launch.list(fx, 0);
    try testing.expectEqual(@as(usize, 1), fx.restarts[0]);
    try launch.attachShown(fx, 0, metadata, state);
    try testing.expect(launch.mini.showing());
    try testing.expect(launch.mini.state() == .attached);
}

test "relaunch selects the remembered tab on the peer's first projection, end to end" {
    // phux-c2td.32: the remembered tab was covered only by takeHint's
    // bookkeeping, never through a real list, attach and projection.
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    // Without a remembered tab, mini's first projection of its two windows
    // selects its first tab; that is the fallback the hint must beat.
    var probe = try Launch.start();
    defer probe.engine.destroy();
    var probe_fx: PeerFx = .{};
    try restoreMini(&probe, &probe_fx, null, "workspace_add_metadata.bin", "workspace_add_state.bin");
    const windows = probe.mini.workspaceSnapshot().windows;
    try testing.expectEqual(@as(usize, 2), windows.len);
    const first = windows[0].id;
    const second = windows[1].id;
    try testing.expect(!std.mem.eql(u8, &first, &second));
    try testing.expectEqual(@as(?contract.ProviderId, probe.mini.providerId()), frontTab(probe.engine.model).authority);
    try testing.expectEqualSlices(u8, &first, &frontTab(probe.engine.model).window.?);

    // The same relaunch remembering mini's second tab selects that one.
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const model = launch.engine.model;
    var fx: PeerFx = .{};
    try restoreMini(&launch, &fx, second, "workspace_add_metadata.bin", "workspace_add_state.bin");
    const front = frontTab(model);
    try testing.expectEqual(@as(?contract.ProviderId, launch.mini.providerId()), front.authority);
    try testing.expectEqualSlices(u8, &second, &front.window.?);
    try testing.expectEqual(@as(usize, 2), model.wsAt(model.active_window).?.tab_count);
    // Consumed: the live selection is what is kept from now on.
    try testing.expect(model.peers.items[0].restore == null);
    // Displaying from the moment it attached, so settling keeps it shown.
    _ = launch.engine.settlePeers(&fx);
    try testing.expect(launch.mini.showing());
    try testing.expectEqual(@as(usize, 1), fx.total());
    try testing.expectEqual(@as(usize, 0), countFrames(launch.here).total);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.studio).attach);
}

test "the active coordinator's first projection landing after the restored peer's keeps the peer in front; a choice landing with it returns the peer to listing" {
    // phux-c2td.32: the ordering race. At launch This Mac projects with no
    // remembered selection, so its tabs land beside mini's selected tab and
    // mini stays in front (rule A holds because mini is displaying).
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const model = launch.engine.model;
    var fx: PeerFx = .{};
    try restoreMini(&launch, &fx, null, "workspace_initial_metadata.bin", "workspace_initial_state.bin");
    try testing.expectEqual(@as(?contract.ProviderId, launch.mini.providerId()), frontTab(model).authority);
    try testing.expect(model.locateTerminal(try refOn(launch.here, 7)) == null);

    launch.projectHere(&fx);
    try testing.expect(model.locateTerminal(try refOn(launch.here, 7)) != null);
    try testing.expectEqual(@as(?contract.ProviderId, launch.mini.providerId()), frontTab(model).authority);
    _ = launch.engine.settlePeers(&fx);
    try testing.expect(launch.mini.showing());
    try testing.expectEqual(@as(usize, 1), fx.total());

    // The other order of events: a choice of This Mac's terminal made after
    // mini attached lands with This Mac's projection and takes the front
    // tab. mini is then hidden, so it stands by: its tabs leave and only its
    // own connection restarts, as a lister.
    var second = try Launch.start();
    defer second.engine.destroy();
    const second_model = second.engine.model;
    var second_fx: PeerFx = .{};
    try restoreMini(&second, &second_fx, null, "workspace_initial_metadata.bin", "workspace_initial_state.bin");
    // What a navigation to that terminal leaves for its projection.
    second_model.shared_workspace.desired_terminal = try refOn(second.here, 7);
    second.projectHere(&second_fx);
    try testing.expectEqual(@as(?contract.ProviderId, second.here.providerId()), frontTab(second_model).authority);
    _ = second.engine.settlePeers(&second_fx);
    try testing.expect(!second.mini.showing());
    try testing.expectEqual(@as(usize, 2), second_fx.restarts[0]);
    try testing.expectEqual(@as(usize, 0), second_fx.restarts[1]);
    try testing.expectEqual(@as(u32, 0), second_model.peers.items[0].workspace.session);
    try testing.expect(second_model.locateTerminal(try refOn(second.mini, 7)) == null);
    // No viewport lingers: its next connection only lists, and never
    // attaches, and nothing reached This Mac or studio.
    _ = countFrames(second.mini);
    second.mini.stop();
    try second.mini.host.reconnect("relaunch");
    try second.hello(&second_fx, 0);
    try testing.expectEqual(@as(usize, 0), countFrames(second.mini).attach);
    try testing.expectEqual(@as(usize, 0), countFrames(second.studio).attach);
}

test "a host whose first connection fails is restored by its backoff redial, once, and only while the user has not chosen" {
    // phux-c2td.32: a front host whose first dial failed stayed listing for
    // the whole launch. One failure now keeps its record for the redial.
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var launch = try Launch.start();
    defer launch.engine.destroy();
    const model = launch.engine.model;
    var fx: PeerFx = .{};
    launch.measure();
    launch.remember(0, .{ .session = 1, .server = launch.fixtureServer(), .front = true });
    // mini's first connection fails before it lists: no show, a redial armed.
    launch.fail(&fx, 0);
    try testing.expectEqual(@as(usize, 0), fx.total());
    try testing.expect(model.peers.items[0].failed);
    try testing.expect(model.peers.items[0].restore.?.pending);
    // The redial is a lister's; only when it lists is the record shown.
    try testing.expect(launch.engine.onPeerRetryTimer(&fx, fx.retry_key.?));
    try testing.expectEqual(@as(usize, 1), fx.restarts[0]);
    try testing.expect(!launch.mini.showing());
    try launch.mini.host.reconnect("relaunch");
    try launch.hello(&fx, 0);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.mini).attach);
    try launch.list(&fx, 0);
    try testing.expectEqual(@as(usize, 2), fx.restarts[0]);
    try testing.expect(launch.mini.showing());
    try testing.expectEqual(@as(?u32, 1), launch.mini.session_id);
    try testing.expectEqual(@as(usize, 0), fx.restarts[1]);
    try testing.expectEqual(@as(usize, 0), countFrames(launch.here).total);

    // Two failures before it lists: the record is gone, and the peer lists.
    var twice = try Launch.start();
    defer twice.engine.destroy();
    var twice_fx: PeerFx = .{};
    twice.measure();
    twice.remember(0, .{ .session = 1, .server = twice.fixtureServer(), .front = true });
    twice.fail(&twice_fx, 0);
    twice.fail(&twice_fx, 0);
    try testing.expect(twice.engine.model.peers.items[0].restore == null);
    try twice.mini.host.reconnect("relaunch");
    try twice.hello(&twice_fx, 0);
    try twice.list(&twice_fx, 0);
    try testing.expect(!twice.mini.showing());
    try testing.expectEqual(@as(usize, 0), countFrames(twice.mini).attach);

    // A choice made while the redial is pending cancels the record. The user
    // picks studio's `deploy` in the switcher, through the engine's own
    // selection path (Engine.showPeerSession, which supersedes every pending
    // selection), not by clearing the record directly.
    var chosen = try Launch.start();
    defer chosen.engine.destroy();
    var chosen_fx: PeerFx = .{};
    chosen.measure();
    try chosen.hello(&chosen_fx, 1);
    try chosen.list(&chosen_fx, 1);
    chosen.remember(0, .{ .session = 1, .server = chosen.fixtureServer(), .front = true });
    chosen.fail(&chosen_fx, 0);
    try testing.expect(chosen.engine.model.peers.items[0].restore.?.pending);
    try testing.expect(chosen.engine.showPeerSession(chosen.studio.providerId(), 2, &chosen_fx));
    try testing.expectEqual(@as(usize, 1), chosen_fx.restarts[1]);
    try testing.expect(chosen.studio.showing());
    try testing.expect(!chosen.engine.model.peers.items[0].restore.?.pending);
    try chosen.mini.host.reconnect("relaunch");
    try chosen.hello(&chosen_fx, 0);
    try chosen.list(&chosen_fx, 0);
    try testing.expect(!chosen.mini.showing());
    try testing.expectEqual(@as(usize, 0), chosen_fx.restarts[0]);
    try testing.expectEqual(@as(usize, 0), countFrames(chosen.mini).attach);
}
