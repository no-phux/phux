//! Captured local tool admission and outcome ownership. The runtime owns session
//! preparation and durable placement; this adapter never drains a provider queue.
const std = @import("std");
const support = @import("../phux_support.zig");
const contract = @import("provider_contract");
const local_tools = @import("local_tools.zig");

pub const PlacementStatus = enum { pending, placed, refused, unknown };
pub const Execution = enum { not_sent, success, refused, unknown };
pub const Placement = enum { not_requested, placed, refused, unknown, destination_lost };
pub const Reason = enum { completed, preparation_failed, spawn_refused, spawn_unknown, invalid_identity, window_closed, context_changed, placement_refused, placement_unknown };
pub const Cleanup = enum { not_requested, pending, admitted, exhausted, unavailable };
const owner_capacity = 16;
/// One admission attempt per owner per advance, with the same finite budget as
/// retained owners. Exhaustion retains authority until definitive source retirement.
pub const cleanup_admission_attempts = owner_capacity;

pub const Context = struct {
    coordinator: support.ProviderId,
    provider: u64,
    host: u64,
    connection: u64,
    session: ?u32,

    fn capture(remote: *support.PhuxProvider) Context {
        return .{ .coordinator = remote.providerId(), .provider = remote.context_id, .host = remote.host.context_id, .connection = remote.connectionEpoch(), .session = remote.selectedSessionId() };
    }

    fn matches(self: Context, remote: *support.PhuxProvider) bool {
        return self.coordinator == remote.providerId() and self.provider == remote.context_id and
            self.host == remote.host.context_id and self.connection == remote.connectionEpoch();
    }
};

pub const Outcome = struct {
    operation_id: u32,
    request_id: u32,
    context: ?Context,
    window: usize,
    window_epoch: u64,
    terminal_ref: ?contract.TerminalRef,
    execution: Execution,
    placement: Placement,
    reason: Reason,
    cleanup: Cleanup = .not_requested,
    cleanup_request: u32 = 0,
};
pub const Status = union(enum) { pending, completed: Outcome };

/// Parent retains the adapter and installs this sink on the Engine. Runtime
/// invokes it before ordinary operation consumers, including disconnect drains.
pub const Sink = struct {
    context: *anyopaque,
    complete: *const fn (*anyopaque, *support.PhuxProvider, support.OperationResult) bool,
};

const Arguments = struct {
    arena: std.heap.ArenaAllocator,
    argv: []const []const u8,
    cwd: []const u8,
    title: []const u8,

    fn copy(gpa: std.mem.Allocator, argv: []const []const u8, cwd: []const u8, title: []const u8) !Arguments {
        if (argv.len == 0) return error.InvalidArguments;
        var arena = std.heap.ArenaAllocator.init(gpa);
        errdefer arena.deinit();
        const allocator = arena.allocator();
        const owned = try allocator.alloc([]const u8, argv.len);
        for (argv, owned) |arg, *out| out.* = try allocator.dupe(u8, arg);
        return .{ .arena = arena, .argv = owned, .cwd = try allocator.dupe(u8, cwd), .title = try allocator.dupe(u8, title) };
    }
};

