//! Actual Model and Phux FFI frames; no process, config, socket or GUI launch.
const std = @import("std");
const support = @import("../cockpit/phux_support.zig");
const model_module = @import("../cockpit/model.zig");
const engine_module = @import("../cockpit/native/ts_engine.zig");
const launch = @import("../cockpit/native/local_tool_launch.zig");
const tools = @import("../cockpit/native/local_tools.zig");
const fixture = support.PhuxProvider.test_support;
const testing = std.testing;

test "localtool disabled provider refuses without effects" {
    if (comptime support.phux_enabled) return error.SkipZigTest;
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    var engine = struct {}{};
    _ = adapter.sink();
    try testing.expectError(error.LocalRuntimeNotReady, adapter.launch(&engine, {}, 0, 0, &.{"/fixture/editor"}, "", "Editor"));
    adapter.advance(&engine, {});
}

// Only the new runtime orchestration hooks are controlled here. The Model,
// providers, operation ledgers, argv encoder and disconnect outcomes are real.
const Harness = struct {
    native: *engine_module.Engine,
    model: *model_module.Model,
    local: *support.PhuxProvider,
    remote: *support.PhuxProvider,
    ready: bool = true,
    selection: u64 = 1,
    placements: usize = 0,
    placement_status: launch.PlacementStatus = .pending,
    placement_refused: bool = false,
    may_focus: bool = true,

    fn init() !Harness {
        const native = try engine_module.Engine.create(testing.allocator, testing.io);
        errdefer native.destroy();
        const remote = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .remote = .{ .target = "fixture-remote" } }, null, "tool-test");
        native.model.phux_provider = remote;
        try fixture.attachHostWith(remote.host, "hello_conditional_kill.bin");
        try native.model.ensurePeerSlots(1);
        const local = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture-local-never-dialed" }, null, "tool-test");
        native.model.peers.items[0].provider = local;
        try fixture.attachHostWith(local.host, "hello_conditional_kill.bin");
        return .{ .native = native, .model = native.model, .local = local, .remote = remote };
    }

    fn deinit(self: *Harness) void {
        self.native.destroy();
    }

    pub fn localToolProvider(self: *Harness) ?*support.PhuxProvider {
        return self.local;
    }

    pub fn localToolSelectionEpoch(self: *Harness) u64 {
        return self.selection;
    }

    pub fn ensureLocalSessionInWindow(self: *Harness, _: usize, _: u64, _: void) !*support.PhuxProvider {
        if (!self.ready) return error.NotReady;
        return self.local;
    }

    pub fn placeLocalToolSpawn(self: *Harness, remote: *support.PhuxProvider, _: usize, _: u64, _: support.OperationResult, may_focus: bool) !u64 {
        try testing.expect(remote == self.local);
        if (self.placement_refused) return error.OperationCapacity;
        self.placements += 1;
        self.may_focus = may_focus;
        return 19;
    }

    pub fn localToolPlacementStatus(self: *Harness, remote: *support.PhuxProvider, ticket: u64) launch.PlacementStatus {
        std.debug.assert(remote == self.local and ticket == 19);
        return self.placement_status;
    }

    fn complete(self: *Harness, adapter: *launch.Adapter, name: []const u8) !void {
        try fixture.stageFixture(self.local.bridge, name);
        _ = try self.local.host.drainReadiness();
        const result = self.local.takeOperationResult() orelse return error.MissingOperation;
        try testing.expect(adapter.completeFrom(self.local, result));
    }
};

fn submit(adapter: *launch.Adapter, harness: *Harness) !u32 {
    return adapter.launch(harness, {}, 0, harness.model.window_epochs[0], &.{ "/fixture/editor", "--wait", "/local config with spaces" }, "/local cwd", "Edit Configuration — This Mac");
}

