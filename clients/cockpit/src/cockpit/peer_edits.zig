//! Workspace edits on a coordinator shown beside the active one
//! (docs/REMOTE_HOSTS.md, "Several coordinators").
//!
//! A showing peer's tabs are that coordinator's shared workspace, projected
//! into Cockpit's windows. Every edit of one of them (close, split, reorder,
//! divider resize, a new tab while one of its panes is focused, Open Here on
//! its directory listing) goes to that coordinator and to no other. The
//! active coordinator's queues (`Model.shared_mutations`,
//! `Engine.creation`) never see it, so a peer's refusal never marks the
//! active coordinator's workspace refused.
//!
//! Each peer slot has its own `shared_mutations.Coordinator`. The queue reads
//! `phux()` and `shared_workspace` of whatever model it is handed; handing it
//! a `PeerView` instead of the Model keeps every check it makes (connection
//! epoch, session, revision, target, one edit at a time) and addresses only
//! that peer's provider and projection.
//!
//! New tabs and splits spawn first, then place: SPAWN on the peer, wait for
//! its terminal to publish live, then refresh-and-add (or split) through the
//! same queue. A confirmed placement asks the peer's projection to select the
//! new terminal. Nothing here outlives the peer's connection: a restart,
//! failure or removal forgets the slot's edits (`forget`).

const std = @import("std");
const support = @import("phux_support.zig");
const model_module = @import("model.zig");
const layout = @import("layout.zig");
const contract = @import("provider_contract");
const shared_mutations = @import("shared_mutations.zig");
const shared_workspace = @import("shared_workspace.zig");

const Model = model_module.Model;
const TerminalRef = contract.TerminalRef;
const Mutation = contract.workspace.Mutation;
const WindowId = shared_mutations.WindowId;
const max_peers = model_module.max_phux_peers;

pub const Kind = enum { tab, split_right, split_down };

/// Which terminal owns a spawn: the focused one (New Tab, a split), none (a
/// directory on the coordinator's own host), or an exact one (a satellite
/// directory, owned by the pane it was listed for). As in durable_creation.
pub const Owner = union(enum) { focused, none, terminal: TerminalRef };

/// Spawns in flight per peer.
pub const max_creations: usize = 4;

/// One showing peer as the shared-mutation queue sees a coordinator.
const PeerView = struct {
    peer: *support.PhuxProvider,
    shared_workspace: *shared_workspace.State,
    /// The queue reports a refusal here and on the projection. Only the
    /// projection's flag is shown, on the peer's switcher rows; this one is
    /// never the active coordinator's terminal limit.
    terminal_limit_refused: bool = false,

    pub fn phux(self: *PeerView) ?*support.PhuxProvider {
        return self.peer;
    }
};

const Stage = enum { spawning, attaching, publishing, placing };

const Creation = struct {
    coordinator: support.ProviderId,
    epoch: u64,
    session: u32,
    kind: Kind,
    stage: Stage = .spawning,
    request: u32,
    /// A split's pane and the shared window holding it.
    origin: ?TerminalRef = null,
    window_id: ?WindowId = null,
    terminal: ?TerminalRef = null,
    ticket: u64 = 0,
    /// Cleared by any later explicit selection: a slow placement must not
    /// take focus from wherever the user went meanwhile.
    may_focus: bool = true,
};

