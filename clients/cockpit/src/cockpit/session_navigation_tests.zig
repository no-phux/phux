const std = @import("std");
const testing = std.testing;
const Navigation = @import("session_navigation.zig").Navigation;
const support = @import("phux_support.zig");
const engine_module = @import("native/ts_engine.zig");
const contract = @import("provider_contract");
const fixture = if (support.phux_enabled) support.PhuxProvider.test_support else struct {};

const Remote = struct {
    context_id: u64 = 3,
    host: struct { context_id: u64 = 4 } = .{},
    epoch: u64 = 8,
    session_id: ?u32 = 2,
    selected: ?u32 = 1,
    attached: bool = false,
    server: ?[]const u8 = "server-incarnation",

    pub fn connectionEpoch(self: *const @This()) u64 {
        return self.epoch;
    }
    pub fn serverId(self: *const @This()) ?[]const u8 {
        return self.server;
    }
    pub fn state(self: *const @This()) enum { attached, negotiated } {
        return if (self.attached) .attached else .negotiated;
    }
    pub fn selectedSessionId(self: *const @This()) ?u32 {
        return self.selected;
    }
};

test "session identity copies exact borrowed bytes and refuses oversize before handoff" {
    var bytes = "server-incarnation".*;
    var remote: Remote = .{ .server = &bytes };
    var navigation = try Navigation.capture(&remote, 2);
    bytes[0] = 'X';
    remote.epoch += 1;
    try navigation.bind(&remote);
    try testing.expectError(error.StaleContext, navigation.observeAttachment(&remote));
    const oversized = [_]u8{'s'} ** (Navigation.max_server_bytes + 1);
    remote.server = &oversized;
    try testing.expectError(error.ServerIdentityCapacity, Navigation.capture(&remote, 2));
    try testing.expectEqual(@as(u64, 9), remote.epoch);
}

test "only original unbound lifetime survives intentional close and binding is exact once" {
    var remote: Remote = .{};
    var navigation = try Navigation.capture(&remote, 2);
    try testing.expect(navigation.preserveDisconnect(&remote));
    try testing.expect(navigation.preserveDisconnect(&remote));
    remote.host.context_id += 1;
    try testing.expect(!navigation.preserveDisconnect(&remote));
    try testing.expectError(error.StaleContext, navigation.bind(&remote));
    remote.host.context_id -= 1;
    try testing.expectError(error.StaleContext, navigation.bind(&remote));
    remote.epoch += 1;
    try navigation.bind(&remote);
    try testing.expect(!navigation.preserveDisconnect(&remote));
    try testing.expectError(error.SessionAlreadyBound, navigation.bind(&remote));
    try testing.expectEqual(@as(?u64, 9), navigation.replacement_epoch);
}

test "requested session alone is not attached evidence and wrong actual session is refused" {
    var remote: Remote = .{};
    var navigation = try Navigation.capture(&remote, 2);
    remote.epoch += 1;
    try navigation.bind(&remote);
    try testing.expect(!try navigation.observeAttachment(&remote));
    remote.attached = true;
    try testing.expectError(error.SessionMismatch, navigation.observeAttachment(&remote));
    remote.selected = 2;
    try testing.expect(try navigation.observeAttachment(&remote));
    try testing.expect(!try navigation.observeAttachment(&remote));
    remote.context_id += 1;
    try testing.expectError(error.StaleContext, navigation.observeAttachment(&remote));
}

fn start() !*engine_module.Engine {
    return @import("durable_creation_tests.zig").start();
}

fn refFor(id: u32) contract.TerminalRef {
    return .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = id } } };
}

fn begin(engine: *engine_module.Engine, terminal: ?contract.TerminalRef) !void {
    const model = engine.model;
    const remote = model.phux().?;
    try engine.creation.reserveSessionCorrelated(model, 1, terminal, 91);
    try model.shared_workspace.leaveSession(model);
    engine.creation.disconnectExcept(model, 91);
    remote.stop();
    engine.creation.observeFocus(model);
    engine.creation.disconnectExcept(model, 91);
    remote.stop();
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try remote.host.reconnect("session-command-test");
    remote.attach_queued = false;
    try testing.expect(engine.creation.bindSessionConnection(model, 91));
    try testing.expect(!engine.creation.bindSessionConnection(model, 91));
    try fixture.stageFixture(remote.bridge, "hello.bin");
    _ = try remote.drainReadiness();
    try fixture.stageFixture(remote.bridge, "attached.bin");
    _ = try remote.drainReadiness();
    try testing.expect(engine.creation.observeSessionAttachment(model));
    try testing.expectEqual(.unavailable, remote.workspaceSnapshot().state);
    try testing.expect(engine.creation.peekCompletion() == null);
}

fn workspace(engine: *engine_module.Engine) !void {
    const remote = engine.model.phux().?;
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_metadata.bin");
    try fixture.stageWorkspaceFixture(remote.bridge, "workspace_initial_state.bin");
    _ = try remote.drainReadiness();
}