// SPEC's length-delimited fields and string-list encoding, independent of the
// adapter. Verify argv boundaries and the absence of satellite/owner routing.
const Wire = struct {
    bytes: []const u8,
    at: usize = 0,

    fn varint(self: *Wire) !usize {
        var value: usize = 0;
        var shift: u6 = 0;
        while (self.at < self.bytes.len) {
            const byte = self.bytes[self.at];
            self.at += 1;
            value |= @as(usize, byte & 0x7f) << shift;
            if (byte < 128) return value;
            shift = std.math.add(u6, shift, 7) catch return error.BadVarint;
        }
        return error.ShortFrame;
    }

    fn take(self: *Wire, size: usize) ![]const u8 {
        if (size > self.bytes.len - self.at) return error.ShortFrame;
        const bytes = self.bytes[self.at..][0..size];
        self.at += size;
        return bytes;
    }
};

fn field(frame: []const u8, wanted: usize) !?[]const u8 {
    var wire: Wire = .{ .bytes = frame, .at = 5 };
    while (wire.at < frame.len) {
        const id = try wire.varint();
        _ = try wire.take(1);
        const value = try wire.take(try wire.varint());
        if (id == wanted) return value;
    }
    return null;
}

fn expectSpawn(provider: *support.PhuxProvider, argv: []const []const u8, cwd: []const u8) !void {
    const frame = provider.bridge.outgoing.take() orelse return error.MissingFrame;
    defer provider.bridge.outgoing.release(frame);
    try testing.expectEqual(@as(u8, 0x22), frame[4]);
    var command: Wire = .{ .bytes = (try field(frame, 3)).? };
    const count = try command.take(4);
    try testing.expectEqual(argv.len, std.mem.readInt(u32, count[0..4], .big));
    for (argv) |expected| {
        const length = try command.take(4);
        try testing.expectEqualStrings(expected, try command.take(std.mem.readInt(u32, length[0..4], .big)));
    }
    try testing.expectEqual(command.bytes.len, command.at);
    try testing.expectEqualStrings(cwd, (try field(frame, 4)).?);
    try testing.expect(try field(frame, 7) == null);
    try testing.expect(try field(frame, 8) == null);
    try testing.expectEqualSlices(u8, &.{1}, (try field(frame, 15)).?);
    try testing.expect(provider.bridge.outgoing.take() == null);
}

fn expectConditionalCleanup(provider: *support.PhuxProvider) !void {
    const frame = provider.bridge.outgoing.take() orelse return error.MissingFrame;
    defer provider.bridge.outgoing.release(frame);
    try testing.expectEqual(@as(u8, 0x31), frame[4]);
    const command = (try field(frame, 2)).?;
    try testing.expectEqual(@as(u8, 0x1b), command[0]);
    const instance: [16]u8 = @splat(0xa5);
    try testing.expect(std.mem.indexOf(u8, command, &instance) != null);
    try testing.expect(!provider.bridge.outgoing.hasPending());
}

test "localtool copies preparing argv and cwd then queues only on local Phux" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    h.ready = false;
    var argument = "/config with spaces".*;
    var cwd = "/local cwd".*;
    const id = try adapter.launch(&h, {}, 0, h.model.window_epochs[0], &.{ "/editor path", "--wait", &argument }, &cwd, "Editor");
    @memset(&argument, 'x');
    @memset(&cwd, 'x');
    try testing.expect(!h.local.bridge.outgoing.hasPending());
    h.ready = true;
    adapter.advance(&h, {});
    try expectSpawn(h.local, &.{ "/editor path", "--wait", "/config with spaces" }, "/local cwd");
    try testing.expect(!h.remote.bridge.outgoing.hasPending());
    try testing.expect(adapter.takeOutcome() == null);
    try testing.expect(!adapter.acknowledge(id));
    try h.complete(&adapter, "spawn-bound.bin");
    adapter.advance(&h, {});
    try testing.expectEqual(@as(usize, 1), h.placements);
    try testing.expect(adapter.takeOutcome() == null);
    h.placement_status = .placed;
    adapter.advance(&h, {});
    const outcome = adapter.takeOutcome().?;
    try testing.expectEqual(id, outcome.operation_id);
    try testing.expectEqual(.success, outcome.execution);
    try testing.expectEqual(.placed, outcome.placement);
    try testing.expect(outcome.terminal_ref.?.provider_id == h.local.providerId());
    try testing.expect(adapter.takeOutcome() == null);
    try testing.expectEqual(.placed, adapter.status(id).?.completed.placement);
    try testing.expect(adapter.acknowledge(id));
    try testing.expect(adapter.status(id) == null);
}