pub const Edits = struct {
    mutations: [max_peers]shared_mutations.Coordinator = @splat(.{}),
    creations: [max_peers][max_creations]?Creation = @splat(@splat(null)),

    /// The slot's connection ended or it now holds another coordinator:
    /// nothing queued for it may continue. A mutation already sent may still
    /// land on that server; forgetting it never rolls one back.
    pub fn forget(self: *Edits, slot: usize) void {
        if (slot >= max_peers) return;
        self.mutations[slot] = .{};
        self.creations[slot] = @splat(null);
    }

    pub fn pendingCreations(self: *const Edits, slot: usize) usize {
        var count: usize = 0;
        for (self.creations[slot]) |entry| {
            if (entry != null) count += 1;
        }
        return count;
    }

    /// A later explicit selection supersedes every pending placement's focus.
    pub fn supersedeFocus(self: *Edits) void {
        for (&self.creations) |*slot| for (slot) |*entry| {
            if (entry.*) |*value| value.may_focus = false;
        };
    }

    /// Close one of the peer's tabs: the layout-only window removal the
    /// active coordinator's tabs send to theirs. Its terminals keep running.
    pub fn removeWindow(self: *Edits, model: *Model, coordinator: support.ProviderId, id: WindowId) !void {
        const slot = try slotOf(model, coordinator);
        var peer_view = try view(model, slot);
        try self.mutations[slot].requestRemoveWindow(&peer_view, id);
    }

    /// Close one of the peer's panes on that coordinator.
    pub fn removePane(self: *Edits, model: *Model, ref: TerminalRef) !void {
        const slot = try slotOf(model, ref.provider_id);
        var peer_view = try view(model, slot);
        try self.mutations[slot].requestRemove(&peer_view, ref);
    }

    /// Move the peer's window `id` one step left or right in its own order.
    pub fn reorder(self: *Edits, model: *Model, coordinator: support.ProviderId, id: WindowId, right: bool) !void {
        const slot = try slotOf(model, coordinator);
        var peer_view = try view(model, slot);
        const target = neighborIndex(peer_view.peer.workspaceSnapshot().windows, id, right) orelse return error.StaleTarget;
        try self.mutations[slot].requestReorder(&peer_view, id, target);
    }

    /// Commit a divider drag on the peer's window `id`. The path was taken
    /// from the projection of the revision the drag started on; the queue
    /// refuses it against any other revision rather than rebasing it.
    pub fn resize(self: *Edits, model: *Model, coordinator: support.ProviderId, id: WindowId, path_bits: u64, path_len: u32, ratio: f32) !void {
        const slot = try slotOf(model, coordinator);
        var peer_view = try view(model, slot);
        try self.mutations[slot].requestResize(&peer_view, id, path_bits, path_len, ratio);
    }

    /// Spawn a terminal on the peer and place it as a new tab or a split of
    /// the focused pane. `cwd` empty: the serving host's default directory.
    /// Returns the queued creation so the caller can supersede every other
    /// pending focus without losing this one's.
    pub fn create(self: *Edits, model: *Model, coordinator: support.ProviderId, kind: Kind, cwd: []const u8, owner_choice: Owner) !*Creation {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const slot = try slotOf(model, coordinator);
        const peer_view = try view(model, slot);
        const peer = peer_view.peer;
        const state = peer_view.shared_workspace;
        const snapshot = peer.workspaceSnapshot();
        if (snapshot.session_id != state.session) return error.StaleContext;
        if (snapshot.state == .unavailable or snapshot.state == .last_good_error) return error.WorkspaceUnavailable;
        if (!model.canAddPane()) return error.TerminalCapacity;
        const free = self.vacant(slot) orelse return error.OperationCapacity;
        var entry: Creation = .{ .coordinator = coordinator, .epoch = peer.connectionEpoch(), .session = state.session, .kind = kind, .request = 0 };
        if (kind != .tab) try self.prepareSplit(model, slot, snapshot, &entry);
        const owner = try spawnOwner(model, coordinator, kind, owner_choice);
        const viewport = if (owner) |ref| peer.lastViewport(ref) orelse peer.attach_viewport else peer.attach_viewport;
        entry.request = try peer.requestSpawnIn(owner, viewport, cwd);
        free.* = entry;
        return &free.*.?;
    }

    fn vacant(self: *Edits, slot: usize) ?*?Creation {
        for (&self.creations[slot]) |*entry| if (entry.* == null) return entry;
        return null;
    }

    /// A split names the focused pane, which must be this peer's, and the
    /// shared window the peer publishes it in.
    fn prepareSplit(self: *const Edits, model: *Model, slot: usize, snapshot: contract.workspace.Snapshot, entry: *Creation) !void {
        const origin = model.focusedTerminalRef() orelse return error.InvalidDestination;
        if (origin.provider_id != entry.coordinator) return error.ForeignCoordinator;
        const tree = model.selectedTree() orelse return error.InvalidDestination;
        if (tree.paneCount() + self.pendingSplits(slot) >= layout.max_panes) return error.PaneCapacity;
        entry.origin = origin;
        entry.window_id = shared_mutations.terminalWindow(snapshot, origin) orelse return error.StaleTarget;
    }

    fn pendingSplits(self: *const Edits, slot: usize) usize {
        var count: usize = 0;
        for (self.creations[slot]) |value| {
            const entry = value orelse continue;
            if (entry.kind != .tab) count += 1;
        }
        return count;
    }

    /// One operation result from the peer's connection. True when it
    /// answered one of this slot's spawns or their follow-up attach.
    pub fn complete(self: *Edits, model: *Model, slot: usize, result: support.OperationResult) bool {
        if (comptime !support.phux_enabled) return false;
        if (slot >= max_peers) return false;
        for (&self.creations[slot]) |*held| {
            const entry = if (held.*) |*value| value else continue;
            if (entry.stage != .spawning and entry.stage != .attaching) continue;
            if (entry.request != result.request_id or entry.epoch != result.connection_epoch) continue;
            accept(model, slot, entry, result) catch {
                held.* = null;
            };
            return true;
        }
        return false;
    }

    /// Advance the slot's queue and its placements; true when one settled.
    /// Called on every showing peer's wake, before its projection applies,
    /// so a confirmed placement's selection lands with the tab.
    pub fn pump(self: *Edits, model: *Model, slot: usize) bool {
        if (comptime !support.phux_enabled) return false;
        var peer_view = view(model, slot) catch return false;
        var changed = self.mutations[slot].pump(&peer_view);
        for (&self.creations[slot]) |*held| {
            const entry = if (held.*) |*value| value else continue;
            if (entry.epoch != peer_view.peer.connectionEpoch() or entry.session != peer_view.shared_workspace.session) {
                held.* = null;
                changed = true;
                continue;
            }
            changed = self.advance(&peer_view, slot, held) or changed;
        }
        return changed;
    }

    fn advance(self: *Edits, peer_view: *PeerView, slot: usize, held: *?Creation) bool {
        const entry = &held.*.?;
        switch (entry.stage) {
            .spawning, .attaching => return false,
            .publishing => {
                const outcome = self.place(peer_view, slot, entry) catch {
                    held.* = null;
                    return true;
                };
                return outcome;
            },
            .placing => {
                const completion = self.mutations[slot].takeCreationCompletion(entry.ticket) orelse return false;
                if (completion.outcome == .confirmed and entry.may_focus) peer_view.shared_workspace.desired_terminal = entry.terminal;
                held.* = null;
                return true;
            },
        }
    }

    /// Once the spawned terminal publishes live, ask the peer to place it:
    /// a new window, or beside the pane it split.
    fn place(self: *Edits, peer_view: *PeerView, slot: usize, entry: *Creation) !bool {
        const peer = peer_view.peer;
        const ref = entry.terminal orelse return error.MissingIdentity;
        if (!peer.terminalKnown(ref)) return error.TerminalLost;
        const presentation = peer.presentation(ref) orelse return false;
        switch (presentation.phase) {
            .tombstoned, .ended, .failed => return error.TerminalLost,
            .live => {},
            else => return false,
        }
        const mutation = try placement(entry.*, ref, peer.workspaceSnapshot().revision);
        entry.ticket = try self.mutations[slot].requestCreation(peer_view, mutation, entry.epoch);
        entry.stage = .placing;
        _ = self.mutations[slot].pump(peer_view);
        return true;
    }
};

