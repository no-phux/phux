//! Opaque catalog authority: full provider identity plus process-local context.
//! Placement, metadata, query and page revisions are deliberately absent.
const std = @import("std");
const model_module = @import("../model.zig");
const support = @import("../phux_support.zig");
const layout = @import("../layout.zig");
const window_navigation = @import("ts_window_navigation.zig");
const Model = model_module.Model;
const Entry = model_module.PaletteDestination;
const TerminalRef = model_module.TerminalRef;

pub const max_len = 34 + 9 + support.RemoteResourceId.max_host_bytes;
/// `peer_session` names a session of the peer coordinator `Target.provider_id`
/// names; its context is that provider's, so a replaced, retargeted or
/// dropped peer invalidates it. `session` is the active coordinator's.
pub const Resource = union(enum) { terminal: TerminalRef, session: u32, peer_session: u32 };

pub const Context = struct {
    provider: u64,
    host: u64 = 0,
    epoch: u64 = 0,
};

pub const Target = struct {
    provider_id: support.ProviderId,
    context: Context,
    resource: Resource,

    pub fn resolve(self: Target, model: *const Model) ?Entry {
        if (self.resource == .peer_session) return resolvePeerSession(model, self.provider_id, self.context, self.resource.peer_session);
        const current = contextFor(model, self.provider_id) orelse return null;
        if (!std.meta.eql(current, self.context)) return null;
        return switch (self.resource) {
            .terminal => |ref| if (ref.provider_id == self.provider_id) resolveTerminal(model, ref) else null,
            // A session by number is only the active coordinator's.
            .session => |id| if (activeId(model) == self.provider_id) resolveSession(model, id) else null,
            .peer_session => unreachable,
        };
    }

    pub fn encode(self: Target, out: *[max_len]u8) []const u8 {
        out[0] = 2;
        std.mem.writeInt(u64, out[2..10], @intFromEnum(self.provider_id), .little);
        std.mem.writeInt(u64, out[10..18], self.context.provider, .little);
        std.mem.writeInt(u64, out[18..26], self.context.host, .little);
        std.mem.writeInt(u64, out[26..34], self.context.epoch, .little);
        switch (self.resource) {
            .session => |id| {
                out[1] = 2;
                std.mem.writeInt(u32, out[34..38], id, .little);
                return out[0..38];
            },
            .peer_session => |id| {
                out[1] = 3;
                std.mem.writeInt(u32, out[34..38], id, .little);
                return out[0..38];
            },
            .terminal => |ref| return encodeTerminal(ref, out),
        }
    }
};

/// A coordinator's context, whichever of the held coordinators minted `id`:
/// its provider and host lifetimes and its connection. A ref of a
/// coordinator no longer held has none, so it resolves to nothing.
fn contextFor(model: *const Model, id: support.ProviderId) ?Context {
    if (id == .local) return .{ .provider = model.provider.context_id };
    if (comptime !support.phux_enabled) return null;
    const remote = model.phuxForConst(id) orelse return null;
    return .{ .provider = remote.context_id, .host = remote.host.context_id, .epoch = remote.connectionEpoch() };
}

fn activeId(model: *const Model) ?support.ProviderId {
    if (comptime !support.phux_enabled) return null;
    const remote = model.phuxConst() orelse return null;
    return remote.providerId();
}

/// A peer coordinator's context: its own provider and host lifetimes and
/// the connection its session catalog came from. Exchanging, retargeting or
/// dropping the peer changes it.
fn peerContext(model: *const Model, coordinator: support.ProviderId) ?Context {
    if (comptime !support.phux_enabled) return null;
    const slot = model.peerSlot(coordinator) orelse return null;
    const peer = model.phuxPeerAtConst(slot).?;
    return .{ .provider = peer.context_id, .host = peer.host.context_id, .epoch = peer.host.sessions_generation };
}

pub fn capture(model: *const Model, entry: Entry) ?Target {
    return switch (entry) {
        .placed_terminal => |placed| captureTerminal(model, entry, placed.terminal_ref),
        .available_terminal => |ref| captureTerminal(model, entry, ref),
        .session => |id| captureSession(model, entry, id),
        .peer_session => |target| capturePeerSession(model, target.coordinator, target.id),
        // No session is numbered 0: it names the group's retry while failed.
        .peer_unavailable => |coordinator| capturePeerSession(model, coordinator, 0),
    };
}