test "localtool setup argv uses a dedicated spawn and unknown disconnect never replays" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    var arena = std.heap.ArenaAllocator.init(testing.allocator);
    defer arena.deinit();
    const argv = try tools.enrollmentArgv(arena.allocator(), "/fixture/phux", "me@fixture", "Fixture Mac");
    _ = try adapter.launch(&h, {}, 0, h.model.window_epochs[0], argv, "/local cwd", "Add Machine");
    try expectSpawn(h.local, &.{ "/fixture/phux", "host", "enroll", "--name", "Fixture Mac", "--", "me@fixture" }, "/local cwd");
    h.local.host.disconnect();
    const result = h.local.takeOperationResult().?;
    try testing.expectEqual(.unknown_outcome, result.status);
    try testing.expect(adapter.completeFrom(h.local, result));
    adapter.advance(&h, {});
    const outcome = adapter.takeOutcome().?;
    try testing.expectEqual(.unknown, outcome.execution);
    try testing.expectEqual(.not_requested, outcome.placement);
    adapter.advance(&h, {});
    try testing.expectEqual(@as(usize, 0), h.placements);
    try testing.expect(!h.local.bridge.outgoing.hasPending());
    try testing.expect(!h.remote.bridge.outgoing.hasPending());
}

test "localtool spawn refusal is final and is never sent to placement" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    _ = try submit(&adapter, &h);
    try h.complete(&adapter, "spawn-refused.bin");
    adapter.advance(&h, {});
    const outcome = adapter.takeOutcome().?;
    try testing.expectEqual(.refused, outcome.execution);
    try testing.expectEqual(.not_requested, outcome.placement);
    try testing.expectEqual(@as(usize, 0), h.placements);
}

test "localtool captured window closure conditionally cleans a known bound spawn only" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    _ = try submit(&adapter, &h);
    h.local.bridge.outgoing.reset();
    h.model.window_epochs[0] += 1;
    try h.complete(&adapter, "spawn-bound.bin");
    adapter.advance(&h, {});
    const outcome = adapter.takeOutcome().?;
    try testing.expectEqual(.success, outcome.execution);
    try testing.expectEqual(.destination_lost, outcome.placement);
    try testing.expectEqual(@as(usize, 0), h.placements);
    try expectConditionalCleanup(h.local);
    try testing.expect(!h.remote.bridge.outgoing.hasPending());
}

test "localtool later selection suppresses focus and uncertain placement never kills" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    _ = try submit(&adapter, &h);
    h.local.bridge.outgoing.reset();
    h.selection += 1;
    try h.complete(&adapter, "spawn-bound.bin");
    adapter.advance(&h, {});
    try testing.expect(!h.may_focus);
    h.placement_status = .unknown;
    adapter.advance(&h, {});
    const outcome = adapter.takeOutcome().?;
    try testing.expectEqual(.success, outcome.execution);
    try testing.expectEqual(.unknown, outcome.placement);
    try testing.expect(!h.local.bridge.outgoing.hasPending());
}

