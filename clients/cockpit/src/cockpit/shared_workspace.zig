//! Identity-based projection of the Rust-owned workspace. Only selection and
//! native-window placement are remembered here; shared trees always come from
//! the provider's confirmed snapshot.
//!
//! Each session attachment projects its own workspace through its own `State`.
//! Its process-local provider context tags the published trees, so a second
//! session on the same coordinator keeps its windows, order and selection.
//! Unbound legacy projections retain coordinator-based compatibility. A bound
//! attachment cannot adopt an unqualified tree merely because its coordinator
//! matches. Scratch trees remain ephemeral and yield to shared projection.
const std = @import("std");
const contract = @import("provider_contract");
const shared = contract.workspace;
const layout = @import("layout.zig");
const model_module = @import("model.zig");
const Model = model_module.Model;
const topology = @import("topology.zig");
const TerminalRef = contract.TerminalRef;
const WindowId = [16]u8;
const capacity = topology.max_terminals;
const support = @import("phux_support.zig");

const Ownership = struct {
    authority: contract.ProviderId,
    attachment_id: ?u64,

    fn owns(self: Ownership, tree: *const layout.Tree) bool {
        return tree.attachment_id == self.attachment_id and tabAuthority(tree) == self.authority;
    }

    fn retains(self: Ownership, model: *const Model, tree: *const layout.Tree) bool {
        // Disconnected siblings still own their frozen presentation. Only
        // their own attachment's explicit leave/replacement may remove it.
        if (tree.attachment_id) |id| return self.attachment_id != id;
        const authority = tabAuthority(tree) orelse return false;
        // A legacy tree has no evidence identifying which same-coordinator
        // attachment owns it. Only an explicit, proven migration may tag it.
        if (self.attachment_id != null and authority == self.authority) return true;
        if (authority == self.authority or authority == .local) return false;
        return model.projectsAuthority(authority);
    }

    fn primary(self: Ownership, model: *const Model) bool {
        if (self.attachment_id) |id| {
            if (comptime !support.phux_enabled) return false;
            const remote = model.phuxConst() orelse return false;
            return remote.context_id == id;
        }
        return self.authority == model.attachmentAuthority();
    }
};
const Subscription = struct {
    ref: TerminalRef,
    request: u32,
    epoch: u64,
    retry_after_refresh: ?u32 = null,
    completed: bool = false,
    succeeded: bool = false,
};

const Placement = struct {
    id: WindowId,
    native_window: usize,
    window_epoch: u64 = 0,
    tab_id: u32,
    tab_generation: u64 = 0,
    focus: ?TerminalRef,
    selected: bool,
    order: usize = 0,
    neighbors: [layout.max_panes]TerminalRef = undefined,
    neighbor_count: usize = 0,
};

const SessionView = struct {
    session: u32 = 0,
    active_window: usize = 0,
    active_epoch: u64 = 0,
    web_selected: [model_module.max_windows]bool = @splat(false),
    placements: [capacity]Placement = undefined,
    count: usize = 0,

    fn find(self: *const SessionView, id: WindowId) ?Placement {
        for (self.placements[0..self.count]) |placement| {
            if (std.mem.eql(u8, &placement.id, &id)) return placement;
        }
        return null;
    }
};