/// Held against the coordinator that minted the ref.
fn captureTerminal(model: *const Model, entry: Entry, ref: TerminalRef) ?Target {
    var context = contextFor(model, ref.provider_id) orelse return null;
    if (ref.provider_id != .local) context.epoch = inventoryEpoch(model, entry);
    return .{ .provider_id = ref.provider_id, .context = context, .resource = .{ .terminal = ref } };
}

/// A session by number is the active coordinator's.
fn captureSession(model: *const Model, entry: Entry, id: u32) ?Target {
    const provider_id = activeId(model) orelse return null;
    var context = contextFor(model, provider_id) orelse return null;
    context.epoch = inventoryEpoch(model, entry);
    return .{ .provider_id = provider_id, .context = context, .resource = .{ .session = id } };
}

fn capturePeerSession(model: *const Model, coordinator: support.ProviderId, id: u32) ?Target {
    const context = peerContext(model, coordinator) orelse return null;
    return .{ .provider_id = coordinator, .context = context, .resource = .{ .peer_session = id } };
}

fn inventoryEpoch(model: *const Model, entry: Entry) u64 {
    if (comptime !support.phux_enabled) return 0;
    return switch (entry) {
        .placed_terminal => |placed| blk: {
            const remote = model.phuxForRefConst(placed.terminal_ref) orelse break :blk 0;
            break :blk if (remote.owner(placed.terminal_ref)) |owner| owner.generation.epoch_id else 0;
        },
        .available_terminal => |ref| blk: {
            const remote = model.phuxForRefConst(ref) orelse break :blk 0;
            break :blk if (catalogContains(model, ref)) remote.connectionEpoch() else 0;
        },
        .session => if (model.phuxConst()) |remote| remote.host.sessions_generation else 0,
        .peer_session, .peer_unavailable => 0,
    };
}

fn placedReplicaCurrent(model: *const Model, ref: TerminalRef) bool {
    if (ref.provider_id == .local) return true;
    const remote = model.phuxForRefConst(ref) orelse return false;
    const owner = remote.owner(ref) orelse return false;
    return owner.generation.epoch_id == remote.connectionEpoch();
}

fn resolveTerminal(model: *const Model, ref: TerminalRef) ?Entry {
    if (model.locateTerminal(ref)) |location| {
        if (!model.containsTerminal(ref)) return null;
        if (!placedReplicaCurrent(model, ref)) return null;
        return .{ .placed_terminal = .{ .window = @intCast(location.window), .tab = @intCast(location.tab), .terminal_ref = ref } };
    }
    if (ref.provider_id == .local) return null;
    if (!catalogContains(model, ref)) return null;
    return .{ .available_terminal = ref };
}

fn catalogContains(model: *const Model, ref: TerminalRef) bool {
    const remote = model.phuxForRefConst(ref) orelse return false;
    for (remote.catalogTerminals()) |terminal| {
        if (terminal.terminal_ref.eql(ref)) return true;
    }
    return false;
}

fn resolveSession(model: *const Model, id: u32) ?Entry {
    if (comptime !support.phux_enabled) return null;
    const remote = model.phuxConst() orelse return null;
    if (remote.host.sessions_generation != remote.connectionEpoch()) return null;
    for (remote.sessionCatalog()) |session| {
        if (session.id == id) return .{ .session = id };
    }
    return null;
}

