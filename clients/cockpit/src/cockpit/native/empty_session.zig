//! Keep-empty sessions (ADR-0105) in Cockpit: the Empty session state
//! (docs/REMOTE_HOSTS.md, "Empty sessions").
//!
//! A keep-empty session with no windows is a real session, not a broken one.
//! A window shows the Empty session state, with New Tab, for one of two:
//!
//! - the window's exact attached session, when it is empty and the
//!   window holds no tab at all;
//! - an exact empty session pick (`Model.empty_picks`), in
//!   the window it was picked in.
//!
//! Bound picks follow their independent attachment through connection and
//! projection. New Tab uses the Engine's exact-provider, exact-window callback.
//! The legacy unqualified pick remains only for older fixtures/callers.
//! A bound empty view holds its own attachment even without a pending tab.
//! Failure withdraws the pending spawn while retaining the named view for
//! reconnect. An empty session has no panes, so its attach sizes none.
//!
//! Runtime order per drain of one exact attachment, primary or peer:
//! `pumpAttachment` (queue a ready first tab), then `settleAttachment` with
//! that source's first-tab outcomes and whether the drain adopted a session
//! list. Only the current connection's list, or a creation receipt noted on
//! it (`noteCreationReceipt`), makes a bound view available.

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
    attachment_id: u64 = 0,
    window_epoch: u64 = 0,
    coordinator: support.ProviderId,
    session: u32,
    name: []const u8,
    host: []const u8,
    /// Picked in the switcher (a peer's session), rather than the active
    /// coordinator's own empty session under an empty window.
    picked: bool,
    /// New Tab was pressed and the tab has not landed yet.
    opening: bool = false,
    unavailable: bool = false,
};

/// The empty session window `index` shows, if any.
pub fn view(model: *const Model, index: usize) ?View {
    if (comptime !support.phux_enabled) return null;
    if (!model.windowOpen(index)) return null;
    if (model.empty_picks[index]) |*pick| return boundPickView(model, pick, index);
    if (qualifiedLegacyPick(model, index)) return null;
    if (legacyPick(model, index)) |pick| if (pickedView(model, pick)) |picked| return picked;
    return attachedView(model, index);
}

fn attachedView(model: *const Model, index: usize) ?View {
    const workspace = model.wsAtConst(index) orelse return null;
    if (workspace.tab_count != 0) return null;
    const active = model.phuxForWindowConst(index) orelse return null;
    if (active.state() != .attached) return null;
    const id = active.selectedSessionId() orelse return null;
    const entry = findEmpty(active.sessionCatalog(), id) orelse return null;
    return .{ .attachment_id = if (model.window_attachments[index] != null) active.context_id else 0, .window_epoch = model.window_epochs[index], .coordinator = active.providerId(), .session = id, .name = entry.name, .host = active.remoteLabel() orelse "This Mac", .picked = false };
}

fn legacyPick(model: *const Model, window: usize) ?*const EmptyPick {
    const pick = if (model.empty_pick) |*value| value else return null;
    if (pick.attachment_id != 0 or pick.window != window) return null;
    return pick;
}

fn qualifiedLegacyPick(model: *const Model, window: usize) bool {
    const pick = model.empty_pick orelse return false;
    return pick.window == window and pick.attachment_id != 0;
}

fn boundRemote(model: *const Model, pick: *const EmptyPick, window: usize) ?*const support.PhuxProvider {
    if (pick.attachment_id == 0 or pick.window != window) return null;
    const target = @import("ts_window_navigation.zig").Target{ .window = @intCast(window), .epoch = pick.window_epoch };
    if (!target.validWindow(model)) return null;
    const remote = model.phuxForAttachmentConst(pick.attachment_id) orelse return null;
    if (remote.providerId() != pick.coordinator or remote.pending_retarget != null) return null;
    if (model.window_attachments[window]) |binding| {
        if (binding.id != pick.attachment_id or binding.epoch != pick.window_epoch) return null;
    }
    return remote;
}

fn boundPickView(model: *const Model, pick: *const EmptyPick, window: usize) ?View {
    const remote = boundRemote(model, pick, window) orelse return null;
    if (windowHoldsTab(model, window)) return null;
    if (remote.state() == .attached and remote.selectedSessionId() != pick.session) return null;
    const name = boundName(remote, pick) orelse return null;
    return .{ .attachment_id = pick.attachment_id, .window_epoch = pick.window_epoch, .coordinator = pick.coordinator, .session = pick.session, .name = name.text, .host = remote.remoteLabel() orelse "This Mac", .picked = true, .opening = pick.tab_requested, .unavailable = !connected(remote) or !name.vouched };
}