test "localtool replaced client with reused request and epoch cannot complete old launch" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    _ = try submit(&adapter, &h);
    const previous = h.local;
    defer previous.destroy();
    const replacement = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture-local-never-dialed" }, null, "replacement");
    h.model.peers.items[0].provider = replacement;
    h.local = replacement;
    try fixture.attachHostWith(replacement.host, "hello_conditional_kill.bin");
    const request = try replacement.requestSpawnArgvBound(null, replacement.attach_viewport, "", &.{"/unrelated"});
    try fixture.stageFixture(replacement.bridge, "spawn-bound.bin");
    _ = try replacement.host.drainReadiness();
    const result = replacement.takeOperationResult().?;
    try testing.expectEqual(@as(u32, 1), request);
    try testing.expectEqual(previous.connectionEpoch(), result.connection_epoch);
    try testing.expect(!adapter.completeFrom(replacement, result));
    adapter.advance(&h, {});
    try testing.expectEqual(.context_changed, adapter.takeOutcome().?.reason);
    try testing.expectEqual(@as(usize, 0), h.placements);
}

test "localtool preparing launch refuses replacement before any spawn" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    h.ready = false;
    _ = try submit(&adapter, &h);
    const previous = h.local;
    defer previous.destroy();
    const replacement = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture-local-never-dialed" }, null, "replacement");
    h.model.peers.items[0].provider = replacement;
    h.local = replacement;
    h.ready = true;
    try fixture.attachHostWith(replacement.host, "hello_conditional_kill.bin");
    adapter.advance(&h, {});
    try testing.expectEqual(.not_sent, adapter.takeOutcome().?.execution);
    try testing.expect(!replacement.bridge.outgoing.hasPending());
}

test "localtool placement admission refusal retains spawn truth and cleans conditionally" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    _ = try submit(&adapter, &h);
    h.local.bridge.outgoing.reset();
    h.placement_refused = true;
    try h.complete(&adapter, "spawn-bound.bin");
    adapter.advance(&h, {});
    const outcome = adapter.takeOutcome().?;
    try testing.expectEqual(.success, outcome.execution);
    try testing.expectEqual(.refused, outcome.placement);
    try expectConditionalCleanup(h.local);
}

test "localtool replaced host inside same provider cannot satisfy captured source context" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    _ = try submit(&adapter, &h);
    const previous = h.local.host;
    defer previous.destroy();
    h.local.host = try @TypeOf(previous.*).create(testing.allocator, h.local.bridge);
    h.local.host.setProviderId(h.local.providerId());
    try fixture.attachHostWith(h.local.host, "hello_conditional_kill.bin");
    _ = try h.local.requestSpawnArgvBound(null, h.local.attach_viewport, "", &.{"/unrelated"});
    try fixture.stageFixture(h.local.bridge, "spawn-bound.bin");
    _ = try h.local.host.drainReadiness();
    const result = h.local.takeOperationResult().?;
    try testing.expectEqual(previous.connectionEpoch(), result.connection_epoch);
    try testing.expect(!adapter.completeFrom(h.local, result));
    adapter.advance(&h, {});
    try testing.expectEqual(.context_changed, adapter.takeOutcome().?.reason);
    try testing.expectEqual(@as(usize, 0), h.placements);
}

test "localtool Service maps captured platform window and retains completion until acknowledgement" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    var service = adapter.service(&h, {}, "/local cwd");
    const id = try service.launchLocalTool({}, h.model.wsAt(0).?.window_id, &.{"/fixture/editor"}, "Editor");
    try expectSpawn(h.local, &.{"/fixture/editor"}, "/local cwd");
    try testing.expectEqual(.queued, service.localToolStatus(id).phase);
    try testing.expect(!service.acknowledgeLocalTool(id));
    try h.complete(&adapter, "spawn-bound.bin");
    adapter.advance(&h, {});
    try testing.expectEqual(.queued, service.localToolStatus(id).phase);
    h.placement_status = .placed;
    adapter.advance(&h, {});
    try testing.expectEqual(.placed, service.localToolStatus(id).phase);
    _ = adapter.takeOutcome();
    try testing.expectEqual(.placed, service.localToolStatus(id).phase);
    try testing.expect(service.acknowledgeLocalTool(id));
    try testing.expectEqual(.unknown, service.localToolStatus(id).phase);
}

