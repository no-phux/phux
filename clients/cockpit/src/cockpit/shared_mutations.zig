//! A bounded intent queue, never a second topology. Only the provider's winning
//! snapshot may change presentation; removal never destroys a durable process.
const std = @import("std");
const contract = @import("provider_contract");
const workspace = contract.workspace;
const results = @import("command_results.zig");
const TerminalRef = contract.TerminalRef;
pub const WindowId = workspace.WindowId;
pub const Outcome = enum { confirmed, refused, unknown_outcome };
pub const Completion = struct { outcome: Outcome, request_id: u32, reason: results.Reason };
const Stage = enum { refresh, refreshing, mutation, mutating, complete };
const Pending = struct {
    ticket: u64,
    epoch: u64,
    mutation: workspace.Mutation,
    stage: Stage,
    request: u32 = 0,
    retained: bool,
    refresh_creation: bool,
    command_id: ?u64 = null,
    origin: results.Origin = .ui,
    mutation_request: u32 = 0,
    outcome: Outcome = .refused,
    reason: results.Reason = .mutation_refused,
};

pub const Coordinator = struct {
    pending: [16]?Pending = @splat(null),
    next_ticket: u64 = 0,

    /// Creation retains its completion until consumed, so a later operation can
    /// never overwrite a result before placement has observed it.
    pub fn requestCreation(self: *Coordinator, model: anytype, mutation: workspace.Mutation, epoch: u64) !u64 {
        if (mutation.kind != .add and mutation.kind != .split) return error.UnsupportedMutation;
        try validateContext(model, mutation.session_id, epoch);
        return self.enqueue(mutation, epoch, true, .refresh, null, .ui);
    }

    pub fn takeCompletion(self: *Coordinator, ticket: u64) ?Outcome {
        const completion = self.takeCreationCompletion(ticket) orelse return null;
        return completion.outcome;
    }

    pub fn takeCreationCompletion(self: *Coordinator, ticket: u64) ?Completion {
        for (&self.pending) |*slot| {
            const entry = slot.* orelse continue;
            if (entry.ticket != ticket or entry.stage != .complete) continue;
            if (entry.command_id != null) return null;
            slot.* = null;
            return .{ .outcome = entry.outcome, .request_id = entry.mutation_request, .reason = entry.reason };
        }
        return null;
    }

    pub fn peekCompletion(self: *const Coordinator) ?results.Result {
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.stage != .complete) continue;
            const command_id = entry.command_id orelse continue;
            return .{
                .command_id = command_id,
                .origin = entry.origin,
                .request_id = entry.mutation_request,
                .mutation_ticket = entry.ticket,
                .connection_epoch = entry.epoch,
                .terminal_ref = entry.mutation.terminal_ref,
                .shared_window_id = entry.mutation.window_id,
                .operation = switch (entry.outcome) {
                    .confirmed => .success,
                    .refused => .refused,
                    .unknown_outcome => .unknown,
                },
                .placement = .not_requested,
                .reason = entry.reason,
            };
        }
        return null;
    }

    pub fn ackCompletion(self: *Coordinator, command_id: u64) bool {
        return self.acknowledge(command_id, .ui);
    }

    pub fn ackNativeCompletion(self: *Coordinator, ticket: u64) bool {
        return self.acknowledge(ticket, .native);
    }

    fn acknowledge(self: *Coordinator, command_id: u64, origin: results.Origin) bool {
        for (&self.pending) |*slot| {
            const entry = slot.* orelse continue;
            if (entry.stage != .complete or entry.command_id != command_id) continue;
            if (entry.origin != origin) continue;
            slot.* = null;
            return true;
        }
        return false;
    }

    pub fn forget(self: *Coordinator, ticket: u64) void {
        for (&self.pending) |*slot| {
            const entry = slot.* orelse continue;
            if (entry.ticket == ticket) slot.* = null;
        }
    }

    /// Capture dispatched write identity before retiring the local waiter.
    /// Forgetting an intent never cancels or rolls back a provider mutation.
    pub fn creationRequest(self: *const Coordinator, ticket: u64) u32 {
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.ticket == ticket) return entry.mutation_request;
        }
        return 0;
    }

    pub fn requestRemove(self: *Coordinator, model: anytype, ref: TerminalRef) !void {
        return self.removeRequest(model, ref, null, .ui);
    }

    pub fn requestRemoveCorrelated(self: *Coordinator, model: anytype, ref: TerminalRef, command_id: u64) !void {
        return self.removeRequest(model, ref, command_id, .ui);
    }

    pub fn requestRemoveNative(self: *Coordinator, model: anytype, ref: TerminalRef) !void {
        return self.removeRequest(model, ref, null, .native);
    }

    fn removeRequest(self: *Coordinator, model: anytype, ref: TerminalRef, command_id: ?u64, origin: results.Origin) !void {
        errdefer reportRefusal(model);
        var mutation = try currentMutation(model);
        const remote = model.phux().?;
        mutation.kind = .remove;
        mutation.window_id = terminalWindow(remote.workspaceSnapshot(), ref) orelse return error.StaleTarget;
        mutation.terminal_ref = ref;
        try self.requestDirect(model, mutation, command_id, origin);
    }

    pub fn requestRemoveWindow(self: *Coordinator, model: anytype, id: WindowId) !void {
        return self.removeWindowRequest(model, id, null, .ui);
    }

    pub fn requestRemoveWindowCorrelated(self: *Coordinator, model: anytype, id: WindowId, command_id: u64) !void {
        return self.removeWindowRequest(model, id, command_id, .ui);
    }

    pub fn requestRemoveWindowNative(self: *Coordinator, model: anytype, id: WindowId) !void {
        return self.removeWindowRequest(model, id, null, .native);
    }

    fn removeWindowRequest(self: *Coordinator, model: anytype, id: WindowId, command_id: ?u64, origin: results.Origin) !void {
        errdefer reportRefusal(model);
        var mutation = try currentMutation(model);
        mutation.kind = .remove_window;
        mutation.window_id = id;
        try self.requestDirect(model, mutation, command_id, origin);
    }

    pub fn requestReorder(self: *Coordinator, model: anytype, id: WindowId, index: usize) !void {
        return self.reorderRequest(model, id, index, null, .ui);
    }

    pub fn requestReorderCorrelated(self: *Coordinator, model: anytype, id: WindowId, index: usize, command_id: u64) !void {
        return self.reorderRequest(model, id, index, command_id, .ui);
    }

    pub fn requestReorderNative(self: *Coordinator, model: anytype, id: WindowId, index: usize) !void {
        return self.reorderRequest(model, id, index, null, .native);
    }

    fn reorderRequest(self: *Coordinator, model: anytype, id: WindowId, index: usize, command_id: ?u64, origin: results.Origin) !void {
        errdefer reportRefusal(model);
        var mutation = try currentMutation(model);
        if (index >= model.phux().?.workspaceSnapshot().windows.len) return error.StaleTarget;
        mutation.kind = .reorder;
        mutation.window_id = id;
        mutation.index = @intCast(index);
        try self.requestDirect(model, mutation, command_id, origin);
    }

    /// The caller captures revision at drag start and discards its preview when
    /// that revision changes. This method never rebases a branch path.
    pub fn requestResize(self: *Coordinator, model: anytype, id: WindowId, path_bits: u64, path_len: u32, ratio: f32) !void {
        return self.resizeRequest(model, id, path_bits, path_len, ratio, null, .ui);
    }

    pub fn requestResizeCorrelated(self: *Coordinator, model: anytype, id: WindowId, path_bits: u64, path_len: u32, ratio: f32, command_id: u64) !void {
        return self.resizeRequest(model, id, path_bits, path_len, ratio, command_id, .ui);
    }

    pub fn requestResizeNative(self: *Coordinator, model: anytype, id: WindowId, path_bits: u64, path_len: u32, ratio: f32) !void {
        return self.resizeRequest(model, id, path_bits, path_len, ratio, null, .native);
    }

    fn resizeRequest(self: *Coordinator, model: anytype, id: WindowId, path_bits: u64, path_len: u32, ratio: f32, command_id: ?u64, origin: results.Origin) !void {
        errdefer reportRefusal(model);
        var mutation = try currentMutation(model);
        mutation.kind = .resize;
        mutation.window_id = id;
        mutation.path_bits = path_bits;
        mutation.path_len = path_len;
        mutation.ratio = ratio;
        try self.requestDirect(model, mutation, command_id, origin);
    }

    /// Native gestures use the coordinator ticket as their command identity in
    /// a distinct namespace. Legacy wrappers remain unretained.
    pub fn requestNative(self: *Coordinator, model: anytype, mutation: workspace.Mutation) !void {
        _ = try currentMutation(model);
        return self.requestDirect(model, mutation, null, .native);
    }

    fn requestDirect(self: *Coordinator, model: anytype, mutation: workspace.Mutation, command_id: ?u64, origin: results.Origin) !void {
        errdefer reportRefusal(model);
        if (self.firstPending() != null) return error.WorkspaceBusy;
        const remote = model.phux().?;
        const snapshot = remote.workspaceSnapshot();
        if (snapshot.status == .pending) return error.WorkspaceBusy;
        try validateTarget(snapshot, mutation);
        const ticket = try self.enqueue(mutation, remote.connectionEpoch(), command_id != null or origin == .native, .mutation, command_id, origin);
        _ = ticket;
        _ = self.pump(model);
    }

    fn enqueue(self: *Coordinator, mutation: workspace.Mutation, epoch: u64, retained: bool, stage: Stage, command_id: ?u64, origin: results.Origin) !u64 {
        // Names borrow caller memory in the provider API. This queue supports
        // unnamed add and identity-only edits; rename needs owned text first.
        if (mutation.name.len != 0) return error.UnsupportedMutation;
        if (origin == .ui) try self.requireUniqueCommand(command_id);
        for (&self.pending) |*slot| {
            if (slot.* != null) continue;
            self.next_ticket = try std.math.add(u64, self.next_ticket, 1);
            slot.* = .{ .ticket = self.next_ticket, .epoch = epoch, .mutation = mutation, .retained = retained, .stage = stage, .refresh_creation = stage == .refresh, .command_id = if (origin == .native) self.next_ticket else command_id, .origin = origin };
            return self.next_ticket;
        }
        return error.OperationCapacity;
    }

    fn requireUniqueCommand(self: *const Coordinator, command_id: ?u64) !void {
        const id = command_id orelse return;
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.origin == .ui and entry.command_id == id) return error.CommandBusy;
        }
    }

    fn firstPending(self: *Coordinator) ?*?Pending {
        var oldest: ?*?Pending = null;
        for (&self.pending) |*slot| {
            const entry = slot.* orelse continue;
            if (entry.stage == .complete) continue;
            if (oldest == null or entry.ticket < oldest.?.*.?.ticket) oldest = slot;
        }
        return oldest;
    }

    pub fn pump(self: *Coordinator, model: anytype) bool {
        const slot = self.firstPending() orelse return false;
        const entry = &slot.*.?;
        advance(model, entry) catch |err| {
            entry.outcome = failureOutcome(entry.*, err);
            entry.reason = failureReason(err);
            entry.stage = .complete;
        };
        if (entry.stage != .complete) return false;
        if (entry.outcome == .refused) reportRefusal(model);
        if (!entry.retained) slot.* = null;
        return true;
    }

    pub fn disconnect(self: *Coordinator, model: anytype) void {
        _ = model;
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.stage == .complete) continue;
            entry.outcome = .unknown_outcome;
            entry.reason = .disconnected;
            entry.stage = .complete;
            if (entry.command_id == null) slot.* = null;
        }
    }
};

