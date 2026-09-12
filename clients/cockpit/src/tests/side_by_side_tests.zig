//! Side by side: the active Phux provider plus one standby coordinator
//! (`Model.peers`), listed as host groups in one switcher. This Mac's
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
const remote_memory = @import("../cockpit/remote_memory.zig");
const attachments = @import("../cockpit/attachment_state.zig");
const grid = @import("../terminal/grid.zig");
const contract = @import("provider_contract");

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
        try engine.model.ensurePeerSlots(1);
        engine.model.peers.items[0].provider = peer;
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
    try testing.expect(model.phuxPeerAt(0) == null);
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
    try testing.expect(pair.engine.onPeerChannel(&fx, .{ .key = pair.engine.peerChannelKey(0), .kind = .closed }, null));
    try testing.expect(model.peers.items[0].failed);
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
    try testing.expect(!model.peers.items[0].failed);
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

    try testing.expect(engine.model.phuxPeerAt(0) == null);
    const connecting = try remote_hosts.handle(engine, &fx, "\x01\x02\x07me@mini", &out);
    try testing.expectEqual(@intFromEnum(remote_hosts.Phase.connecting), connecting[1]);
    try testing.expectEqualStrings("me@mini", local.remoteTarget().?);
    // The coordinator it left is listed beside it, as a standby.
    const peer = engine.model.phuxPeerAt(0).?;
    try testing.expect(peer.standby);
    try testing.expect(peer.remoteTarget() == null);
    try testing.expectEqualStrings("/side-by-side-unused", peer.endpointDescriptor().unix);

    // Use this Mac: this Mac active again, the host still beside it.
    try testing.expectEqualSlices(u8, "\x01\x00\x00\x00", try remote_hosts.handle(engine, &fx, "\x01\x03\x00", &out));
    try testing.expect(local.remoteTarget() == null);
    try testing.expect(engine.model.phuxPeerAt(0) == peer);
    try testing.expectEqualStrings("me@mini", peer.remoteTarget().?);
    try testing.expectEqualStrings("work", peer.pending_retarget.?.session.?);

    // Disconnect: the host's group goes.
    try testing.expectEqualSlices(u8, "\x01\x00\x00\x00", try remote_hosts.handle(engine, &fx, "\x01\x04\x00", &out));
    try testing.expect(engine.model.phuxPeerAt(0) == null);
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
    const peers = &engine.model.peers;

    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x04mini", &out);
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x06studio", &out);
    // studio is active; this Mac and mini are both listed, each in its slot.
    try testing.expectEqualStrings("studio", local.remoteTarget().?);
    try testing.expect(peers.items[0].provider.?.remoteTarget() == null);
    try testing.expectEqualStrings("mini", peers.items[1].provider.?.remoteTarget().?);
    try testing.expect(peers.items[0].provider.?.standby and peers.items[1].provider.?.standby);
    // Three coordinators, three identities.
    try testing.expect(local.effectiveProviderId() != peers.items[0].provider.?.effectiveProviderId());
    try testing.expect(local.effectiveProviderId() != peers.items[1].provider.?.effectiveProviderId());
    try testing.expect(peers.items[0].provider.?.effectiveProviderId() != peers.items[1].provider.?.effectiveProviderId());

    // Use this Mac trades places with this Mac's slot; mini is untouched.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x03\x00", &out);
    try testing.expect(local.remoteTarget() == null);
    try testing.expectEqualStrings("studio", peers.items[0].provider.?.remoteTarget().?);
    try testing.expectEqualStrings("mini", peers.items[1].provider.?.remoteTarget().?);
    try testing.expect(peers.items[1].provider.?.pending_retarget == null);

    // Reconnecting to a listed host trades places with it, never copies it.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x04mini", &out);
    try testing.expectEqualStrings("mini", local.remoteTarget().?);
    try testing.expect(peers.items[1].provider.?.remoteTarget() == null);
    try testing.expect(engine.model.phuxPeerAt(2) == null);

    // Disconnect removes every host; this Mac is active alone.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x04\x00", &out);
    try testing.expect(local.remoteTarget() == null);
    for (peers.items) |entry| try testing.expect(entry.provider == null);
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