fn fillOperationLedger(remote: *support.PhuxProvider) !u32 {
    const ref: support.TerminalRef = .{ .provider_id = remote.providerId(), .terminal_id = .{ .phux = try support.RemoteResourceId.fromPhux(0, 8, "") } };
    var first: u32 = 0;
    while (true) {
        const request = remote.requestKillIf(ref, @splat(0xa5)) catch |err| {
            try testing.expectEqual(error.OperationCapacity, err);
            break;
        };
        if (first == 0) first = request;
    }
    try testing.expect(first != 0);
    // These are background fixture operations, not adapter cleanup attempts.
    remote.bridge.outgoing.reset();
    return first;
}

fn freeOperationSlot(remote: *support.PhuxProvider, request: u32) !void {
    try remote.host.operation_ledger.complete(.{ .request_id = request, .connection_epoch = remote.connectionEpoch(), .kind = .kill_if, .status = .success });
    try testing.expectEqual(request, remote.takeOperationResult().?.request_id);
}

fn cleanupCapacityRegression(close_window: bool) !void {
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    const id = try submit(&adapter, &h);
    try h.complete(&adapter, "spawn-bound.bin");
    const occupied = try fillOperationLedger(h.local);
    if (close_window) h.model.window_epochs[0] += 1 else h.placement_refused = true;
    adapter.advance(&h, {});
    try testing.expectEqual(.success, adapter.status(id).?.completed.execution);
    try testing.expect(!h.local.bridge.outgoing.hasPending());
    try testing.expect(adapter.acknowledge(id));
    try freeOperationSlot(h.local, occupied);
    adapter.advance(&h, {});
    // d4463859 discarded the OperationCapacity error then ack freed its owner.
    try expectConditionalCleanup(h.local);
    adapter.advance(&h, {});
    try testing.expect(!h.local.bridge.outgoing.hasPending());
    try testing.expect(!h.remote.bridge.outgoing.hasPending());
    try testing.expectEqual(@as(usize, 0), h.placements);
}

test "localtool cleanup capacity retries after window closure and UI acknowledgement" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    try cleanupCapacityRegression(true);
}

test "localtool cleanup capacity retries after placement refusal and UI acknowledgement" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    try cleanupCapacityRegression(false);
}

fn lateSessionResultRegression(acknowledge: bool) !void {
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    const id = try submit(&adapter, &h);
    h.local.bridge.outgoing.reset();
    const source = h.local.host.context_id;
    const epoch = h.local.connectionEpoch();
    // Change the actual host's published selection, not its client or ledger.
    // The outstanding FFI spawn still belongs to precisely this source/epoch.
    h.local.host.attached_session_id = 2;
    adapter.advance(&h, {});
    try testing.expectEqual(.unknown, adapter.status(id).?.completed.execution);
    if (acknowledge) try testing.expect(adapter.acknowledge(id));
    try testing.expectEqual(source, h.local.host.context_id);
    try testing.expectEqual(epoch, h.local.connectionEpoch());
    try h.complete(&adapter, "spawn-bound.bin");
    adapter.advance(&h, {});
    // d4463859 either swallowed this result behind outcome!=null, or no longer
    // owned it after UI ack. Neither path disposed of the known bound process.
    try expectConditionalCleanup(h.local);
    if (!acknowledge) {
        const updated = adapter.status(id).?.completed;
        try testing.expectEqual(.success, updated.execution);
        try testing.expect(updated.terminal_ref != null);
        try testing.expectEqual(.destination_lost, updated.placement);
    }
    adapter.advance(&h, {});
    try testing.expectEqual(@as(usize, 0), h.placements);
    try testing.expect(!h.local.bridge.outgoing.hasPending());
    try testing.expect(!h.remote.bridge.outgoing.hasPending());
}

test "localtool late same-source spawn after session change updates receipt and cleans" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    try lateSessionResultRegression(false);
}

test "localtool late same-source spawn after session change survives UI acknowledgement" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    try lateSessionResultRegression(true);
}

fn retainedOwners(adapter: *const launch.Adapter) usize {
    var count: usize = 0;
    for (adapter.pending) |slot| if (slot != null) {
        count += 1;
    };
    return count;
}

