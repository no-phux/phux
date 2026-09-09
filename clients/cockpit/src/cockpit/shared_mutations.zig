//! A bounded intent queue, never a second topology. Only the provider's winning
//! snapshot may change presentation; removal never destroys a durable process.
const std = @import("std");
const contract = @import("provider_contract");
const workspace = contract.workspace;
const TerminalRef = contract.TerminalRef;
pub const WindowId = workspace.WindowId;
pub const Outcome = enum { confirmed, refused, unknown_outcome };
const Stage = enum { refresh, refreshing, mutation, mutating, complete };
const Pending = struct {
    ticket: u64,
    epoch: u64,
    mutation: workspace.Mutation,
    stage: Stage,
    request: u32 = 0,
    retained: bool,
    outcome: Outcome = .refused,
};

pub const Coordinator = struct {
    pending: [16]?Pending = @splat(null),
    next_ticket: u64 = 0,

    /// Creation retains its completion until consumed, so a later operation can
    /// never overwrite a result before placement has observed it.
    pub fn requestCreation(self: *Coordinator, model: anytype, mutation: workspace.Mutation, epoch: u64) !u64 {
        if (mutation.kind != .add and mutation.kind != .split) return error.UnsupportedMutation;
        try validateContext(model, mutation.session_id, epoch);
        return self.enqueue(mutation, epoch, true, .refresh);
    }

    pub fn takeCompletion(self: *Coordinator, ticket: u64) ?Outcome {
        for (&self.pending) |*slot| {
            const entry = slot.* orelse continue;
            if (entry.ticket != ticket or entry.stage != .complete) continue;
            slot.* = null;
            return entry.outcome;
        }
        return null;
    }

    pub fn forget(self: *Coordinator, ticket: u64) void {
        for (&self.pending) |*slot| {
            const entry = slot.* orelse continue;
            if (entry.ticket == ticket) slot.* = null;
        }
    }

    pub fn requestRemove(self: *Coordinator, model: anytype, ref: TerminalRef) !void {
        errdefer reportRefusal(model);
        var mutation = try currentMutation(model);
        const remote = model.phux().?;
        mutation.kind = .remove;
        mutation.window_id = terminalWindow(remote.workspaceSnapshot(), ref) orelse return error.StaleTarget;
        mutation.terminal_ref = ref;
        try self.requestDirect(model, mutation);
    }

    pub fn requestRemoveWindow(self: *Coordinator, model: anytype, id: WindowId) !void {
        errdefer reportRefusal(model);
        var mutation = try currentMutation(model);
        mutation.kind = .remove_window;
        mutation.window_id = id;
        try self.requestDirect(model, mutation);
    }

    pub fn requestReorder(self: *Coordinator, model: anytype, id: WindowId, index: usize) !void {
        errdefer reportRefusal(model);
        var mutation = try currentMutation(model);
        if (index >= model.phux().?.workspaceSnapshot().windows.len) return error.StaleTarget;
        mutation.kind = .reorder;
        mutation.window_id = id;
        mutation.index = @intCast(index);
        try self.requestDirect(model, mutation);
    }

    /// The caller captures revision at drag start and discards its preview when
    /// that revision changes. This method never rebases a branch path.
    pub fn requestResize(self: *Coordinator, model: anytype, id: WindowId, path_bits: u64, path_len: u32, ratio: f32) !void {
        errdefer reportRefusal(model);
        var mutation = try currentMutation(model);
        mutation.kind = .resize;
        mutation.window_id = id;
        mutation.path_bits = path_bits;
        mutation.path_len = path_len;
        mutation.ratio = ratio;
        try self.requestDirect(model, mutation);
    }

    fn requestDirect(self: *Coordinator, model: anytype, mutation: workspace.Mutation) !void {
        errdefer reportRefusal(model);
        if (self.firstPending() != null) return error.WorkspaceBusy;
        const remote = model.phux().?;
        const snapshot = remote.workspaceSnapshot();
        if (snapshot.status == .pending) return error.WorkspaceBusy;
        try validateTarget(snapshot, mutation);
        const ticket = try self.enqueue(mutation, remote.connectionEpoch(), false, .mutation);
        _ = ticket;
        _ = self.pump(model);
    }

    fn enqueue(self: *Coordinator, mutation: workspace.Mutation, epoch: u64, retained: bool, stage: Stage) !u64 {
        // Names borrow caller memory in the provider API. This queue supports
        // unnamed add and identity-only edits; rename needs owned text first.
        if (mutation.name.len != 0) return error.UnsupportedMutation;
        for (&self.pending) |*slot| {
            if (slot.* != null) continue;
            self.next_ticket = try std.math.add(u64, self.next_ticket, 1);
            slot.* = .{ .ticket = self.next_ticket, .epoch = epoch, .mutation = mutation, .retained = retained, .stage = stage };
            return self.next_ticket;
        }
        return error.OperationCapacity;
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
            entry.outcome = if (err == error.StaleContext) .unknown_outcome else .refused;
            entry.stage = .complete;
        };
        if (entry.stage != .complete) return false;
        if (entry.outcome != .confirmed) reportRefusal(model);
        if (!entry.retained) slot.* = null;
        return true;
    }

    pub fn disconnect(self: *Coordinator, model: anytype) void {
        for (self.pending) |entry| {
            if (entry != null) reportRefusal(model);
        }
        @memset(&self.pending, null);
    }
};

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
    if (entry.retained) entry.mutation.expected_revision = snapshot.revision;
    try validateTarget(snapshot, entry.mutation);
    entry.request = try remote.requestWorkspaceMutation(entry.mutation);
    entry.stage = .mutating;
}

fn requestConfirmed(snapshot: workspace.Snapshot, request: u32) !bool {
    if (snapshot.request_id != request) return error.LostCompletion;
    return switch (snapshot.status) {
        .pending => false,
        .confirmed => true,
        .unknown_outcome => error.StaleContext,
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