/// Held against the peer that listed it: exchanging, retargeting or dropping
/// that peer, or a catalog from an older connection of it, refuses. Shown by
/// id, so an unnamed session is as selectable as a named one.
fn resolvePeerSession(model: *const Model, coordinator: support.ProviderId, expected: Context, id: u32) ?Entry {
    if (comptime !support.phux_enabled) return null;
    const current = peerContext(model, coordinator) orelse return null;
    if (!std.meta.eql(current, expected)) return null;
    const slot = model.peerSlot(coordinator) orelse return null;
    // Session 0 is the group's unavailable row: it retries a failed peer.
    if (id == 0) return if (model.peers.items[slot].failed) .{ .peer_unavailable = coordinator } else null;
    const peer = model.phuxPeerAtConst(slot).?;
    // A peer that disconnected, is being retargeted, or listed on an older
    // connection offers nothing; forgetting also moved the context above.
    for (peer.standbyCatalog()) |session| {
        if (session.id == id) return .{ .peer_session = .{ .coordinator = coordinator, .id = id } };
    }
    return null;
}

fn encodeTerminal(ref: TerminalRef, out: *[max_len]u8) []const u8 {
    switch (ref.terminal_id) {
        .local => |id| {
            out[1] = 0;
            std.mem.writeInt(u64, out[34..42], @intFromEnum(id), .little);
            return out[0..42];
        },
        .phux => |id| {
            out[1] = 1;
            std.mem.writeInt(u32, out[34..38], id.kind, .little);
            std.mem.writeInt(u32, out[38..42], id.id, .little);
            out[42] = id.host_len;
            @memcpy(out[43..][0..id.host_len], id.host());
            return out[0 .. 43 + @as(usize, id.host_len)];
        },
    }
}

pub fn decode(bytes: []const u8) ?Target {
    if (bytes.len < 38 or bytes.len > max_len) return null;
    if (bytes[0] != 2) return null;
    const provider_id: support.ProviderId = @enumFromInt(std.mem.readInt(u64, bytes[2..10], .little));
    return .{
        .provider_id = provider_id,
        .context = .{
            .provider = std.mem.readInt(u64, bytes[10..18], .little),
            .host = std.mem.readInt(u64, bytes[18..26], .little),
            .epoch = std.mem.readInt(u64, bytes[26..34], .little),
        },
        .resource = decodeResource(bytes, provider_id) orelse return null,
    };
}

fn decodeResource(bytes: []const u8, provider_id: support.ProviderId) ?Resource {
    switch (bytes[1]) {
        0 => {
            if (provider_id != .local or bytes.len != 42) return null;
            return .{ .terminal = .{ .provider_id = provider_id, .terminal_id = .{ .local = @enumFromInt(std.mem.readInt(u64, bytes[34..42], .little)) } } };
        },
        1 => return decodeRemote(bytes, provider_id),
        // Any Phux coordinator; resolution then refuses one no longer held.
        2 => {
            if (!isCoordinator(provider_id) or bytes.len != 38) return null;
            return .{ .session = std.mem.readInt(u32, bytes[34..38], .little) };
        },
        3 => {
            if (!isCoordinator(provider_id) or bytes.len != 38) return null;
            return .{ .peer_session = std.mem.readInt(u32, bytes[34..38], .little) };
        },
        else => return null,
    }
}

fn isCoordinator(id: support.ProviderId) bool {
    return @import("provider_contract").isPhuxCoordinator(id);
}

fn decodeRemote(bytes: []const u8, provider_id: support.ProviderId) ?Resource {
    if (!isCoordinator(provider_id) or bytes.len < 43) return null;
    if (bytes.len != 43 + @as(usize, bytes[42])) return null;
    const id = support.RemoteResourceId.fromPhux(
        std.mem.readInt(u32, bytes[34..38], .little),
        std.mem.readInt(u32, bytes[38..42], .little),
        bytes[43..],
    ) catch return null;
    return .{ .terminal = .{ .provider_id = provider_id, .terminal_id = .{ .phux = id } } };
}