fn failureOutcome(entry: Pending, err: anyerror) Outcome {
    if (err == error.StaleContext) return .unknown_outcome;
    if (err == error.MutationUnknown) return .unknown_outcome;
    if (err == error.LostCompletion and entry.stage == .mutating) return .unknown_outcome;
    return .refused;
}

fn failureReason(err: anyerror) results.Reason {
    return switch (err) {
        error.StaleContext => .context_changed,
        error.MutationUnknown => .mutation_unknown,
        error.LostCompletion => .lost_completion,
        error.WorkspaceUnavailable => .workspace_unavailable,
        error.StaleTarget => .stale_target,
        // Provider refusal includes a losing confirmation read; this does not
        // assert that the underlying metadata SET was rejected or rolled back.
        error.MutationRefused => .mutation_not_confirmed,
        else => .mutation_refused,
    };
}

fn currentMutation(model: anytype) !workspace.Mutation {
    const remote = model.phux() orelse return error.NoProvider;
    const snapshot = remote.workspaceSnapshot();
    try validateContext(model, snapshot.session_id, remote.connectionEpoch());
    if (snapshot.revision != model.shared_workspace.revision) return error.StaleTarget;
    return .{ .expected_revision = snapshot.revision, .session_id = snapshot.session_id, .kind = .remove };
}

