//! Side by side: the active Phux provider plus one standby coordinator
//! (`Model.phux_peers`), listed as host groups in one switcher. This Mac's
//! group comes first whichever coordinator is active; selecting the other
//! group's session exchanges the two; Use this Mac keeps the remote host
//! listed; Disconnect removes only its group; relaunch reattaches the
//! remembered host beside this Mac. The standby never attaches: it lists
//! sessions with GET_STATE, so it sizes nobody's panes and streams nothing.

const std = @import("std");
const native_sdk = @import("native_sdk");
const support = @import("../cockpit/phux_support.zig");
const model_module = @import("../cockpit/model.zig");
const ts_engine = @import("../cockpit/native/ts_engine.zig");
const navigation = @import("../cockpit/native/ts_navigation.zig");
const projection = @import("../cockpit/native/workspace_projection.zig");
const remote_hosts = @import("../cockpit/native/remote_hosts.zig");
const startup = @import("../cockpit/startup.zig");
const config = @import("../config/config.zig");

const testing = std.testing;
const targets = navigation.targets;
const fixture = support.PhuxProvider.test_support;

/// Wire type bytes after the 4-byte length prefix (phux-protocol frame/mod.rs).
const type_attach: u8 = 0x02;
const type_command: u8 = 0x31;

/// An active provider's catalog, set directly: the active side's listing is
/// not what these tests are about.
fn addSessions(provider: *support.PhuxProvider, names: []const []const u8) !void {
    for (names, 0..) |name, index| try provider.host.sessions.append(testing.allocator, .{
        .id = @intCast(index + 1),
        .name = try testing.allocator.dupe(u8, name),
        .created_at_unix_secs = 0,
        .window_count = 1,
        .attached_client_count = 0,
        .focused = false,
    });
    provider.host.sessions_generation = provider.connectionEpoch();
}

/// A standby connected through real frames: HELLO_OK, then the GET_STATE
/// reply `standby_state.bin` listing `build` (1) and `deploy` (2).
fn connectStandby(provider: *support.PhuxProvider) !void {
    provider.standBy();
    try provider.host.start("side-by-side");
    try fixture.stageFixture(provider.bridge, "hello.bin");
    _ = try provider.drainReadiness();
    try fixture.stageFixture(provider.bridge, "standby_state.bin");
    try testing.expect((try provider.drainReadiness()).sessions_listed);
}

/// Every frame the provider has staged since the last call: how many were
/// ATTACH and how many COMMAND.
fn countFrames(provider: *support.PhuxProvider) struct { attach: usize, command: usize } {
    var attach: usize = 0;
    var command: usize = 0;
    while (provider.bridge.outgoing.take()) |frame| {
        defer provider.bridge.outgoing.release(frame);
        if (frame[4] == type_attach) attach += 1;
        if (frame[4] == type_command) command += 1;
    }
    return .{ .attach = attach, .command = command };
}

const Pair = struct {
    engine: *ts_engine.Engine,
    active: *support.PhuxProvider,
    peer: *support.PhuxProvider,

    /// `remote_active` false: this Mac active (`home`), the host `mini`
    /// standing by. True: the host active (`home`), this Mac standing by.
    fn start(remote_active: bool) !Pair {
        const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
        errdefer engine.destroy();
        const local: support.PhuxEndpoint = .{ .unix = "/side-by-side-unused" };
        const remote: support.PhuxEndpoint = .{ .remote = .{ .target = "mini" } };
        const active = try support.PhuxProvider.create(testing.allocator, testing.io, if (remote_active) remote else local, null, "side-by-side");
        engine.model.phux_provider = active;
        const peer = try support.PhuxProvider.create(testing.allocator, testing.io, if (remote_active) local else remote, null, "side-by-side");
        engine.model.phux_peers[0] = peer;
        try addSessions(active, &.{"home"});
        try connectStandby(peer);
        return .{ .engine = engine, .active = active, .peer = peer };
    }
};

fn sessionEntries(model: *const model_module.Model, out: []model_module.PaletteDestination) []model_module.PaletteDestination {
    const workspace: model_module.Workspace = .{};
    var iterator = projection.PaletteIterator.init(model, &workspace);
    var count: usize = 0;
    while (iterator.next()) |entry| {
        if (entry != .session and entry != .peer_session) continue;
        out[count] = entry;
        count += 1;
    }
    return out[0..count];
}

fn sessionsPage(model: *const model_module.Model, out: *[navigation.max_bytes]u8) ![]const u8 {
    var request = [_]u8{0} ** 15;
    request[0] = 1;
    request[1] = 4;
    request[2] = 7;
    request[13] = @intFromEnum(navigation.Scope.sessions);
    return navigation.encode(model, 7, &request, out);
}