pub const State = struct {
    /// Stable PhuxProvider.context_id, independent of coordinator identity and
    /// connection epoch. Null retains the legacy unbound projection behavior.
    attachment_id: ?u64 = null,
    presentation_window: ?struct { window: usize, window_epoch: u64 } = null,
    hidden_windows: [capacity]WindowId = undefined,
    hidden_count: usize = 0,
    detached: bool = false,
    projection_dirty: bool = false,
    session: u32 = 0,
    revision: u64 = 0,
    projection_generation: u64 = 0,
    epoch: u64 = 0,
    refused: bool = false,
    subscription_refused: bool = false,
    desired_terminal: ?TerminalRef = null,
    placement_hint: ?struct { shared_id: WindowId, window: usize, window_epoch: u64 } = null,
    subscriptions: [capacity]?Subscription = @splat(null),
    detachments: [capacity]?Subscription = @splat(null),
    views: std.ArrayListUnmanaged(SessionView) = .empty,
    context_hash: ?u64 = null,
    /// The coordinator whose workspace this projects. Every leaf of every
    /// window it publishes must carry this id (MixedAuthority otherwise).
    authority: contract.ProviderId = .phux,

    pub fn deinit(self: *State) void {
        self.views.deinit(std.heap.page_allocator);
    }

    fn ownership(self: *const State) Ownership {
        return .{ .authority = self.authority, .attachment_id = self.attachment_id };
    }

    /// An attachment lookup must never fall back to a same-endpoint sibling.
    fn providerForProjection(self: *const State, model: *Model) ?*support.PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        const id = self.attachment_id orelse return model.phuxFor(self.authority);
        if (model.phux()) |active| if (active.context_id == id) return active;
        for (model.peers.items) |entry| {
            const peer = entry.provider orelse continue;
            if (peer.context_id == id) return peer;
        }
        return null;
    }

    fn locate(self: *const State, model: *const Model, ref: TerminalRef) ?model_module.TerminalLocation {
        for (0..model_module.max_windows) |index| {
            const workspace = model.wsAtConst(index) orelse continue;
            for (workspace.tabs[0..workspace.tab_count], 0..) |*tree, tab| {
                if (!self.ownership().owns(tree)) continue;
                if (tree.find(ref) != null) return .{ .window = index, .tab = tab };
            }
        }
        return null;
    }

    fn defaultWindow(self: *const State, model: *const Model) !usize {
        const target = self.presentation_window orelse return model.firstOpenWindow();
        if (!model.windowOpen(target.window)) return error.StalePresentationWindow;
        if (model.window_epochs[target.window] != target.window_epoch) return error.StalePresentationWindow;
        return target.window;
    }

    /// Explicit user navigation reopens hidden views at its captured native
    /// destination. Publication never raises a window or changes global focus.
    pub fn showInWindow(self: *State, window: usize, window_epoch: u64) void {
        self.presentation_window = .{ .window = window, .window_epoch = window_epoch };
        self.hidden_count = 0;
        self.detached = false;
        self.projection_dirty = true;
        self.desired_terminal = null;
        self.placement_hint = null;
    }

    /// Call before Model.closeWindow. This only records client-view intent;
    /// shared layout and durable resources remain owned by the coordinator.
    pub fn hideNativeWindow(self: *State, model: *const Model, window: usize) !void {
        const workspace = model.wsAtConst(window) orelse return error.UnknownWindow;
        const previous_count = self.hidden_count;
        errdefer self.hidden_count = previous_count;
        var affected = self.targetsWindow(model, window);
        for (workspace.tabs[0..workspace.tab_count], 0..) |*tree, tab| {
            if (!self.ownership().owns(tree)) continue;
            affected = true;
            const id = workspace.shared_ids[tab] orelse continue;
            if (!self.shownElsewhere(model, window, id)) try self.hideSharedWindow(id);
        }
        if (!affected) return;
        self.presentation_window = self.remainingWindow(model, window);
        self.detached = self.presentation_window == null;
        self.projection_dirty = true;
        self.desired_terminal = null;
        self.placement_hint = null;
    }

    fn targetsWindow(self: *const State, model: *const Model, window: usize) bool {
        const target = self.presentation_window orelse return false;
        return target.window == window and target.window_epoch == model.window_epochs[window];
    }

    fn shownElsewhere(self: *const State, model: *const Model, excluded: usize, id: WindowId) bool {
        const view = self.current(model);
        for (view.placements[0..view.count]) |placement| {
            if (placement.native_window == excluded) continue;
            if (std.mem.eql(u8, &placement.id, &id)) return true;
        }
        return false;
    }

    fn remainingWindow(self: *const State, model: *const Model, excluded: usize) @FieldType(State, "presentation_window") {
        const view = self.current(model);
        for (view.placements[0..view.count]) |placement| {
            if (placement.native_window == excluded) continue;
            return .{ .window = placement.native_window, .window_epoch = placement.window_epoch };
        }
        return null;
    }

    fn hideSharedWindow(self: *State, id: WindowId) !void {
        if (hiddenWindow(self.hidden_windows[0..self.hidden_count], id)) return;
        if (self.hidden_count == self.hidden_windows.len) return error.WorkspaceCapacity;
        self.hidden_windows[self.hidden_count] = id;
        self.hidden_count += 1;
    }

    fn pruneHidden(self: *State, snapshot: shared.Snapshot) void {
        var kept: usize = 0;
        for (self.hidden_windows[0..self.hidden_count]) |id| {
            if (!snapshotHasWindow(snapshot, id)) continue;
            self.hidden_windows[kept] = id;
            kept += 1;
        }
        self.hidden_count = kept;
    }

    /// The caller supplies endpoint/incarnation evidence, excluding session.
    /// Numeric identities from a replacement server cannot recover old focus.
    pub fn setContext(self: *State, hash: u64) void {
        if (self.context_hash == hash) return;
        const replaced = self.context_hash != null;
        self.views.clearRetainingCapacity();
        self.context_hash = hash;
        self.session = 0;
        self.revision = 0;
        self.epoch = 0;
        self.subscriptions = @splat(null);
        self.detachments = @splat(null);
        self.subscription_refused = false;
        if (replaced) {
            self.desired_terminal = null;
            self.placement_hint = null;
        }
    }

    fn current(self: *const State, model: *const Model) SessionView {
        var result: SessionView = .{
            .session = self.session,
            .active_window = model.active_window,
            .active_epoch = model.window_epochs[model.active_window],
        };
        for (0..model_module.max_windows) |index| {
            const workspace = model.wsAtConst(index) orelse continue;
            captureWindow(&result, workspace, index, model.window_epochs[index], self.ownership());
        }
        return result;
    }

    fn remember(self: *State, model: *const Model) !void {
        if (self.session == 0) return;
        const view = self.current(model);
        for (self.views.items) |*entry| {
            if (entry.session != self.session) continue;
            entry.* = view;
            return;
        }
        if (self.views.items.len == 256) return error.SessionCapacity;
        try self.views.append(std.heap.page_allocator, view);
    }

    pub fn leaveSession(self: *State, model: *Model) !void {
        try self.remember(model);
        _ = clearAuthority(model, self.ownership());
        self.session = 0;
        self.revision = 0;
        self.epoch = 0;
        self.subscriptions = @splat(null);
        self.detachments = @splat(null);
        self.subscription_refused = false;
        self.desired_terminal = null;
        self.placement_hint = null;
        self.presentation_window = null;
        self.hidden_count = 0;
        self.detached = false;
        self.projection_dirty = false;
    }

    fn priorView(self: *const State, model: *const Model, session: u32) SessionView {
        if (self.session == session) return self.current(model);
        for (self.views.items) |view| if (view.session == session) return view;
        return .{ .session = session };
    }

    pub fn apply(self: *State, model: *Model, snapshot: shared.Snapshot, epoch: u64) !bool {
        if (snapshot.state == .unavailable) return false;
        if (snapshot.state == .last_good_error) return error.UnavailableWorkspace;
        if (snapshot.session_id == 0) return error.InvalidSession;
        if (self.samePublication(snapshot, epoch)) {
            const refused = operationRefused(snapshot);
            const recovered = self.refused != refused;
            self.refused = refused;
            const selected = self.selectDesired(model);
            return recovered or selected;
        }
        var previous = self.priorView(model, snapshot.session_id);
        self.applyHint(model, &previous);
        const candidate = try std.heap.page_allocator.create(Candidate);
        defer std.heap.page_allocator.destroy(candidate);
        candidate.* = .{
            .owner = self.ownership(),
            .default_window = try self.defaultWindow(model),
            .hidden_windows = self.hidden_windows[0..self.hidden_count],
            .detached = self.detached,
        };
        try candidate.prepare(model, snapshot, &previous);
        candidate.restoreSelection(&previous);
        candidate.publish(model, &previous);
        self.projection_generation +%= 1;
        self.placement_hint = null;
        self.projection_dirty = false;
        self.pruneHidden(snapshot);
        if (self.epoch != epoch) {
            self.subscriptions = @splat(null);
            self.detachments = @splat(null);
        }
        self.session = snapshot.session_id;
        self.revision = snapshot.revision;
        self.epoch = epoch;
        self.refused = operationRefused(snapshot);
        _ = self.selectDesired(model);
        return true;
    }

    fn samePublication(self: *const State, snapshot: shared.Snapshot, epoch: u64) bool {
        return self.session == snapshot.session_id and self.revision == snapshot.revision and
            self.epoch == epoch and self.placement_hint == null and !self.projection_dirty;
    }

    fn applyHint(self: *State, model: *const Model, previous: *SessionView) void {
        const hint = self.placement_hint orelse return;
        if (hint.window >= model_module.max_windows) return;
        if (model.window_epochs[hint.window] != hint.window_epoch) return;
        if (!model.windowOpen(hint.window)) return;
        for (previous.placements[0..previous.count]) |*placement| {
            if (!std.mem.eql(u8, &placement.id, &hint.shared_id)) continue;
            placement.native_window = hint.window;
            placement.window_epoch = hint.window_epoch;
            return;
        }
        if (previous.count == previous.placements.len) return;
        previous.placements[previous.count] = .{
            .id = hint.shared_id,
            .native_window = hint.window,
            .window_epoch = hint.window_epoch,
            .tab_id = 0,
            .focus = null,
            .selected = false,
        };
        previous.count += 1;
    }

    /// Shared membership authorizes discovery/subscription, never input before
    /// the exact replica has completed its ordinary bootstrap barrier.
    pub fn subscribe(self: *State, model: *Model) void {
        const remote = self.providerForProjection(model) orelse return;
        if (remote.state() != .attached) return;
        self.subscription_refused = false;
        self.pruneSubscriptions(remote);
        var refs: [capacity]TerminalRef = undefined;
        var count: usize = 0;
        for (remote.workspaceSnapshot().nodes) |node| {
            const ref = node.terminal_ref orelse continue;
            if (node.kind != .leaf or remote.terminalKnown(ref)) continue;
            if (self.locate(model, ref) == null) continue;
            if (count == refs.len) break;
            refs[count] = ref;
            count += 1;
        }
        for (refs[0..count]) |ref| self.subscribeOne(model, ref);
    }

    fn pruneSubscriptions(self: *State, remote: anytype) void {
        for (&self.subscriptions) |*entry| {
            const pending = entry.* orelse continue;
            if (subscriptionFinished(pending, remote) or retryReady(pending, remote.workspaceSnapshot())) entry.* = null;
        }
    }

    fn subscriptionFinished(pending: Subscription, remote: anytype) bool {
        if (!pending.completed) return false;
        if (!hasLeaf(remote.workspaceSnapshot(), pending.ref)) return true;
        return pending.succeeded and remote.terminalKnown(pending.ref);
    }

    pub fn completeSubscription(self: *State, result: @import("phux_support.zig").OperationResult, refresh_request: u32) bool {
        if (self.attachment_id != null) return false;
        return self.completeSubscriptionResult(result, refresh_request);
    }

    fn completeSubscriptionResult(self: *State, result: support.OperationResult, refresh_request: u32) bool {
        const entries = switch (result.kind) {
            .attach => &self.subscriptions,
            .detach => &self.detachments,
            else => return false,
        };
        for (entries) |*slot| {
            const entry = if (slot.*) |*entry| entry else continue;
            if (entry.request != result.request_id or entry.epoch != result.connection_epoch) continue;
            entry.completed = true;
            entry.succeeded = result.status == .success;
            if (result.status == .refused) entry.retry_after_refresh = refresh_request;
            return true;
        }
        return false;
    }

    /// Results are connection-local; equal request IDs and epochs on another
    /// attachment cannot complete this attachment's work.
    pub fn completeSubscriptionFrom(self: *State, attachment_id: u64, result: support.OperationResult, refresh_request: u32) bool {
        if (self.attachment_id != attachment_id) return false;
        return self.completeSubscriptionResult(result, refresh_request);
    }

    fn retryReady(entry: Subscription, snapshot: shared.Snapshot) bool {
        const failed_at = entry.retry_after_refresh orelse return false;
        return snapshot.status == .confirmed and failed_at != snapshot.request_id;
    }

    /// Catalog-only terminals need no replica. Release stream capacity after a
    /// confirmed topology change, while the durable process stays discoverable.
    pub fn releaseUnused(self: *State, model: *Model) void {
        const remote = self.providerForProjection(model) orelse return;
        if (remote.state() != .attached) return;
        for (&self.detachments) |*entry| {
            const pending = entry.* orelse continue;
            if (!remote.contains(pending.ref) or retryReady(pending, remote.workspaceSnapshot())) entry.* = null;
        }
        var refs: [capacity]TerminalRef = undefined;
        const count = remote.terminalRefs(&refs);
        for (refs[0..count]) |ref| {
            if (self.locate(model, ref) != null) continue;
            self.releaseOne(model, ref);
        }
    }

    fn releaseOne(self: *State, model: *Model, ref: TerminalRef) void {
        var free: ?usize = null;
        for (self.detachments, 0..) |entry, index| {
            if (entry) |known| {
                if (known.ref.eql(ref)) return;
            } else free = index;
        }
        const index = free orelse return;
        const remote = self.providerForProjection(model) orelse return;
        const request = remote.requestDetach(ref) catch return;
        self.detachments[index] = .{ .ref = ref, .request = request, .epoch = remote.connectionEpoch() };
    }

    fn subscribeOne(self: *State, model: *Model, ref: TerminalRef) void {
        var free: ?usize = null;
        for (self.subscriptions, 0..) |entry, index| {
            if (entry) |known| {
                if (known.ref.eql(ref)) {
                    if (known.retry_after_refresh != null) self.subscription_refused = true;
                    return;
                }
            } else free = index;
        }
        const index = free orelse return;
        const remote = self.providerForProjection(model) orelse return;
        const request = remote.requestAttach(ref) catch {
            self.subscription_refused = true;
            return;
        };
        self.subscriptions[index] = .{ .ref = ref, .request = request, .epoch = remote.connectionEpoch() };
    }

    pub fn selectDesired(self: *State, model: *Model) bool {
        const ref = self.desired_terminal orelse return false;
        const place = self.locate(model, ref) orelse return false;
        const workspace = model.wsAt(place.window) orelse return false;
        if (!workspace.selectTab(place.tab)) return false;
        _ = workspace.tabs[place.tab].focusTerminal(ref);
        model.active_window = place.window;
        self.desired_terminal = null;
        return true;
    }
};