fn windowHoldsTab(model: *const Model, window: usize) bool {
    const ws = model.wsAtConst(window) orelse return true;
    return ws.tab_count != 0;
}

fn connected(remote: *const support.PhuxProvider) bool {
    return remote.state() == .attached or remote.state() == .negotiated;
}

/// The attachment holds a session list from its present connection. A list
/// from an earlier connection says nothing about what exists now.
fn catalogCurrent(remote: *const support.PhuxProvider) bool {
    return remote.host.sessions_generation == remote.connectionEpoch();
}

/// A pick's display name, and whether the attachment's present connection
/// vouches for the session. Only a vouched name may offer New Tab; a name
/// retained from an earlier list still displays, as unavailable, so a
/// reconnect cannot queue a first tab for a session that may be gone.
const Name = struct { text: []const u8, vouched: bool };

fn boundName(remote: *const support.PhuxProvider, pick: *const EmptyPick) ?Name {
    if (catalogCurrent(remote)) {
        if (findListed(remote.sessionCatalog(), pick.session)) |entry| {
            return if (entry.empty) .{ .text = entry.name, .vouched = true } else null;
        }
        // The list omits it. Only an unexpired creation receipt from this
        // very connection outranks that: the list may predate the create.
        if (receiptLive(remote, pick)) return retainedName(pick, true);
        if (connected(remote)) return null;
    }
    return retainedName(pick, false);
}

fn retainedName(pick: *const EmptyPick, vouched: bool) ?Name {
    if (pick.name_len == 0) return null;
    return .{ .text = pick.nameSlice(), .vouched = vouched };
}

fn findListed(catalog: anytype, id: u32) ?*const @typeInfo(@TypeOf(catalog)).pointer.child {
    for (catalog) |*entry| if (entry.id == id) return entry;
    return null;
}

/// `created` is a receipt from the attachment's connection that was current
/// when it was noted (noteCreationReceipt). It stands in for a list only
/// while that list is current and the connection still up; settleAttachment
/// and forgetAttachment consume it on the first later list or disconnect.
fn receiptLive(remote: *const support.PhuxProvider, pick: *const EmptyPick) bool {
    return pick.created and connected(remote) and catalogCurrent(remote);
}

fn pickedView(model: *const Model, pick: *const EmptyPick) ?View {
    if (pick.attachment_id != 0) return null;
    const slot = legacyPeerSlot(model, pick.coordinator) orelse return null;
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
    const slot = legacyPeerSlot(model, coordinator) orelse return false;
    return findEmpty(model.phuxPeerAtConst(slot).?.standbyCatalog(), id) != null;
}

/// A peer's empty session was picked: show the Empty session state in the
/// active window instead of attaching it. False when the session is not an
/// empty one, so the caller shows it as ever.
pub fn pickPeer(model: *Model, coordinator: support.ProviderId, id: u32) bool {
    if (!peerSessionEmpty(model, coordinator, id)) return false;
    const slot = legacyPeerSlot(model, coordinator).?;
    const entry = findEmpty(model.phuxPeerAt(slot).?.standbyCatalog(), id).?;
    var pick: EmptyPick = .{ .coordinator = coordinator, .session = id, .window = model.active_window };
    pick.setName(entry.name);
    model.empty_pick = pick;
    return true;
}

fn legacyPeerSlot(model: *const Model, coordinator: support.ProviderId) ?usize {
    var found: ?usize = null;
    for (model.peers.items, 0..) |entry, slot| {
        const remote = entry.provider orelse continue;
        if (remote.providerId() != coordinator) continue;
        if (found != null) return null;
        found = slot;
    }
    return found;
}

pub const Opened = union(enum) { opened: View, refused: []const u8 };

/// New Tab in the active window's exact Empty session state. Only legacy
/// unqualified picks may redirect to their owning window and restart a peer.
pub fn newTab(engine: anytype, fx: anytype) Opened {
    const model = engine.model;
    const index = windowShowing(model) orelse return .{ .refused = "No empty session is on screen." };
    const current = view(model, index).?;
    if (current.opening) return .{ .refused = "A new tab is already opening there." };
    if (current.attachment_id != 0) return openBound(engine, index, current);
    model.active_window = index;
    if (!current.picked) {
        if (!engine.openTabAt("", null)) return .{ .refused = "Cockpit could not open a new tab in that session." };
        return .{ .opened = current };
    }
    return openPickedPeer(engine, fx, index, current);
}

