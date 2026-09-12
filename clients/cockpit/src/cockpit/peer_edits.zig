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

/// `window` is New Window with one of the peer's panes focused: a new tab on
/// that peer, placed in a native window opened for it.
pub const Kind = enum { tab, window, split_right, split_down };

fn isSplit(kind: Kind) bool {
    return kind == .split_right or kind == .split_down;
}

/// A native window opened for a New Window whose tab never landed there; it
/// is closed if it is still empty (`retireOrphans`).
const Provisional = struct { window: usize, epoch: u64 };

/// A terminal this peer spawned for a tab or split, bound to its instance
/// token, whose placement was never sent: the connection ended, or the peer
/// stopped showing, first. Killed conditionally once the peer lists again
/// (`sendStrays`), never unconditionally (ADR-0109).
const Stray = struct { ref: TerminalRef, instance: [16]u8 };
pub const max_strays: usize = 8;

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
    /// New Window's native window, opened for this tab, and its epoch.
    window: ?usize = null,
    window_epoch: u64 = 0,
    /// The spawn's instance token, when the peer bound it (CONDITIONAL_KILL).
    instance: ?[16]u8 = null,

    /// Spawned and bound, and its placement never sent: the only terminal a
    /// peer may later kill, and only conditionally. A placement already sent
    /// may land on that server, so a `placing` entry never qualifies.
    fn stray(entry: Creation) ?Stray {
        const instance = entry.instance orelse return null;
        const ref = entry.terminal orelse return null;
        if (entry.stage != .publishing and entry.stage != .attaching) return null;
        return .{ .ref = ref, .instance = instance };
    }
};