// ---------------------------------------------------------------------------
// Exact source targets (phux-2jza.4.3).
//
// A v2 `Target` names its source by coordinator: `resolve` finds the provider
// through `provider_id` (`phuxForConst` / `peerSlot`), which answers the
// active attachment or the FIRST peer of that coordinator. Several Client
// attachments to one machine now project identical refs and session numbers
// into different windows, so that lookup can land on a sibling attachment
// and act on the wrong window's session. `Context.provider` already holds the
// capturing `PhuxProvider.context_id`, but nothing resolves through it, and
// a placed row carries neither its window lifetime nor its tab identity, so
// `locateTerminal` picks whichever window shows the ref first.
//
// `Exact` is the in-process capture that closes those gaps: the exact
// attachment (a provider context from the process-wide monotonic allocator,
// never recycled, and disjoint between the local provider and every Phux
// attachment), that attachment's host lifetime and connection, the created
// identity of the session the row belongs to, and for a placed row its
// window epoch and tab identity. It resolves only while all of them are
// current and never falls back to a coordinator, a name or a first match.
// Menu/page rows keep the live v2 encoder above: host-filter tokens already
// use tag 3, and a catalog version-3 byte would collide with that filter in
// the opaque target slot. `Context.provider` already stores the attachment
// context_id; Exact is the in-process capture that *resolves* through it
// (plus placement and session creation). A distinct wire TAG, if needed,
// stays a parent/frontend coordination item — not this lane.
// ---------------------------------------------------------------------------

/// A Phux session as created: its number on that server and the creation
/// time its catalog reported. Numbers are reused after a session ends, so
/// the number alone never authorizes acting on "the same" session. Null
/// `created_at` means the source had no current listing of it at capture;
/// it then matches only a source that still has none.
pub const SessionIdentity = struct { id: u32, created_at: ?i64 };

/// Where a placed row was captured: the native window's lifetime and the
/// tab's process-local identity. Reordering keeps the tab; closing and
/// reopening the window, or retiring the tab, refuses.
pub const Placement = struct { window: u8, window_epoch: u64, tab_generation: u64, tab_id: u32 };

pub const Exact = struct {
    provider_id: support.ProviderId,
    /// The capturing attachment's provider context: `PhuxProvider.context_id`
    /// (the value trees and `PeerSession` carry as `attachment_id`), or the
    /// local provider's own context for an ephemeral local PTY.
    attachment: u64,
    /// That attachment's host lifetime; 0 for the local provider.
    host: u64 = 0,
    /// The connection the row was listed on: the replica owner's epoch for a
    /// placed terminal, the connection for an available one, and the session
    /// catalog's generation for a session row.
    epoch: u64 = 0,
    session: ?SessionIdentity = null,
    placement: ?Placement = null,
    /// `session` is the active attachment's listing and `peer_session` a peer
    /// attachment's; `peer_session` 0 is a failed peer's retry row.
    resource: Resource,

    pub fn resolve(self: Exact, model: *const Model) ?Resolved {
        return switch (self.resource) {
            .terminal => |ref| self.resolveTerminal(model, ref),
            .session => |id| self.resolveListed(model, id, .active),
            .peer_session => |id| self.resolveListed(model, id, .peer),
        };
    }

    fn resolveTerminal(self: Exact, model: *const Model, ref: TerminalRef) ?Resolved {
        if (ref.provider_id != self.provider_id) return null;
        if (self.placement) |placement| return self.resolvePlaced(model, ref, placement);
        return self.resolveAvailable(model, ref);
    }

    fn resolvePlaced(self: Exact, model: *const Model, ref: TerminalRef, placement: Placement) ?Resolved {
        const tab = placedTab(model, placement) orelse return null;
        const tree = model.wsAtConst(placement.window).?.treeConst(tab) orelse return null;
        if (tree.find(ref) == null) return null;
        if (!self.treeSourceCurrent(model, tree, ref)) return null;
        return self.resolved(.{ .placed_terminal = .{ .window = placement.window, .tab = @intCast(tab), .terminal_ref = ref } });
    }

    fn treeSourceCurrent(self: Exact, model: *const Model, tree: *const layout.Tree, ref: TerminalRef) bool {
        if (self.provider_id == .local) return self.attachment == model.provider.context_id;
        if (comptime !support.phux_enabled) return false;
        const remote = heldSource(model, self) orelse return false;
        // The tree's own attachment, never a ref-wide lookup: the sibling
        // attachment may show this ref in another window.
        if (model.phuxForTreeConst(tree) != remote) return false;
        if (!replicaCurrent(remote, ref, self.epoch)) return false;
        return std.meta.eql(self.session, attachedSession(model, remote));
    }

    fn resolveAvailable(self: Exact, model: *const Model, ref: TerminalRef) ?Resolved {
        if (comptime !support.phux_enabled) return null;
        const remote = heldSource(model, self) orelse return null;
        if (self.epoch != remote.connectionEpoch()) return null;
        if (!listsTerminal(remote, ref)) return null;
        if (!std.meta.eql(self.session, terminalSession(remote, ref))) return null;
        // Placed since capture, by this attachment only: reveal that pane.
        if (placedUnder(model, remote, ref)) |placed| return self.resolved(.{ .placed_terminal = placed });
        return self.resolved(.{ .available_terminal = ref });
    }

    fn resolveListed(self: Exact, model: *const Model, id: u32, role: Role) ?Resolved {
        if (comptime !support.phux_enabled) return null;
        const remote = heldSource(model, self) orelse return null;
        if (roleOf(model, remote) != role) return null;
        if (remote.host.sessions_generation != self.epoch) return null;
        if (id == 0) return self.resolveUnavailable(model, remote, role);
        const created = createdIn(listing(remote, role), id) orelse return null;
        if (!std.meta.eql(self.session, SessionIdentity{ .id = id, .created_at = created })) return null;
        if (role == .active) return self.resolved(.{ .session = id });
        return self.resolved(.{ .peer_session = .{ .coordinator = remote.providerId(), .id = id, .attachment_id = remote.context_id } });
    }

    fn resolveUnavailable(self: Exact, model: *const Model, remote: *const support.PhuxProvider, role: Role) ?Resolved {
        if (role != .peer) return null;
        const slot = model.peerSlotForAttachment(remote.context_id) orelse return null;
        if (!model.peers.items[slot].failed) return null;
        return self.resolved(.{ .peer_unavailable = remote.providerId() });
    }

    fn resolved(self: Exact, entry: Entry) Resolved {
        return .{ .entry = entry, .attachment = self.attachment };
    }
};