fn project(engine: *engine_module.Engine) !void {
    const model = engine.model;
    const remote = model.phux().?;
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    _ = model.shared_workspace.selectDesired(model);
    _ = engine.creation.observeProjection(model);
}

fn catalogTarget(engine: *engine_module.Engine, target: contract.TerminalRef) !void {
    const remote = engine.model.phux().?;
    const store = &remote.host.workspace_store;
    const catalog = try remote.gpa.dupe(contract.workspace.CatalogTerminal, &.{.{ .terminal_ref = target, .session_id = 1 }});
    remote.gpa.free(store.catalog);
    store.catalog = catalog;
}

test "session reservation cancellation and result capacity precede all effects" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const remote = engine.model.phux().?;
    const epoch = remote.connectionEpoch();
    try engine.creation.reserveSessionCorrelated(engine.model, 2, null, 10);
    try testing.expectError(error.OperationBusy, engine.creation.reserveSessionCorrelated(engine.model, 2, null, 11));
    try testing.expect(engine.creation.cancelSessionReservation(10));
    try testing.expectEqual(@as(usize, 0), engine.creation.count());
    for (0..16) |index| {
        const id: u64 = @intCast(index + 20);
        try engine.creation.reserveSessionCorrelated(engine.model, 2, null, id);
        engine.creation.failSessionAttempt(engine.model, id);
    }
    try testing.expectError(error.OperationCapacity, engine.creation.reserveSessionCorrelated(engine.model, 2, null, 90));
    try testing.expectEqual(epoch, remote.connectionEpoch());
    try testing.expect(!remote.bridge.outgoing.hasPending());
    try testing.expectEqual(@as(?u32, 1), remote.session_id);
    try testing.expect(engine.creation.ackCompletion(20));
    try engine.creation.reserveSessionCorrelated(engine.model, 2, null, 90);
    try testing.expect(engine.creation.cancelSessionReservation(90));
}

test "new session reservation does not interrupt prior command until committed disconnect" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try engine.creation.reserveSessionCorrelated(engine.model, 2, null, 10);
    try engine.creation.reserveSessionCorrelated(engine.model, 3, null, 11);
    try testing.expectEqual(@as(usize, 2), engine.creation.count());
    try testing.expect(engine.creation.cancelSessionReservation(11));
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try engine.creation.reserveSessionCorrelated(engine.model, 3, null, 12);
    engine.creation.disconnectExcept(engine.model, 12);
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try testing.expectEqual(@as(u64, 10), engine.creation.peekCompletion().?.command_id);
    try testing.expectEqual(.unknown, engine.creation.peekCompletion().?.operation);
}

test "ordinary disconnect retires reserved handoff and immediate attempt failure retains a result" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try engine.creation.reserveSessionCorrelated(engine.model, 2, null, 10);
    engine.creation.disconnect(engine.model);
    try testing.expectEqual(.unknown, engine.creation.peekCompletion().?.operation);
    try testing.expect(engine.creation.ackCompletion(10));
    try engine.creation.reserveSessionCorrelated(engine.model, 2, null, 11);
    engine.creation.failSessionAttempt(engine.model, 11);
    try testing.expectEqual(@as(u64, 11), engine.creation.peekCompletion().?.command_id);
    try testing.expectEqual(@as(u32, 2), engine.creation.peekCompletion().?.target_session_id);
    try testing.expectEqual(@as(u32, 0), engine.creation.peekCompletion().?.request_id);
    try testing.expect(engine.creation.ackCompletion(11));
    try engine.creation.reserveSessionCorrelated(engine.model, 2, null, 12);
    const remote = engine.model.phux_provider;
    engine.model.phux_provider = null;
    defer engine.model.phux_provider = remote;
    try testing.expect(engine.creation.observeSessionAttachment(engine.model));
    try testing.expectEqual(.unknown, engine.creation.peekCompletion().?.operation);
    try testing.expectEqual(.context_changed, engine.creation.peekCompletion().?.reason);
}

test "authoritative empty session succeeds only after applied projection without resource request IDs" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try begin(engine, null);
    const remote = engine.model.phux().?;
    remote.host.workspace_store.deinit(remote.gpa);
    remote.host.workspace_store.info = .{ .session_id = 1, .revision = 9, .state = .authoritative };
    try testing.expect(!engine.creation.observeProjection(engine.model));
    try testing.expect(engine.creation.peekCompletion() == null);
    try project(engine);
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.not_requested, result.placement);
    try testing.expectEqual(@as(u32, 1), result.target_session_id);
    try testing.expectEqual(@as(u32, 0), result.request_id);
    try testing.expectEqual(@as(u64, 0), result.connection_epoch);
    try testing.expectEqual(@as(u32, 0), result.attach_request_id);
    try testing.expectEqual(@as(u64, 0), result.attach_connection_epoch);
    try testing.expectEqual(@as(u32, 0), remote.host.operation_ledger.last_id);
    try testing.expectEqual(@as(usize, 0), engine.model.ws().tab_count);
}