fn operationRefused(snapshot: shared.Snapshot) bool {
    // Missing confirmation does not establish that the shared operation failed.
    // Its retained command result carries uncertainty independently of layout.
    return snapshot.status == .refused;
}

/// Native-window placement is client-local. Closing one presentation rehomes
/// its shared tabs in another open presentation without publishing topology.
pub fn closeNativeWindow(model: *Model, index: usize) !void {
    if (!model.windowOpen(index)) return error.UnknownWindow;
    const source = model.wsAt(index) orelse return error.UnknownWindow;
    if (model.openWindowCount() == 1) {
        model.closeWindow(index);
        return;
    }
    var available: usize = 0;
    for (0..model_module.max_windows) |other| {
        if (other == index or !model.windowOpen(other)) continue;
        available += model_module.max_tabs - model.wsAtConst(other).?.tab_count;
    }
    if (available < source.tab_count) return error.TabCapacity;
    rehomeTabs(model, index);
    model.closeWindow(index);
}

fn rehomeTabs(model: *Model, source_index: usize) void {
    const source = model.wsAt(source_index).?;
    var copied: usize = 0;
    for (0..model_module.max_windows) |other| {
        if (other == source_index or !model.windowOpen(other)) continue;
        const target = model.wsAt(other).?;
        while (copied < source.tab_count and target.tab_count < model_module.max_tabs) : (copied += 1) {
            const at = target.tab_count;
            target.tabs[at] = source.tabs[copied];
            target.shared_ids[at] = source.shared_ids[copied];
            target.tab_ids[at] = 0;
            target.tab_count += 1;
            target.assignRestoredTabId(at);
            if (model.active_window == source_index and source.selected_tab == copied) {
                target.selected_tab = at;
                target.web_selected = false;
                model.active_window = other;
            }
        }
    }
}

fn captureWindow(view: *SessionView, workspace: *const model_module.Workspace, native_window: usize, epoch: u64, owner: Ownership) void {
    view.web_selected[native_window] = workspace.web_selected;
    for (0..workspace.tab_count) |index| {
        const id = workspace.shared_ids[index] orelse continue;
        if (!owner.owns(&workspace.tabs[index])) continue;
        if (view.count == view.placements.len) return;
        view.placements[view.count] = .{
            .id = id,
            .native_window = native_window,
            .window_epoch = epoch,
            .tab_id = workspace.tab_ids[index],
            .tab_generation = workspace.tab_generation,
            .focus = workspace.tabs[index].focusedTerminal(),
            .selected = workspace.selected_tab == index,
            .order = index,
        };
        const placement = &view.placements[view.count];
        placement.neighbor_count = workspace.tabs[index].terminals(&placement.neighbors);
        view.count += 1;
    }
}

/// The coordinator a tab belongs to: the id its leaves carry. A shared tab's
/// leaves all carry one (MixedAuthority otherwise); an empty tab has none.
pub fn tabAuthority(tree: *const layout.Tree) ?contract.ProviderId {
    for (tree.nodes) |node| {
        if (node.kind != .leaf) continue;
        const ref = node.terminal orelse continue;
        return ref.provider_id;
    }
    return null;
}

/// Where a window's cleared group sat, and which kept tab was selected, so a
/// republished group goes back in place and a kept selection survives.
const Cleared = struct { anchor: ?usize = null, selected: ?usize = null };

/// Drop this projection's tabs while preserving sibling attachments. Unbound
/// legacy tabs follow the coordinator compatibility rule in Ownership.retains.
fn clearAuthority(model: *Model, owner: Ownership) [model_module.max_windows]Cleared {
    var result: [model_module.max_windows]Cleared = @splat(.{});
    for (0..model_module.max_windows) |index| {
        const workspace = model.wsAt(index) orelse continue;
        result[index] = clearWindow(model, workspace, owner);
    }
    // Saved attachment evidence is the active coordinator's alone
    // (Model.captureAttachmentContexts): its projection replaces it.
    if (owner.primary(model)) model.saved_attachments = .{};
    return result;
}

fn clearWindow(model: *const Model, workspace: *model_module.Workspace, owner: Ownership) Cleared {
    var cleared: Cleared = .{};
    var kept: usize = 0;
    for (0..workspace.tab_count) |tab| {
        if (!owner.retains(model, &workspace.tabs[tab])) {
            if (cleared.anchor == null) cleared.anchor = kept;
            continue;
        }
        if (tab != kept) {
            workspace.tabs[kept] = workspace.tabs[tab];
            workspace.tab_ids[kept] = workspace.tab_ids[tab];
            workspace.shared_ids[kept] = workspace.shared_ids[tab];
        }
        if (tab == workspace.selected_tab) cleared.selected = kept;
        kept += 1;
    }
    for (kept..model_module.max_tabs) |tab| {
        workspace.tabs[tab] = .{};
        workspace.tab_ids[tab] = 0;
        workspace.shared_ids[tab] = null;
    }
    workspace.tab_count = kept;
    workspace.selected_tab = cleared.selected orelse 0;
    workspace.hovered_tab = model_module.no_hovered_tab;
    return cleared;
}

fn retainedCount(model: *const Model, window: usize, owner: Ownership) usize {
    const workspace = model.wsAtConst(window) orelse return 0;
    var count: usize = 0;
    for (workspace.tabs[0..workspace.tab_count]) |*tree| {
        if (owner.retains(model, tree)) count += 1;
    }
    return count;
}

