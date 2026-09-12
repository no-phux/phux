//! Keep-empty sessions (ADR-0105) in Cockpit: the Empty session state
//! (docs/REMOTE_HOSTS.md, "Empty sessions").
//!
//! A keep-empty session with no windows is a real session, not a broken one.
//! A window shows the Empty session state, with New Tab, for one of two:
//!
//! - the active coordinator's attached session, when it is empty and the
//!   window holds no tab at all;
//! - a peer's empty session picked in the switcher (`Model.empty_pick`), in
//!   the window it was picked in.
//!
//! Picking a peer's empty session never attaches it: a peer attaches only
//! what it displays, and an empty session has no tab to display. New Tab is
//! what shows it. The peer then attaches that session by id on its own
//! connection, and once its (empty) workspace projects, a new tab is spawned
//! there through its own edit queue, never through another coordinator's.
//! Until that tab lands the peer is held shown; if the spawn is refused or
//! the peer's connection ends, the hold ends and the peer returns to listing
//! as any hidden peer does. The session has no panes while it is held, so
//! the attach sizes nobody's panes.

const std = @import("std");
const support = @import("../phux_support.zig");
const model_module = @import("../model.zig");
const projection = @import("workspace_projection.zig");
const navigation = @import("ts_navigation.zig");
const peer_edits = @import("../peer_edits.zig");

const Model = model_module.Model;
const EmptyPick = model_module.EmptyPick;

/// Display bounds for the snapshot record (ts_snapshot.zig).
pub const max_name_bytes: usize = 64;
pub const max_host_bytes: usize = 64;
/// Kind, length, window mask, flags, and the two length-prefixed texts.
pub const record_bytes: usize = 3 + 2 + 2 + max_name_bytes + max_host_bytes;

pub const View = struct {
    coordinator: support.ProviderId,
    session: u32,
    name: []const u8,
    host: []const u8,
    /// Picked in the switcher (a peer's session), rather than the active
    /// coordinator's own empty session under an empty window.
    picked: bool,
    /// New Tab was pressed and the tab has not landed yet.
    opening: bool = false,
};

/// The empty session window `index` shows, if any.
pub fn view(model: *const Model, index: usize) ?View {
    if (comptime !support.phux_enabled) return null;
    if (!model.windowOpen(index)) return null;
    if (model.empty_pick) |*pick| {
        if (pick.window == index) if (pickedView(model, pick)) |picked| return picked;
    }
    const workspace = model.wsAtConst(index) orelse return null;
    if (workspace.tab_count != 0) return null;
    const active = model.phuxConst() orelse return null;
    if (active.state() != .attached) return null;
    const id = active.selectedSessionId() orelse return null;
    const entry = findEmpty(active.sessionCatalog(), id) orelse return null;
    return .{ .coordinator = active.providerId(), .session = id, .name = entry.name, .host = active.remoteLabel() orelse "This Mac", .picked = false };
}

fn pickedView(model: *const Model, pick: *const EmptyPick) ?View {
    const slot = model.peerSlot(pick.coordinator) orelse return null;
    const host = projection.peerHostLabel(model, pick.coordinator);
    if (pick.tab_requested) return .{ .coordinator = pick.coordinator, .session = pick.session, .name = pick.nameSlice(), .host = host, .picked = true, .opening = true };
    // Before New Tab it lists: shown only while its list still says empty.
    const peer = model.phuxPeerAtConst(slot).?;
    const entry = findEmpty(peer.standbyCatalog(), pick.session) orelse return null;
    return .{ .coordinator = pick.coordinator, .session = pick.session, .name = entry.name, .host = host, .picked = true };
}

fn findEmpty(catalog: anytype, id: u32) ?*const @typeInfo(@TypeOf(catalog)).pointer.child {
    for (catalog) |*entry| if (entry.id == id and entry.empty) return entry;
    return null;
}

/// Whether coordinator `coordinator` lists session `id` as empty.
pub fn peerSessionEmpty(model: *const Model, coordinator: support.ProviderId, id: u32) bool {
    if (comptime !support.phux_enabled) return false;
    const slot = model.peerSlot(coordinator) orelse return false;
    return findEmpty(model.phuxPeerAtConst(slot).?.standbyCatalog(), id) != null;
}

/// A peer's empty session was picked: show the Empty session state in the
/// active window instead of attaching it. False when the session is not an
/// empty one, so the caller shows it as ever.
pub fn pickPeer(model: *Model, coordinator: support.ProviderId, id: u32) bool {
    if (!peerSessionEmpty(model, coordinator, id)) return false;
    const slot = model.peerSlot(coordinator).?;
    const entry = findEmpty(model.phuxPeerAt(slot).?.standbyCatalog(), id).?;
    var pick: EmptyPick = .{ .coordinator = coordinator, .session = id, .window = model.active_window };
    pick.setName(entry.name);
    model.empty_pick = pick;
    return true;
}

pub const Opened = union(enum) { opened: View, refused: []const u8 };