fn openBound(engine: anytype, window: usize, shown: View) Opened {
    const model = engine.model;
    const remote = model.phuxForAttachment(shown.attachment_id) orelse return .{ .refused = "That session attachment is no longer available." };
    if (shown.unavailable) return .{ .refused = unavailableReason(remote) };
    var pick = model.empty_picks[window] orelse EmptyPick{ .coordinator = shown.coordinator, .session = shown.session, .window = window, .attachment_id = shown.attachment_id, .window_epoch = shown.window_epoch };
    pick.tab_requested = true;
    pick.setName(shown.name);
    model.empty_picks[window] = pick;
    if (boundReady(model, remote, &pick)) {
        queueBound(engine, remote, &model.empty_picks[window].?) catch {
            model.empty_picks[window].?.tab_requested = false;
            return .{ .refused = "Cockpit could not open a tab in that exact session." };
        };
    }
    var opened = shown;
    opened.opening = true;
    return .{ .opened = opened };
}

fn unavailableReason(remote: *const support.PhuxProvider) []const u8 {
    if (!connected(remote)) return "That session is disconnected. Reconnect its machine before opening a tab.";
    return "That session is not confirmed on its machine yet. Wait for its session list.";
}

fn queueBound(engine: anytype, remote: *support.PhuxProvider, pick: *EmptyPick) !void {
    if (comptime !@hasDecl(@TypeOf(engine.*), "openPeerTabFromInWindow")) return error.Unsupported;
    const selected = boundRemote(engine.model, pick, pick.window) orelse return error.StaleWindow;
    if (selected != remote or remote.selectedSessionId() != pick.session) return error.StaleSession;
    const may_focus = engine.model.active_window == pick.window;
    try engine.openPeerTabFromInWindow(remote, pick.window, pick.window_epoch, "", may_focus);
    pick.tab_queued = true;
}

fn openPickedPeer(engine: anytype, fx: anytype, index: usize, current: View) Opened {
    const model = engine.model;
    const Fx = switch (@typeInfo(@TypeOf(fx))) {
        .pointer => |pointer| pointer.child,
        else => @TypeOf(fx),
    };
    if (comptime !@hasDecl(Fx, "restartPeer")) return .{ .refused = "Cockpit could not show that session." };
    const slot = legacyPeerSlot(model, current.coordinator) orelse return .{ .refused = "That host is no longer connected." };
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
    // The old singleton could be summoned from anywhere. Bound windows never
    // redirect New Tab away from the invoking window.
    if (model.empty_pick) |pick| {
        if (pick.attachment_id == 0 and view(model, pick.window) != null) return pick.window;
    }
    return null;
}

/// Dismiss a picked empty session's state.
pub fn dismiss(model: *Model) void {
    dismissWindow(model, model.active_window);
}

pub fn dismissWindow(model: *Model, window: usize) void {
    if (window >= model_module.max_windows) return;
    if (model.empty_picks[window]) |pick| {
        if (!pick.tab_requested) model.empty_picks[window] = null;
    }
    if (legacyPick(model, window)) |pick| if (!pick.tab_requested) {
        model.empty_pick = null;
    };
}

/// A showing peer's wake, after its projection: once the picked session is
/// attached and projected, spawn its first tab on that peer. True when a
/// spawn was queued or refused.
pub fn pump(engine: anytype, slot: usize) bool {
    const peer = engine.model.phuxPeerAt(slot) orelse return false;
    const changed = pumpAttachment(engine, peer);
    const legacy_changed = pumpLegacy(engine, slot);
    return changed or legacy_changed;
}

/// After an exact provider's projection, advance only its captured windows.
/// The primary provider has no peer slot and uses this hook directly.
pub fn pumpAttachment(engine: anytype, remote: *support.PhuxProvider) bool {
    var changed = false;
    for (0..model_module.max_windows) |window| {
        changed = pumpBound(engine, remote, window) or changed;
    }
    return changed;
}

fn pumpBound(engine: anytype, remote: *support.PhuxProvider, window: usize) bool {
    const cell = &engine.model.empty_picks[window];
    const pick = if (cell.*) |*value| value else return false;
    if (pick.attachment_id != remote.context_id) return false;
    if (boundRemote(engine.model, pick, window) == null) {
        cell.* = null;
        return true;
    }
    if (!pick.tab_requested or pick.tab_queued) return false;
    if (!boundReady(engine.model, remote, pick)) return false;
    queueBound(engine, remote, pick) catch {
        pick.tab_requested = false;
        pick.tab_queued = false;
    };
    return true;
}

