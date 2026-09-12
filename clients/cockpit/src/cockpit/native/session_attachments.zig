//! Per-window session selection. A connection is shared by views of one
//! session; selecting another session never retargets a connection still used
//! by another native window.
const std = @import("std");
const support = @import("../phux_support.zig");
const model_module = @import("../model.zig");
const Model = model_module.Model;
const Remote = support.PhuxProvider;
const State = @import("../shared_workspace.zig").State;

pub fn hideWindow(model: *Model, window: usize) !void {
    if (comptime !support.phux_enabled) return;
    if (model.phux() != null) try model.shared_workspace.hideNativeWindow(model, window);
    for (model.peers.items) |entry| {
        if (entry.provider == null) continue;
        try entry.workspace.hideNativeWindow(model, window);
    }
}

pub fn releaseHidden(model: *Model) void {
    if (comptime !support.phux_enabled) return;
    if (model.phux() != null) model.shared_workspace.releaseUnused(model);
    for (model.peers.items) |entry| {
        if (entry.provider == null) continue;
        entry.workspace.releaseUnused(model);
    }
}

pub fn closeWindow(model: *Model, fx: anytype, window: usize) !void {
    try hideWindow(model, window);
    // Ephemeral local PTYs still follow their ordinary close cascade. Remote
    // refs can coincide across attachments, so never run a global ref lookup
    // to remove a specific remote window's leaves.
    var locals: [model_module.max_terminals]support.TerminalRef = undefined;
    const count = localRefsInWindow(model, window, &locals);
    for (locals[0..count]) |ref| {
        _ = @import("../workspace_lifecycle.zig").closePane(model, fx, ref, true);
    }
    if (model.windowOpen(window)) {
        model.closeWindow(window);
        fx.closeWindow(@import("scene.zig").windowLabelFor(window));
        if (model.openWindowCount() == 0) fx.quitApp();
    }
    releaseHidden(model);
}

fn localRefsInWindow(model: *const Model, window: usize, out: []support.TerminalRef) usize {
    const workspace = model.wsAtConst(window) orelse return 0;
    var count: usize = 0;
    for (workspace.tabs[0..workspace.tab_count]) |*tree| {
        var refs: [@import("../layout.zig").max_panes]support.TerminalRef = undefined;
        for (refs[0..tree.terminals(&refs)]) |ref| {
            if (support.providerKind(ref) != .local) continue;
            out[count] = ref;
            count += 1;
        }
    }
    return count;
}

pub fn show(engine: anytype, source: *Remote, session: u32, window: usize, epoch: u64, fx: anytype) !void {
    if (comptime !support.phux_enabled) return error.NoProvider;
    const model = engine.model;
    const selection = try prepareSelection(engine, source, session, window, epoch);
    const remote = selection.remote;
    if (revealExisting(model, remote, session, window)) return;
    try hidePrevious(model, window, remote);
    const state = model.sharedWorkspaceForAttachment(remote.context_id) orelse return error.NoProvider;
    state.attachment_id = remote.context_id;
    state.authority = remote.providerId();
    const must_restart = (remote.session_id orelse remote.selectedSessionId()) != session or !remote.showing();
    if (must_restart) {
        try state.leaveSession(model);
        try remote.show(session);
        rememberCreation(model, remote, selection.created);
    }
    state.showInWindow(window, epoch);
    model.bindWindowAttachment(window, remote.context_id);
    model.active_window = window;
    // Selecting the destination changes which chrome is present. Measure its
    // attach grid after that selection rather than from the departed view.
    if (@import("peer_restore.zig").frontViewport(model)) |viewport| remote.attach_viewport = viewport;
    if (must_restart) try restart(engine, remote, fx);
    try projectReady(model, remote, state);
}

fn prepareSelection(engine: anytype, source: *Remote, session: u32, window: usize, epoch: u64) !struct { remote: *Remote, created: i64 } {
    const model = engine.model;
    try validateWindow(model, window, epoch);
    if (model.phuxForAttachment(source.context_id) != source) return error.StaleSource;
    const created = sessionCreation(source, session) orelse return error.StaleSession;
    model.bindSharedAttachment(source);
    const remote = try chooseAttachment(model, source, session, created, window);
    engine.supersedeSelection();
    if (model.peerSlotForAttachment(remote.context_id)) |slot| model.peers.items[slot].selection_epoch = engine.selection_epoch;
    return .{ .remote = remote, .created = created };
}