const Candidate = struct {
    owner: Ownership,
    default_window: usize,
    hidden_windows: []const WindowId,
    detached: bool,
    trees: [capacity]layout.Tree = @splat(.{}),
    placements: [capacity]Placement = undefined,
    count: usize = 0,
    counts: [model_module.max_windows]usize = @splat(0),
    seen: [512]bool = @splat(false),
    terminals: [capacity]TerminalRef = undefined,
    terminal_count: usize = 0,

    fn prepare(self: *Candidate, model: *const Model, snapshot: shared.Snapshot, previous: *const SessionView) !void {
        if (snapshot.windows.len > capacity or snapshot.nodes.len > self.seen.len) return error.WorkspaceCapacity;
        // Another showing coordinator's tabs keep their slots.
        for (0..model_module.max_windows) |window| self.counts[window] = retainedCount(model, window, self.owner);
        if (self.detached) return;
        for (snapshot.windows) |window| {
            if (hiddenWindow(self.hidden_windows, window.id)) continue;
            try self.prepareWindow(model, snapshot.nodes, window, self.count, previous);
        }
    }

    fn prepareWindow(self: *Candidate, model: *const Model, nodes: []const shared.Node, window: shared.Window, index: usize, previous: *const SessionView) !void {
        var placement = previous.find(window.id) orelse Placement{
            .id = window.id,
            .native_window = self.default_window,
            .window_epoch = model.window_epochs[self.default_window],
            .tab_id = 0,
            .focus = null,
            .selected = false,
        };
        if (!validPlacement(model, placement)) {
            placement.native_window = self.default_window;
            placement.window_epoch = model.window_epochs[placement.native_window];
            placement.tab_id = 0;
        }
        if (!model.windowOpen(placement.native_window)) return error.NoPresentationWindow;
        if (self.counts[placement.native_window] == model_module.max_tabs) return error.TabCapacity;
        for (self.placements[0..self.count]) |known| {
            if (std.mem.eql(u8, &known.id, &placement.id)) return error.DuplicateWindow;
        }
        var used: usize = 0;
        const tree = &self.trees[index];
        tree.attachment_id = self.owner.attachment_id;
        tree.root = try self.copyNode(tree, &used, nodes, window.root, layout.none);
        restoreFocus(tree, &placement);
        self.placements[index] = placement;
        self.counts[placement.native_window] += 1;
        self.count += 1;
    }

    fn restoreSelection(self: *Candidate, previous: *const SessionView) void {
        for (0..model_module.max_windows) |window| {
            var wanted: ?usize = null;
            for (previous.placements[0..previous.count]) |old| {
                if (old.native_window == window and old.selected) wanted = old.order;
            }
            self.selectNearest(window, wanted orelse continue);
        }
    }

    fn selectNearest(self: *Candidate, window: usize, wanted: usize) void {
        var best: ?usize = null;
        var distance: usize = std.math.maxInt(usize);
        for (self.placements[0..self.count], 0..) |placement, index| {
            if (placement.native_window != window) continue;
            if (placement.selected) return;
            const delta = if (placement.order > wanted) placement.order - wanted else wanted - placement.order;
            if (delta >= distance) continue;
            best = index;
            distance = delta;
        }
        if (best) |index| self.placements[index].selected = true;
    }

    fn copyNode(self: *Candidate, tree: *layout.Tree, used: *usize, nodes: []const shared.Node, index: u32, parent: layout.NodeId) anyerror!layout.NodeId {
        if (index >= nodes.len) return error.InvalidNode;
        if (self.seen[index]) return error.RepeatedNode;
        if (used.* == layout.max_nodes) return error.PaneCapacity;
        self.seen[index] = true;
        const out: layout.NodeId = @intCast(used.*);
        used.* += 1;
        const source = nodes[index];
        tree.nodes[out].parent = parent;
        if (source.kind == .leaf) {
            const ref = source.terminal_ref orelse return error.MissingTerminal;
            try self.recordTerminal(ref);
            tree.nodes[out].kind = .leaf;
            tree.nodes[out].terminal = ref;
            if (tree.focus == layout.none) tree.focus = out;
            return out;
        }
        if (!std.math.isFinite(source.ratio) or source.ratio <= 0 or source.ratio >= 1) return error.InvalidRatio;
        tree.nodes[out].kind = .branch;
        tree.nodes[out].orientation = if (source.kind == .horizontal) .horizontal else .vertical;
        tree.nodes[out].fraction = source.ratio;
        tree.nodes[out].first = try self.copyNode(tree, used, nodes, source.first, out);
        tree.nodes[out].second = try self.copyNode(tree, used, nodes, source.second, out);
        return out;
    }

    fn recordTerminal(self: *Candidate, ref: TerminalRef) !void {
        if (self.terminal_count == shared.max_replicas) return error.ReplicaCapacity;
        if (ref.provider_id != self.owner.authority) return error.MixedAuthority;
        if (self.terminal_count == self.terminals.len) return error.TerminalCapacity;
        for (self.terminals[0..self.terminal_count]) |known| {
            if (known.eql(ref)) return error.DuplicateTerminal;
        }
        self.terminals[self.terminal_count] = ref;
        self.terminal_count += 1;
    }

    fn publish(self: *const Candidate, model: *Model, previous: *const SessionView) void {
        const cleared = clearAuthority(model, self.owner);
        // Tagged attachments restore only their own window's Web selection.
        // Publication is asynchronous and must never raise a native window.
        const home = self.owner.primary(model);
        for (0..model_module.max_windows) |window| {
            const workspace = model.wsAt(window) orelse continue;
            if (self.restoresWeb(home, window, cleared[window])) workspace.web_selected = previous.web_selected[window];
            self.publishWindow(workspace, window, cleared[window]);
        }
        mintUnrestoredTabIds(model);
        const sibling_active = cleared[model.active_window].selected != null;
        if (home and self.owner.attachment_id == null and !sibling_active) restoreActiveWindow(model, previous);
        model.pruneAttachmentState();
    }

    fn restoresWeb(self: *const Candidate, home: bool, window: usize, cleared: Cleared) bool {
        if (cleared.selected != null) return false;
        if (self.owner.attachment_id == null) return home;
        return self.placementsIn(window) != 0;
    }

    /// Insert this window's placements where the group sat before (after
    /// the kept tabs when it had none), shifting later kept tabs right.
    fn publishWindow(self: *const Candidate, workspace: *model_module.Workspace, window: usize, cleared: Cleared) void {
        const incoming = self.placementsIn(window);
        const kept = workspace.tab_count;
        const at = @min(cleared.anchor orelse kept, kept);
        openGap(workspace, at, kept, incoming);
        const published = self.fillGap(workspace, window, at);
        workspace.tab_count = kept + incoming;
        workspace.selected_tab = selectedAfter(published, cleared.selected, at, incoming);
    }

    fn placementsIn(self: *const Candidate, window: usize) usize {
        var count: usize = 0;
        for (self.placements[0..self.count]) |placement| {
            if (placement.native_window == window) count += 1;
        }
        return count;
    }

    /// Write this window's placements into the gap at `at`; the tab a
    /// placement selects, if any.
    fn fillGap(self: *const Candidate, workspace: *model_module.Workspace, window: usize, at: usize) ?usize {
        var tab = at;
        var selected: ?usize = null;
        for (self.placements[0..self.count], 0..) |placement, index| {
            if (placement.native_window != window) continue;
            workspace.tabs[tab] = self.trees[index];
            workspace.shared_ids[tab] = placement.id;
            workspace.tab_ids[tab] = if (placement.tab_generation == workspace.tab_generation) placement.tab_id else 0;
            if (placement.selected) selected = tab;
            tab += 1;
        }
        return selected;
    }
};

fn restoreActiveWindow(model: *Model, previous: *const SessionView) void {
    if (model.windowOpen(previous.active_window) and model.window_epochs[previous.active_window] == previous.active_epoch) {
        model.active_window = previous.active_window;
    } else model.active_window = model.firstOpenWindow();
}

/// Shift the kept tabs from `at` right by `width`, leaving a gap there.
fn openGap(workspace: *model_module.Workspace, at: usize, kept: usize, width: usize) void {
    var from = kept;
    while (from > at) {
        from -= 1;
        workspace.tabs[from + width] = workspace.tabs[from];
        workspace.tab_ids[from + width] = workspace.tab_ids[from];
        workspace.shared_ids[from + width] = workspace.shared_ids[from];
    }
}

/// A retained sibling's current selection wins over cached incoming selection.
fn selectedAfter(published: ?usize, kept: ?usize, at: usize, width: usize) usize {
    if (kept) |value| return if (value >= at) value + width else value;
    return published orelse 0;
}

/// Reserve every preserved key before minting missing ones. A rollover can
/// revisit a key belonging to a later candidate; incremental publication would
/// not yet expose that reservation to the workspace allocator.
fn mintUnrestoredTabIds(model: *Model) void {
    for (0..model_module.max_windows) |window| {
        const workspace = model.wsAt(window) orelse continue;
        for (0..workspace.tab_count) |tab| workspace.assignRestoredTabId(tab);
    }
}

fn validPlacement(model: *const Model, placement: Placement) bool {
    return model.windowOpen(placement.native_window) and model.window_epochs[placement.native_window] == placement.window_epoch;
}

