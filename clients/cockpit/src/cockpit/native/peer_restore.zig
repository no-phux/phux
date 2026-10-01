//! A showing peer's session across relaunch (ADR-0110). The `.remote` file
//! keeps, per remembered host, the session its coordinator was showing (id
//! and creation time), its selected tab's shared window, and whether that tab
//! was in front (`remote_memory.Shown`); the coordinator owns the layout.
//!
//! Every remembered host lists first, and its record is judged on its first
//! list (`onListed`). A non-front record only hints which tab to select if
//! the user picks that session. The front record is shown once the front
//! window has a measured size, so ATTACH carries its real grid. A record that
//! no longer matches is dropped, and records only ever apply to the peer
//! holding their coordinator id.

const std = @import("std");
const contract = @import("provider_contract");
const support = @import("../phux_support.zig");
const model_module = @import("../model.zig");
const remote_memory = @import("../remote_memory.zig");
const shared_workspace = @import("../shared_workspace.zig");
const projection = @import("workspace_projection.zig");
const grid = @import("../../terminal/grid.zig");

const Model = model_module.Model;
const Shown = remote_memory.Shown;

/// A listing peer's list arrived: judge its record against it. True when a
/// front record was shown now.
pub fn onListed(engine: anytype, fx: anytype, slot: usize) bool {
    if (comptime !support.phux_enabled or !canShow(@TypeOf(fx))) return false;
    const model = engine.model;
    const restore = model.peers.items[slot].restore orelse return false;
    if (!current(model, slot, restore)) return drop(model, slot);
    if (!restore.pending) return false;
    return showFront(engine, fx, slot);
}

/// A frame measured a window. A front record whose list already reconciled
/// and that waited only for a real size is shown now.
pub fn onFrame(engine: anytype, fx: anytype) bool {
    if (comptime !support.phux_enabled or !canShow(@TypeOf(fx))) return false;
    const model = engine.model;
    var shown = false;
    for (model.peers.items, 0..) |entry, slot| {
        const restore = entry.restore orelse continue;
        if (!restore.pending or !restore.listed) continue;
        if (!current(model, slot, restore)) {
            _ = drop(model, slot);
            continue;
        }
        shown = showFront(engine, fx, slot) or shown;
    }
    return shown;
}

/// Only effects that can restart a peer can show one; any other caller
/// leaves every record as it is.
fn canShow(comptime Fx: type) bool {
    const T = switch (@typeInfo(Fx)) {
        .pointer => |pointer| pointer.child,
        else => Fx,
    };
    return @hasDecl(T, "restartPeer");
}

/// The user chose what to show before a front record was: it is not shown.
/// Its tab hint stays, in case the user picks that session.
pub fn cancelFront(model: *Model) void {
    for (model.peers.items) |entry| {
        if (entry.restore) |*restore| {
            restore.pending = false;
            restore.listed = false;
        }
    }
}

/// The peer failed before its front record was shown: keep the record for
/// the backoff redial once, and drop it on a second failure.
pub fn failed(model: *Model, slot: usize) void {
    const restore = if (model.peers.items[slot].restore) |*value| value else return;
    if (!restore.pending) return;
    if (restore.retried) {
        model.peers.items[slot].restore = null;
        return;
    }
    restore.retried = true;
    // Whatever that connection listed is gone with it.
    restore.listed = false;
}

/// The shared window of the tab to select when `coordinator` first
/// projects `session`, if its record remembers one. Consumed: the live
/// selection is what is kept from then on.
pub fn takeHint(model: *Model, coordinator: support.ProviderId, session: u32) ?[16]u8 {
    const slot = model.peerSlot(coordinator) orelse return null;
    const restore = model.peers.items[slot].restore orelse return null;
    if (restore.coordinator != coordinator or restore.shown.session != session) return null;
    model.peers.items[slot].restore = null;
    return restore.shown.window;
}

/// Whether the slot's record still applies: it names the peer in the slot,
/// and the peer lists that same session (not merely the id), non-empty.
fn current(model: *const Model, slot: usize, restore: model_module.PeerRestore) bool {
    const peer = model.phuxPeerAtConst(slot) orelse return false;
    if (peer.providerId() != restore.coordinator) return false;
    for (peer.standbyCatalog()) |entry| {
        if (entry.id != restore.shown.session) continue;
        return !entry.empty and sameSession(peer, restore.shown, entry.created_at_unix_secs);
    }
    return false;
}

/// The same session: same id and creation second (a graceful upgrade keeps
/// both). Records without a creation time match the server incarnation.
fn sameSession(peer: anytype, shown: Shown, created: i64) bool {
    if (shown.created) |value| return value == created;
    const server = peer.serverId() orelse return false;
    return remote_memory.serverHash(server) == shown.server;
}

fn drop(model: *Model, slot: usize) bool {
    model.peers.items[slot].restore = null;
    return false;
}

fn showFront(engine: anytype, fx: anytype, slot: usize) bool {
    const model = engine.model;
    const restore = &(model.peers.items[slot].restore.?);
    // Before the front window is measured there is no real size to attach
    // with; the frame that measures it shows the session (`onFrame`).
    const viewport = frontViewport(model) orelse {
        restore.listed = true;
        return false;
    };
    restore.pending = false;
    restore.listed = false;
    const peer = model.phuxPeerAt(slot).?;
    peer.attach_viewport = viewport;
    if (engine.showPeerSession(restore.coordinator, restore.shown.session, fx)) return true;
    return drop(model, slot);
}