test "Disconnect names one host: a listed host leaves alone, and the active host hands over to this Mac" {
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
    const peers = &engine.model.peers;

    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x07me@mini", &out);
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x06studio", &out);
    // studio is active; this Mac stands by in slot 0 and mini in slot 1.
    try testing.expectEqualStrings("studio", local.remoteTarget().?);
    try testing.expect(peers.items[0].provider.?.remoteTarget() == null);
    const mini = peers.items[1].provider.?;
    try testing.expectEqualStrings("me@mini", mini.remoteTarget().?);

    // A host Cockpit does not hold is refused, and nothing moves.
    const refused = try remote_hosts.handle(engine, &fx, "\x01\x04\x06nosuch", &out);
    try testing.expectEqual(@intFromEnum(remote_hosts.Phase.refused), refused[1]);
    try testing.expect(peers.items[0].provider != null and peers.items[1].provider == mini);
    try testing.expectEqualStrings("studio", local.remoteTarget().?);

    // The active studio goes: this Mac is active again, its standby slot is
    // freed rather than listing it twice, and mini keeps its slot and its
    // connection.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x04\x06studio", &out);
    try testing.expect(local.remoteTarget() == null);
    try testing.expect(peers.items[0].provider == null);
    try testing.expect(peers.items[1].provider == mini);
    try testing.expectEqualStrings("me@mini", mini.remoteTarget().?);
    try testing.expect(mini.pending_retarget == null);

    // mini, named by its registry name: only its slot goes.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x04\x04mini", &out);
    for (peers.items) |entry| try testing.expect(entry.provider == null);
    try testing.expect(local.remoteTarget() == null);
}

test "Disconnect matches an exact target before a registry name, the active host included, and refuses an ambiguous name" {
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
    const peers = &engine.model.peers;

    // me@mini, then mini: the one registry entry held twice. mini is active,
    // me@mini listed in slot 1 (this Mac in slot 0); both are named "mini".
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x07me@mini", &out);
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x04mini", &out);
    try testing.expectEqualStrings("mini", local.remoteTarget().?);
    const me = peers.items[1].provider.?;
    try testing.expectEqualStrings("me@mini", me.remoteTarget().?);
    try testing.expectEqualStrings("mini", me.remoteLabel().?);
    // "mini" is the active host's exact target: it goes, me@mini stays.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x04\x04mini", &out);
    try testing.expect(local.remoteTarget() == null);
    try testing.expect(peers.items[1].provider == me);
    try testing.expect(peers.items[0].provider == null);

    // you@mini joins: now "mini" names you@mini (active) and me@mini, and is
    // the exact target of neither. It is refused, naming both; nothing moves.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x08you@mini", &out);
    try testing.expectEqualStrings("you@mini", local.remoteTarget().?);
    const before = [_]?*support.PhuxProvider{ peers.items[0].provider, peers.items[1].provider };
    const refused = try remote_hosts.handle(engine, &fx, "\x01\x04\x04mini", &out);
    try testing.expectEqual(@intFromEnum(remote_hosts.Phase.refused), refused[1]);
    try testing.expect(std.mem.indexOf(u8, refused, "me@mini") != null);
    try testing.expect(std.mem.indexOf(u8, refused, "you@mini") != null);
    try testing.expect(std.mem.indexOf(u8, refused, "exact target") != null);
    for (before, 0..) |provider, slot| try testing.expect(provider == peers.items[slot].provider);
    try testing.expectEqualStrings("you@mini", local.remoteTarget().?);
    // The exact target removes only that one.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x04\x07me@mini", &out);
    try testing.expect(peers.items[1].provider == null);
    try testing.expectEqualStrings("you@mini", local.remoteTarget().?);
}

test "Disconnect of a remembered host Cockpit does not hold forgets it and says it was not connected" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var registry = try IsolatedRegistry.init(two_host_registry);
    defer registry.deinit();
    const gpa = testing.allocator;
    const io = testing.io;
    const config_file = try registry.tmp.dir.realPathFileAlloc(io, "phux/config.toml", gpa);
    defer gpa.free(config_file);
    const state_path = try std.fs.path.join(gpa, &.{ std.fs.path.dirname(config_file).?, "workspace.state" });
    defer gpa.free(state_path);
    const memory = remote_memory.setPathFor(state_path).?;
    defer _ = remote_memory.setPathFor(null);
    remote_hosts.forgetForTests();
    defer remote_hosts.forgetForTests();
    var hosts: remote_memory.Hosts = .{};
    defer hosts.deinit();
    try testing.expect(hosts.add("me@mini"));
    try testing.expect(hosts.add("studio"));
    remote_memory.storeAll(io, memory, &hosts);

    const engine = try ts_engine.Engine.create(gpa, io);
    defer engine.destroy();
    engine.model.phux_provider = try support.PhuxProvider.create(gpa, io, .{ .unix = "/side-by-side-unused" }, null, "side-by-side");
    const fx = ts_engine.NoShells{};
    var out: [remote_hosts.max_bytes]u8 = undefined;
    // By exact target.
    const by_target = try remote_hosts.handle(engine, &fx, "\x01\x04\x06studio", &out);
    try testing.expectEqual(@intFromEnum(remote_hosts.Phase.refused), by_target[1]);
    try testing.expect(std.mem.indexOf(u8, by_target, "no longer reattached") != null);
    remote_memory.loadAll(io, memory, &hosts);
    try testing.expectEqual(@as(usize, 1), hosts.count);
    try testing.expectEqualStrings("me@mini", hosts.get(0));
    // By its registry name, which names that one remembered host.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x04\x04mini", &out);
    remote_memory.loadAll(io, memory, &hosts);
    try testing.expectEqual(@as(usize, 0), hosts.count);
    // Neither held nor remembered: plainly refused.
    const unknown = try remote_hosts.handle(engine, &fx, "\x01\x04\x06nosuch", &out);
    try testing.expect(std.mem.indexOf(u8, unknown, "not connected to that host") != null);
}