fn revealExisting(model: *Model, remote: *Remote, session: u32, preferred: usize) bool {
    if (remote.state() != .attached or remote.selectedSessionId() != session) return false;
    if (selectView(model, remote.context_id, preferred)) return true;
    for (0..model_module.max_windows) |window| {
        if (selectView(model, remote.context_id, window)) return true;
    }
    return false;
}

pub fn revealProjected(model: *Model, attachment: u64, preferred: ?[16]u8) void {
    for (0..model_module.max_windows) |window| {
        if (!model.windowOpen(window)) continue;
        const workspace = model.wsAt(window) orelse continue;
        if (selectProjected(workspace, attachment, preferred)) {
            model.active_window = window;
            model.bindWindowAttachment(window, attachment);
            return;
        }
    }
    if (preferred != null) revealProjected(model, attachment, null);
}

fn selectProjected(workspace: *model_module.Workspace, attachment: u64, preferred: ?[16]u8) bool {
    for (workspace.tabs[0..workspace.tab_count], 0..) |*tree, tab| {
        if (tree.attachment_id != attachment) continue;
        if (preferred) |wanted| {
            const id = workspace.shared_ids[tab] orelse continue;
            if (!std.mem.eql(u8, &id, &wanted)) continue;
        }
        workspace.selected_tab = tab;
        workspace.web_selected = false;
        return true;
    }
    return false;
}

fn selectView(model: *Model, attachment: u64, window: usize) bool {
    if (!model.windowOpen(window)) return false;
    const workspace = model.wsAt(window) orelse return false;
    for (workspace.tabs[0..workspace.tab_count], 0..) |*tree, tab| {
        if (tree.attachment_id != attachment) continue;
        workspace.selected_tab = tab;
        workspace.web_selected = false;
        model.active_window = window;
        model.bindWindowAttachment(window, attachment);
        return true;
    }
    return false;
}

fn validateWindow(model: *const Model, window: usize, epoch: u64) !void {
    if (!model.windowOpen(window)) return error.StalePresentationWindow;
    if (model.window_epochs[window] != epoch) return error.StalePresentationWindow;
}

fn sessionCreation(remote: *const Remote, session: u32) ?i64 {
    for (remote.standbyCatalog()) |entry| {
        if (entry.id == session) return entry.created_at_unix_secs;
    }
    return null;
}

fn rootAttachment(model: *const Model, remote: *const Remote) u64 {
    const slot = model.peerSlotForAttachment(remote.context_id) orelse return remote.context_id;
    return model.peers.items[slot].coordinator_context orelse remote.context_id;
}

fn sameSession(model: *const Model, remote: *const Remote, root: u64, session: u32, created: i64) bool {
    if (rootAttachment(model, remote) != root) return false;
    if (remote.pending_retarget != null) return false;
    if (!remote.showing()) return false;
    if ((remote.session_id orelse remote.selectedSessionId()) != session) return false;
    const known = sessionCreation(remote, session) orelse blk: {
        const slot = model.peerSlotForAttachment(remote.context_id) orelse return false;
        break :blk model.peers.items[slot].session_created_at orelse return false;
    };
    return known == created;
}

fn findSession(model: *Model, root: u64, session: u32, created: i64) ?*Remote {
    if (model.phux()) |remote| if (sameSession(model, remote, root, session, created)) return remote;
    for (model.peers.items) |entry| {
        const remote = entry.provider orelse continue;
        if (sameSession(model, remote, root, session, created)) return remote;
    }
    return null;
}

fn chooseAttachment(model: *Model, source: *Remote, session: u32, created: i64, window: usize) !*Remote {
    const root = rootAttachment(model, source);
    if (findSession(model, root, session, created)) |remote| return remote;
    if (!visibleElsewhere(model, source.context_id, window)) return source;
    const slot = try model.freePeerSlot();
    const remote = try source.createSiblingAttachment(source.gpa, source.io, "cockpit-session");
    const entry = model.peers.items[slot];
    entry.provider = remote;
    entry.coordinator_context = root;
    entry.session_created_at = created;
    entry.workspace.attachment_id = remote.context_id;
    entry.workspace.authority = remote.providerId();
    remote.standBy();
    return remote;
}