fn peerEntries(model: *const model_module.Model) usize {
    var entries: [8]model_module.PaletteDestination = undefined;
    var count: usize = 0;
    for (sessionEntries(model, &entries)) |entry| {
        if (entry == .peer_session) count += 1;
    }
    return count;
}

test "the standby never attaches, so it contributes no viewport to any pane" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const peer = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "mini" } }, null, "standby");
    defer peer.destroy();
    peer.standBy();
    try peer.host.start("standby");
    try fixture.stageFixture(peer.bridge, "hello.bin");
    _ = try peer.drainReadiness();
    // An active provider would queue ATTACH here; the standby asks GET_STATE.
    const after_hello = countFrames(peer);
    try testing.expectEqual(@as(usize, 0), after_hello.attach);
    try testing.expectEqual(@as(usize, 1), after_hello.command);
    try testing.expect(!peer.attach_queued);

    try fixture.stageFixture(peer.bridge, "standby_state.bin");
    try testing.expect((try peer.drainReadiness()).sessions_listed);
    try testing.expectEqual(.negotiated, peer.state());
    try testing.expectEqual(@as(usize, 2), peer.standbyCatalog().len);

    // Later wakes and switcher refreshes list again; they never attach.
    peer.refreshStandby();
    _ = try peer.drainReadiness();
    const refreshed = countFrames(peer);
    try testing.expectEqual(@as(usize, 0), refreshed.attach);
    try testing.expectEqual(@as(usize, 1), refreshed.command);
    try testing.expectEqual(.negotiated, peer.state());
}

test "the switcher lists this Mac's sessions first, then the remote host's, whichever is active" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var entries: [8]model_module.PaletteDestination = undefined;
    var page: [navigation.max_bytes]u8 = undefined;

    var local_active = try Pair.start(false);
    defer local_active.engine.destroy();
    const first = sessionEntries(local_active.engine.model, &entries);
    try testing.expectEqual(@as(usize, 3), first.len);
    try testing.expectEqual(@as(u32, 1), first[0].session);
    try testing.expectEqual(@as(u32, 1), first[1].peer_session.id);
    try testing.expectEqual(@as(u32, 2), first[2].peer_session.id);
    try testing.expectEqual(local_active.peer.providerId(), first[1].peer_session.coordinator);
    const labels = try sessionsPage(local_active.engine.model, &page);
    try testing.expect(std.mem.indexOf(u8, labels, "Phux session · This Mac") != null);
    try testing.expect(std.mem.indexOf(u8, labels, "Phux session · mini") != null);
    try testing.expect(std.mem.indexOf(u8, labels, "deploy") != null);

    // The host active and this Mac standing by: this Mac still leads.
    var remote_active = try Pair.start(true);
    defer remote_active.engine.destroy();
    const second = sessionEntries(remote_active.engine.model, &entries);
    try testing.expectEqual(@as(usize, 3), second.len);
    try testing.expectEqual(@as(u32, 1), second[0].peer_session.id);
    try testing.expectEqual(@as(u32, 2), second[1].peer_session.id);
    try testing.expectEqual(@as(u32, 1), second[2].session);
    const swapped = try sessionsPage(remote_active.engine.model, &page);
    try testing.expect(std.mem.indexOf(u8, swapped, "Phux session · This Mac") != null);
    try testing.expect(std.mem.indexOf(u8, swapped, "Phux session · mini") != null);
}

const RestartCounter = struct {
    active: usize = 0,
    peer: usize = 0,

    pub fn restartPhux(self: *@This(), _: *ts_engine.Engine) bool {
        self.active += 1;
        return true;
    }
    pub fn restartPeer(self: *@This(), _: *ts_engine.Engine, slot: usize) bool {
        std.debug.assert(slot == 0);
        self.peer += 1;
        return true;
    }
    // A peer's channel events reach these; none may take effect here.
    pub fn openChannel(_: *const @This(), _: anytype) native_sdk.ChannelHandle {
        return .{};
    }
    pub fn closeChannel(_: *const @This(), _: u64) void {}
    pub fn showNotification(_: *const @This(), _: anytype) void {}
};