/// New Tab in the Empty session state of the active window (else the first
/// window showing one). The active coordinator's own empty session takes an
/// ordinary new tab there. A peer's is shown: its connection restarts to
/// attach that session, and `pump` spawns the tab once it projects.
pub fn newTab(engine: anytype, fx: anytype) Opened {
    const model = engine.model;
    const index = windowShowing(model) orelse return .{ .refused = "No empty session is on screen." };
    const current = view(model, index).?;
    if (current.opening) return .{ .refused = "A new tab is already opening there." };
    model.active_window = index;
    if (!current.picked) {
        if (!engine.openTabAt("", null)) return .{ .refused = "Cockpit could not open a new tab in that session." };
        return .{ .opened = current };
    }
    return openPickedPeer(engine, fx, index, current);
}

fn openPickedPeer(engine: anytype, fx: anytype, index: usize, current: View) Opened {
    const model = engine.model;
    const Fx = switch (@typeInfo(@TypeOf(fx))) {
        .pointer => |pointer| pointer.child,
        else => @TypeOf(fx),
    };
    if (comptime !@hasDecl(Fx, "restartPeer")) return .{ .refused = "Cockpit could not show that session." };
    const slot = model.peerSlot(current.coordinator) orelse return .{ .refused = "That host is no longer connected." };
    const peer = model.peers.items[slot].provider.?;
    const state = &model.peers.items[slot].workspace;
    state.authority = peer.providerId();
    if (peer.showing()) state.leaveSession(model) catch return .{ .refused = "Cockpit could not show that session." };
    peer.show(current.session) catch return .{ .refused = "Cockpit could not show that session." };
    model.empty_pick.?.tab_requested = true;
    _ = fx.restartPeer(engine, slot);
    return .{ .opened = view(model, index).? };
}

fn windowShowing(model: *const Model) ?usize {
    if (view(model, model.active_window) != null) return model.active_window;
    for (0..model_module.max_windows) |index| if (view(model, index) != null) return index;
    return null;
}

/// Dismiss a picked empty session's state.
pub fn dismiss(model: *Model) void {
    if (model.empty_pick) |pick| if (!pick.tab_requested) {
        model.empty_pick = null;
    };
}

/// A showing peer's wake, after its projection: once the picked session is
/// attached and projected, spawn its first tab on that peer. True when a
/// spawn was queued or refused.
pub fn pump(engine: anytype, slot: usize) bool {
    const model = engine.model;
    const pick = if (model.empty_pick) |*value| value else return false;
    if (!pick.tab_requested or pick.tab_queued) return false;
    if (model.peerSlot(pick.coordinator) != slot) return false;
    const peer = model.phuxPeerAt(slot) orelse return false;
    if (peer.selectedSessionId() != pick.session) return false;
    if (model.peers.items[slot].workspace.session != pick.session) return false;
    if (!peer_edits.editable(model, pick.coordinator)) return false;
    if (engine.openPeerTabAt(pick.coordinator, "", null)) {
        model.empty_pick.?.tab_queued = true;
    } else {
        // Refused: stop holding the peer; it returns to listing.
        model.empty_pick = null;
    }
    return true;
}

/// Whether slot `slot`'s peer is held shown for a first tab that has not
/// landed yet. Settles the hold first: it ends once one of that peer's tabs
/// is on screen, or once its spawn was queued and is gone without one.
pub fn holds(model: *Model, slot: usize, pending_creations: usize, visible: bool) bool {
    const pick = model.empty_pick orelse return false;
    if (!pick.tab_requested) return false;
    if (model.peerSlot(pick.coordinator) != slot) return false;
    if (visible or (pick.tab_queued and pending_creations == 0)) {
        model.empty_pick = null;
        return false;
    }
    return true;
}

/// The peer the pick names failed, closed, or went: nothing is opening
/// there any more. A pick that only shows the state is kept, and reads
/// again when that peer lists.
pub fn forgetPeer(model: *Model, coordinator: support.ProviderId, dropped: bool) void {
    const pick = model.empty_pick orelse return;
    if (pick.coordinator != coordinator) return;
    if (dropped or pick.tab_requested) model.empty_pick = null;
}

/// The snapshot record: which windows show the Empty session state, whether
/// it was picked, and the first one's session name and host. Written only
/// when some window shows it, so every other snapshot is byte-identical.
pub fn encode(model: *const Model, kind: u8, out: []u8, start: usize) error{BufferTooSmall}!usize {
    var mask: u8 = 0;
    var first: ?View = null;
    for (0..model_module.max_windows) |index| {
        const current = view(model, index) orelse continue;
        mask |= @as(u8, 1) << @intCast(index);
        if (first == null) first = current;
    }
    const shown = first orelse return start;
    var name_buffer: [max_name_bytes]u8 = undefined;
    var host_buffer: [max_host_bytes]u8 = undefined;
    const name = navigation.displayText(shown.name, &name_buffer);
    const host = navigation.displayText(shown.host, &host_buffer);
    const payload = 4 + name.len + host.len;
    if (start + 3 + payload > out.len) return error.BufferTooSmall;
    out[start] = kind;
    std.mem.writeInt(u16, out[start + 1 ..][0..2], @intCast(payload), .little);
    var at = start + 3;
    out[at] = mask;
    out[at + 1] = (if (shown.picked) @as(u8, 1) else 0) | (if (shown.opening) @as(u8, 2) else 0);
    out[at + 2] = @intCast(name.len);
    @memcpy(out[at + 3 ..][0..name.len], name);
    at += 3 + name.len;
    out[at] = @intCast(host.len);
    @memcpy(out[at + 1 ..][0..host.len], host);
    return at + 1 + host.len;
}