test "a remembered first host that cannot be set up is skipped, not the launch" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var registry = try IsolatedRegistry.init(mini_registry);
    defer registry.deinit();
    var remembered = startup.resolvePhuxConfig(config.parse(""), .{ .runtime_dir = "/tmp/rt" });
    try testing.expect(remembered.setPhuxRemote("me@mini", .default));
    var failing = std.testing.FailingAllocator.init(testing.allocator, .{ .fail_index = 0 });
    try testing.expect((try startup.createPhuxPeerFromConfig(failing.allocator(), testing.io, &remembered)) == null);
}

test "a fifth coordinator connects while existing providers remain owned" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var registry = try IsolatedRegistry.init(two_host_registry ++ "[[remote]]\nname = \"lab\"\nendpoint = \"ws://127.0.0.1:3\"\n[[remote]]\nname = \"rack\"\nendpoint = \"ws://127.0.0.1:4\"\n");
    defer registry.deinit();
    remote_hosts.forgetForTests();
    defer remote_hosts.forgetForTests();
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const local = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/side-by-side-unused" }, null, "side-by-side");
    engine.model.phux_provider = local;
    const fx = ts_engine.NoShells{};
    var out: [remote_hosts.max_bytes]u8 = undefined;
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x04mini", &out);
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x06studio", &out);
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x03lab", &out);
    const first = engine.model.phuxPeerAt(0).?;
    const reply = try remote_hosts.handle(engine, &fx, "\x01\x02\x04rack", &out);
    try testing.expectEqual(@intFromEnum(remote_hosts.Phase.connecting), reply[1]);
    try testing.expectEqualStrings("rack", local.remoteTarget().?);
    try testing.expect(first == engine.model.phuxPeerAt(0).?);
    try testing.expect(engine.model.phuxPeerAt(3) != null);
}

test "six independent standby FFI catalogs preserve stable ownership and never attach" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const model = engine.model;
    const names = [_][]const u8{ "one", "two", "three", "four", "five", "six" };
    var providers: [names.len]*support.PhuxProvider = undefined;
    var entries: [names.len]*model_module.Peer = undefined;
    for (names, 0..) |name, slot| {
        try model.ensurePeerSlots(slot + 1);
        entries[slot] = model.peers.items[slot];
        const peer = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = name } }, null, "six-hosts");
        entries[slot].provider = peer;
        providers[slot] = peer;
        try connectStandby(peer);
        const frames = countFrames(peer);
        try testing.expectEqual(@as(usize, 0), frames.attach);
        try testing.expectEqual(@as(usize, 1), frames.command);
        try testing.expectEqual(@as(usize, 2), peer.standbyCatalog().len);
        for (0..slot) |previous| {
            try testing.expect(entries[previous] == model.peers.items[previous]);
            try testing.expect(providers[previous] == model.phuxPeerAt(previous).?);
            try testing.expect(providers[previous].providerId() != peer.providerId());
            try testing.expect(entries[previous].channel_key != entries[slot].channel_key);
        }
    }
    var destinations: [names.len * 2]model_module.PaletteDestination = undefined;
    try testing.expectEqual(@as(usize, names.len * 2), sessionEntries(model, &destinations).len);
    // One failed catalog cannot retire another provider or its published list.
    providers[4].stop();
    for (providers, 0..) |peer, slot| {
        if (slot == 4) continue;
        try testing.expectEqual(.negotiated, peer.state());
        try testing.expectEqual(@as(usize, 2), peer.standbyCatalog().len);
    }
    std.debug.print("owned coordinator bytes: peer={d} provider={d} host={d} bridge={d}; six standby catalogs=12 sessions\n", .{
        @sizeOf(model_module.Peer), @sizeOf(support.PhuxProvider), @sizeOf(@TypeOf(providers[0].host.*)), @sizeOf(@TypeOf(providers[0].bridge.*)),
    });
}