test "selecting a peer session shows it beside the active coordinator: only the peer restarts" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(false);
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const mini = pair.peer.providerId();

    const target = targets.capture(model, .{ .peer_session = .{ .coordinator = mini, .id = 2 } }).?;
    var bytes: [targets.max_len]u8 = undefined;
    const decoded = targets.decode(target.encode(&bytes)).?;
    try testing.expectEqual(@as(u32, 2), decoded.resolve(model).?.peer_session.id);
    try testing.expectEqual(mini, decoded.resolve(model).?.peer_session.coordinator);
    // The active coordinator's session 2 does not exist; the peer's does.
    try testing.expect(targets.capture(model, .{ .session = 2 }).?.resolve(model) == null);

    var fx: RestartCounter = .{};
    try testing.expect(pair.engine.showPeerSession(mini, 2, &fx));
    // The active coordinator neither redials nor moves.
    try testing.expectEqual(@as(usize, 0), fx.active);
    try testing.expect(pair.active.pending_retarget == null);
    try testing.expect(pair.active.remoteTarget() == null);
    // The peer shows `deploy` (by id) on its own restarted connection.
    try testing.expectEqual(@as(usize, 1), fx.peer);
    try testing.expect(pair.peer.showing());
    try testing.expectEqual(@as(?u32, 2), pair.peer.session_id);
    try testing.expect(pair.peer.pending_retarget == null);
    // A session no peer lists is refused.
    try testing.expect(!pair.engine.showPeerSession(mini, 9, &fx));
    try testing.expect(!pair.engine.showPeerSession(pair.active.providerId(), 1, &fx));

    // Removing the peer withdraws its group entirely.
    pair.engine.dropPeer(&fx, 0);
    try testing.expect(model.phux_peers[0] == null);
    try testing.expect(decoded.resolve(model) == null);
}

test "an exchange withdraws the old host's rows at once" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(false);
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const mini = pair.peer.providerId();
    const held = targets.capture(model, .{ .peer_session = .{ .coordinator = mini, .id = 2 } }).?;

    var fx: RestartCounter = .{};
    try pair.engine.exchangeCoordinators(&fx, .{ .remote = .{ .target = "mini" } }, "deploy", null);
    // The active provider now dials mini for `deploy`; this Mac stands by.
    try testing.expectEqualStrings("mini", pair.active.remoteTarget().?);
    try testing.expectEqualStrings("deploy", pair.active.pending_retarget.?.session.?);
    try testing.expect(pair.peer.remoteTarget() == null);
    try testing.expectEqualStrings("/side-by-side-unused", pair.peer.pending_retarget.?.endpoint.unix);
    try testing.expectEqual(@as(usize, 1), fx.active);
    try testing.expectEqual(@as(usize, 1), fx.peer);
    // The peer still held mini's sessions while its group is already
    // labelled This Mac: they must not list, and a held row must not show
    // `deploy` on the wrong host.
    try testing.expectEqual(@as(usize, 0), peerEntries(model));
    try testing.expectEqual(@as(usize, 0), pair.peer.standbyCatalog().len);
    try testing.expect(held.resolve(model) == null);
    try testing.expect(!pair.engine.showPeerSession(mini, 2, &fx));
}

test "a peer that disconnects stops listing, refuses its old sessions, and says why" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(false);
    defer pair.engine.destroy();
    const model = pair.engine.model;
    const mini = pair.peer.providerId();
    try testing.expectEqual(@as(usize, 2), peerEntries(model));
    const held = targets.capture(model, .{ .peer_session = .{ .coordinator = mini, .id = 1 } }).?;
    try testing.expect(held.resolve(model) != null);
    var page: [navigation.max_bytes]u8 = undefined;
    try testing.expect(std.mem.indexOf(u8, try sessionsPage(model, &page), "Unavailable") == null);

    // The peer's connection closes: its sessions stop listing, a row held
    // from before cannot be activated, and its group says why instead of
    // disappearing.
    var fx: RestartCounter = .{};
    try testing.expect(pair.engine.onPeerChannel(&fx, .{ .key = support.phuxPeerChannelKey(0), .kind = .closed }, null));
    try testing.expect(model.peer_failed[0]);
    try testing.expectEqual(@as(usize, 0), peerEntries(model));
    try testing.expectEqual(@as(usize, 0), pair.peer.host.sessionCatalog().len);
    try testing.expect(held.resolve(model) == null);
    try testing.expect(!pair.engine.showPeerSession(mini, 1, &fx));
    try testing.expectEqual(@as(usize, 0), fx.active);
    const degraded = try sessionsPage(model, &page);
    try testing.expect(std.mem.indexOf(u8, degraded, "Unavailable · the connection was lost") != null);
    // Its one row names the host and cannot be picked.
    var entries: [8]model_module.PaletteDestination = undefined;
    const workspace: model_module.Workspace = .{};
    var iterator = projection.PaletteIterator.init(model, &workspace);
    var count: usize = 0;
    while (iterator.next()) |entry| {
        if (entry != .peer_unavailable) continue;
        entries[count] = entry;
        count += 1;
    }
    try testing.expectEqual(@as(usize, 1), count);
    try testing.expectEqual(mini, entries[0].peer_unavailable);
    // Picking it retries that peer alone; the group then reads Connecting….
    const retry = targets.capture(model, entries[0]).?;
    try testing.expectEqual(mini, retry.resolve(model).?.peer_unavailable);
    try testing.expect(pair.engine.retryPeer(mini, &fx));
    try testing.expectEqual(@as(usize, 1), fx.peer);
    try testing.expectEqual(@as(usize, 0), fx.active);
    try testing.expect(!model.peer_failed[0]);
    try testing.expect(std.mem.indexOf(u8, try sessionsPage(model, &page), "Connecting") != null);
    // Retrying is only for a failed peer; the held row stops resolving.
    try testing.expect(!pair.engine.retryPeer(mini, &fx));
    try testing.expect(retry.resolve(model) == null);
}