pub fn splitPath(tree: *const layout.Tree, node: layout.NodeId) !struct { bits: u64, len: u32 } {
    if (node >= layout.max_nodes or tree.nodes[node].kind != .branch) return error.InvalidNode;
    var current = node;
    var result: struct { bits: u64, len: u32 } = .{ .bits = 0, .len = 0 };
    while (current != tree.root) {
        if (result.len == layout.max_nodes) return error.InvalidNode;
        const parent = tree.nodes[current].parent;
        if (parent >= layout.max_nodes) return error.InvalidNode;
        const branch = tree.nodes[parent];
        if (branch.first != current and branch.second != current) return error.InvalidNode;
        result.bits = (result.bits << 1) | @as(u64, @intFromBool(branch.second == current));
        result.len += 1;
        current = parent;
    }
    return .{ .bits = result.bits, .len = result.len };
}

fn hasLeaf(snapshot: shared.Snapshot, ref: TerminalRef) bool {
    for (snapshot.nodes) |node| {
        if (node.kind != .leaf) continue;
        if (node.terminal_ref) |known| if (known.eql(ref)) return true;
    }
    return false;
}

fn hiddenWindow(ids: []const WindowId, id: WindowId) bool {
    for (ids) |known| if (std.mem.eql(u8, &known, &id)) return true;
    return false;
}

fn snapshotHasWindow(snapshot: shared.Snapshot, id: WindowId) bool {
    for (snapshot.windows) |window| if (std.mem.eql(u8, &window.id, &id)) return true;
    return false;
}

fn restoreFocus(tree: *layout.Tree, placement: *const Placement) void {
    const ref = placement.focus orelse return;
    if (tree.find(ref)) |node| {
        tree.focus = node;
        return;
    }
    var wanted: usize = 0;
    for (placement.neighbors[0..placement.neighbor_count], 0..) |old, index| {
        if (old.eql(ref)) wanted = index;
    }
    var distance: usize = std.math.maxInt(usize);
    for (placement.neighbors[0..placement.neighbor_count], 0..) |old, index| {
        const node = tree.find(old) orelse continue;
        const delta = if (index > wanted) index - wanted else wanted - index;
        if (delta >= distance) continue;
        distance = delta;
        tree.focus = node;
    }
}

fn testRef(id: u32) TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = id } } };
}

fn testWindow(id: u8, root: u32) shared.Window {
    return .{ .id = @splat(id), .root = root };
}

test "shared projection adopts every window and nested pane before replicas exist" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const windows = [_]shared.Window{ testWindow(1, 0), testWindow(2, 3) };
    const nodes = [_]shared.Node{
        .{ .kind = .horizontal, .first = 1, .second = 2, .ratio = 0.3 },
        .{ .kind = .leaf, .terminal_ref = testRef(11) },
        .{ .kind = .leaf, .terminal_ref = testRef(12) },
        .{ .kind = .leaf, .terminal_ref = testRef(13) },
    };
    const snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes };
    try std.testing.expect(try model.shared_workspace.apply(model, snapshot, 1));
    try std.testing.expectEqual(@as(usize, 2), model.primary.tab_count);
    try std.testing.expectEqual(@as(usize, 2), model.primary.tabs[0].paneCount());
    try std.testing.expectApproxEqAbs(@as(f32, 0.3), model.primary.tabs[0].nodes[0].fraction, 0.0001);
    try std.testing.expect(model.primary.tabs[0].find(testRef(12)) != null);
    try std.testing.expect(!model.containsTerminal(testRef(12)));
    try std.testing.expectEqual(@as(u8, 0), (try model.topologySnapshot()).tab_count);
}

test "shared reorder preserves window identity focus and process-local tab key" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const windows = [_]shared.Window{ testWindow(1, 0), testWindow(2, 3) };
    const nodes = [_]shared.Node{
        .{ .kind = .horizontal, .first = 1, .second = 2 },
        .{ .kind = .leaf, .terminal_ref = testRef(11) },
        .{ .kind = .leaf, .terminal_ref = testRef(12) },
        .{ .kind = .leaf, .terminal_ref = testRef(13) },
    };
    var snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes };
    _ = try model.shared_workspace.apply(model, snapshot, 1);
    try std.testing.expect(model.selectTerminal(testRef(12)));
    const key = model.primary.tab_ids[0];
    const reordered = [_]shared.Window{ windows[1], windows[0] };
    snapshot.windows = &reordered;
    snapshot.revision = 2;
    _ = try model.shared_workspace.apply(model, snapshot, 1);
    try std.testing.expectEqual(@as(usize, 1), model.primary.selected_tab);
    try std.testing.expectEqual(key, model.primary.tab_ids[1]);
    try std.testing.expect(model.focusedTerminalRef().?.eql(testRef(12)));
}

test "session A B A restores local native placement and exact terminal selection" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const aw = [_]shared.Window{testWindow(1, 0)};
    const an = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    const a: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &aw, .nodes = &an };
    const bw = [_]shared.Window{testWindow(2, 0)};
    const bn = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(21) }};
    const b: shared.Snapshot = .{ .session_id = 2, .revision = 1, .state = .authoritative, .windows = &bw, .nodes = &bn };
    model.shared_workspace.setContext(100);
    _ = model.openWindow(1).?;
    model.shared_workspace.placement_hint = .{ .shared_id = aw[0].id, .window = 1, .window_epoch = model.window_epochs[1] };
    _ = try model.shared_workspace.apply(model, a, 1);
    model.shared_workspace.placement_hint = null;
    model.active_window = 1;
    try model.shared_workspace.leaveSession(model);
    _ = try model.shared_workspace.apply(model, b, 2);
    try std.testing.expect(model.locateTerminal(testRef(11)) == null);
    try std.testing.expect(model.focusedTerminalRef().?.eql(testRef(21)));
    try model.shared_workspace.leaveSession(model);
    model.shared_workspace.desired_terminal = testRef(11);
    _ = try model.shared_workspace.apply(model, a, 3);
    try std.testing.expectEqual(@as(usize, 1), model.active_window);
    try std.testing.expectEqual(@as(usize, 0), model.primary.tab_count);
    try std.testing.expectEqual(@as(usize, 1), model.wsAt(1).?.tab_count);
    try std.testing.expect(model.focusedTerminalRef().?.eql(testRef(11)));
    try std.testing.expect(model.shared_workspace.desired_terminal == null);
}

test "cached session tab identity cannot alias a newer allocation generation" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const commands = @import("native/tab_commands.zig");
    const aw = [_]shared.Window{testWindow(1, 0)};
    const an = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    const a: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &aw, .nodes = &an };
    const bw = [_]shared.Window{ testWindow(2, 0), testWindow(3, 1), testWindow(4, 2) };
    const bn = [_]shared.Node{ .{ .kind = .leaf, .terminal_ref = testRef(21) }, .{ .kind = .leaf, .terminal_ref = testRef(22) }, .{ .kind = .leaf, .terminal_ref = testRef(23) } };
    const b: shared.Snapshot = .{ .session_id = 2, .revision = 1, .state = .authoritative, .windows = &bw, .nodes = &bn };
    _ = try model.shared_workspace.apply(model, a, 1);
    const old = commands.capture(model, 0, 0).?;
    try model.shared_workspace.leaveSession(model);
    model.primary.next_tab_id = std.math.maxInt(u32);
    _ = try model.shared_workspace.apply(model, b, 2);
    const newer = commands.capture(model, 0, 2).?;
    try std.testing.expectEqual(old.tab_id, newer.tab_id);
    try std.testing.expect(old.tab_generation != newer.tab_generation);
    try model.shared_workspace.leaveSession(model);
    _ = try model.shared_workspace.apply(model, a, 3);
    try std.testing.expect(newer.resolve(model) == null);
    try std.testing.expect(old.resolve(model) == null);
}

test "shared reconstruction reserves later preserved tab IDs before rollover allocation" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    var snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes };
    _ = try model.shared_workspace.apply(model, snapshot, 1);
    const preserved = model.primary.tab_ids[0];
    model.primary.next_tab_id = std.math.maxInt(u32);
    const replacement = [_]shared.Window{ testWindow(2, 0), testWindow(3, 1), testWindow(4, 2), testWindow(1, 3) };
    const expanded = [_]shared.Node{
        .{ .kind = .leaf, .terminal_ref = testRef(21) },
        .{ .kind = .leaf, .terminal_ref = testRef(22) },
        .{ .kind = .leaf, .terminal_ref = testRef(23) },
        .{ .kind = .leaf, .terminal_ref = testRef(11) },
    };
    snapshot.windows = &replacement;
    snapshot.nodes = &expanded;
    snapshot.revision = 2;
    _ = try model.shared_workspace.apply(model, snapshot, 1);
    try std.testing.expectEqual(preserved, model.primary.tab_ids[3]);
    for (model.primary.tab_ids[0..4], 0..) |id, index| {
        for (model.primary.tab_ids[index + 1 .. 4]) |later| try std.testing.expect(id != later);
    }
}