test "relaunch reattaches every remembered host beside the coordinators held, each listing and never attached" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var registry = try IsolatedRegistry.init(two_host_registry);
    defer registry.deinit();
    const gpa = testing.allocator;
    const io = testing.io;
    const config_file = try registry.tmp.dir.realPathFileAlloc(io, "phux/config.toml", gpa);
    defer gpa.free(config_file);
    const state_path = try std.fs.path.join(gpa, &.{ std.fs.path.dirname(config_file).?, "workspace.state" });
    defer gpa.free(state_path);
    const memory = remote_memory.setPathFor(state_path).?;
    defer _ = remote_memory.setPathFor(null);
    var hosts: remote_memory.Hosts = .{};
    defer hosts.deinit();
    try testing.expect(hosts.add("me@mini"));
    try testing.expect(hosts.add("studio"));
    remote_memory.storeAll(io, memory, &hosts);

    // No host configured: this Mac is active and both hosts stand beside it.
    var remembered = startup.resolvePhuxConfig(config.parse(""), .{ .runtime_dir = "/tmp/rt" });
    startup.restoreRememberedRemote(io, state_path, &remembered);
    const engine = try ts_engine.Engine.create(gpa, io);
    defer engine.destroy();
    const model = engine.model;
    model.phux_provider = (try startup.createPhuxProviderFromConfig(gpa, io, &remembered)).?;
    try model.ensurePeerSlots(1);
    model.peers.items[0].provider = try startup.createPhuxPeerFromConfig(gpa, io, &remembered);
    try startup.attachRememberedPeers(gpa, io, model);
    try testing.expect(model.phux_provider.?.remoteTarget() == null);
    try testing.expectEqualStrings("me@mini", model.peers.items[0].provider.?.remoteTarget().?);
    try testing.expectEqualStrings("studio", model.peers.items[1].provider.?.remoteTarget().?);
    try testing.expectEqualStrings("studio", model.peers.items[1].provider.?.remoteLabel().?);
    try testing.expect(model.phuxPeerAt(2) == null);
    for (model.peers.items[0..2]) |entry| try testing.expect(entry.provider.?.standby);
    // Listing only: after HELLO_OK the restored studio asks GET_STATE, never
    // ATTACH, so it holds no viewport on anyone's session.
    const studio = model.peers.items[1].provider.?;
    try studio.host.start("side-by-side");
    try fixture.stageFixture(studio.bridge, "hello.bin");
    _ = try studio.drainReadiness();
    const frames = countFrames(studio);
    try testing.expectEqual(@as(usize, 0), frames.attach);
    try testing.expectEqual(@as(usize, 1), frames.command);

    // A configured host is active: this Mac and the other remembered host
    // stand beside it, and the configured one is not held twice.
    var configured = startup.resolvePhuxConfig(config.parse("phux-remote = studio\n"), .{ .runtime_dir = "/tmp/rt" });
    startup.restoreRememberedRemote(io, state_path, &configured);
    const second = try ts_engine.Engine.create(gpa, io);
    defer second.destroy();
    second.model.phux_provider = (try startup.createPhuxProviderFromConfig(gpa, io, &configured)).?;
    try second.model.ensurePeerSlots(1);
    second.model.peers.items[0].provider = try startup.createPhuxPeerFromConfig(gpa, io, &configured);
    try startup.attachRememberedPeers(gpa, io, second.model);
    try testing.expectEqualStrings("studio", second.model.phux_provider.?.remoteTarget().?);
    try testing.expect(second.model.peers.items[0].provider.?.remoteTarget() == null);
    try testing.expectEqualStrings("me@mini", second.model.peers.items[1].provider.?.remoteTarget().?);
    try testing.expect(second.model.peers.items[1].provider.?.standby);
    try testing.expect(second.model.phuxPeerAt(2) == null);
}

/// A launch as initializeModel composes it: this Mac active, the first
/// remembered host beside it, then every other remembered host.
fn launchRemembered(gpa: std.mem.Allocator, io: std.Io, state_path: []const u8) !*ts_engine.Engine {
    var remembered = startup.resolvePhuxConfig(config.parse(""), .{ .runtime_dir = "/tmp/rt" });
    startup.restoreRememberedRemote(io, state_path, &remembered);
    const engine = try ts_engine.Engine.create(gpa, io);
    errdefer engine.destroy();
    const model = engine.model;
    model.phux_provider = (try startup.createPhuxProviderFromConfig(gpa, io, &remembered)).?;
    try model.ensurePeerSlots(1);
    model.peers.items[0].provider = try startup.createPhuxPeerFromConfig(gpa, io, &remembered);
    try startup.attachRememberedPeers(gpa, io, model);
    return engine;
}