fn validateContext(model: anytype, session: u32, epoch: u64) !void {
    const remote = model.phux() orelse return error.StaleContext;
    if (remote.connectionEpoch() != epoch) return error.StaleContext;
    if (session == 0 or model.shared_workspace.session != session) return error.StaleContext;
    if (remote.workspaceSnapshot().session_id != session) return error.StaleContext;
    if (remote.state() != .attached) return error.StaleContext;
}

fn advance(model: anytype, entry: *Pending) !void {
    try validateContext(model, entry.mutation.session_id, entry.epoch);
    const remote = model.phux().?;
    const snapshot = remote.workspaceSnapshot();
    switch (entry.stage) {
        .refresh => try beginRefresh(remote, snapshot, entry),
        .refreshing => {
            if (!try requestConfirmed(snapshot, entry.request)) return;
            entry.stage = .mutation;
            try beginMutation(remote, snapshot, entry);
        },
        .mutation => try beginMutation(remote, snapshot, entry),
        .mutating => {
            if (!try requestConfirmed(snapshot, entry.request)) return;
            entry.outcome = .confirmed;
            entry.reason = .completed;
            entry.stage = .complete;
        },
        .complete => {},
    }
}

fn beginRefresh(remote: anytype, snapshot: workspace.Snapshot, entry: *Pending) !void {
    if (snapshot.status == .pending) return;
    entry.request = (try remote.requestWorkspaceRefresh()) orelse return;
    entry.stage = .refreshing;
}

