//! Captured destructive intent, separate from view detach and layout mutation.
//! FFI close receipts own both proofs: command acknowledgement and every
//! captured RESOURCE_CLOSED. Replica absence is never completion evidence.
const std = @import("std");
const contract = @import("provider_contract");
const layout = @import("../layout.zig");
const tabs = @import("tab_commands.zig");

pub const Kind = enum { pane, tab };
pub const Phase = enum { waiting, resources_closed, refused, unknown, stale, completed };

pub const Source = struct {
    provider_id: contract.ProviderId,
    provider_context: u64,
    host_context: u64,
    connection_epoch: u64,
    session_id: ?u32,

    pub fn capture(remote: anytype) Source {
        return .{ .provider_id = remote.providerId(), .provider_context = remote.context_id, .host_context = remote.host.context_id, .connection_epoch = remote.connectionEpoch(), .session_id = remote.host.selectedSessionId() };
    }

    pub fn matches(self: Source, remote: anytype) bool {
        return std.meta.eql(self, capture(remote));
    }
};

pub const Target = struct {
    kind: Kind,
    source: Source,
    view: tabs.Target,
    shared_id: ?[16]u8,
    owners: [layout.max_panes]contract.ReplicaOwner = undefined,
    count: usize = 0,

    pub fn capturePane(model: anytype, remote: anytype, window: u8, tab: u8, ref: contract.TerminalRef) !Target {
        var target = try captureView(model, remote, window, tab, .pane);
        const tree = &model.wsAtConst(window).?.tabs[tab];
        if (tree.find(ref) == null) return error.InvalidTarget;
        try target.appendOwner(remote, ref);
        return target;
    }

    pub fn captureTab(model: anytype, remote: anytype, window: u8, tab: u8) !Target {
        var target = try captureView(model, remote, window, tab, .tab);
        var refs: [layout.max_panes]contract.TerminalRef = undefined;
        const count = model.wsAtConst(window).?.tabs[tab].terminals(&refs);
        if (count == 0) return error.InvalidTarget;
        for (refs[0..count]) |ref| try target.appendOwner(remote, ref);
        return target;
    }

    fn captureView(model: anytype, remote: anytype, window: u8, tab: u8, kind: Kind) !Target {
        if (!model.windowOpen(window)) return error.InvalidTarget;
        const ws = model.wsAtConst(window) orelse return error.InvalidTarget;
        if (tab >= ws.tab_count) return error.InvalidTarget;
        const target: Target = .{
            .kind = kind,
            .source = Source.capture(remote),
            .view = .{ .window = window, .window_epoch = model.window_epochs[window], .tab_id = ws.tab_ids[tab], .tab_generation = ws.tab_generation },
            .shared_id = ws.shared_ids[tab],
        };
        if (target.resolve(model) == null) return error.InvalidTarget;
        if (!target.viewSourceCurrent(model, tab)) return error.InvalidIdentity;
        return target;
    }

    fn appendOwner(self: *Target, remote: anytype, ref: contract.TerminalRef) !void {
        const owner = remote.owner(ref) orelse return error.InvalidIdentity;
        if (owner.source_context != self.source.host_context) return error.InvalidIdentity;
        if (!remote.ownerIsCurrent(owner)) return error.InvalidIdentity;
        self.owners[self.count] = owner;
        self.count += 1;
    }

    /// Resolve identity, never current focus or a saved tab index. Engine must
    /// call this again immediately before applying a returned metadata target.
    pub fn resolve(self: *const Target, model: anytype) ?u8 {
        const ws = self.resolveWindow(model) orelse return null;
        for (ws.tab_ids[0..ws.tab_count], 0..) |id, index| {
            if (id != self.view.tab_id) continue;
            if (!std.meta.eql(ws.shared_ids[index], self.shared_id)) return null;
            return @intCast(index);
        }
        return null;
    }

    fn resolveWindow(self: *const Target, model: anytype) @TypeOf(model.wsAtConst(self.view.window)) {
        if (!model.windowOpen(self.view.window)) return null;
        if (self.view.window_epoch == std.math.maxInt(u64)) return null;
        if (model.window_epochs[self.view.window] != self.view.window_epoch) return null;
        const ws = model.wsAtConst(self.view.window) orelse return null;
        if (self.view.tab_generation == std.math.maxInt(u64)) return null;
        if (ws.tab_generation != self.view.tab_generation) return null;
        return ws;
    }

    fn contains(self: *const Target, ref: contract.TerminalRef) bool {
        for (self.owners[0..self.count]) |owner| if (owner.terminal_ref.eql(ref)) return true;
        return false;
    }

    fn validateBeforeEnqueue(self: *const Target, model: anytype, remote: anytype) !void {
        if (!self.source.matches(remote)) return error.InvalidIdentity;
        const tab = self.resolve(model) orelse return error.InvalidTarget;
        if (!self.viewSourceCurrent(model, tab)) return error.InvalidIdentity;
        if (self.count == 0 or self.count > layout.max_panes) return error.InvalidTarget;
        const tree = &model.wsAtConst(self.view.window).?.tabs[tab];
        try self.validateOwners(remote, tree);
        if (self.kind == .tab and !self.onlyCapturedLeaves(tree)) return error.InvalidTarget;
    }

    fn validateOwners(self: *const Target, remote: anytype, tree: *const layout.Tree) !void {
        for (self.owners[0..self.count]) |owner| {
            if (tree.find(owner.terminal_ref) == null) return error.InvalidTarget;
            if (!remote.ownerIsCurrent(owner)) return error.InvalidIdentity;
        }
    }

    fn onlyCapturedLeaves(self: *const Target, tree: *const layout.Tree) bool {
        var refs: [layout.max_panes]contract.TerminalRef = undefined;
        const count = tree.terminals(&refs);
        for (refs[0..count]) |ref| if (!self.contains(ref)) return false;
        return true;
    }

    fn completionCurrent(self: *const Target, model: anytype, remote: anytype) bool {
        if (!self.source.matches(remote)) return false;
        const tab = self.resolve(model) orelse return false;
        if (!self.viewSourceCurrent(model, tab)) return false;
        if (self.kind == .tab and !self.onlyCapturedLeaves(&model.wsAtConst(self.view.window).?.tabs[tab])) return false;
        // A newly published replica at a recycled numeric ID is not the work
        // that was closed, even within the same connection epoch.
        for (self.owners[0..self.count]) |owner| {
            const current = remote.owner(owner.terminal_ref) orelse continue;
            if (!current.eql(owner)) return false;
        }
        return true;
    }

    fn viewSourceCurrent(self: *const Target, model: anytype, tab: u8) bool {
        const Model = @typeInfo(@TypeOf(model)).pointer.child;
        if (comptime @hasDecl(Model, "phuxForTreeConst")) {
            const remote = model.phuxForTreeConst(&model.wsAtConst(self.view.window).?.tabs[tab]) orelse return false;
            return self.source.matches(remote);
        }
        // The pre-cutover Model fixture has one attachment per provider. The
        // shipping runtime must expose exact tree attachment lookup; accepting
        // a provider-ID-only lookup there would authorize a replacement view.
        if (comptime !@import("builtin").is_test) @compileError("close runtime requires Model.phuxForTreeConst");
        const remote = model.phuxForConst(self.source.provider_id) orelse return false;
        return self.source.matches(remote);
    }
};