/// A current destination plus the exact attachment that must act on it.
/// `entry` alone is not enough for `.session`, `.placed_terminal` or
/// `.peer_unavailable`, whose payloads name only a coordinator or a ref;
/// callers reach the provider through `Model.phuxForAttachment(attachment)`
/// and never through `phux()`, `phuxFor(coordinator)` or `peerSlot`.
pub const Resolved = struct { entry: Entry, attachment: u64 };

const Role = enum { active, peer, other };

/// Capture a catalog row against the attachment that produced it. Null when
/// that attachment cannot be named exactly (a `PeerSession` without its
/// `attachment_id`, or a `peer_unavailable` coordinator that several held
/// peers share); callers must then refuse the row rather than guess.
pub fn captureExact(model: *const Model, entry: Entry) ?Exact {
    return switch (entry) {
        .placed_terminal => |placed| capturePlaced(model, placed),
        .available_terminal => |ref| captureAvailable(model, ref),
        .session => |id| captureActiveSession(model, id),
        .peer_session => |row| capturePeerRow(model, row),
        .peer_unavailable => |coordinator| captureUnavailablePeer(model, solePeer(model, coordinator) orelse return null),
    };
}

/// A failed peer's retry row, named by the peer attachment that failed.
pub fn captureUnavailablePeer(model: *const Model, attachment: u64) ?Exact {
    if (comptime !support.phux_enabled) return null;
    const slot = model.peerSlotForAttachment(attachment) orelse return null;
    const remote = model.phuxPeerAtConst(slot).?;
    var exact = sourceOf(remote, .{ .peer_session = 0 });
    exact.epoch = remote.host.sessions_generation;
    return exact;
}

fn capturePlaced(model: *const Model, placed: model_module.PlacedTerminalDestination) ?Exact {
    const workspace = model.wsAtConst(placed.window) orelse return null;
    const tree = workspace.treeConst(placed.tab) orelse return null;
    if (tree.find(placed.terminal_ref) == null) return null;
    const placement: Placement = .{
        .window = placed.window,
        .window_epoch = model.window_epochs[placed.window],
        .tab_generation = workspace.tab_generation,
        .tab_id = workspace.tabId(placed.tab) orelse return null,
    };
    var exact = placedSource(model, tree, placed.terminal_ref) orelse return null;
    exact.placement = placement;
    return exact;
}