test "relaunch hands each remembered host its own record, and a removed host's record is gone" {
    // ADR-0110: a record comes back only with its host, keyed by that host's
    // coordinator id, and every host still lists: nothing attaches here.
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var registry = try IsolatedRegistry.init(two_host_registry);
    defer registry.deinit();
    const gpa = testing.allocator;
    const io = testing.io;
    const config_file = try registry.tmp.dir.realPathFileAlloc(io, "phux/config.toml", gpa);
    defer gpa.free(config_file);
    const state_path = try std.fs.path.join(gpa, &.{ std.fs.path.dirname(config_file).?, "workspace.state" });
    defer gpa.free(state_path);
    const memory = remote_memory.setPathFor(state_path).?;
    defer _ = remote_memory.setPathFor(null);
    const mini_id = support.PhuxProvider.coordinatorId(.{ .remote = .{ .target = "me@mini" } });
    const studio_id = support.PhuxProvider.coordinatorId(.{ .remote = .{ .target = "studio" } });
    var hosts: remote_memory.Hosts = .{};
    defer hosts.deinit();
    try testing.expect(hosts.add("me@mini"));
    try testing.expect(hosts.add("studio"));
    try testing.expect(hosts.setShown(0, .{ .session = 1, .server = 7, .front = true }));
    try testing.expect(hosts.setShown(1, .{ .session = 2, .server = 7 }));
    remote_memory.storeAll(io, memory, &hosts);

    {
        const engine = try launchRemembered(gpa, io, state_path);
        defer engine.destroy();
        const model = engine.model;
        // mini stands beside this Mac first (createPhuxPeerFromConfig), and
        // still receives its own record; studio joins with its own.
        try testing.expectEqual(mini_id, model.peers.items[0].provider.?.providerId());
        try testing.expectEqual(studio_id, model.peers.items[1].provider.?.providerId());
        const mini_record = model.peers.items[0].restore.?;
        try testing.expectEqual(mini_id, mini_record.coordinator);
        try testing.expect(mini_record.pending);
        try testing.expectEqual(@as(u32, 1), mini_record.shown.session);
        const studio_record = model.peers.items[1].restore.?;
        try testing.expectEqual(studio_id, studio_record.coordinator);
        try testing.expect(!studio_record.pending);
        for (model.peers.items[0..2]) |entry| try testing.expect(entry.provider.?.standby);
    }

    // Disconnect forgets mini (remote_hosts.forgetHost removes it from the
    // same list), and its record goes with it.
    try testing.expect(hosts.remove("me@mini"));
    remote_memory.storeAll(io, memory, &hosts);
    {
        const engine = try launchRemembered(gpa, io, state_path);
        defer engine.destroy();
        const model = engine.model;
        try testing.expectEqual(studio_id, model.peers.items[0].provider.?.providerId());
        for (model.peers.items) |entry| if (entry.provider) |peer| try testing.expect(peer.providerId() != mini_id);
        var records: usize = 0;
        for (model.peers.items) |entry| {
            const value = entry.restore;
            const record = value orelse continue;
            records += 1;
            try testing.expect(record.coordinator != mini_id);
            try testing.expect(!record.pending);
        }
        try testing.expectEqual(@as(usize, 1), records);
    }
}

test "a placement saved under .phux for a remote host before coordinator ids is dropped at launch, and that host's workspace projects it again" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const gpa = testing.allocator;
    const io = testing.io;
    const session = try grid.Session.create(gpa, io, 80, 24);
    const model = try std.heap.page_allocator.create(model_module.Model);
    model.* = try model_module.initialModelWithIo(gpa, io, session);
    // What an earlier release saved for mini's terminal 7: the old `.phux`
    // id beside mini's `phux-remote:mini` endpoint, placed in a tab.
    const old: support.TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, 7, "") } };
    _ = try model.saved_attachments.append(.{ .terminal_ref = old, .context = try attachments.Context.init("phux-remote:mini", "server-a", 1) });
    model.pending_attachments[0] = true;
    try testing.expect(model.primary.admitTab(old));
    const mini = try support.PhuxProvider.create(gpa, io, .{ .remote = .{ .target = "mini" } }, null, "old-placement");
    model_module.attachPhuxProvider(model, mini);
    const engine = try ts_engine.Engine.createFromInitialized(.{ .model = model, .provenance = .restored });
    defer engine.destroy();

    // A Phux-backed launch keeps no saved evidence: the old placement is
    // gone, and nothing can match it or route it.
    try testing.expectEqual(@as(u8, 0), model.saved_attachments.count);
    try testing.expect(model.locateTerminal(old) == null);
    try testing.expect(!model.containsTerminal(old));
    try testing.expect(model.phuxForRef(old) == null);
    // mini's own workspace projects its terminal 7 under mini's id.
    try fixture.attachHost(mini.host);
    model.shared_workspace.authority = mini.providerId();
    _ = try model.shared_workspace.apply(model, mini.workspaceSnapshot(), mini.connectionEpoch());
    const projected = model.focusedTerminalRef().?;
    try testing.expectEqual(contract.phuxCoordinatorId("mini"), projected.provider_id);
    try testing.expectEqual(@as(u32, 7), projected.terminal_id.phux.id);
    try testing.expect(!projected.eql(old));
    try testing.expect(model.phuxForRef(projected) == mini);
}