pub fn visibleElsewhere(model: *const Model, attachment: u64, excluded: usize) bool {
    for (0..model_module.max_windows) |window| {
        if (window == excluded) continue;
        if (!model.windowOpen(window)) continue;
        const remote = model.phuxForWindowConst(window) orelse continue;
        if (remote.context_id == attachment) return true;
    }
    return false;
}

fn rememberCreation(model: *Model, remote: *const Remote, created: i64) void {
    const slot = model.peerSlotForAttachment(remote.context_id) orelse return;
    model.peers.items[slot].session_created_at = created;
}

fn hidePrevious(model: *Model, window: usize, destination: *Remote) !void {
    const previous = model.phuxForWindow(window) orelse return;
    if (previous == destination) return;
    const state = model.sharedWorkspaceForAttachment(previous.context_id) orelse return;
    // An empty destination using the default local provider is not itself a
    // view of that provider. Do not touch another window's subscription set.
    const workspace = model.wsAtConst(window) orelse return;
    if (workspace.tab_count == 0 and model.window_attachments[window] == null) return;
    try state.hideNativeWindow(model, window);
    try projectReady(model, previous, state);
    state.releaseUnused(model);
}

fn projectReady(model: *Model, remote: *Remote, state: *State) !void {
    if (remote.state() != .attached) return;
    const snapshot = remote.workspaceSnapshot();
    if (snapshot.state != .authoritative) return;
    if (snapshot.session_id != remote.selectedSessionId()) return;
    _ = try state.apply(model, snapshot, remote.connectionEpoch());
    state.subscribe(model);
}

fn restart(engine: anytype, remote: *Remote, fx: anytype) !void {
    const Fx = switch (@typeInfo(@TypeOf(fx))) {
        .pointer => |p| p.child,
        else => @TypeOf(fx),
    };
    if (engine.model.peerSlotForAttachment(remote.context_id)) |slot| {
        if (comptime @hasDecl(Fx, "restartPeer")) {
            if (!fx.restartPeer(engine, slot)) return error.ConnectionUnavailable;
        } else return error.ConnectionUnavailable;
    } else {
        if (comptime @hasDecl(Fx, "restartPhux")) {
            if (!fx.restartPhux(engine)) return error.ConnectionUnavailable;
        } else return error.ConnectionUnavailable;
    }
}