test "invalid shared replacement leaves last good geometry selection and revision intact" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const windows = [_]shared.Window{testWindow(1, 0)};
    const good = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    var snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &good };
    _ = try model.shared_workspace.apply(model, snapshot, 1);
    const fingerprint = model.topologyFingerprint();
    const cycle = [_]shared.Node{.{ .kind = .horizontal, .first = 0, .second = 0 }};
    snapshot.nodes = &cycle;
    snapshot.revision = 2;
    try std.testing.expectError(error.RepeatedNode, model.shared_workspace.apply(model, snapshot, 1));
    try std.testing.expectEqual(fingerprint, model.topologyFingerprint());
    try std.testing.expectEqual(@as(u64, 1), model.shared_workspace.revision);
}

test "shared discovery uses an open native window and rejects recycled placement epochs" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    _ = model.openWindow(1).?;
    model.closeWindow(0);
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    const snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes };
    _ = try model.shared_workspace.apply(model, snapshot, 1);
    try std.testing.expectEqual(@as(usize, 1), model.locateTerminal(testRef(11)).?.window);
    try model.shared_workspace.leaveSession(model);
    model.closeWindow(1);
    _ = model.openWindow(0).?;
    _ = model.openWindow(1).?;
    _ = try model.shared_workspace.apply(model, snapshot, 2);
    try std.testing.expectEqual(@as(usize, 0), model.locateTerminal(testRef(11)).?.window);
}

test "shared deletion picks nearest surviving tab and pane while web remains client-local" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const windows = [_]shared.Window{ testWindow(1, 0), testWindow(2, 1), testWindow(3, 2) };
    const nodes = [_]shared.Node{
        .{ .kind = .leaf, .terminal_ref = testRef(11) },
        .{ .kind = .leaf, .terminal_ref = testRef(12) },
        .{ .kind = .leaf, .terminal_ref = testRef(13) },
    };
    var snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes };
    _ = try model.shared_workspace.apply(model, snapshot, 1);
    try std.testing.expect(model.selectTerminal(testRef(13)));
    snapshot.windows = windows[0..2];
    snapshot.revision = 2;
    _ = try model.shared_workspace.apply(model, snapshot, 1);
    try std.testing.expect(model.focusedTerminalRef().?.eql(testRef(12)));
    model.selectWeb();
    snapshot.revision = 3;
    _ = try model.shared_workspace.apply(model, snapshot, 1);
    try std.testing.expect(model.primary.web_selected);
    model.shared_workspace.refused = true;
    const generation = model.shared_workspace.projection_generation;
    try std.testing.expect(try model.shared_workspace.apply(model, snapshot, 1));
    try std.testing.expect(!model.shared_workspace.refused);
    try std.testing.expectEqual(generation, model.shared_workspace.projection_generation);
}

test "unknown shared completion remains distinct from refusal after real projection" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    var snapshot: shared.Snapshot = .{
        .session_id = 1,
        .revision = 1,
        .state = .authoritative,
        .windows = &windows,
        .nodes = &nodes,
        .status = .unknown_outcome,
    };
    _ = try model.shared_workspace.apply(model, snapshot, 1);
    try std.testing.expect(!model.shared_workspace.refused);
    try std.testing.expectEqual(@as(usize, 1), model.wsConst().tab_count);
    snapshot.status = .refused;
    try std.testing.expect(try model.shared_workspace.apply(model, snapshot, 1));
    try std.testing.expect(model.shared_workspace.refused);
    const generation = model.shared_workspace.projection_generation;
    snapshot.status = .unknown_outcome;
    try std.testing.expect(try model.shared_workspace.apply(model, snapshot, 1));
    try std.testing.expect(!model.shared_workspace.refused);
    try std.testing.expectEqual(generation, model.shared_workspace.projection_generation);
}

test "superseded navigation and replacement server discard deferred focus and placement" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    model.shared_workspace.setContext(1);
    model.shared_workspace.desired_terminal = testRef(42);
    try model.shared_workspace.leaveSession(model);
    try std.testing.expect(model.shared_workspace.desired_terminal == null);
    model.shared_workspace.desired_terminal = testRef(42);
    model.shared_workspace.placement_hint = .{ .shared_id = @splat(1), .window = 0, .window_epoch = 0 };
    model.shared_workspace.setContext(2);
    try std.testing.expect(model.shared_workspace.desired_terminal == null);
    try std.testing.expect(model.shared_workspace.placement_hint == null);
}

test "authoritative empty clears presentation while unavailable preserves last good" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    _ = try model.shared_workspace.apply(model, .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes }, 1);
    try std.testing.expect(!try model.shared_workspace.apply(model, .{ .session_id = 1, .revision = 2, .state = .unavailable, .status = .pending }, 1));
    try std.testing.expect(model.locateTerminal(testRef(11)) != null);
    _ = try model.shared_workspace.apply(model, .{ .session_id = 1, .revision = 3, .state = .authoritative }, 1);
    try std.testing.expectEqual(@as(usize, 0), model.primary.tab_count);
    try std.testing.expect(model.locateTerminal(testRef(11)) == null);
    try std.testing.expect(model.primary_open);
}

test "empty session restores its local Web selection after visiting another session" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const empty: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative };
    _ = try model.shared_workspace.apply(model, empty, 1);
    model.selectWeb();
    try model.shared_workspace.leaveSession(model);
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    _ = try model.shared_workspace.apply(model, .{ .session_id = 2, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes }, 2);
    try std.testing.expect(model.selectTerminal(testRef(11)));
    try std.testing.expect(!model.primary.web_selected);
    try model.shared_workspace.leaveSession(model);
    _ = try model.shared_workspace.apply(model, empty, 3);
    try std.testing.expect(model.primary.web_selected);
    try std.testing.expectEqual(@as(usize, 0), model.primary.tab_count);
}

test "native close rehomes shared tabs with identity and selection intact" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    _ = model.openWindow(1).?;
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    _ = try model.shared_workspace.apply(model, .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes }, 1);
    try std.testing.expect(model.selectTerminal(testRef(11)));
    try closeNativeWindow(model, 0);
    try std.testing.expect(!model.primary_open);
    try std.testing.expectEqual(@as(usize, 1), model.locateTerminal(testRef(11)).?.window);
    try std.testing.expectEqualSlices(u8, &windows[0].id, &model.wsAt(1).?.shared_ids[0].?);
    try std.testing.expect(model.focusedTerminalRef().?.eql(testRef(11)));
    try std.testing.expectEqual(@as(u64, 1), model.shared_workspace.revision);
}

test "split resize path records root to branch bits and rejects a leaf" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{
        .{ .kind = .horizontal, .first = 1, .second = 2 },
        .{ .kind = .leaf, .terminal_ref = testRef(11) },
        .{ .kind = .vertical, .first = 3, .second = 4 },
        .{ .kind = .leaf, .terminal_ref = testRef(12) },
        .{ .kind = .horizontal, .first = 5, .second = 6 },
        .{ .kind = .leaf, .terminal_ref = testRef(13) },
        .{ .kind = .leaf, .terminal_ref = testRef(14) },
    };
    _ = try model.shared_workspace.apply(model, .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes }, 1);
    const tree = &model.primary.tabs[0];
    const branch = tree.nodes[tree.find(testRef(13)).?].parent;
    const path = try splitPath(tree, branch);
    try std.testing.expectEqual(@as(u64, 3), path.bits);
    try std.testing.expectEqual(@as(u32, 2), path.len);
    try std.testing.expectError(error.InvalidNode, splitPath(tree, tree.find(testRef(13)).?));
    try std.testing.expectEqual(@as(u32, 0), (try splitPath(tree, tree.root)).len);
}