/// Channel keys closed and opened, and restarts counted; a channel it opens
/// is never live, and one it is asked about is live when `live` says so.
const ChannelLog = struct {
    closed: [8]u64 = @splat(0),
    closes: usize = 0,
    opened: [8]u64 = @splat(0),
    opens: usize = 0,
    live: bool = false,
    restarts: usize = 0,

    pub fn restartPhux(_: *@This(), _: *ts_engine.Engine) bool {
        return true;
    }
    pub fn restartPeer(self: *@This(), _: *ts_engine.Engine, _: usize) bool {
        self.restarts += 1;
        return true;
    }
    pub fn peerChannelLive(self: *const @This(), _: u64) bool {
        return self.live;
    }
    pub fn closeChannel(self: *@This(), key: u64) void {
        if (self.closes < self.closed.len) self.closed[self.closes] = key;
        self.closes += 1;
    }
    pub fn openChannel(self: *@This(), options: anytype) native_sdk.ChannelHandle {
        if (self.opens < self.opened.len) self.opened[self.opens] = options.key;
        self.opens += 1;
        return .{};
    }
    pub fn showNotification(_: *const @This(), _: anytype) void {}
};

test "twelve FFI catalog wakes share one real SDK channel and advance round robin" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const Message = union(enum) { event: native_sdk.EffectChannelEvent };
    const Effects = native_sdk.Effects(Message);
    var fx = Effects.init(testing.allocator);
    defer fx.deinit();
    fx.executor = .fake;
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    engine.openPeerChannels(&fx, Effects.channelMsg(.event));
    try testing.expect(engine.peer_wake_handle.live());
    try engine.model.ensurePeerSlots(12);
    for (engine.model.peers.items, 0..) |entry, index| {
        var name: [32]u8 = undefined;
        const target = try std.fmt.bufPrint(&name, "mux-{d}", .{index});
        const peer = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = target } }, null, "multiplexed");
        entry.provider = peer;
        peer.standBy();
        try peer.host.start("multiplexed");
        try fixture.stageFixture(peer.bridge, "hello.bin");
    }
    // Real SDK still admits seven other channels: peer count did not spend
    // its eight-channel table, and this test does not raise the SDK limit.
    for (0..7) |index| {
        const spare = fx.openChannel(.{ .key = 500 + index, .on_event = Effects.channelMsg(.event) });
        try testing.expect(spare.live());
    }
    _ = engine.peer_wake_handle.post("");
    for (engine.model.peers.items, 0..) |entry, index| {
        try testing.expectEqual(.hello_queued, entry.provider.?.state());
        const message = fx.takeMsg() orelse return error.ExpectedWake;
        _ = engine.onPeerChannel(&fx, message.event, Effects.channelMsg(.event));
        try testing.expectEqual(.negotiated, entry.provider.?.state());
        if (index + 1 < engine.model.peers.items.len) try testing.expectEqual(.hello_queued, engine.model.peers.items[index + 1].provider.?.state());
        try fixture.stageFixture(entry.provider.?.bridge, "standby_state.bin");
    }
    // Each next turn consumes exactly one published catalog, including the
    // last host beyond the former application and SDK channel limits.
    for (engine.model.peers.items) |entry| {
        const message = fx.takeMsg() orelse return error.ExpectedWake;
        _ = engine.onPeerChannel(&fx, message.event, Effects.channelMsg(.event));
        try testing.expectEqual(@as(usize, 2), entry.provider.?.standbyCatalog().len);
        try testing.expectEqual(@as(usize, 0), countFrames(entry.provider.?).attach);
    }
    const retained = engine.peer_wake_handle;
    engine.dropPeer(&fx, 5);
    try testing.expect(retained.live());
    try testing.expectEqual(.negotiated, engine.model.phuxPeerAt(11).?.state());
}

test "allocated peer handles stay unique beyond sixteen entries and across growth" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    try engine.model.ensurePeerSlots(1);
    const first = engine.model.peers.items[0];
    const first_key = engine.peerChannelKey(0);
    try engine.model.ensurePeerSlots(65);
    try testing.expect(first == engine.model.peers.items[0]);
    try testing.expectEqual(first_key, engine.peerChannelKey(0));
    for (engine.model.peers.items, 0..) |entry, index| {
        for (engine.model.peers.items[0..index]) |previous| {
            try testing.expect(entry.channel_key != previous.channel_key);
        }
    }
}