fn replaceSource(h: *Harness) !void {
    const previous = h.local.host;
    defer previous.destroy();
    h.local.host = try @TypeOf(previous.*).create(testing.allocator, h.local.bridge);
    h.local.host.setProviderId(h.local.providerId());
    try fixture.attachHostWith(h.local.host, "hello_conditional_kill.bin");
}

test "localtool cleanup exhaustion records known identity and retains ownership after ack" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    const id = try submit(&adapter, &h);
    try h.complete(&adapter, "spawn-bound.bin");
    const occupied = try fillOperationLedger(h.local);
    h.placement_refused = true;
    adapter.advance(&h, {});
    try testing.expectEqual(.pending, adapter.status(id).?.completed.cleanup);
    var service = adapter.service(&h, {}, "");
    try testing.expectEqual(.unknown, service.localToolStatus(id).phase);
    for (0..launch.cleanup_admission_attempts * 2) |_| adapter.advance(&h, {});
    const exhausted = adapter.status(id).?.completed;
    try testing.expectEqual(.success, exhausted.execution);
    try testing.expect(exhausted.terminal_ref != null);
    try testing.expectEqual(.exhausted, exhausted.cleanup);
    try testing.expectEqual(@as(u32, 0), exhausted.cleanup_request);
    try testing.expectEqual(launch.cleanup_admission_attempts, adapter.pending[0].?.cleanup_attempts);
    try testing.expect(std.mem.indexOf(u8, service.localToolStatus(id).message, "could not be queued") != null);
    try testing.expect(adapter.acknowledge(id));
    try testing.expect(adapter.status(id) == null);
    try testing.expectEqual(@as(usize, 1), retainedOwners(&adapter));
    try freeOperationSlot(h.local, occupied);
    adapter.advance(&h, {});
    try testing.expect(!h.local.bridge.outgoing.hasPending());
    try testing.expectEqual(@as(usize, 1), retainedOwners(&adapter));
    // Only source retirement ends this retained exhausted authority; no new
    // client is allowed to inherit or replay the conditional kill.
    try replaceSource(&h);
    adapter.advance(&h, {});
    try testing.expectEqual(@as(usize, 0), retainedOwners(&adapter));
    try testing.expect(!h.local.bridge.outgoing.hasPending());
}

test "localtool acknowledged unresolved requests backpressure before writes until source retirement" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    var ids: [16]u32 = undefined;
    // Each attached fixture already publishes terminals. Use two real clients
    // so adapter ownership capacity, rather than one host's terminal limit, binds.
    const first = h.local;
    for (ids[0..8]) |*id| id.* = try submit(&adapter, &h);
    first.bridge.outgoing.reset();
    first.host.attached_session_id = 2;
    try h.model.ensurePeerSlots(2);
    const second = try support.PhuxProvider.create(testing.allocator, testing.io, .{ .unix = "/fixture-second-local-never-dialed" }, null, "tool-test");
    h.model.peers.items[1].provider = second;
    h.local = second;
    try fixture.attachHostWith(second.host, "hello_conditional_kill.bin");
    for (ids[8..]) |*id| id.* = try submit(&adapter, &h);
    h.local.bridge.outgoing.reset();
    h.local.host.attached_session_id = 2;
    adapter.advance(&h, {});
    for (ids) |id| try testing.expect(adapter.acknowledge(id));
    try testing.expectEqual(ids.len, retainedOwners(&adapter));
    try testing.expectError(error.OperationCapacity, submit(&adapter, &h));
    try testing.expect(!h.local.bridge.outgoing.hasPending());
    try replaceSource(&h);
    h.local = first;
    try replaceSource(&h);
    adapter.advance(&h, {});
    try testing.expectEqual(@as(usize, 0), retainedOwners(&adapter));
    try testing.expect(!h.local.bridge.outgoing.hasPending());
    _ = try submit(&adapter, &h);
    try expectSpawn(h.local, &.{ "/fixture/editor", "--wait", "/local config with spaces" }, "/local cwd");
}