extern "c" fn setenv(name: [*:0]const u8, value: [*:0]const u8, overwrite: c_int) c_int;
extern "c" fn unsetenv(name: [*:0]const u8) c_int;

/// The phux CLI registry at `$XDG_CONFIG_HOME/phux/config.toml`, disposable.
const IsolatedRegistry = struct {
    tmp: testing.TmpDir,
    previous: ?[:0]u8,

    fn init(body: []const u8) !IsolatedRegistry {
        const io = testing.io;
        const gpa = testing.allocator;
        var tmp = testing.tmpDir(.{});
        errdefer tmp.cleanup();
        try tmp.dir.createDirPath(io, "phux");
        try tmp.dir.writeFile(io, .{ .sub_path = "phux/config.toml", .data = body });
        const file = try tmp.dir.realPathFileAlloc(io, "phux/config.toml", gpa);
        defer gpa.free(file);
        const root = try gpa.dupeZ(u8, std.fs.path.dirname(std.fs.path.dirname(file).?).?);
        defer gpa.free(root);
        const previous: ?[:0]u8 = if (std.c.getenv("XDG_CONFIG_HOME")) |value| try gpa.dupeZ(u8, std.mem.span(value)) else null;
        errdefer if (previous) |value| gpa.free(value);
        if (setenv("XDG_CONFIG_HOME", root, 1) != 0) return error.SetEnvFailed;
        return .{ .tmp = tmp, .previous = previous };
    }

    fn deinit(self: *IsolatedRegistry) void {
        if (self.previous) |value| {
            _ = setenv("XDG_CONFIG_HOME", value, 1);
            testing.allocator.free(value);
        } else {
            _ = unsetenv("XDG_CONFIG_HOME");
        }
        self.tmp.cleanup();
    }
};

const mini_registry = "[[remote]]\nname = \"mini\"\nendpoint = \"ws://127.0.0.1:1\"\nsession = \"work\"\n";

test "Connect to Host keeps this Mac beside the host; Use this Mac keeps the host; Disconnect removes it" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var registry = try IsolatedRegistry.init(mini_registry);
    defer registry.deinit();
    remote_hosts.forgetForTests();
    defer remote_hosts.forgetForTests();
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const local = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/side-by-side-unused" }, null, "side-by-side");
    engine.model.phux_provider = local;
    const fx = ts_engine.NoShells{};
    var out: [remote_hosts.max_bytes]u8 = undefined;

    try testing.expect(engine.model.phux_peers[0] == null);
    const connecting = try remote_hosts.handle(engine, &fx, "\x01\x02\x07me@mini", &out);
    try testing.expectEqual(@intFromEnum(remote_hosts.Phase.connecting), connecting[1]);
    try testing.expectEqualStrings("me@mini", local.remoteTarget().?);
    // The coordinator it left is listed beside it, as a standby.
    const peer = engine.model.phux_peers[0].?;
    try testing.expect(peer.standby);
    try testing.expect(peer.remoteTarget() == null);
    try testing.expectEqualStrings("/side-by-side-unused", peer.endpointDescriptor().unix);

    // Use this Mac: this Mac active again, the host still beside it.
    try testing.expectEqualSlices(u8, "\x01\x00\x00\x00", try remote_hosts.handle(engine, &fx, "\x01\x03\x00", &out));
    try testing.expect(local.remoteTarget() == null);
    try testing.expect(engine.model.phux_peers[0] == peer);
    try testing.expectEqualStrings("me@mini", peer.remoteTarget().?);
    try testing.expectEqualStrings("work", peer.pending_retarget.?.session.?);

    // Disconnect: the host's group goes.
    try testing.expectEqualSlices(u8, "\x01\x00\x00\x00", try remote_hosts.handle(engine, &fx, "\x01\x04\x00", &out));
    try testing.expect(engine.model.phux_peers[0] == null);
    try testing.expect(local.remoteTarget() == null);
}