test "removed focused leaf selects its nearest surviving spatial neighbor" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{
        .{ .kind = .horizontal, .first = 1, .second = 2 },
        .{ .kind = .leaf, .terminal_ref = testRef(11) },
        .{ .kind = .vertical, .first = 3, .second = 4 },
        .{ .kind = .leaf, .terminal_ref = testRef(12) },
        .{ .kind = .leaf, .terminal_ref = testRef(13) },
    };
    _ = try model.shared_workspace.apply(model, .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes }, 1);
    try std.testing.expect(model.selectTerminal(testRef(13)));
    const remaining = [_]shared.Node{ nodes[0], nodes[1], nodes[3] };
    _ = try model.shared_workspace.apply(model, .{ .session_id = 1, .revision = 2, .state = .authoritative, .windows = &windows, .nodes = &remaining }, 1);
    try std.testing.expect(model.focusedTerminalRef().?.eql(testRef(12)));
}

test "subscription churn releases completed slots and retries refusal after fresh evidence" {
    const Remote = struct {
        ref: TerminalRef,
        revision: u64,
        known: bool = false,
        nodes: [1]shared.Node,
        pub fn terminalKnown(self: *const @This(), ref: TerminalRef) bool {
            return self.known and self.ref.eql(ref);
        }
        pub fn workspaceSnapshot(self: *const @This()) shared.Snapshot {
            return .{ .revision = 1, .request_id = @intCast(self.revision), .status = .confirmed, .nodes = &self.nodes };
        }
    };
    var state: State = .{ .revision = 1 };
    defer state.deinit();
    for (1..65) |id| {
        const ref = testRef(@intCast(id));
        var remote: Remote = .{ .ref = ref, .revision = id, .known = true, .nodes = .{.{ .kind = .leaf, .terminal_ref = ref }} };
        state.subscriptions[0] = .{ .ref = ref, .request = @intCast(id), .epoch = 1, .completed = true, .succeeded = true };
        state.pruneSubscriptions(&remote);
        try std.testing.expect(state.subscriptions[0] == null);
    }
    const ref = testRef(99);
    state.subscriptions[0] = .{ .ref = ref, .request = 70, .epoch = 2 };
    var remote: Remote = .{ .ref = ref, .revision = 1, .nodes = .{.{ .kind = .leaf, .terminal_ref = ref }} };
    remote.known = true;
    state.pruneSubscriptions(&remote);
    try std.testing.expect(state.subscriptions[0] != null);
    remote.known = false;
    const Result = @import("phux_support.zig").OperationResult;
    var result: Result = .{ .kind = .attach, .status = .refused, .request_id = 70, .connection_epoch = 1, .terminal_ref = ref, .error_domain = .none, .error_code = 0 };
    try std.testing.expect(!state.completeSubscription(result, 1));
    result.connection_epoch = 2;
    try std.testing.expect(state.completeSubscription(result, 1));
    state.pruneSubscriptions(&remote);
    try std.testing.expect(state.subscriptions[0] != null);
    remote.revision = 2;
    state.pruneSubscriptions(&remote);
    try std.testing.expect(state.subscriptions[0] == null);
}

test "projection refuses a seventeenth replica before replacing last good composition" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    _ = try model.shared_workspace.apply(model, .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes }, 1);
    var oversized_windows: [16]shared.Window = undefined;
    var oversized_nodes: [18]shared.Node = undefined;
    oversized_nodes[0] = .{ .kind = .horizontal, .first = 1, .second = 2 };
    for (1..18) |index| oversized_nodes[index] = .{ .kind = .leaf, .terminal_ref = testRef(@intCast(index)) };
    oversized_windows[0] = testWindow(1, 0);
    for (1..16) |index| oversized_windows[index] = testWindow(@intCast(index + 1), @intCast(index + 2));
    try std.testing.expectError(error.ReplicaCapacity, model.shared_workspace.apply(model, .{ .session_id = 1, .revision = 2, .state = .authoritative, .windows = &oversized_windows, .nodes = &oversized_nodes }, 1));
    try std.testing.expectEqual(@as(u64, 1), model.shared_workspace.revision);
    try std.testing.expectEqual(@as(usize, 1), model.primary.tab_count);
    try std.testing.expect(model.focusedTerminalRef().?.eql(testRef(11)));
}

test "same coordinator sessions preserve independent windows through update cold reconnect and detach" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    _ = model.openWindow(1).?;
    var a: State = .{ .attachment_id = 100 };
    defer a.deinit();
    var b: State = .{ .attachment_id = 200 };
    defer b.deinit();
    a.setContext(1);
    b.setContext(1);
    // Deliberately collide shared window and terminal IDs: neither is an
    // attachment identity. The second connection must never adopt A's view.
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    var snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes };
    _ = try a.apply(model, snapshot, 1);
    const a_key = model.primary.tab_ids[0];
    model.primary.web_selected = true;
    b.placement_hint = .{ .shared_id = windows[0].id, .window = 1, .window_epoch = model.window_epochs[1] };
    snapshot.session_id = 2;
    _ = try b.apply(model, snapshot, 1);
    try std.testing.expectEqual(@as(usize, 1), model.primary.tab_count);
    try std.testing.expectEqual(a_key, model.primary.tab_ids[0]);
    try std.testing.expect(model.primary.web_selected);
    try std.testing.expectEqual(@as(usize, 1), model.wsAt(1).?.tab_count);
    try std.testing.expectEqual(@as(usize, 0), model.active_window);
    snapshot.revision = 2;
    _ = try b.apply(model, snapshot, 1);
    try std.testing.expectEqual(a_key, model.primary.tab_ids[0]);
    try std.testing.expect(model.primary.web_selected);
    b.setContext(2);
    b.presentation_window = .{ .window = 1, .window_epoch = model.window_epochs[1] };
    snapshot.revision = 1;
    _ = try b.apply(model, snapshot, 2);
    try std.testing.expectEqual(a_key, model.primary.tab_ids[0]);
    try std.testing.expectEqual(@as(?u64, 100), model.primary.tabs[0].attachment_id);
    try std.testing.expectEqual(@as(?u64, 200), model.wsAt(1).?.tabs[0].attachment_id);
    b.desired_terminal = testRef(11);
    try std.testing.expect(b.selectDesired(model));
    try std.testing.expectEqual(@as(usize, 1), model.active_window);
    try b.leaveSession(model);
    try std.testing.expectEqual(@as(usize, 0), model.wsAt(1).?.tab_count);
    try std.testing.expectEqual(a_key, model.primary.tab_ids[0]);
    try std.testing.expect(model.primary.web_selected);
}

test "scoped projection selects colliding refs in the exact attachment tab and retains neighboring selection" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    var a: State = .{ .attachment_id = 100 };
    defer a.deinit();
    var b: State = .{ .attachment_id = 200 };
    defer b.deinit();
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    var snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes };
    _ = try a.apply(model, snapshot, 1);
    snapshot.session_id = 2;
    _ = try b.apply(model, snapshot, 1);
    try std.testing.expectEqual(@as(usize, 2), model.primary.tab_count);
    try std.testing.expectEqual(@as(usize, 0), model.primary.selected_tab);
    b.desired_terminal = testRef(11);
    try std.testing.expect(b.selectDesired(model));
    try std.testing.expectEqual(@as(usize, 1), model.primary.selected_tab);
    try b.leaveSession(model);
    // B remembers its old selection, but A is now selected by the user.
    _ = try b.apply(model, snapshot, 2);
    try std.testing.expectEqual(@as(usize, 0), model.primary.selected_tab);
    try std.testing.expectEqual(@as(?u64, 100), model.primary.tabs[0].attachment_id);
}

test "same endpoint detach queue ignores a sibling attachment showing the same terminal" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    const endpoint: support.PhuxEndpoint = .{ .unix = "/projection-fixture-unused" };
    const a = try support.PhuxProvider.create(std.testing.allocator, std.testing.io, endpoint, null, "a");
    model.phux_provider = a;
    try model.ensurePeerSlots(1);
    const b = try support.PhuxProvider.create(std.testing.allocator, std.testing.io, endpoint, null, "b");
    model.peers.items[0].provider = b;
    const fixture = support.PhuxProvider.test_support;
    try fixture.attachHost(a.host);
    try fixture.attachHost(b.host);
    try std.testing.expectEqual(a.providerId(), b.providerId());
    var state: State = .{ .authority = b.providerId(), .attachment_id = b.context_id };
    defer state.deinit();
    try std.testing.expect(state.providerForProjection(model).? == b);
    var refs: [capacity]TerminalRef = undefined;
    const count = b.terminalRefs(&refs);
    try std.testing.expect(count > 0);
    model.shared_workspace.authority = a.providerId();
    model.shared_workspace.attachment_id = a.context_id;
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = refs[0] }};
    _ = try model.shared_workspace.apply(model, .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes }, 1);
    state.releaseUnused(model);
    try fixture.expectOutgoingCount(a.bridge, 0);
    try fixture.expectOutgoingCount(b.bridge, 1);
    // Removing B never turns its stale projection into an alias for A.
    model.peers.items[0].provider = null;
    defer b.destroy();
    try std.testing.expect(state.providerForProjection(model) == null);
}