pub const Edits = struct {
    pub const State = struct {
        mutations: shared_mutations.Coordinator = .{},
        creations: [max_creations]?Creation = @splat(null),
        strays: [max_strays]?Stray = @splat(null),
    };
    states: std.ArrayList(*State) = .empty,
    // At most one provisional operation owns each physical window.
    orphans: [model_module.max_windows]?Provisional = @splat(null),

    pub fn deinit(self: *Edits) void {
        for (self.states.items) |state| std.heap.page_allocator.destroy(state);
        self.states.deinit(std.heap.page_allocator);
    }

    fn ensure(self: *Edits, slot: usize) !void {
        const gpa = std.heap.page_allocator;
        try self.states.ensureTotalCapacity(gpa, slot + 1);
        while (self.states.items.len <= slot) {
            const state = try gpa.create(State);
            state.* = .{};
            self.states.appendAssumeCapacity(state);
        }
    }

    /// The slot's connection ended or it now holds another coordinator:
    /// nothing queued for it may continue. A mutation already sent may still
    /// land on that server; forgetting it never rolls one back. A bound
    /// spawn whose placement was never sent is kept as a stray.
    pub fn forget(self: *Edits, slot: usize) void {
        if (slot >= self.states.items.len) return;
        const state = self.states.items[slot];
        for (state.creations) |held| if (held) |entry| self.drop(slot, entry);
        state.mutations = .{};
        state.creations = @splat(null);
    }

    fn drop(self: *Edits, slot: usize, entry: Creation) void {
        self.orphan(entry);
        const stray = entry.stray() orelse return;
        for (&self.states.items[slot].strays) |*held| {
            if (held.* != null) continue;
            held.* = stray;
            return;
        }
    }

    /// The slot now holds another coordinator, or none: its strays are not
    /// this coordinator's to kill.
    pub fn dropStrays(self: *Edits, slot: usize) void {
        if (slot >= self.states.items.len) return;
        self.states.items[slot].strays = @splat(null);
    }

    pub fn strayCount(self: *const Edits, slot: usize) usize {
        if (slot >= self.states.items.len) return 0;
        var count: usize = 0;
        for (self.states.items[slot].strays) |held| {
            if (held != null) count += 1;
        }
        return count;
    }

    /// The peer lists again: ask it to kill each of its strays, each only
    /// if its instance token still matches and no other connection has
    /// attached or used it (KILL_RESOURCE_IF). Best effort: the outcome is
    /// not waited for, and a refusal leaves the terminal running. A peer
    /// without CONDITIONAL_KILL on this connection kills nothing; its strays
    /// are dropped. Only that peer's own terminals are ever named.
    pub fn sendStrays(self: *Edits, model: *Model, slot: usize) usize {
        if (comptime !support.phux_enabled) return 0;
        if (slot >= self.states.items.len) return 0;
        const peer = model.phuxPeerAt(slot) orelse return 0;
        const supported = peer.conditionalKillSupported();
        var sent: usize = 0;
        for (&self.states.items[slot].strays) |*held| {
            const stray = held.* orelse continue;
            held.* = null;
            if (!supported or stray.ref.provider_id != peer.providerId()) continue;
            _ = peer.requestKillIf(stray.ref, stray.instance) catch continue;
            sent += 1;
        }
        return sent;
    }

    fn orphan(self: *Edits, entry: Creation) void {
        if (entry.kind != .window) return;
        const window = entry.window orelse return;
        for (&self.orphans) |*held| {
            if (held.* != null) continue;
            held.* = .{ .window = window, .epoch = entry.window_epoch };
            return;
        }
    }

    /// Close every window a peer's New Window opened for a tab that never
    /// landed there, while it is still that window and still empty. True
    /// when one closed.
    pub fn retireOrphans(self: *Edits, model: *Model) bool {
        var closed = false;
        for (&self.orphans) |*held| {
            const value = held.* orelse continue;
            held.* = null;
            if (value.window >= model_module.max_windows or model.window_epochs[value.window] != value.epoch) continue;
            if (!model.windowOpen(value.window)) continue;
            const workspace = model.wsAt(value.window) orelse continue;
            if (workspace.tab_count != 0) continue;
            model.closeWindow(value.window);
            closed = true;
        }
        return closed;
    }

    /// One of the peer's own terminals that no tab shows (the available
    /// inventory follows the focused pane's coordinator): place it as a new
    /// tab on that peer. Attached first unless its replica is already live.
    /// Only a terminal of the session the peer shows; never another
    /// coordinator's.
    pub fn adopt(self: *Edits, model: *Model, ref: TerminalRef) !*Creation {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const slot = try slotOf(model, ref.provider_id);
        try self.ensure(slot);
        const peer_view = try view(model, slot);
        const peer = peer_view.peer;
        const state = peer_view.shared_workspace;
        _ = try currentSnapshot(peer_view);
        if (peer.terminalSession(ref) != state.session) return error.StaleTarget;
        if (model.locateTerminal(ref) != null) return error.AlreadyPlaced;
        if (self.holdsTerminal(slot, ref)) return error.AlreadyPending;
        if (!model.canAddPane()) return error.TerminalCapacity;
        const free = self.vacant(slot) orelse return error.OperationCapacity;
        var entry: Creation = .{ .coordinator = ref.provider_id, .epoch = peer.connectionEpoch(), .session = state.session, .kind = .tab, .request = 0, .terminal = ref };
        const live = if (peer.presentation(ref)) |current| current.phase == .live else false;
        if (live) {
            entry.stage = .publishing;
        } else {
            entry.request = try peer.requestAttach(ref);
            entry.stage = .attaching;
        }
        free.* = entry;
        return &free.*.?;
    }

    /// Adopt an already completed, instance-bound spawn from the exact client.
    pub fn adoptSpawnIn(self: *Edits, model: *Model, remote: *support.PhuxProvider, result: support.OperationResult, window: usize, epoch: u64, may_focus: bool) !*Creation {
        if (comptime !support.phux_enabled) return error.NoProvider;
        if (result.kind != .spawn or result.status != .success) return error.OperationFailed;
        if (result.connection_epoch != remote.connectionEpoch()) return error.StaleContext;
        const ref = result.terminal_ref orelse return error.MissingIdentity;
        if (ref.provider_id != remote.providerId()) return error.ForeignCoordinator;
        const instance = result.instance orelse return error.MissingIdentity;
        const slot = model.peerSlotForAttachment(remote.context_id) orelse return error.NoCoordinator;
        const entry = try self.prepareTabIn(model, slot, window, epoch, may_focus);
        errdefer entry.* = null;
        entry.*.?.instance = instance;
        try accept(model, slot, &entry.*.?, result);
        return &entry.*.?;
    }

    pub fn createTabIn(self: *Edits, model: *Model, remote: *support.PhuxProvider, window: usize, epoch: u64, cwd: []const u8, may_focus: bool) !*Creation {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const slot = model.peerSlotForAttachment(remote.context_id) orelse return error.NoCoordinator;
        const entry = try self.prepareTabIn(model, slot, window, epoch, may_focus);
        errdefer entry.* = null;
        entry.*.?.request = try requestCreationSpawn(remote, null, cwd);
        return &entry.*.?;
    }

    fn prepareTabIn(self: *Edits, model: *Model, slot: usize, window: usize, epoch: u64, may_focus: bool) !*?Creation {
        if (!model.windowOpen(window) or model.window_epochs[window] != epoch) return error.StaleDestination;
        const workspace = model.wsAtConst(window) orelse return error.StaleDestination;
        if (workspace.tab_count == model_module.max_tabs) return error.TabCapacity;
        try self.ensure(slot);
        const peer_view = try view(model, slot);
        _ = try currentSnapshot(peer_view);
        if (!model.canAddPane()) return error.TerminalCapacity;
        const free = self.vacant(slot) orelse return error.OperationCapacity;
        free.* = .{ .coordinator = peer_view.peer.providerId(), .epoch = peer_view.peer.connectionEpoch(), .session = peer_view.shared_workspace.session, .kind = .tab, .request = 0, .window = window, .window_epoch = epoch, .may_focus = may_focus };
        return free;
    }

    pub fn pendingTerminal(self: *const Edits, slot: usize, ref: TerminalRef) bool {
        if (slot >= self.states.items.len) return false;
        return self.holdsTerminal(slot, ref);
    }

    fn holdsTerminal(self: *const Edits, slot: usize, ref: TerminalRef) bool {
        for (self.states.items[slot].creations) |held| {
            const entry = held orelse continue;
            if (entry.terminal) |terminal| if (terminal.eql(ref)) return true;
        }
        return false;
    }

    pub fn pendingCreations(self: *const Edits, slot: usize) usize {
        if (slot >= self.states.items.len) return 0;
        var count: usize = 0;
        for (self.states.items[slot].creations) |entry| {
            if (entry != null) count += 1;
        }
        return count;
    }

    /// A later explicit selection supersedes every pending placement's focus.
    pub fn supersedeFocus(self: *Edits) void {
        for (self.states.items) |state| for (&state.creations) |*entry| {
            if (entry.*) |*value| value.may_focus = false;
        };
    }

    /// Close one of the peer's tabs: the layout-only window removal the
    /// active coordinator's tabs send to theirs. Its terminals keep running.
    pub fn removeWindow(self: *Edits, model: *Model, coordinator: support.ProviderId, id: WindowId) !void {
        const slot = try slotOf(model, coordinator);
        try self.ensure(slot);
        var peer_view = try view(model, slot);
        try self.states.items[slot].mutations.requestRemoveWindow(&peer_view, id);
    }

    /// Close one of the peer's panes on that coordinator.
    pub fn removePane(self: *Edits, model: *Model, ref: TerminalRef) !void {
        const slot = try slotOf(model, ref.provider_id);
        try self.ensure(slot);
        var peer_view = try view(model, slot);
        try self.states.items[slot].mutations.requestRemove(&peer_view, ref);
    }

    /// Move the peer's window `id` one step left or right in its own order.
    pub fn reorder(self: *Edits, model: *Model, coordinator: support.ProviderId, id: WindowId, right: bool) !void {
        const slot = try slotOf(model, coordinator);
        try self.ensure(slot);
        var peer_view = try view(model, slot);
        const target = neighborIndex(peer_view.peer.workspaceSnapshot().windows, id, right) orelse return error.StaleTarget;
        try self.states.items[slot].mutations.requestReorder(&peer_view, id, target);
    }

    /// Commit a divider drag on the peer's window `id`. The path was taken
    /// from the projection of the revision the drag started on; the queue
    /// refuses it against any other revision rather than rebasing it.
    pub fn resize(self: *Edits, model: *Model, coordinator: support.ProviderId, id: WindowId, path_bits: u64, path_len: u32, ratio: f32) !void {
        const slot = try slotOf(model, coordinator);
        try self.ensure(slot);
        var peer_view = try view(model, slot);
        try self.states.items[slot].mutations.requestResize(&peer_view, id, path_bits, path_len, ratio);
    }

    /// Spawn a terminal on the peer and place it as a new tab or a split of
    /// the focused pane. `cwd` empty: the serving host's default directory.
    /// Returns the queued creation so the caller can supersede every other
    /// pending focus without losing this one's.
    pub fn create(self: *Edits, model: *Model, coordinator: support.ProviderId, kind: Kind, cwd: []const u8, owner_choice: Owner) !*Creation {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const slot = try slotOf(model, coordinator);
        try self.ensure(slot);
        const peer_view = try view(model, slot);
        const peer = peer_view.peer;
        const state = peer_view.shared_workspace;
        const snapshot = try currentSnapshot(peer_view);
        if (!model.canAddPane()) return error.TerminalCapacity;
        const free = self.vacant(slot) orelse return error.OperationCapacity;
        var entry: Creation = .{ .coordinator = coordinator, .epoch = peer.connectionEpoch(), .session = state.session, .kind = kind, .request = 0 };
        if (isSplit(kind)) try self.prepareSplit(model, slot, snapshot, &entry);
        const owner = try spawnOwner(model, coordinator, kind, owner_choice);
        // New Window: the native window its tab will land in, opened now as
        // the active coordinator's New Window opens one.
        try prepareCreationWindow(model, peer, &entry);
        entry.request = requestCreationSpawn(peer, owner, cwd) catch |err| {
            if (entry.window) |index| model.closeWindow(index);
            return err;
        };
        if (entry.window) |index| model.active_window = index;
        free.* = entry;
        return &free.*.?;
    }

    /// New Window's native window, bound to the peer's exact attachment the
    /// moment it opens. It is selected before its tab exists, and an unbound
    /// empty window resolves to the canonical local provider
    /// (`Model.phuxForWindowConst`), so New Session from it asked This Mac
    /// instead of the machine the window was opened for. Every failure path
    /// closes the window (`create`, `retireOrphans`), and closing clears the
    /// binding.
    fn prepareCreationWindow(model: *Model, peer: *const support.PhuxProvider, entry: *Creation) !void {
        if (entry.kind != .window) return;
        const index = model.freeWindowIndex() orelse return error.WindowCapacity;
        _ = model.openWindow(index) orelse return error.WindowCapacity;
        model.bindWindowAttachment(index, peer.context_id);
        entry.window = index;
        entry.window_epoch = model.window_epochs[index];
    }

    fn requestCreationSpawn(peer: *support.PhuxProvider, owner: ?TerminalRef, cwd: []const u8) !u32 {
        const viewport = if (owner) |ref| peer.lastViewport(ref) orelse peer.attach_viewport else peer.attach_viewport;
        // Bound when the peer can kill it conditionally later, should the
        // placement never be sent (`Creation.stray`).
        return if (peer.conditionalKillSupported())
            peer.requestSpawnBound(owner, viewport, cwd)
        else
            peer.requestSpawnIn(owner, viewport, cwd);
    }

    fn currentSnapshot(peer_view: PeerView) !contract.workspace.Snapshot {
        const snapshot = peer_view.peer.workspaceSnapshot();
        if (snapshot.session_id != peer_view.shared_workspace.session) return error.StaleContext;
        if (snapshot.state == .unavailable or snapshot.state == .last_good_error) return error.WorkspaceUnavailable;
        return snapshot;
    }

    fn vacant(self: *Edits, slot: usize) ?*?Creation {
        for (&self.states.items[slot].creations) |*entry| if (entry.* == null) return entry;
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
        for (self.states.items[slot].creations) |value| {
            const entry = value orelse continue;
            if (isSplit(entry.kind)) count += 1;
        }
        return count;
    }

    /// One operation result from the peer's connection. True when it
    /// answered one of this slot's spawns or their follow-up attach.
    pub fn complete(self: *Edits, model: *Model, slot: usize, result: support.OperationResult) bool {
        if (comptime !support.phux_enabled) return false;
        if (slot >= self.states.items.len) return false;
        for (&self.states.items[slot].creations) |*held| {
            const entry = if (held.*) |*value| value else continue;
            if (entry.stage != .spawning and entry.stage != .attaching) continue;
            if (entry.request != result.request_id or entry.epoch != result.connection_epoch) continue;
            accept(model, slot, entry, result) catch {
                self.orphan(entry.*);
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
        if (slot >= self.states.items.len) return false;
        var peer_view = view(model, slot) catch return false;
        var changed = self.states.items[slot].mutations.pump(&peer_view);
        for (&self.states.items[slot].creations) |*held| {
            const entry = if (held.*) |*value| value else continue;
            if (entry.epoch != peer_view.peer.connectionEpoch() or entry.session != peer_view.shared_workspace.session) {
                self.drop(slot, entry.*);
                held.* = null;
                changed = true;
                continue;
            }
            changed = self.advance(model, &peer_view, slot, held) or changed;
        }
        return changed;
    }

    fn advance(self: *Edits, model: *const Model, peer_view: *PeerView, slot: usize, held: *?Creation) bool {
        const entry = &held.*.?;
        switch (entry.stage) {
            .spawning, .attaching => return false,
            .publishing => {
                if (!creationWindowCurrent(model, entry.*)) {
                    self.drop(slot, entry.*);
                    held.* = null;
                    return true;
                }
                const outcome = self.place(peer_view, slot, entry) catch {
                    self.drop(slot, entry.*);
                    held.* = null;
                    return true;
                };
                return outcome;
            },
            .placing => {
                const completion = self.states.items[slot].mutations.takeCreationCompletion(entry.ticket) orelse return false;
                if (completion.outcome == .confirmed) {
                    placeInWindow(model, peer_view, entry.*);
                    if (entry.may_focus) peer_view.shared_workspace.desired_terminal = entry.terminal;
                } else self.orphan(entry.*);
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
        entry.ticket = try self.states.items[slot].mutations.requestCreation(peer_view, mutation, entry.epoch);
        entry.stage = .placing;
        _ = self.states.items[slot].mutations.pump(peer_view);
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
    if (result.kind == .spawn) entry.instance = result.instance;
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

/// A confirmed New Window: its new shared window goes to the native window
/// opened for it, when that is still the window opened (the projection
/// consumes the hint as it does for the active coordinator's New Window).
fn placeInWindow(model: *const Model, peer_view: *PeerView, entry: Creation) void {
    const index = entry.window orelse return;
    if (index >= model_module.max_windows or model.window_epochs[index] != entry.window_epoch) return;
    if (!model.windowOpen(index)) return;
    const ref = entry.terminal orelse return;
    const id = shared_mutations.terminalWindow(peer_view.peer.workspaceSnapshot(), ref) orelse return;
    peer_view.shared_workspace.placement_hint = .{ .shared_id = id, .window = index, .window_epoch = entry.window_epoch };
}

fn creationWindowCurrent(model: *const Model, entry: Creation) bool {
    const window = entry.window orelse return true;
    return model.windowOpen(window) and model.window_epochs[window] == entry.window_epoch;
}

fn placement(entry: Creation, ref: TerminalRef, revision: u64) !Mutation {
    var mutation: Mutation = .{ .expected_revision = revision, .session_id = entry.session, .kind = .add, .terminal_ref = ref };
    if (!isSplit(entry.kind)) return mutation;
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
    const ref = candidate orelse return if (!isSplit(kind)) null else error.InvalidDestination;
    if (support.providerKind(ref) != .phux or ref.provider_id != coordinator) return error.ForeignCoordinator;
    _ = model.terminalOwner(ref) orelse return error.NotReady;
    return ref;
}

/// The slot of the peer an edit addresses. When the active window's selected
/// tab is `coordinator`'s and names its exact attachment, that attachment
/// decides. Sibling attachments of one coordinator share its ID but show other
/// sessions, so an attachment that is not a peer (the primary, or one since
/// removed) refuses rather than falling back to whichever peer shares the ID:
/// that fallback once spawned a New Window in another session.
fn slotOf(model: *const Model, coordinator: support.ProviderId) !usize {
    if (selectedAttachment(model, coordinator)) |attachment| {
        return model.peerSlotForAttachment(attachment) orelse error.NotPeerAttachment;
    }
    return model.peerSlot(coordinator) orelse error.NoCoordinator;
}

/// The exact attachment the active window's selected tab names, when that tab
/// is `coordinator`'s. Null for a tab without one: the coordinator ID is then
/// the only identity there is.
fn selectedAttachment(model: *const Model, coordinator: support.ProviderId) ?u64 {
    const workspace = model.wsAtConst(model.active_window) orelse return null;
    const tree = workspace.treeConst(workspace.selected_tab) orelse return null;
    if (shared_workspace.tabAuthority(tree) != coordinator) return null;
    return tree.attachment_id;
}

/// A showing, attached peer whose workspace is projected for this
/// connection: the only kind of coordinator an edit may address.
fn view(model: *Model, slot: usize) !PeerView {
    if (comptime !support.phux_enabled) return error.NoProvider;
    const peer = model.phuxPeerAt(slot) orelse return error.NoCoordinator;
    if (!peer.showing() or peer.state() != .attached) return error.NotShowing;
    const state = &model.peers.items[slot].workspace;
    if (state.session == 0 or state.epoch != peer.connectionEpoch()) return error.WorkspaceUnavailable;
    if (state.authority != peer.providerId()) return error.StaleContext;
    return .{ .peer = peer, .shared_workspace = state };
}

/// Whether coordinator `id` is a peer that can take an edit now.
pub fn editable(model: *Model, coordinator: support.ProviderId) bool {
    const slot = slotOf(model, coordinator) catch return false;
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