test "session success survives replacement disconnect while projection remains unavailable" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try begin(engine, null);
    engine.creation.disconnectExcept(engine.model, 91);
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.unknown, result.placement);
    try testing.expectEqual(.disconnected, result.reason);
}

test "real shared apply failure retires session placement while preserving successful attachment" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try begin(engine, null);
    try workspace(engine);
    const remote = engine.model.phux().?;
    remote.host.workspace_store.windows[0].root = std.math.maxInt(u32);
    try testing.expectError(error.InvalidNode, engine.model.shared_workspace.apply(engine.model, remote.workspaceSnapshot(), remote.connectionEpoch()));
    try testing.expect(engine.creation.projectionFailed(engine.model));
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.refused, result.placement);
    try testing.expectEqual(.projection_refused, result.reason);
    try testing.expectEqual(@as(u32, 0), result.request_id);
    try testing.expect(!engine.creation.projectionFailed(engine.model));
}

test "same session entry retains command and successful session outcome through exact attach refusal" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    const target = refFor(8);
    try begin(engine, target);
    try workspace(engine);
    const remote = engine.model.phux().?;
    try catalogTarget(engine, target);
    try project(engine);
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    const entry = engine.creation.pending[0].?;
    try testing.expectEqual(@as(?u64, 91), entry.command_id);
    try testing.expectEqual(@as(u32, 0), entry.operation_request);
    try testing.expect(entry.attach_request != 0);
    try testing.expectEqual(entry.attach_request, entry.request);
    try testing.expect(engine.model.shared_workspace.desired_terminal == null);
    const refused: support.OperationResult = .{
        .request_id = entry.attach_request,
        .connection_epoch = remote.connectionEpoch(),
        .kind = .attach,
        .status = .refused,
        .terminal_ref = target,
    };
    var stale = refused;
    stale.connection_epoch -= 1;
    try testing.expect(!engine.creation.complete(engine.model, stale));
    try testing.expect(engine.creation.complete(engine.model, refused));
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.refused, result.placement);
    try testing.expectEqual(.attach_refused, result.reason);
    try testing.expectEqual(entry.request, result.attach_request_id);
    try testing.expectEqual(entry.epoch, result.attach_connection_epoch);
    try testing.expectEqual(@as(u32, 0), result.request_id);
}

test "session terminal preserves intentional focus but explicit supersession stays revoked" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try begin(engine, refFor(7));
    try testing.expect(engine.creation.pending[0].?.may_focus);
    engine.creation.supersedeFocus();
    try workspace(engine);
    try project(engine);
    try project(engine);
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.placed, result.placement);
    try testing.expectEqual(.superseded, result.focus);
    try testing.expectEqual(@as(u64, 91), result.command_id);
    try testing.expectEqual(@as(u32, 0), result.attach_request_id);
}

test "session admission uses last native tab capacity without counting its own reservation twice" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try begin(engine, refFor(8));
    try workspace(engine);
    try catalogTarget(engine, refFor(8));
    const model = engine.model;
    const remote = model.phux().?;
    _ = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
    const max_tabs = @import("model.zig").max_tabs;
    // Native placement occupancy is local to the model, separate from the
    // provider's catalog. Fill all but the last tab before the continuation.
    for (1..max_tabs - 1) |index| model.primary.tabs[index] = model.primary.tabs[0];
    model.primary.tab_count = max_tabs - 1;
    try testing.expect(engine.creation.observeProjection(model));
    try testing.expect(engine.creation.peekCompletion() == null);
    try testing.expectEqual(@as(usize, 1), engine.creation.count());
    try testing.expect(engine.creation.pending[0].?.attach_request != 0);
    engine.creation.disconnect(model);
}

test "new session admission retains native destination epoch and reports reuse after session success" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try begin(engine, refFor(8));
    try workspace(engine);
    try catalogTarget(engine, refFor(8));
    engine.model.window_epochs[0] += 1;
    try project(engine);
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.destination_lost, result.placement);
    try testing.expectEqual(@as(u32, 0), result.attach_request_id);
}

test "existing session member follows current native placement after the captured destination was reused" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try start();
    defer engine.destroy();
    try begin(engine, refFor(7));
    try workspace(engine);
    engine.model.window_epochs[0] += 1;
    try project(engine);
    try project(engine);
    const result = engine.creation.peekCompletion().?;
    try testing.expectEqual(.success, result.operation);
    try testing.expectEqual(.placed, result.placement);
    try testing.expectEqual(engine.model.window_epochs[0], result.destination_window_epoch);
    try testing.expectEqual(@as(u32, 0), result.attach_request_id);
}