test "localtool late success after session selection returns cannot reacquire placement" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    const id = try submit(&adapter, &h);
    h.local.bridge.outgoing.reset();
    h.local.host.attached_session_id = 2;
    adapter.advance(&h, {});
    _ = adapter.takeOutcome();
    h.local.host.attached_session_id = 1;
    try h.complete(&adapter, "spawn-bound.bin");
    adapter.advance(&h, {});
    try expectConditionalCleanup(h.local);
    const updated = adapter.takeOutcome().?;
    try testing.expectEqual(.success, updated.execution);
    try testing.expectEqual(.destination_lost, updated.placement);
    try testing.expectEqual(.admitted, updated.cleanup);
    try testing.expect(updated.cleanup_request != 0);
    try testing.expectEqual(@as(usize, 0), h.placements);
    try testing.expect(adapter.acknowledge(id));
    try testing.expectEqual(@as(usize, 0), retainedOwners(&adapter));
}

test "localtool late refused or disconnect result retires acknowledged owner without cleanup" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    for ([_]bool{ false, true }) |disconnect| {
        var h = try Harness.init();
        defer h.deinit();
        var adapter: launch.Adapter = .{ .gpa = testing.allocator };
        defer adapter.deinit();
        const id = try submit(&adapter, &h);
        h.local.bridge.outgoing.reset();
        h.local.host.attached_session_id = 2;
        adapter.advance(&h, {});
        try testing.expect(adapter.acknowledge(id));
        if (disconnect) {
            h.local.host.disconnect();
            try testing.expect(adapter.completeFrom(h.local, h.local.takeOperationResult().?));
        } else try h.complete(&adapter, "spawn-refused.bin");
        adapter.advance(&h, {});
        try testing.expectEqual(@as(usize, 0), retainedOwners(&adapter));
        try testing.expect(!h.local.bridge.outgoing.hasPending());
        try testing.expectEqual(@as(usize, 0), h.placements);
    }
}

test "localtool handler acknowledgement retires receipt but preserves blocked cleanup owner" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    var h = try Harness.init();
    defer h.deinit();
    var adapter: launch.Adapter = .{ .gpa = testing.allocator };
    defer adapter.deinit();
    const id = try submit(&adapter, &h);
    try h.complete(&adapter, "spawn-bound.bin");
    const occupied = try fillOperationLedger(h.local);
    h.model.window_epochs[0] += 1;
    adapter.advance(&h, {});
    var state: tools.State = .{};
    defer state.deinit();
    // Install the receipt that the successful launch boundary retains, without
    // invoking an editor or creating a configuration file in this fixture.
    try state.receipts.put(std.heap.page_allocator, 1, .{ .operation_id = id, .target = try std.heap.page_allocator.dupe(u8, "/local config") });
    var service = adapter.service(&h, {}, "");
    var request = [_]u8{ 1, 4, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0 };
    var output: [tools.max_bytes]u8 = undefined;
    const status = try tools.handle(&state, &service, {}, false, &request, &output);
    try testing.expectEqual(@intFromEnum(tools.Phase.unknown), status[1]);
    try testing.expectEqual(id, std.mem.readInt(u32, status[2..6], .little));
    request[1] = 5;
    const ack = try tools.handle(&state, &service, {}, false, &request, &output);
    try testing.expectEqual(@intFromEnum(tools.Phase.unknown), ack[1]);
    try testing.expectEqual(@as(usize, 0), state.receipts.count());
    try testing.expectEqual(@as(usize, 1), retainedOwners(&adapter));
    const again = try tools.handle(&state, &service, {}, false, &request, &output);
    try testing.expectEqual(id, std.mem.readInt(u32, again[2..6], .little));
    try freeOperationSlot(h.local, occupied);
    adapter.advance(&h, {});
    try expectConditionalCleanup(h.local);
    try testing.expectEqual(@as(usize, 0), retainedOwners(&adapter));
}