/// The grid a single pane gets in the front window's terminal area: what
/// the front record's ATTACH carries. Null before a frame measured it.
pub fn frontViewport(model: *const Model) ?contract.Viewport {
    if (!model.windowOpen(model.active_window)) return null;
    const workspace = model.wsAtConst(model.active_window) orelse return null;
    // The default surface size is a placeholder; only a frame's is real.
    if (!workspace.surface_measured) return null;
    const size = workspace.surface_size;
    if (size.width <= 0 or size.height <= 0) return null;
    const content = projection.workspaceChromeIn(model, workspace, size).content;
    if (content.width <= 0 or content.height <= 0) return null;
    // ATTACH needs a provisional grid before this remote pane can paint.
    // The viewport pump replaces it with the painter's actual measurement.
    const metrics = projection.terminalCellMetricsFor(projection.terminalTokens(model));
    if (metrics.width <= 0 or metrics.height <= 0) return null;
    const cells = grid.Session.clampGrid(
        @intFromFloat(@max(2, content.width / metrics.width)),
        @intFromFloat(@max(2, content.height / metrics.height)),
    );
    return .{ .cols = cells.x, .rows = cells.y };
}

/// Bring every remembered host's record in line with what is on screen.
/// True when one changed, so the caller writes the file.
pub fn capture(model: *const Model, hosts: *remote_memory.Hosts) bool {
    if (comptime !support.phux_enabled) return false;
    const live_front = hasLiveFront(model, hosts);
    var changed = false;
    for (0..hosts.count) |index| {
        const id = support.PhuxProvider.coordinatorId(.{ .remote = .{ .target = hosts.get(index) } });
        const next = if (unsettled(model, id)) retainedRecord(hosts.shown[index], live_front) else liveRecord(model, id);
        changed = hosts.setShown(index, next) or changed;
    }
    return changed;
}

fn hasLiveFront(model: *const Model, hosts: *const remote_memory.Hosts) bool {
    for (0..hosts.count) |index| {
        const id = support.PhuxProvider.coordinatorId(.{ .remote = .{ .target = hosts.get(index) } });
        if (unsettled(model, id)) continue;
        const record = liveRecord(model, id) orelse continue;
        if (record.front) return true;
    }
    return false;
}

/// A current front selection outranks a restore still waiting on its host.
fn retainedRecord(record: ?Shown, live_front: bool) ?Shown {
    const value = record orelse return null;
    if (live_front and value.front) return null;
    return value;
}

/// Whether what coordinator `id` shows is not settled yet: a launch restore
/// not judged, or a retarget in flight. Its loaded record is kept then, so a
/// quit before its host lists loses nothing.
fn unsettled(model: *const Model, id: support.ProviderId) bool {
    for (model.peers.items) |entry| {
        const restore = entry.restore orelse continue;
        if (restore.coordinator == id and restore.pending) return true;
    }
    if (model.phuxConst()) |active| if (active.pending_retarget != null) return true;
    return false;
}

/// What coordinator `id` shows now: its session and its tab on screen.
fn liveRecord(model: *const Model, id: support.ProviderId) ?Shown {
    const showing = showingSession(model, id) orelse return null;
    const tab = selectedTab(model, id) orelse return null;
    // Named by its creation time when the list gives it, so the record
    // survives a graceful upgrade; else by the server incarnation, which
    // still restores it under the same server.
    if (showing.created) |created| return .{ .session = showing.session, .created = created, .window = tab.window, .front = tab.front };
    return .{ .session = showing.session, .server = remote_memory.serverHash(showing.server), .window = tab.window, .front = tab.front };
}

const Showing = struct { session: u32, server: []const u8, created: ?i64 };

/// The session coordinator `id` is attached to and displaying: the active
/// coordinator's own, or a showing peer's projected one.
fn showingSession(model: *const Model, id: support.ProviderId) ?Showing {
    if (model.phuxConst()) |active| if (active.providerId() == id) return activeShowing(active);
    const slot = model.peerSlot(id) orelse return null;
    const peer = model.phuxPeerAtConst(slot) orelse return null;
    if (!peer.showing() or peer.state() != .attached) return null;
    const session = model.peers.items[slot].workspace.session;
    if (session == 0) return null;
    return .{ .session = session, .server = peer.serverId() orelse return null, .created = createdAt(peer, session) };
}

fn activeShowing(active: anytype) ?Showing {
    if (active.state() != .attached) return null;
    const session = active.selectedSessionId() orelse return null;
    return .{ .session = session, .server = active.serverId() orelse return null, .created = createdAt(active, session) };
}

/// `session`'s creation time in the coordinator's own session list.
fn createdAt(provider: anytype, session: u32) ?i64 {
    for (provider.sessionCatalog()) |entry| {
        if (entry.id == session) return entry.created_at_unix_secs;
    }
    return null;
}

const Tab = struct { window: ?[16]u8, front: bool };

/// Coordinator `id`'s tab that is the selected tab of an open window: the
/// front window's first, then any other's.
fn selectedTab(model: *const Model, id: support.ProviderId) ?Tab {
    if (selectedIn(model, model.active_window, id)) |window| return .{ .window = window, .front = true };
    for (0..model_module.max_windows) |index| {
        if (index == model.active_window) continue;
        if (selectedIn(model, index, id)) |window| return .{ .window = window, .front = false };
    }
    return null;
}

fn selectedIn(model: *const Model, index: usize, id: support.ProviderId) ??[16]u8 {
    if (!model.windowOpen(index)) return null;
    const workspace = model.wsAtConst(index) orelse return null;
    if (workspace.web_selected or workspace.tab_count == 0) return null;
    const tree = workspace.treeConst(workspace.selected_tab) orelse return null;
    if (shared_workspace.tabAuthority(tree) != id) return null;
    return workspace.shared_ids[workspace.selected_tab];
}