/// The spawn's (or its satellite attach's) result: its terminal identity,
/// which must be this coordinator's.
fn accept(model: *Model, slot: usize, entry: *Creation, result: support.OperationResult) !void {
    if (result.status != .success) return error.OperationFailed;
    const ref = result.terminal_ref orelse return error.MissingIdentity;
    if (ref.provider_id != entry.coordinator) return error.ForeignCoordinator;
    if (entry.terminal) |expected| if (!expected.eql(ref)) return error.IdentityMismatch;
    entry.terminal = ref;
    // A satellite terminal publishes only once subscribed, as for the
    // active coordinator's spawns.
    if (entry.stage == .spawning and result.kind == .spawn and ref.terminal_id.phux.kind == 1) {
        const peer = model.phuxPeerAt(slot) orelse return error.NoCoordinator;
        entry.request = try peer.requestAttach(ref);
        entry.stage = .attaching;
        return;
    }
    entry.stage = .publishing;
}

fn placement(entry: Creation, ref: TerminalRef, revision: u64) !Mutation {
    var mutation: Mutation = .{ .expected_revision = revision, .session_id = entry.session, .kind = .add, .terminal_ref = ref };
    if (entry.kind == .tab) return mutation;
    mutation.kind = .split;
    mutation.window_id = entry.window_id orelse return error.StaleDestination;
    mutation.terminal_ref = entry.origin orelse return error.StaleDestination;
    mutation.new_terminal_ref = ref;
    mutation.direction = if (entry.kind == .split_right) .horizontal else .vertical;
    return mutation;
}