const Pending = struct {
    id: u32,
    window: usize,
    window_epoch: u64,
    selection_epoch: u64,
    arguments: Arguments,
    preparation_provider: ?u64 = null,
    context: ?Context = null,
    request: u32 = 0,
    result: ?support.OperationResult = null,
    ticket: ?u64 = null,
    outcome: ?Outcome = null,
    reported: bool = false,
    acknowledged: bool = false,
    ownership_settled: bool = false,
    abandoned: bool = false,
    cleanup: Cleanup = .not_requested,
    cleanup_attempts: usize = 0,
    cleanup_request: u32 = 0,

    fn ownsResult(self: *const Pending, remote: *support.PhuxProvider, result: support.OperationResult) bool {
        const context = self.context orelse return false;
        if (!context.matches(remote)) return false;
        return self.request == result.request_id and context.connection == result.connection_epoch;
    }

    fn execution(self: *const Pending) Execution {
        const result = self.result orelse return if (self.request == 0) .not_sent else .unknown;
        return switch (result.status) {
            .success => .success,
            .refused => .refused,
            .unknown_outcome => .unknown,
        };
    }

    fn finish(self: *Pending, placement: Placement, reason: Reason) void {
        const outcome: Outcome = .{
            .operation_id = self.id,
            .request_id = self.request,
            .context = self.context,
            .window = self.window,
            .window_epoch = self.window_epoch,
            .terminal_ref = if (self.result) |result| result.terminal_ref else null,
            .execution = self.execution(),
            .placement = placement,
            .reason = reason,
            .cleanup = self.cleanup,
            .cleanup_request = self.cleanup_request,
        };
        if (self.outcome) |previous| if (std.meta.eql(previous, outcome)) return;
        self.outcome = outcome;
        self.reported = false;
    }

    fn settle(self: *Pending, placement: Placement, reason: Reason) void {
        self.ownership_settled = true;
        self.finish(placement, reason);
    }

    fn abandon(self: *Pending, placement: Placement, reason: Reason) void {
        self.abandoned = true;
        self.finish(placement, reason);
    }
};

pub const Adapter = struct {
    gpa: std.mem.Allocator = std.heap.page_allocator,
    // Matches the native durable-creation queue's admission bound.
    pending: [owner_capacity]?Pending = @splat(null),
    next_id: u32 = 1,

    pub fn deinit(self: *Adapter) void {
        for (&self.pending) |*slot| {
            if (slot.*) |*entry| entry.arguments.arena.deinit();
            slot.* = null;
        }
    }

    pub fn sink(self: *Adapter) Sink {
        return .{ .context = self, .complete = receive };
    }

    /// A transient view for local_tools.handle; parent retains Adapter and the
    /// handler's capture State. `cwd` must be local, never the focused remote CWD.
    pub fn service(self: *Adapter, engine: anytype, fx: anytype, cwd: []const u8) Service(@TypeOf(engine), @TypeOf(fx)) {
        return .{ .adapter = self, .engine = engine, .model = engine.model, .fx = fx, .cwd = cwd };
    }

    fn receive(context: *anyopaque, remote: *support.PhuxProvider, result: support.OperationResult) bool {
        const self: *Adapter = @ptrCast(@alignCast(context));
        return self.completeFrom(remote, result);
    }

    fn vacant(self: *Adapter) !*?Pending {
        for (&self.pending) |*slot| if (slot.* == null) return slot;
        return error.OperationCapacity;
    }

    /// Admission owns argv/cwd before any asynchronous continuation. IDs are
    /// adapter receipts, not provider request IDs (which can collide across clients).
    pub fn launch(self: *Adapter, engine: anytype, fx: anytype, window: usize, epoch: u64, argv: []const []const u8, cwd: []const u8, title: []const u8) !u32 {
        if (comptime !support.phux_enabled) return error.LocalRuntimeNotReady;
        if (!windowCurrent(engine.model, window, epoch)) return error.InvalidWindow;
        const slot = try self.vacant();
        const id = self.next_id;
        const next = std.math.add(u32, id, 1) catch return error.OperationCapacity;
        var arguments = try Arguments.copy(self.gpa, argv, cwd, title);
        errdefer arguments.arena.deinit();
        slot.* = .{ .id = id, .window = window, .window_epoch = epoch, .selection_epoch = engine.localToolSelectionEpoch(), .arguments = arguments };
        errdefer slot.* = null;
        try prepare(&slot.*.?, engine, fx);
        self.next_id = next;
        return id;
    }

    /// Exactly owned SPAWN results only. Other consumers retain every unrelated
    /// operation, even one with an equal per-client request/connection number.
    pub fn completeFrom(self: *Adapter, remote: *support.PhuxProvider, result: support.OperationResult) bool {
        if (comptime !support.phux_enabled) return false;
        if (result.kind != .spawn) return false;
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (!entry.ownsResult(remote, result)) continue;
            if (entry.result == null) entry.result = result;
            return true;
        }
        return false;
    }

    /// Call after runtime channel/timer processing. No accepted write is retried:
    /// preparation can wait, but a nonzero request is never submitted again.
    pub fn advance(self: *Adapter, engine: anytype, fx: anytype) void {
        if (comptime !support.phux_enabled) return;
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (!entry.ownership_settled) advanceOne(entry, engine, fx);
            releaseAcknowledged(slot);
        }
    }

    pub fn takeOutcome(self: *Adapter) ?Outcome {
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.reported or entry.acknowledged) continue;
            const outcome = entry.outcome orelse continue;
            entry.reported = true;
            return outcome;
        }
        return null;
    }

    pub fn status(self: *const Adapter, id: u32) ?Status {
        for (self.pending) |slot| {
            const entry = slot orelse continue;
            if (entry.id != id or entry.acknowledged) continue;
            return if (entry.outcome) |outcome| .{ .completed = outcome } else .pending;
        }
        return null;
    }

    pub fn acknowledge(self: *Adapter, id: u32) bool {
        for (&self.pending) |*slot| {
            const entry = if (slot.*) |*value| value else continue;
            if (entry.id != id or entry.outcome == null) continue;
            entry.acknowledged = true;
            releaseAcknowledged(slot);
            return true;
        }
        return false;
    }
};