const two_host_registry = mini_registry ++ "[[remote]]\nname = \"studio\"\nendpoint = \"ws://127.0.0.1:2\"\n";

test "a second host keeps the first listed beside it, and Use this Mac keeps both" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var registry = try IsolatedRegistry.init(two_host_registry);
    defer registry.deinit();
    remote_hosts.forgetForTests();
    defer remote_hosts.forgetForTests();
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const local = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/side-by-side-unused" }, null, "side-by-side");
    engine.model.phux_provider = local;
    const fx = ts_engine.NoShells{};
    var out: [remote_hosts.max_bytes]u8 = undefined;
    const peers = &engine.model.phux_peers;

    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x04mini", &out);
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x06studio", &out);
    // studio is active; this Mac and mini are both listed, each in its slot.
    try testing.expectEqualStrings("studio", local.remoteTarget().?);
    try testing.expect(peers[0].?.remoteTarget() == null);
    try testing.expectEqualStrings("mini", peers[1].?.remoteTarget().?);
    try testing.expect(peers[0].?.standby and peers[1].?.standby);
    // Three coordinators, three identities.
    try testing.expect(local.effectiveProviderId() != peers[0].?.effectiveProviderId());
    try testing.expect(local.effectiveProviderId() != peers[1].?.effectiveProviderId());
    try testing.expect(peers[0].?.effectiveProviderId() != peers[1].?.effectiveProviderId());

    // Use this Mac trades places with this Mac's slot; mini is untouched.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x03\x00", &out);
    try testing.expect(local.remoteTarget() == null);
    try testing.expectEqualStrings("studio", peers[0].?.remoteTarget().?);
    try testing.expectEqualStrings("mini", peers[1].?.remoteTarget().?);
    try testing.expect(peers[1].?.pending_retarget == null);

    // Reconnecting to a listed host trades places with it, never copies it.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x04mini", &out);
    try testing.expectEqualStrings("mini", local.remoteTarget().?);
    try testing.expect(peers[1].?.remoteTarget() == null);
    try testing.expect(peers[2] == null);

    // Disconnect removes every host; this Mac is active alone.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x04\x00", &out);
    try testing.expect(local.remoteTarget() == null);
    for (peers) |slot| try testing.expect(slot == null);
}

test "relaunch reattaches a remembered host beside this Mac; a configured host keeps this Mac beside it" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var registry = try IsolatedRegistry.init(mini_registry);
    defer registry.deinit();
    const gpa = testing.allocator;
    const io = testing.io;

    var remembered = startup.resolvePhuxConfig(config.parse(""), .{ .runtime_dir = "/tmp/rt" });
    try testing.expect(remembered.setPhuxRemote("me@mini", .default));
    const home = (try startup.createPhuxProviderFromConfig(gpa, io, &remembered)).?;
    defer home.destroy();
    try testing.expect(home.remoteTarget() == null);
    try testing.expect(!home.standby);
    const beside = (try startup.createPhuxPeerFromConfig(gpa, io, &remembered)).?;
    defer beside.destroy();
    try testing.expect(beside.standby);
    try testing.expectEqualStrings("me@mini", beside.remoteTarget().?);
    try testing.expectEqualStrings("mini", beside.remoteLabel().?);
    try testing.expectEqualStrings("work", beside.session.?);

    var configured = startup.resolvePhuxConfig(config.parse("phux-remote = me@mini\n"), .{ .runtime_dir = "/tmp/rt" });
    const active = (try startup.createPhuxProviderFromConfig(gpa, io, &configured)).?;
    defer active.destroy();
    try testing.expectEqualStrings("me@mini", active.remoteTarget().?);
    const this_mac = (try startup.createPhuxPeerFromConfig(gpa, io, &configured)).?;
    defer this_mac.destroy();
    try testing.expect(this_mac.standby);
    try testing.expectEqualStrings("/tmp/rt/phux/phux.sock", this_mac.endpointDescriptor().unix);

    var none = startup.resolvePhuxConfig(config.parse(""), .{ .runtime_dir = "/tmp/rt" });
    try testing.expect((try startup.createPhuxPeerFromConfig(gpa, io, &none)) == null);
}