fn boundReady(model: *Model, remote: *const support.PhuxProvider, pick: *const EmptyPick) bool {
    if (remote.state() != .attached or remote.selectedSessionId() != pick.session) return false;
    const state = model.sharedWorkspaceForAttachment(remote.context_id) orelse return false;
    return state.attachment_id == pick.attachment_id and state.session == pick.session and state.epoch == remote.connectionEpoch();
}

fn pumpLegacy(engine: anytype, slot: usize) bool {
    const model = engine.model;
    const pick = if (model.empty_pick) |*value| value else return false;
    if (pick.attachment_id != 0) return false;
    if (!pick.tab_requested or pick.tab_queued) return false;
    if (legacyPeerSlot(model, pick.coordinator) != slot) return false;
    const peer = model.phuxPeerAt(slot) orelse return false;
    if (!legacyReady(model, slot, peer, pick)) return false;
    if (engine.openPeerTabAt(pick.coordinator, "", null)) {
        model.empty_pick.?.tab_queued = true;
    } else {
        // Refused: stop holding the peer; it returns to listing.
        model.empty_pick = null;
    }
    return true;
}

fn legacyReady(model: *Model, slot: usize, peer: *const support.PhuxProvider, pick: *const EmptyPick) bool {
    return peer.selectedSessionId() == pick.session and model.peers.items[slot].workspace.session == pick.session and peer_edits.editable(model, pick.coordinator);
}

/// Whether this exact peer owns a bound empty view or a legacy first-tab hold.
/// A bound view's opening flags settle only in settleAttachment, from the
/// creation's own outcome: `pending_creations` reaching zero is queue absence,
/// not an outcome, and serves the legacy singleton alone. The caller
/// separately accounts for visible tabs.
pub fn holds(model: *Model, slot: usize, pending_creations: usize, visible: bool) bool {
    const remote = model.phuxPeerAt(slot) orelse return false;
    var held = false;
    for (0..model_module.max_windows) |window| {
        held = holdsBound(model, remote, window) or held;
    }
    const legacy_held = holdsLegacy(model, slot, pending_creations, visible);
    return held or legacy_held;
}

fn holdsBound(model: *Model, remote: *const support.PhuxProvider, window: usize) bool {
    const binding = model.window_attachments[window] orelse return false;
    if (binding.id != remote.context_id) return false;
    const shown = view(model, window) orelse {
        model.empty_picks[window] = null;
        return false;
    };
    return shown.attachment_id == remote.context_id and !shown.unavailable;
}

fn holdsLegacy(model: *Model, slot: usize, pending_creations: usize, visible: bool) bool {
    const pick = model.empty_pick orelse return false;
    if (pick.attachment_id != 0) return false;
    if (!pick.tab_requested) return false;
    if (legacyPeerSlot(model, pick.coordinator) != slot) return false;
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
    if (pick.attachment_id != 0) return;
    if (pick.coordinator != coordinator) return;
    if (dropped or pick.tab_requested) model.empty_pick = null;
}

/// Exact attachment failure/retirement. A sibling on the same machine keeps
/// its own display and pending first-tab request. Disconnect withdraws pending
/// spawn intent and the connection's creation receipt, but retains the named
/// empty session, as unavailable, until a later connection lists it.
pub fn forgetAttachment(model: *Model, attachment_id: u64, dropped: bool) void {
    if (attachment_id == 0) return;
    for (&model.empty_picks) |*cell| {
        const pick = if (cell.*) |*value| value else continue;
        if (pick.attachment_id != attachment_id) continue;
        if (dropped) {
            cell.* = null;
        } else {
            pick.tab_requested = false;
            pick.tab_queued = false;
            pick.created = false;
        }
    }
}

/// A creation receipt for `session`, from exactly `remote` on connection
/// `connection_epoch` (the epoch the create was sent and answered on). The
/// window must already be bound to that attachment. The receipt lets the
/// Empty session state show before any list names the new session; it is
/// refused when that connection is no longer current, so a receipt can
/// never authorize a reconnected Client.
pub const Receipt = struct { session: u32, name: []const u8, connection_epoch: u64 };