test "selecting another same-machine session preserves the first window connection and subscriptions" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const Engine = @import("ts_engine.zig").Engine;
    const fixture = Remote.test_support;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const remote = try Remote.create(std.testing.allocator, std.testing.io, .{ .unix = "/session-attachments-unused" }, null, "sessions");
    model.phux_provider = remote;
    model.primary = .{};
    remote.standBy();
    try remote.host.start("sessions");
    try fixture.stageFixture(remote.bridge, "hello.bin");
    _ = try remote.drainReadiness();
    try fixture.stageFixture(remote.bridge, "standby_state.bin");
    _ = try remote.drainReadiness();
    try std.testing.expectEqual(@as(usize, 2), remote.standbyCatalog().len);
    try remote.show(1);
    _ = try remote.drainReadiness();
    try fixture.stageFixture(remote.bridge, "attached.bin");
    _ = try remote.drainReadiness();
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_metadata.bin");
    try fixture.stageFixture(remote.bridge, "workspace_sessions_a_state.bin");
    _ = try remote.drainReadiness();
    model.shared_workspace.attachment_id = remote.context_id;
    model.shared_workspace.showInWindow(0, model.window_epochs[0]);
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    model.bindWindowAttachment(0, remote.context_id);
    _ = model.openWindow(1) orelse return error.NoWindow;
    const original_epoch = remote.connectionEpoch();
    const original_owner = remote.owner(model.primary.focusedTerminalRef().?).?;
    remote.bridge.outgoing.reset();
    const Fx = struct {
        restarted: ?usize = null,
        pub fn restartPeer(self: *@This(), _: *Engine, slot: usize) bool {
            self.restarted = slot;
            return true;
        }
        pub fn restartPhux(_: *@This(), _: *Engine) bool {
            return false;
        }
        pub fn openChannel(_: *@This(), _: anytype) @import("native_sdk").ChannelHandle {
            return .{};
        }
        pub fn closeChannel(_: *@This(), _: u64) void {}
        pub fn showNotification(_: *@This(), _: anytype) void {}
    };
    var fx: Fx = .{};
    try engine.showSessionFromInWindow(remote, 2, 1, model.window_epochs[1], &fx);
    const second = model.phuxPeerAt(fx.restarted.?).?;
    try std.testing.expect(second != remote);
    try std.testing.expectEqual(original_epoch, remote.connectionEpoch());
    try std.testing.expectEqual(@as(?u32, 1), remote.selectedSessionId());
    try std.testing.expectEqual(@as(?u32, 2), second.session_id);
    try std.testing.expect(remote.ownerIsCurrent(original_owner));
    try std.testing.expect(model.phuxForWindowConst(0) == remote);
    try std.testing.expect(model.phuxForWindowConst(1) == second);
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    const held = model.peers.items.len;
    try engine.showSessionFromInWindow(remote, 2, 1, model.window_epochs[1], &fx);
    try std.testing.expectEqual(held, model.peers.items.len);
    try second.host.start("independent");
    try fixture.stageFixture(second.bridge, "hello.bin");
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(fx.restarted.?), .kind = .data }, null);
    try fixture.stageFixture(second.bridge, "attached_session_b.bin");
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(fx.restarted.?), .kind = .data }, null);
    try fixture.stageWorkspaceFixture(second.bridge, "workspace_initial_metadata.bin");
    try fixture.stageFixture(second.bridge, "workspace_session_b_state.bin");
    _ = engine.onPeerChannel(&fx, .{ .key = engine.peerChannelKey(fx.restarted.?), .kind = .data }, null);
    try std.testing.expectEqual(@as(u32, 2), model.peers.items[fx.restarted.?].workspace.session);
    try std.testing.expectEqual(@as(u32, 1), model.shared_workspace.session);
    const second_owner = second.owner(original_owner.terminal_ref).?;
    try std.testing.expect(!second_owner.eql(original_owner));
    try std.testing.expect(model.phuxForWindowConst(0) == remote);
    try std.testing.expect(model.phuxForWindowConst(1) == second);
    try std.testing.expectEqual(@as(usize, 1), model.primary.tab_count);
    try std.testing.expectEqual(@as(usize, 1), model.wsAtConst(1).?.tab_count);
    try std.testing.expect(model.terminalOwner(original_owner.terminal_ref).?.eql(second_owner));
    try std.testing.expect(model.remoteUi(original_owner.terminal_ref).?.owner.eql(second_owner));
    model.active_window = 0;
    try std.testing.expect(model.terminalOwner(original_owner.terminal_ref).?.eql(original_owner));
    try std.testing.expect(model.remoteUi(original_owner.terminal_ref).?.owner.eql(original_owner));
    model.active_window = 1;
    const scene = @import("scene.zig");
    var no_shells: @import("ts_engine.zig").NoShells = .{};
    engine.pumpViewports(&no_shells, .{ .label = scene.canvasLabelFor(0), .size = .{ .width = 1200, .height = 800 }, .scale_factor = 1, .frame_index = 1, .timestamp_ns = 1 });
    const first_viewport = remote.lastViewport(original_owner.terminal_ref).?;
    engine.pumpViewports(&no_shells, .{ .label = scene.canvasLabelFor(1), .size = .{ .width = 800, .height = 600 }, .scale_factor = 2, .frame_index = 2, .timestamp_ns = 2 });
    try std.testing.expect(remote.lastViewport(original_owner.terminal_ref).?.eql(first_viewport));
    try std.testing.expect(!second.lastViewport(original_owner.terminal_ref).?.eql(first_viewport));
    remote.bridge.outgoing.reset();
    second.bridge.outgoing.reset();
    try second.sendFocus(second_owner, true);
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    try std.testing.expect(second.bridge.outgoing.hasPending());
    const old_window = model.window_epochs[1];
    const protocol = @import("ts_protocol.zig");
    const close = protocol.encodeIntent(.{ .kind = .close_window, .expected_revision = engine.revision, .window = 1, .argument = 0 });
    try std.testing.expect(engine.applyIntent(&close, &@import("ts_engine.zig").NoShells{}));
    try std.testing.expect(!model.windowOpen(1));
    try std.testing.expect(remote.ownerIsCurrent(original_owner));
    try std.testing.expectEqual(@as(usize, 1), model.primary.tab_count);
    _ = model.openWindow(1) orelse return error.NoWindow;
    try std.testing.expectError(error.StalePresentationWindow, engine.showSessionFromInWindow(remote, 2, 1, old_window, &fx));
}