/// Acknowledgement retires only UI interest. Outstanding requests and cleanup
/// keep their bounded slot until disposition or an exact-source retirement.
fn releaseAcknowledged(slot: *?Pending) void {
    const entry = if (slot.*) |*value| value else return;
    if (!entry.acknowledged or !entry.ownership_settled) return;
    entry.arguments.arena.deinit();
    slot.* = null;
}

pub fn Service(comptime Engine: type, comptime Effects: type) type {
    return struct {
        const Self = @This();
        adapter: *Adapter,
        engine: Engine,
        model: @FieldType(std.meta.Child(Engine), "model"),
        fx: Effects,
        cwd: []const u8,

        pub fn launchLocalTool(self: *Self, _: anytype, platform_window: u64, argv: []const []const u8, title: []const u8) !u32 {
            for (0..self.model.window_epochs.len) |window| {
                const workspace = self.model.wsAt(window) orelse continue;
                if (workspace.window_id != platform_window) continue;
                return self.adapter.launch(self.engine, self.fx, window, self.model.window_epochs[window], argv, self.cwd, title);
            }
            return error.InvalidWindow;
        }

        pub fn localToolCli(self: *Self, out: []u8) ![]const u8 {
            if (comptime !support.phux_enabled) return error.LocalRuntimeNotReady;
            const remote = self.engine.localToolProvider() orelse return error.LocalRuntimeNotReady;
            return remote.localToolCli(out) orelse error.LocalRuntimeNotReady;
        }

        pub fn localToolStatus(self: *Self, id: u32) local_tools.ToolStatus {
            const current = self.adapter.status(id) orelse return .{ .phase = .unknown, .message = "The launch outcome is unavailable. Check This Mac; this request will not be replayed." };
            return switch (current) {
                .pending => .{ .phase = .queued, .message = "Waiting for the local terminal and its placement" },
                .completed => |outcome| describeOutcome(outcome),
            };
        }

        pub fn acknowledgeLocalTool(self: *Self, id: u32) bool {
            return self.adapter.acknowledge(id);
        }
    };
}