pub fn noteCreationReceipt(model: *Model, remote: *const support.PhuxProvider, window: usize, receipt: Receipt) bool {
    if (comptime !support.phux_enabled) return false;
    if (!receiptCurrent(remote, receipt)) return false;
    const window_epoch = boundWindowEpoch(model, window, remote) orelse return false;
    // A first tab already on its way keeps its pick; the receipt waits.
    if (model.empty_picks[window]) |held| if (held.tab_requested) return false;
    var pick: EmptyPick = .{ .attachment_id = remote.context_id, .window_epoch = window_epoch, .coordinator = remote.providerId(), .session = receipt.session, .window = window, .created = true };
    pick.setName(receipt.name);
    model.empty_picks[window] = pick;
    return true;
}

fn receiptCurrent(remote: *const support.PhuxProvider, receipt: Receipt) bool {
    return receipt.connection_epoch == remote.connectionEpoch() and connected(remote);
}

/// The epoch of `window`'s binding, when that binding is exactly `remote`.
fn boundWindowEpoch(model: *const Model, window: usize, remote: *const support.PhuxProvider) ?u64 {
    if (window >= model_module.max_windows) return null;
    const binding = model.window_attachments[window] orelse return null;
    return if (binding.id == remote.context_id) binding.epoch else null;
}

/// The outcome of one first-tab creation the engine's pending creation for
/// an exact source holds, keyed by the destination it captured.
pub const FirstTab = struct {
    window: usize,
    window_epoch: u64,
    connection_epoch: u64,
    outcome: Outcome,

    pub const Outcome = enum {
        /// Spawning or placing, or its reply not yet reconciled.
        pending,
        /// Refused or failed on that connection: New Tab may be pressed again.
        refused,
        /// Placed. Its projected tab retires the pick.
        placed,
    };
};

/// What one drain of an exact source proved. An empty `first_tabs` proves
/// nothing about any creation, so opening flags stay as they are.
pub const Evidence = struct {
    first_tabs: []const FirstTab = &.{},
    /// This drain adopted a session list (`SyncDelta.sessions_listed`).
    catalog_listed: bool = false,
};

/// After pumping `remote` and projecting it, settle every window bound to
/// exactly that attachment: primary and peer alike, since the primary has
/// no peer slot and so never reaches `holds`. A projected tab or a list
/// without the session retires the pick; a refused or placed creation clears
/// New Tab's opening state; a later list or a lost connection consumes the
/// creation receipt. True when anything shown may have changed.
pub fn settleAttachment(model: *Model, remote: *const support.PhuxProvider, evidence: Evidence) bool {
    if (comptime !support.phux_enabled) return false;
    var changed = false;
    for (0..model_module.max_windows) |window| {
        changed = settleWindow(model, remote, window, evidence) or changed;
    }
    return changed;
}

fn settleWindow(model: *Model, remote: *const support.PhuxProvider, window: usize, evidence: Evidence) bool {
    const cell = &model.empty_picks[window];
    const pick = if (cell.*) |*value| value else return false;
    if (pick.attachment_id != remote.context_id) return false;
    const expired = expireReceipt(remote, pick, evidence.catalog_listed);
    if (pickRetired(model, remote, pick, window)) {
        cell.* = null;
        return true;
    }
    return settleFirstTab(remote, pick, evidence.first_tabs) or expired;
}

fn expireReceipt(remote: *const support.PhuxProvider, pick: *EmptyPick, listed: bool) bool {
    if (!pick.created) return false;
    if (!listed and receiptLive(remote, pick)) return false;
    pick.created = false;
    return true;
}

/// A pick drives only an empty window. Once a tab is projected there its
/// opening flags must not return when the window is empty again, and once
/// the present list lacks the session there is nothing left to open.
fn pickRetired(model: *const Model, remote: *const support.PhuxProvider, pick: *const EmptyPick, window: usize) bool {
    if (boundRemote(model, pick, window) == null) return true;
    if (windowHoldsTab(model, window)) return true;
    return boundName(remote, pick) == null;
}

fn settleFirstTab(remote: *const support.PhuxProvider, pick: *EmptyPick, first_tabs: []const FirstTab) bool {
    if (!pick.tab_queued) return false;
    const outcome = firstTabOutcome(remote, pick, first_tabs) orelse return false;
    if (outcome == .pending) return false;
    pick.tab_requested = false;
    pick.tab_queued = false;
    return true;
}

fn firstTabOutcome(remote: *const support.PhuxProvider, pick: *const EmptyPick, first_tabs: []const FirstTab) ?FirstTab.Outcome {
    for (first_tabs) |entry| {
        if (entry.window != pick.window or entry.window_epoch != pick.window_epoch) continue;
        if (entry.connection_epoch != remote.connectionEpoch()) continue;
        return entry.outcome;
    }
    return null;
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