/// The source that published this very tree, not whichever attachment a
/// ref-wide lookup would answer.
fn placedSource(model: *const Model, tree: *const layout.Tree, ref: TerminalRef) ?Exact {
    if (ref.provider_id == .local) return .{ .provider_id = .local, .attachment = model.provider.context_id, .resource = .{ .terminal = ref } };
    if (comptime !support.phux_enabled) return null;
    const remote = model.phuxForTreeConst(tree) orelse return null;
    var exact = sourceOf(remote, .{ .terminal = ref });
    // A stale replica captures its own old epoch, so it never resolves.
    exact.epoch = if (remote.owner(ref)) |owner| owner.generation.epoch_id else 0;
    exact.session = attachedSession(model, remote);
    return exact;
}

fn captureAvailable(model: *const Model, ref: TerminalRef) ?Exact {
    if (comptime !support.phux_enabled) return null;
    if (ref.provider_id == .local) return null;
    const remote = inventorySource(model) orelse return null;
    if (remote.providerId() != ref.provider_id) return null;
    var exact = sourceOf(remote, .{ .terminal = ref });
    exact.epoch = if (listsTerminal(remote, ref)) remote.connectionEpoch() else 0;
    exact.session = terminalSession(remote, ref);
    return exact;
}

fn captureActiveSession(model: *const Model, id: u32) ?Exact {
    if (comptime !support.phux_enabled) return null;
    const remote = model.phuxConst() orelse return null;
    return listedSession(remote, .active, .{ .session = id }, id);
}

fn capturePeerRow(model: *const Model, row: model_module.PeerSession) ?Exact {
    if (comptime !support.phux_enabled) return null;
    const attachment = row.attachment_id orelse return null;
    const slot = model.peerSlotForAttachment(attachment) orelse return null;
    const remote = model.phuxPeerAtConst(slot).?;
    if (remote.providerId() != row.coordinator) return null;
    return listedSession(remote, .peer, .{ .peer_session = row.id }, row.id);
}

fn listedSession(remote: *const support.PhuxProvider, role: Role, resource: Resource, id: u32) Exact {
    var exact = sourceOf(remote, resource);
    exact.epoch = remote.host.sessions_generation;
    exact.session = .{ .id = id, .created_at = createdIn(listing(remote, role), id) };
    return exact;
}

fn sourceOf(remote: *const support.PhuxProvider, resource: Resource) Exact {
    return .{ .provider_id = remote.providerId(), .attachment = remote.context_id, .host = remote.host.context_id, .resource = resource };
}

/// The captured attachment, still held, still dialing the same coordinator
/// and on the same host lifetime. Context ids are never recycled, so a
/// replaced or re-created attachment can never answer for an old capture.
fn heldSource(model: *const Model, exact: Exact) ?*const support.PhuxProvider {
    if (comptime !support.phux_enabled) return null;
    const remote = model.phuxForAttachmentConst(exact.attachment) orelse return null;
    if (remote.providerId() != exact.provider_id) return null;
    if (remote.host.context_id != exact.host) return null;
    return remote;
}

fn roleOf(model: *const Model, remote: *const support.PhuxProvider) Role {
    if (model.phuxConst() == remote) return .active;
    if (model.peerSlotForAttachment(remote.context_id) != null) return .peer;
    return .other;
}

/// The listing a row of that role came from: the active attachment's catalog
/// only while it is from this connection, a peer's standby catalog (which
/// also refuses while disconnected or retargeting).
fn listing(remote: *const support.PhuxProvider, role: Role) []const support.PhuxProvider.SessionSummary {
    if (role == .peer) return remote.standbyCatalog();
    return currentCatalog(remote);
}

fn currentCatalog(remote: *const support.PhuxProvider) []const support.PhuxProvider.SessionSummary {
    if (remote.host.sessions_generation != remote.connectionEpoch()) return &.{};
    return remote.sessionCatalog();
}

fn createdIn(catalog: []const support.PhuxProvider.SessionSummary, id: u32) ?i64 {
    for (catalog) |session| {
        if (session.id == id) return session.created_at_unix_secs;
    }
    return null;
}