pub const Outcome = struct {
    target: Target,
    request_id: u32,
    phase: Phase,
    reason_storage: [4096]u8 = undefined,
    reason_len: usize = 0,

    pub fn reason(self: *const Outcome) []const u8 {
        return self.reason_storage[0..self.reason_len];
    }

    fn setReason(self: *Outcome, text: []const u8) void {
        self.reason_len = @min(text.len, self.reason_storage.len);
        @memcpy(self.reason_storage[0..self.reason_len], text[0..self.reason_len]);
    }
};

const Pending = struct {
    outcome: Outcome,

    fn accepts(self: *const Pending, remote: anytype, result: anytype) bool {
        if (!self.outcome.target.source.matches(remote)) return false;
        if (result.connection_epoch != self.outcome.target.source.connection_epoch) return false;
        if (result.request_id != self.outcome.request_id) return false;
        if (self.outcome.phase != .waiting) return false;
        if (result.terminal_ref) |ref| if (!self.outcome.target.contains(ref)) return false;
        return switch (self.outcome.target.kind) {
            .pane => result.kind == .close_resource,
            .tab => result.kind == .close_resources,
        };
    }
};

/// Capacity is selected by Engine's existing outstanding-intent budget.
pub fn Coordinator(comptime capacity: usize) type {
    return struct {
        const Self = @This();
        pending: [capacity]?Pending = @splat(null),

        /// Synchronous preflight refusal leaves no pending item and consumes no
        /// request ID. Caller displays the provider's reason and retains views.
        pub fn begin(self: *Self, model: anytype, remote: anytype, target: Target) !u32 {
            try target.validateBeforeEnqueue(model, remote);
            const slot = self.freeSlot() orelse return error.CloseQueueFull;
            var refs: [layout.max_panes]contract.TerminalRef = undefined;
            for (target.owners[0..target.count], 0..) |owner, index| refs[index] = owner.terminal_ref;
            const request = switch (target.kind) {
                .pane => try remote.requestCloseResource(refs[0], target.source.connection_epoch),
                .tab => try remote.requestCloseResources(refs[0..target.count], target.source.connection_epoch),
            };
            self.pending[slot] = .{ .outcome = .{ .target = target, .request_id = request, .phase = .waiting } };
            return request;
        }

        fn freeSlot(self: *const Self) ?usize {
            for (self.pending, 0..) |pending, index| if (pending == null) return index;
            return null;
        }

        /// Kinds 5/6 success is an FFI-owned completed close, not raw command Ok:
        /// the owning Client has already processed every RESOURCE_CLOSED. A
        /// second Host observation queue would duplicate and risk losing proof.
        pub fn completeFrom(self: *Self, remote: anytype, result: anytype) bool {
            for (&self.pending) |*slot| {
                const pending = if (slot.*) |*value| value else continue;
                if (!pending.accepts(remote, result)) continue;
                pending.outcome.phase = switch (result.status) {
                    .success => .resources_closed,
                    .refused => .refused,
                    .unknown_outcome => .unknown,
                };
                pending.outcome.setReason(result.message());
                return true;
            }
            return false;
        }

        /// Retire intent when its exact attachment leaves the runtime. This is
        /// an unknown outcome, never a retry or metadata-removal authorization.
        pub fn invalidateSource(self: *Self, source: Source) void {
            for (&self.pending) |*slot| {
                const pending = if (slot.*) |*value| value else continue;
                if (!std.meta.eql(pending.outcome.target.source, source)) continue;
                pending.outcome.phase = .unknown;
                pending.outcome.setReason("close outcome unknown: original attachment ended");
            }
        }

        /// Only `.completed` authorizes Engine's captured layout mutation. All
        /// other returned phases retain views and carry an owned reason.
        pub fn takeOutcomeFrom(self: *Self, model: anytype, remote: anytype) ?Outcome {
            for (&self.pending) |*slot| {
                const pending = if (slot.*) |*value| value else continue;
                // A reconnect of this provider may retire an old request, but
                // an independent provider with a colliding request may not.
                if (pending.outcome.target.source.provider_context != remote.context_id) continue;
                if (!pending.outcome.target.source.matches(remote)) {
                    pending.outcome.phase = .unknown;
                    pending.outcome.setReason("close outcome unknown: original attachment replaced");
                }
                if (pending.outcome.phase == .waiting) continue;
                var outcome = pending.outcome;
                slot.* = null;
                if (outcome.phase == .resources_closed) {
                    outcome.phase = if (outcome.target.completionCurrent(model, remote)) .completed else .stale;
                    if (outcome.phase == .stale) outcome.setReason("close completed for a retired view; layout was not removed");
                }
                return outcome;
            }
            return null;
        }
    };
}
