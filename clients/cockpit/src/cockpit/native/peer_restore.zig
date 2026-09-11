//! A showing peer's session across relaunch (ADR-0110).
//!
//! The `.remote` file keeps, beside each remembered host, the one session
//! its coordinator was showing, the server incarnation it belongs to, the
//! shared window of its selected tab, and whether that tab was the selected
//! tab of the front window (`remote_memory.Shown`). Nothing else about the
//! layout is kept: the coordinator's shared workspace owns its windows,
//! tabs and splits.
//!
//! At launch every remembered host lists first. Its record is judged when
//! its first list arrives (`onListed`), against that list and that server:
//!
//! - A record that is not front never attaches anything. It is kept only as
//!   a hint for which tab to select if the user picks that session.
//! - The front record shows its session through the ordinary show path, and
//!   only once the front window has a measured size, so the ATTACH carries
//!   that window's real grid. Its first projection takes the selection, so
//!   the peer is displaying from the moment it attaches (rule A in the ADR).
//!
//! A record that no longer matches is dropped. It is never applied to
//! another coordinator: every record names its coordinator id, and only the
//! peer holding that id may consult it (rule B).

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
    const restore = model.peer_restore[slot] orelse return false;
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
    for (0..model_module.max_phux_peers) |slot| {
        const restore = model.peer_restore[slot] orelse continue;
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
    for (&model.peer_restore) |*value| {
        if (value.*) |*restore| {
            restore.pending = false;
            restore.listed = false;
        }
    }
}

/// The peer's connection failed before its front record was shown: that
/// record is dropped, so the peer comes back listing only.
pub fn failed(model: *Model, slot: usize) void {
    const restore = model.peer_restore[slot] orelse return;
    if (restore.pending) model.peer_restore[slot] = null;
}

/// The shared window of the tab to select when `coordinator` first
/// projects `session`, if its record remembers one. Consumed: the live
/// selection is what is kept from then on.
pub fn takeHint(model: *Model, coordinator: support.ProviderId, session: u32) ?[16]u8 {
    const slot = model.peerSlot(coordinator) orelse return null;
    const restore = model.peer_restore[slot] orelse return null;
    if (restore.coordinator != coordinator or restore.shown.session != session) return null;
    model.peer_restore[slot] = null;
    return restore.shown.window;
}

/// Whether the slot's record may still be applied: it names the peer held
/// in that slot, the peer's list is of the server incarnation the record
/// was kept for, and it still carries that session, not as an empty session
/// (which has no tab to display; the server's EMPTY flag says so, as it does
/// for a pick in the switcher).
fn current(model: *const Model, slot: usize, restore: model_module.PeerRestore) bool {
    const peer = model.phuxPeerAtConst(slot) orelse return false;
    if (peer.providerId() != restore.coordinator) return false;
    const server = peer.serverId() orelse return false;
    if (remote_memory.serverHash(server) != restore.shown.server) return false;
    for (peer.standbyCatalog()) |entry| {
        if (entry.id == restore.shown.session) return !entry.empty;
    }
    return false;
}

fn drop(model: *Model, slot: usize) bool {
    model.peer_restore[slot] = null;
    return false;
}

fn showFront(engine: anytype, fx: anytype, slot: usize) bool {
    const model = engine.model;
    const restore = &(model.peer_restore[slot].?);
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
    // The cell a remote pane is sized with (workspace_projection's own
    // estimate for panes no local session has measured).
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
    var next: [remote_memory.max_hosts]?Shown = @splat(null);
    var kept: [remote_memory.max_hosts]bool = @splat(false);
    var live_front = false;
    for (0..hosts.count) |index| {
        const id = support.PhuxProvider.coordinatorId(.{ .remote = .{ .target = hosts.get(index) } });
        kept[index] = unsettled(model, id);
        next[index] = if (kept[index]) hosts.shown[index] else liveRecord(model, id);
        if (kept[index]) continue;
        if (next[index]) |record| {
            if (record.front) live_front = true;
        }
    }
    // At most one record is front. A remembered host's tab in front now
    // outranks a loaded front record not shown yet (another host was chosen,
    // or the config made a remembered host active), so the file never names
    // two.
    for (0..hosts.count) |index| {
        if (!live_front or !kept[index]) continue;
        if (next[index]) |record| {
            if (record.front) next[index] = null;
        }
    }
    var changed = false;
    for (0..hosts.count) |index| changed = hosts.setShown(index, next[index]) or changed;
    return changed;
}

/// Whether what coordinator `id` shows is not settled yet: a launch restore
/// not judged, or a retarget in flight. Its loaded record is kept then, so a
/// quit before its host lists loses nothing.
fn unsettled(model: *const Model, id: support.ProviderId) bool {
    for (model.peer_restore) |value| {
        const restore = value orelse continue;
        if (restore.coordinator == id and restore.pending) return true;
    }
    if (model.phuxConst()) |active| if (active.pending_retarget != null) return true;
    return false;
}

/// What coordinator `id` shows now: its session and its tab on screen.
fn liveRecord(model: *const Model, id: support.ProviderId) ?Shown {
    const showing = showingSession(model, id) orelse return null;
    const tab = selectedTab(model, id) orelse return null;
    return .{ .session = showing.session, .server = remote_memory.serverHash(showing.server), .window = tab.window, .front = tab.front };
}

const Showing = struct { session: u32, server: []const u8 };

/// The session coordinator `id` is attached to and displaying: the active
/// coordinator's own, or a showing peer's projected one.
fn showingSession(model: *const Model, id: support.ProviderId) ?Showing {
    if (model.phuxConst()) |active| if (active.providerId() == id) {
        if (active.state() != .attached) return null;
        return .{ .session = active.selectedSessionId() orelse return null, .server = active.serverId() orelse return null };
    };
    const slot = model.peerSlot(id) orelse return null;
    const peer = model.phuxPeerAtConst(slot) orelse return null;
    if (!peer.showing() or peer.state() != .attached) return null;
    const session = model.peer_workspaces[slot].session;
    if (session == 0) return null;
    return .{ .session = session, .server = peer.serverId() orelse return null };
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