/// The session this attachment shows. Its creation comes from the current
/// catalog, else from the creation recorded when a peer was pointed at it
/// (the same evidence `session_attachments.sameSession` uses).
fn attachedSession(model: *const Model, remote: *const support.PhuxProvider) ?SessionIdentity {
    const id = remote.selectedSessionId() orelse return null;
    return .{ .id = id, .created_at = createdIn(currentCatalog(remote), id) orelse recordedCreation(model, remote) };
}

fn recordedCreation(model: *const Model, remote: *const support.PhuxProvider) ?i64 {
    const slot = model.peerSlotForAttachment(remote.context_id) orelse return null;
    return model.peers.items[slot].session_created_at;
}

fn terminalSession(remote: *const support.PhuxProvider, ref: TerminalRef) ?SessionIdentity {
    const id = remote.terminalSession(ref) orelse return null;
    return .{ .id = id, .created_at = createdIn(currentCatalog(remote), id) };
}

fn replicaCurrent(remote: *const support.PhuxProvider, ref: TerminalRef, epoch: u64) bool {
    const owner = remote.owner(ref) orelse return false;
    return owner.generation.epoch_id == epoch and epoch == remote.connectionEpoch();
}

fn listsTerminal(remote: *const support.PhuxProvider, ref: TerminalRef) bool {
    for (remote.catalogTerminals()) |terminal| {
        if (terminal.terminal_ref.eql(ref)) return true;
    }
    return false;
}

/// The window and tab identity still current, as `ts_window_navigation`
/// resolves them: same window lifetime, same tab generation and id.
fn placedTab(model: *const Model, placement: Placement) ?usize {
    const target: window_navigation.Target = .{
        .window = placement.window,
        .epoch = placement.window_epoch,
        .tab = .{ .generation = placement.tab_generation, .id = placement.tab_id },
    };
    const selection = target.resolve(model) orelse return null;
    const tab = selection.tab orelse return null;
    return @as(usize, tab);
}

/// A pane of `ref` in a tree this attachment published, if any.
fn placedUnder(model: *const Model, remote: *const support.PhuxProvider, ref: TerminalRef) ?model_module.PlacedTerminalDestination {
    for (0..model_module.max_windows) |window| {
        const workspace = model.wsAtConst(window) orelse continue;
        for (workspace.tabs[0..workspace.tab_count], 0..) |*tree, tab| {
            if (tree.find(ref) == null or model.phuxForTreeConst(tree) != remote) continue;
            return .{ .window = @intCast(window), .tab = @intCast(tab), .terminal_ref = ref };
        }
    }
    return null;
}

/// The attachment whose unplaced terminals the catalog lists, by the rule
/// `workspace_projection.inventoryPeer` applies (not public there): a showing,
/// attached peer while one of its panes is focused, else the active one.
fn inventorySource(model: *const Model) ?*const support.PhuxProvider {
    const focused = model.focusedTerminalRef() orelse return model.phuxConst();
    if (support.providerKind(focused) != .phux or model.activeOwnsRef(focused)) return model.phuxConst();
    const peer = model.phuxForRefConst(focused) orelse return model.phuxConst();
    if (!peer.showing() or peer.state() != .attached) return model.phuxConst();
    return peer;
}

/// The one held peer of `coordinator`, or null when none or several are: a
/// bare coordinator can never pick between sibling attachments.
fn solePeer(model: *const Model, coordinator: support.ProviderId) ?u64 {
    if (comptime !support.phux_enabled) return null;
    var found: ?u64 = null;
    for (model.peers.items) |entry| {
        const peer = entry.provider orelse continue;
        if (peer.providerId() != coordinator) continue;
        if (found != null) return null;
        found = peer.context_id;
    }
    return found;
}

test {
    _ = @import("catalog_targets_tests.zig");
}