test "subscription completions require attachment identity as well as request and epoch" {
    var state: State = .{ .attachment_id = 200 };
    defer state.deinit();
    const ref = testRef(11);
    state.subscriptions[0] = .{ .ref = ref, .request = 7, .epoch = 2 };
    var result: support.OperationResult = .{ .kind = .attach, .status = .success, .request_id = 7, .connection_epoch = 2, .terminal_ref = ref, .error_domain = .none, .error_code = 0 };
    try std.testing.expect(!state.completeSubscription(result, 1));
    try std.testing.expect(!state.completeSubscriptionFrom(100, result, 1));
    try std.testing.expect(!state.subscriptions[0].?.completed);
    result.connection_epoch = 1;
    try std.testing.expect(!state.completeSubscriptionFrom(200, result, 1));
    result.connection_epoch = 2;
    try std.testing.expect(state.completeSubscriptionFrom(200, result, 1));
    state.setContext(1);
    try std.testing.expect(!state.completeSubscriptionFrom(200, result, 1));
}

test "closing session views suppresses rediscovery until explicit show without disturbing a sibling" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    _ = model.openWindow(1).?;
    _ = model.openWindow(2).?;
    var a: State = .{ .attachment_id = 100 };
    defer a.deinit();
    var b: State = .{ .attachment_id = 200 };
    defer b.deinit();
    const windows = [_]shared.Window{ testWindow(1, 0), testWindow(2, 1), testWindow(3, 2) };
    const nodes = [_]shared.Node{
        .{ .kind = .leaf, .terminal_ref = testRef(11) },
        .{ .kind = .leaf, .terminal_ref = testRef(12) },
        .{ .kind = .leaf, .terminal_ref = testRef(13) },
    };
    a.showInWindow(0, model.window_epochs[0]);
    a.placement_hint = .{ .shared_id = windows[1].id, .window = 1, .window_epoch = model.window_epochs[1] };
    var snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = windows[0..2], .nodes = &nodes };
    _ = try a.apply(model, snapshot, 1);
    b.showInWindow(2, model.window_epochs[2]);
    _ = try b.apply(model, .{ .session_id = 2, .revision = 1, .state = .authoritative, .windows = windows[0..1], .nodes = nodes[0..1] }, 1);
    const b_key = model.wsAt(2).?.tab_ids[0];
    try std.testing.expectEqual(@as(usize, 1), model.wsAt(1).?.tab_count);
    try a.hideNativeWindow(model, 1);
    model.closeWindow(1);
    snapshot.revision = 2;
    _ = try a.apply(model, snapshot, 1);
    try std.testing.expectEqual(@as(usize, 1), model.primary.tab_count);
    try std.testing.expect(a.locate(model, testRef(12)) == null);
    try std.testing.expectEqual(b_key, model.wsAt(2).?.tab_ids[0]);
    try a.hideNativeWindow(model, 0);
    model.closeWindow(0);
    // New remote layout does not revive a completely detached presentation.
    snapshot.windows = &windows;
    snapshot.revision = 3;
    _ = try a.apply(model, snapshot, 2);
    try std.testing.expect(a.detached);
    try std.testing.expectEqual(@as(usize, 1), model.wsAt(2).?.tab_count);
    a.showInWindow(2, model.window_epochs[2]);
    // Explicit reopening invalidates the unchanged revision/epoch fast path.
    try std.testing.expect(try a.apply(model, snapshot, 2));
    try std.testing.expectEqual(@as(usize, 4), model.wsAt(2).?.tab_count);
    try std.testing.expectEqual(b_key, model.wsAt(2).?.tab_ids[0]);
    try std.testing.expectEqual(@as(usize, 0), model.wsAt(2).?.selected_tab);
    try std.testing.expect(a.locate(model, testRef(12)) != null);
}

test "delayed first session publication refuses a recycled native destination" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    _ = model.openWindow(1).?;
    var state: State = .{ .attachment_id = 100 };
    defer state.deinit();
    state.showInWindow(1, model.window_epochs[1]);
    model.closeWindow(1);
    _ = model.openWindow(1).?;
    const primary_tabs = model.primary.tab_count;
    const secondary_tabs = model.wsAt(1).?.tab_count;
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    const snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes };
    try std.testing.expectError(error.StalePresentationWindow, state.apply(model, snapshot, 1));
    try std.testing.expectEqual(@as(u64, 0), state.revision);
    try std.testing.expectEqual(primary_tabs, model.primary.tab_count);
    try std.testing.expectEqual(secondary_tabs, model.wsAt(1).?.tab_count);
    try std.testing.expect(state.locate(model, testRef(11)) == null);
    state.showInWindow(1, model.window_epochs[1]);
    _ = try state.apply(model, snapshot, 1);
    try std.testing.expectEqual(@as(usize, 1), model.wsAt(1).?.tab_count);
    try std.testing.expectEqual(@as(usize, 0), model.active_window);
}

test "tagged attachment cannot clear an unqualified same coordinator projection" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    _ = model.openWindow(1).?;
    var legacy: State = .{};
    defer legacy.deinit();
    var tagged: State = .{ .attachment_id = 200 };
    defer tagged.deinit();
    const windows = [_]shared.Window{testWindow(1, 0)};
    const nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(11) }};
    var snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes };
    _ = try legacy.apply(model, snapshot, 1);
    const key = model.primary.tab_ids[0];
    model.primary.web_selected = true;
    tagged.showInWindow(1, model.window_epochs[1]);
    snapshot.session_id = 2;
    _ = try tagged.apply(model, snapshot, 1);
    try std.testing.expectEqual(@as(usize, 1), model.primary.tab_count);
    try std.testing.expectEqual(key, model.primary.tab_ids[0]);
    try std.testing.expect(model.primary.web_selected);
    try tagged.leaveSession(model);
    try std.testing.expectEqual(key, model.primary.tab_ids[0]);
    try std.testing.expect(model.primary.web_selected);
}

test "legacy cached selection cannot override retained tagged tabs web or active window" {
    const engine = try @import("native/ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    _ = model.openWindow(1).?;
    var legacy: State = .{};
    defer legacy.deinit();
    var tagged: State = .{ .attachment_id = 200 };
    defer tagged.deinit();
    const windows = [_]shared.Window{ testWindow(1, 0), testWindow(2, 1) };
    const nodes = [_]shared.Node{ .{ .kind = .leaf, .terminal_ref = testRef(11) }, .{ .kind = .leaf, .terminal_ref = testRef(12) } };
    tagged.placement_hint = .{ .shared_id = windows[1].id, .window = 1, .window_epoch = model.window_epochs[1] };
    _ = try tagged.apply(model, .{ .session_id = 2, .revision = 1, .state = .authoritative, .windows = &windows, .nodes = &nodes }, 1);
    const legacy_windows = [_]shared.Window{testWindow(3, 0)};
    const legacy_nodes = [_]shared.Node{.{ .kind = .leaf, .terminal_ref = testRef(13) }};
    const snapshot: shared.Snapshot = .{ .session_id = 1, .revision = 1, .state = .authoritative, .windows = &legacy_windows, .nodes = &legacy_nodes };
    _ = try legacy.apply(model, snapshot, 1);
    legacy.desired_terminal = testRef(13);
    try std.testing.expect(legacy.selectDesired(model));
    try legacy.leaveSession(model);
    model.active_window = 1;
    model.wsAt(1).?.web_selected = true;
    _ = try legacy.apply(model, snapshot, 2);
    try std.testing.expectEqual(@as(?u64, 200), model.primary.tabs[model.primary.selected_tab].attachment_id);
    try std.testing.expect(model.wsAt(1).?.web_selected);
    try std.testing.expectEqual(@as(usize, 1), model.active_window);
}