test "a close from before Disconnect and a new Connect reused the slot is ignored; the new peer's own close is not" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var registry = try IsolatedRegistry.init(two_host_registry);
    defer registry.deinit();
    remote_hosts.forgetForTests();
    defer remote_hosts.forgetForTests();
    const engine = try ts_engine.Engine.create(testing.allocator, testing.io);
    defer engine.destroy();
    const local = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/side-by-side-unused" }, null, "side-by-side");
    engine.model.phux_provider = local;
    var fx: ChannelLog = .{};
    var out: [remote_hosts.max_bytes]u8 = undefined;

    // Connect to mini: this Mac stands by in slot 0, on its first channel.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x04mini", &out);
    const first_key = engine.peerChannelKey(0);
    try testing.expect(first_key != support.phux_channel_key);
    // Disconnect closes that channel; its close event is still on its way.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x04\x00", &out);
    try testing.expect(engine.model.peers.items[0].provider == null);
    try testing.expectEqual(first_key, fx.closed[fx.closes - 1]);
    // Connect to studio: this Mac takes slot 0 again, under a new key.
    _ = try remote_hosts.handle(engine, &fx, "\x01\x02\x06studio", &out);
    const again = engine.model.peers.items[0].provider.?;
    try testing.expect(engine.peerChannelKey(0) != first_key);

    // The old channel's close and a late post arrive now: neither is the new
    // peer's, so neither stops it or marks it failed.
    try testing.expect(!engine.onPeerChannel(&fx, .{ .key = first_key, .kind = .closed }, null));
    try testing.expect(!engine.onPeerChannel(&fx, .{ .key = first_key, .kind = .data }, null));
    try testing.expect(!engine.model.peers.items[0].failed);
    try testing.expect(engine.model.peers.items[0].provider == again);
    try testing.expectEqual(@as(usize, 0), fx.opens);
    // The new channel's own close is the new peer's: it failed, and says so.
    try testing.expect(engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null));
    try testing.expect(engine.model.peers.items[0].failed);
}

test "a restart reopens a peer on its own close, under the next key, and never before" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(false);
    defer pair.engine.destroy();
    const engine = pair.engine;
    var fx: ChannelLog = .{ .live = true };
    const first_key = engine.peerChannelKey(0);

    // The live channel is closed first; nothing opens yet.
    try testing.expect(engine.restartPeerConnection(&fx, 0, null));
    try testing.expectEqual(@as(usize, 1), fx.closes);
    try testing.expectEqual(first_key, fx.closed[0]);
    try testing.expectEqual(@as(usize, 0), fx.opens);
    try testing.expect(engine.model.peers.items[0].reopen);
    // A second restart while that close is pending opens nothing more.
    try testing.expect(engine.restartPeerConnection(&fx, 0, null));
    try testing.expectEqual(@as(usize, 1), fx.closes);
    try testing.expectEqual(@as(usize, 0), fx.opens);
    // A late post of the closed channel is ignored.
    try testing.expect(!engine.onPeerChannel(&fx, .{ .key = first_key, .kind = .data }, null));
    try testing.expectEqual(@as(usize, 0), fx.opens);
    // Its close opens the next channel, under the next key.
    _ = engine.onPeerChannel(&fx, .{ .key = first_key, .kind = .closed }, null);
    try testing.expect(!engine.model.peers.items[0].reopen);
    try testing.expectEqual(@as(usize, 1), fx.opens);
    try testing.expectEqual(engine.peerChannelKey(0), fx.opened[0]);
    try testing.expect(fx.opened[0] != first_key);
}

/// Retry timers asked for, by key and wait; restarts counted, never real.
const RetryLog = struct {
    keys: [16]u64 = @splat(0),
    delays: [16]u64 = @splat(0),
    count: usize = 0,
    restarts: usize = 0,

    pub fn schedulePeerRetry(self: *@This(), key: u64, delay_ms: u64) void {
        if (self.count < self.keys.len) {
            self.keys[self.count] = key;
            self.delays[self.count] = delay_ms;
        }
        self.count += 1;
    }
    pub fn restartPeer(self: *@This(), _: *ts_engine.Engine, _: usize) bool {
        self.restarts += 1;
        return true;
    }
    pub fn closeChannel(_: *const @This(), _: u64) void {}
    pub fn openChannel(_: *const @This(), _: anytype) native_sdk.ChannelHandle {
        return .{};
    }
    pub fn showNotification(_: *const @This(), _: anytype) void {}
};