/// The spawn's owner. Only this coordinator's own attached pane may own it:
/// another coordinator's pane is never handed to this one.
fn spawnOwner(model: *Model, coordinator: support.ProviderId, kind: Kind, choice: Owner) !?TerminalRef {
    const candidate: ?TerminalRef = switch (choice) {
        .none => return null,
        .terminal => |ref| ref,
        .focused => model.focusedTerminalRef(),
    };
    const ref = candidate orelse return if (kind == .tab) null else error.InvalidDestination;
    if (support.providerKind(ref) != .phux or ref.provider_id != coordinator) return error.ForeignCoordinator;
    _ = model.terminalOwner(ref) orelse return error.NotReady;
    return ref;
}

fn slotOf(model: *const Model, coordinator: support.ProviderId) !usize {
    return model.peerSlot(coordinator) orelse error.NoCoordinator;
}

/// A showing, attached peer whose workspace is projected for this
/// connection: the only kind of coordinator an edit may address.
fn view(model: *Model, slot: usize) !PeerView {
    if (comptime !support.phux_enabled) return error.NoProvider;
    const peer = model.phuxPeerAt(slot) orelse return error.NoCoordinator;
    if (!peer.showing() or peer.state() != .attached) return error.NotShowing;
    const state = &model.peer_workspaces[slot];
    if (state.session == 0 or state.epoch != peer.connectionEpoch()) return error.WorkspaceUnavailable;
    if (state.authority != peer.providerId()) return error.StaleContext;
    return .{ .peer = peer, .shared_workspace = state };
}

/// Whether coordinator `id` is a peer that can take an edit now.
pub fn editable(model: *Model, coordinator: support.ProviderId) bool {
    const slot = model.peerSlot(coordinator) orelse return false;
    _ = view(model, slot) catch return false;
    return true;
}

/// Where window `id` moves one step left or right in a coordinator's order;
/// null at either end or when it is not listed.
pub fn neighborIndex(windows: anytype, id: WindowId, right: bool) ?usize {
    for (windows, 0..) |window, index| {
        if (!std.mem.eql(u8, &window.id, &id)) continue;
        if ((!right and index == 0) or (right and index + 1 == windows.len)) return null;
        return if (right) index + 1 else index - 1;
    }
    return null;
}

test "a creation's placement is an add, or a split beside the pane it split" {
    const origin: TerminalRef = .{ .provider_id = contract.phuxCoordinatorId("mini"), .terminal_id = .{ .phux = .{ .kind = 0, .id = 7 } } };
    const created: TerminalRef = .{ .provider_id = origin.provider_id, .terminal_id = .{ .phux = .{ .kind = 0, .id = 8 } } };
    const window: WindowId = @splat(3);
    var entry: Creation = .{ .coordinator = origin.provider_id, .epoch = 1, .session = 2, .kind = .tab, .request = 1 };
    const add = try placement(entry, created, 9);
    try std.testing.expectEqual(.add, add.kind);
    try std.testing.expect(add.terminal_ref.?.eql(created));
    try std.testing.expectEqual(@as(u64, 9), add.expected_revision);
    entry.kind = .split_down;
    try std.testing.expectError(error.StaleDestination, placement(entry, created, 9));
    entry.origin = origin;
    entry.window_id = window;
    const split = try placement(entry, created, 9);
    try std.testing.expectEqual(.split, split.kind);
    try std.testing.expect(split.terminal_ref.?.eql(origin));
    try std.testing.expect(split.new_terminal_ref.?.eql(created));
    try std.testing.expectEqual(.vertical, split.direction);
    try std.testing.expectEqual(window, split.window_id);
}

test "neighbors stop at either end of the coordinator's order" {
    const Window = struct { id: WindowId };
    const windows = [_]Window{ .{ .id = @splat(1) }, .{ .id = @splat(2) } };
    try std.testing.expectEqual(@as(?usize, 1), neighborIndex(&windows, @splat(1), true));
    try std.testing.expectEqual(@as(?usize, null), neighborIndex(&windows, @splat(1), false));
    try std.testing.expectEqual(@as(?usize, 0), neighborIndex(&windows, @splat(2), false));
    try std.testing.expectEqual(@as(?usize, null), neighborIndex(&windows, @splat(3), true));
}