test "navigation opaque catalog targets preserve full identity and bounded host bytes" {
    const host = [_]u8{0xfe} ** support.RemoteResourceId.max_host_bytes;
    const target: Target = .{
        .provider_id = .phux,
        .context = .{ .provider = 0xfedc_ba98_7654_3210, .host = 0xabcd_ef01_2345_6789, .epoch = 0xffff_ffff_ffff_fffe },
        .resource = .{ .terminal = .{ .provider_id = .phux, .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0xfedc_ba98, 0xabcd_ef01, &host) } } },
    };
    var out: [max_len]u8 = undefined;
    const bytes = target.encode(&out);
    try std.testing.expectEqual(max_len, bytes.len);
    const roundtrip = decode(bytes).?;
    try std.testing.expectEqual(target.provider_id, roundtrip.provider_id);
    try std.testing.expectEqual(target.context, roundtrip.context);
    try std.testing.expect(target.resource.terminal.eql(roundtrip.resource.terminal));
    for (0..bytes.len) |len| try std.testing.expect(decode(bytes[0..len]) == null);
    var excess: [max_len + 1]u8 = undefined;
    @memcpy(excess[0..max_len], bytes);
    excess[max_len] = 0;
    try std.testing.expect(decode(&excess) == null);
    out[42] -= 1;
    try std.testing.expect(decode(&out) == null);
    out[42] += 1;
    // Same resource under another coordinator never aliases: it decodes as
    // that coordinator's terminal, not this one's.
    const mini = @import("provider_contract").phuxCoordinatorId("mini");
    std.mem.writeInt(u64, out[2..10], @intFromEnum(mini), .little);
    const other = decode(&out).?;
    try std.testing.expectEqual(mini, other.provider_id);
    try std.testing.expect(!other.resource.terminal.eql(target.resource.terminal));
    // An id no coordinator can have, and a local id, carry no Phux resource.
    std.mem.writeInt(u64, out[2..10], @intFromEnum(support.ProviderId.phux) ^ 1, .little);
    try std.testing.expect(decode(&out) == null);
    std.mem.writeInt(u64, out[2..10], @intFromEnum(support.ProviderId.local), .little);
    try std.testing.expect(decode(&out) == null);

    const local: Target = .{ .provider_id = .local, .context = target.context, .resource = .{ .terminal = .{
        .provider_id = .local,
        .terminal_id = .{ .local = @enumFromInt(0xfedc_ba98_7654_3210) },
    } } };
    try std.testing.expect(local.resource.terminal.eql(decode(local.encode(&out)).?.resource.terminal));
    const session: Target = .{ .provider_id = .phux, .context = target.context, .resource = .{ .session = 0xffff_fffe } };
    try std.testing.expectEqual(session.resource.session, decode(session.encode(&out)).?.resource.session);
}

test "navigation catalog context rejects provider host and connection replacement" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try @import("ts_engine.zig").Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const remote = try model_module.PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/catalog-context-unused" }, null, "test");
    engine.model.phux_provider = remote;
    try remote.host.sessions.append(std.testing.allocator, .{
        .id = 0xffff_fffe,
        .name = try std.testing.allocator.dupe(u8, "session"),
        .created_at_unix_secs = 0,
        .window_count = 1,
        .attached_client_count = 0,
        .focused = false,
    });
    remote.host.sessions_generation = remote.connectionEpoch();
    const target = capture(engine.model, .{ .session = 0xffff_fffe }).?;
    try std.testing.expectEqual(@as(u32, 0xffff_fffe), target.resolve(engine.model).?.session);
    const replacement = try model_module.PhuxProvider.create(std.testing.allocator, std.testing.io, .{ .unix = "/catalog-context-unused" }, null, "test");
    defer replacement.destroy();
    engine.model.phux_provider = replacement;
    try std.testing.expect(target.resolve(engine.model) == null);
    engine.model.phux_provider = remote;
    const host = remote.host;
    remote.host = replacement.host;
    try std.testing.expect(target.resolve(engine.model) == null);
    remote.host = host;
    remote.host.client_generation += 1;
    try std.testing.expect(target.resolve(engine.model) == null);
    remote.host.client_generation -= 1;
    try std.testing.expect(target.resolve(engine.model) != null);
    remote.host.sessions.items[0].id = 7;
    try std.testing.expect(target.resolve(engine.model) == null);
}