test "a failed listing peer is redialed as a lister after 1 s, twice as long after each failure to 60 s, and from 1 s only once it stayed listed" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(false);
    defer pair.engine.destroy();
    const engine = pair.engine;
    const model = engine.model;
    var fx: RetryLog = .{};

    // mini's connection closes: its group says so, and a redial is armed.
    try testing.expect(engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null));
    try testing.expect(model.peers.items[0].failed);
    try testing.expectEqual(@as(usize, 1), fx.count);
    try testing.expectEqual(@as(u64, 1000), fx.delays[0]);
    try testing.expectEqual(@as(usize, 0), fx.restarts);

    // It fires: mini is dialed again, still standing by, so its new
    // connection asks GET_STATE and never ATTACH.
    try testing.expect(engine.onPeerRetryTimer(&fx, fx.keys[0]));
    try testing.expectEqual(@as(usize, 1), fx.restarts);
    try testing.expect(!model.peers.items[0].failed);
    try testing.expect(pair.peer.standby);
    pair.peer.stop();
    try pair.peer.host.reconnect("side-by-side");
    try fixture.stageFixture(pair.peer.bridge, "hello.bin");
    _ = try pair.peer.drainReadiness();
    const redial = countFrames(pair.peer);
    try testing.expectEqual(@as(usize, 0), redial.attach);
    try testing.expectEqual(@as(usize, 1), redial.command);
    // The same timer firing again redials nothing more.
    try testing.expect(!engine.onPeerRetryTimer(&fx, fx.keys[0]));
    try testing.expectEqual(@as(usize, 1), fx.restarts);

    // Each further failure waits twice as long, never more than 60 s.
    const waits = [_]u64{ 2000, 4000, 8000, 16000, 32000, 60000, 60000 };
    for (waits, 1..) |wait, index| {
        try testing.expect(engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null));
        try testing.expectEqual(wait, fx.delays[index]);
        try testing.expect(engine.onPeerRetryTimer(&fx, fx.keys[index]));
    }
    try testing.expectEqual(@as(usize, 1 + waits.len), fx.restarts);

    // Listing again does not start it over: a host that lists and then
    // fails at once keeps backing off instead of being redialed every second.
    pair.peer.stop();
    try pair.peer.host.reconnect("side-by-side");
    try fixture.stageFixture(pair.peer.bridge, "hello.bin");
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .data }, null);
    try fixture.stageFixture(pair.peer.bridge, "standby_state.bin");
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .data }, null);
    try testing.expect(!model.peers.items[0].failed);
    try testing.expect(engine.model.peers.items[0].listed_since != null);
    try testing.expect(engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null));
    try testing.expectEqual(@as(u64, 60_000), fx.delays[fx.count - 1]);
    try testing.expect(engine.model.peers.items[0].listed_since == null);

    // One that stayed listed for the stable window before failing starts
    // over at 1 s.
    try testing.expect(engine.onPeerRetryTimer(&fx, fx.keys[fx.count - 1]));
    pair.peer.stop();
    try pair.peer.host.reconnect("side-by-side");
    try fixture.stageFixture(pair.peer.bridge, "hello.bin");
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .data }, null);
    try fixture.stageFixture(pair.peer.bridge, "standby_state.bin");
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .data }, null);
    const listed = engine.model.peers.items[0].listed_since.?;
    engine.model.peers.items[0].listed_since = listed.subDuration(std.Io.Duration.fromMilliseconds(ts_engine.Engine.peer_retry_stable_ms));
    try testing.expect(engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null));
    try testing.expectEqual(@as(u64, 1000), fx.delays[fx.count - 1]);
}

test "a showing peer is not redialed automatically, and a timer from before a pick or a drop does nothing" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var pair = try Pair.start(false);
    defer pair.engine.destroy();
    const engine = pair.engine;
    const model = engine.model;
    var fx: RetryLog = .{};
    const key = ts_engine.Engine.peer_retry_timer_key;

    // Showing a session, it keeps its frozen tabs: no redial is armed, and
    // a timer for its slot does nothing.
    try pair.peer.show(2);
    try testing.expect(engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null));
    try testing.expect(model.peers.items[0].failed);
    try testing.expectEqual(@as(usize, 0), fx.count);
    try testing.expect(!engine.onPeerRetryTimer(&fx, key));
    try testing.expectEqual(@as(usize, 0), fx.restarts);

    // Listing, it is armed; picking its row first makes that timer stale.
    pair.peer.standBy();
    try testing.expect(engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null));
    try testing.expectEqual(@as(usize, 1), fx.count);
    try testing.expect(engine.retryPeer(pair.peer.providerId(), &fx));
    try testing.expectEqual(@as(usize, 1), fx.restarts);
    try testing.expect(!engine.onPeerRetryTimer(&fx, fx.keys[0]));
    try testing.expectEqual(@as(usize, 1), fx.restarts);

    // Armed again, then dropped: its timer does nothing. Nor does a key
    // past the last slot.
    try testing.expect(engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(0), .kind = .closed }, null));
    try testing.expectEqual(@as(usize, 2), fx.count);
    engine.dropPeer(&fx, 0);
    try testing.expect(!engine.onPeerRetryTimer(&fx, fx.keys[1]));
    try testing.expect(!engine.onPeerRetryTimer(&fx, std.math.maxInt(u64)));
    try testing.expectEqual(@as(usize, 1), fx.restarts);
}