fn beginMutation(remote: anytype, snapshot: workspace.Snapshot, entry: *Pending) !void {
    if (snapshot.status == .pending) return;
    // Revalidate stable creation targets after refresh, but never rebase direct
    // edits (especially paths). Dispatch now, before the next idle refresh.
    if (entry.refresh_creation) entry.mutation.expected_revision = snapshot.revision;
    try validateTarget(snapshot, entry.mutation);
    entry.request = try remote.requestWorkspaceMutation(entry.mutation);
    entry.mutation_request = entry.request;
    entry.stage = .mutating;
}

fn requestConfirmed(snapshot: workspace.Snapshot, request: u32) !bool {
    if (snapshot.request_id != request) return error.LostCompletion;
    return switch (snapshot.status) {
        .pending => false,
        .confirmed => true,
        .unknown_outcome => error.MutationUnknown,
        .idle, .refused => error.MutationRefused,
    };
}

pub fn validateTarget(snapshot: workspace.Snapshot, mutation: workspace.Mutation) !void {
    if (snapshot.state == .unavailable or snapshot.state == .last_good_error) return error.WorkspaceUnavailable;
    if (snapshot.revision != mutation.expected_revision) return error.StaleTarget;
    if (snapshot.session_id != mutation.session_id) return error.StaleContext;
    if (mutation.kind == .add) return;
    const window = findWindow(snapshot, mutation.window_id) orelse return error.StaleTarget;
    try validateWindowMutation(snapshot, window, mutation);
}

fn validateWindowMutation(snapshot: workspace.Snapshot, window: workspace.Window, mutation: workspace.Mutation) !void {
    switch (mutation.kind) {
        .split, .remove => {
            const ref = mutation.terminal_ref orelse return error.StaleTarget;
            if (!contains(snapshot, window.root, ref, 0)) return error.StaleTarget;
        },
        .resize => try validateResize(snapshot, window.root, mutation),
        .reorder => if (mutation.index >= snapshot.windows.len) return error.StaleTarget,
        .remove_window => {},
        else => return error.UnsupportedMutation,
    }
}

fn validateResize(snapshot: workspace.Snapshot, root: u32, mutation: workspace.Mutation) !void {
    if (!std.math.isFinite(mutation.ratio)) return error.InvalidRatio;
    if (mutation.ratio <= 0 or mutation.ratio >= 1) return error.InvalidRatio;
    if (mutation.path_len > 64) return error.InvalidPath;
    const node = try resizeNode(snapshot, root, mutation.path_bits, mutation.path_len);
    if (node.kind == .leaf) return error.InvalidPath;
}

fn resizeNode(snapshot: workspace.Snapshot, root: u32, path_bits: u64, path_len: u32) !workspace.Node {
    var node = root;
    for (0..path_len) |depth| {
        if (node >= snapshot.nodes.len) return error.InvalidPath;
        const branch = snapshot.nodes[node];
        if (branch.kind == .leaf) return error.InvalidPath;
        node = if ((path_bits >> @intCast(depth)) & 1 == 0) branch.first else branch.second;
    }
    if (node >= snapshot.nodes.len) return error.InvalidPath;
    return snapshot.nodes[node];
}

pub fn findWindow(snapshot: workspace.Snapshot, id: WindowId) ?workspace.Window {
    for (snapshot.windows) |window| {
        if (std.mem.eql(u8, &window.id, &id)) return window;
    }
    return null;
}

pub fn terminalWindow(snapshot: workspace.Snapshot, ref: TerminalRef) ?WindowId {
    for (snapshot.windows) |window| {
        if (contains(snapshot, window.root, ref, 0)) return window.id;
    }
    return null;
}

fn contains(snapshot: workspace.Snapshot, index: u32, ref: TerminalRef, depth: usize) bool {
    if (index >= snapshot.nodes.len or depth > 64) return false;
    const node = snapshot.nodes[index];
    if (node.kind == .leaf) return if (node.terminal_ref) |terminal| terminal.eql(ref) else false;
    return contains(snapshot, node.first, ref, depth + 1) or contains(snapshot, node.second, ref, depth + 1);
}

fn reportRefusal(model: anytype) void {
    model.shared_workspace.refused = true;
    model.terminal_limit_refused = true;
}

test {
    _ = @import("shared_mutations_test.zig");
}