fn describeOutcome(outcome: Outcome) local_tools.ToolStatus {
    if (cleanupNotice(outcome.cleanup)) |message| return .{ .phase = .unknown, .message = message };
    if (outcome.execution == .success and outcome.placement == .placed) return .{ .phase = .placed, .message = "The dedicated local Phux terminal is open" };
    if (outcome.execution == .unknown or outcome.placement == .unknown) return .{ .phase = .unknown, .message = "The launch outcome is uncertain. Check This Mac before another launch; this request will not be replayed." };
    return .{ .phase = .failed, .message = switch (outcome.reason) {
        .window_closed => "The invoking window closed before the local terminal could be placed.",
        .preparation_failed => "Local Phux could not prepare the captured session. Check This Mac and try the action again.",
        .spawn_refused => "Local Phux refused the tool process. Check This Mac before trying the action again.",
        else => "The local tool terminal could not be placed. Check This Mac; existing work is intact.",
    } };
}

fn cleanupNotice(cleanup: Cleanup) ?[]const u8 {
    return switch (cleanup) {
        .pending => "The local process started but could not be placed. Conditional cleanup is waiting for capacity; its identity remains tracked. Check This Mac.",
        .exhausted, .unavailable => "The local process started but conditional cleanup could not be queued. Its identity remains recorded. Check This Mac before another launch.",
        .not_requested, .admitted => null,
    };
}

fn windowCurrent(model: anytype, window: usize, epoch: u64) bool {
    if (window >= model.window_epochs.len) return false;
    return model.windowOpen(window) and model.window_epochs[window] == epoch;
}

fn prepare(entry: *Pending, engine: anytype, fx: anytype) !void {
    if (!windowCurrent(engine.model, entry.window, entry.window_epoch)) return error.InvalidWindow;
    try bindPreparation(entry, engine.localToolProvider());
    const remote = engine.ensureLocalSessionInWindow(entry.window, entry.window_epoch, fx) catch |err| {
        if (err != error.NotReady) return err;
        // Session preparation may allocate the first local provider. Once
        // captured, even this pre-write continuation cannot follow a replacement.
        try bindPreparation(entry, engine.localToolProvider());
        return;
    };
    try bindPreparation(entry, remote);
    if (remote.endpointDescriptor() != .unix or remote.state() != .attached) return error.LocalRuntimeNotReady;
    entry.context = Context.capture(remote);
    entry.request = try remote.requestSpawnArgvBound(null, remote.attach_viewport, entry.arguments.cwd, entry.arguments.argv);
}

fn bindPreparation(entry: *Pending, remote: ?*support.PhuxProvider) !void {
    const provider = remote orelse {
        if (entry.preparation_provider != null) return error.ContextChanged;
        return;
    };
    if (entry.preparation_provider) |context| {
        if (context != provider.context_id) return error.ContextChanged;
    }
    entry.preparation_provider = provider.context_id;
}

fn advanceOne(entry: *Pending, engine: anytype, fx: anytype) void {
    if (entry.request == 0) {
        prepare(entry, engine, fx) catch entry.settle(.not_requested, .preparation_failed);
        return;
    }
    const context = entry.context orelse return entry.settle(.not_requested, .preparation_failed);
    const remote = engine.model.phuxForAttachment(context.provider) orelse return retireSource(entry);
    if (!context.matches(remote)) return retireSource(entry);
    advanceCurrentSource(entry, engine, remote);
}

/// Missing/replaced attachment, host or connection epoch is definitive: no new
/// client may inherit this request or replay cleanup with old authority.
fn retireSource(entry: *Pending) void {
    if (entry.cleanup == .pending) entry.cleanup = .unavailable;
    entry.settle(.unknown, .context_changed);
}

fn advanceCurrentSource(entry: *Pending, engine: anytype, remote: *support.PhuxProvider) void {
    if (entry.abandoned) return disposeAbandoned(entry, remote);
    if (entry.context.?.session != remote.selectedSessionId()) {
        if (entry.ticket != null) return entry.settle(.unknown, .context_changed);
        entry.abandon(.unknown, .context_changed);
        return disposeAbandoned(entry, remote);
    }
    if (entry.ticket) |ticket| return observePlacement(entry, engine, remote, ticket);
    const result = entry.result orelse {
        if (remote.state() != .attached) entry.abandon(.not_requested, .spawn_unknown);
        return;
    };
    placeResult(entry, engine, remote, result);
}

fn placeResult(entry: *Pending, engine: anytype, remote: *support.PhuxProvider, result: support.OperationResult) void {
    if (result.status != .success) return settleUnsuccessful(entry, result);
    if (remote.state() != .attached) {
        entry.abandon(.unknown, .context_changed);
        return disposeAbandoned(entry, remote);
    }
    if (!validSpawn(remote, result)) return entry.settle(.refused, .invalid_identity);
    if (!windowCurrent(engine.model, entry.window, entry.window_epoch)) {
        entry.abandon(.destination_lost, .window_closed);
        return disposeAbandoned(entry, remote);
    }
    beginPlacement(entry, engine, remote, result);
}

fn validSpawn(remote: *support.PhuxProvider, result: support.OperationResult) bool {
    const ref = result.terminal_ref orelse return false;
    if (ref.provider_id != remote.providerId()) return false;
    return switch (ref.terminal_id) {
        .local => false,
        .phux => |id| id.kind == 0,
    };
}

fn beginPlacement(entry: *Pending, engine: anytype, remote: *support.PhuxProvider, result: support.OperationResult) void {
    const may_focus = entry.selection_epoch == engine.localToolSelectionEpoch();
    entry.ticket = engine.placeLocalToolSpawn(remote, entry.window, entry.window_epoch, result, may_focus) catch {
        entry.abandon(.refused, .placement_refused);
        return disposeAbandoned(entry, remote);
    };
}

fn observePlacement(entry: *Pending, engine: anytype, remote: *support.PhuxProvider, ticket: u64) void {
    switch (engine.localToolPlacementStatus(remote, ticket)) {
        .pending => {},
        .placed => entry.settle(.placed, .completed),
        .refused => entry.settle(.refused, .placement_refused),
        .unknown => entry.settle(.unknown, .placement_unknown),
    }
}

fn settleUnsuccessful(entry: *Pending, result: support.OperationResult) void {
    entry.settle(.not_requested, if (result.status == .refused) .spawn_refused else .spawn_unknown);
}

/// A terminal UI outcome does not finish a source request. Late success is
/// accounted for here even after acknowledgement, without placing or respawning.
fn disposeAbandoned(entry: *Pending, remote: *support.PhuxProvider) void {
    const result = entry.result orelse return;
    if (result.status != .success) return settleUnsuccessful(entry, result);
    if (!validSpawn(remote, result)) return entry.settle(.refused, .invalid_identity);
    if (entry.cleanup == .not_requested) entry.cleanup = .pending;
    if (entry.cleanup == .pending) admitCleanup(entry, remote, result);
    const prior = entry.outcome.?;
    entry.finish(if (prior.placement == .unknown) .destination_lost else prior.placement, prior.reason);
}

fn admitCleanup(entry: *Pending, remote: *support.PhuxProvider, result: support.OperationResult) void {
    const instance = result.instance orelse {
        entry.cleanup = .unavailable;
        return;
    };
    if (!remote.conditionalKillSupported()) {
        entry.cleanup = .unavailable;
        return;
    }
    entry.cleanup_attempts += 1;
    entry.cleanup_request = remote.requestKillIf(result.terminal_ref.?, instance) catch |err| {
        entry.cleanup = cleanupAdmissionFailure(err, entry.cleanup_attempts);
        return;
    };
    // Transfer to the exact host's accepted operation ledger. Admission is not
    // evidence that the conditional kill executed; never resend an accepted write.
    entry.cleanup = .admitted;
    entry.ownership_settled = true;
}

fn cleanupAdmissionFailure(err: anyerror, attempts: usize) Cleanup {
    if (err != error.OperationCapacity) return .unavailable;
    if (attempts >= cleanup_admission_attempts) return .exhausted;
    return .pending;
}

test {
    _ = @import("../../tests/local_tool_launch_tests.zig");
}
