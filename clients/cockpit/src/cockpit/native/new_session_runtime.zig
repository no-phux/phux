//! Owning-Client adapter for New Session. The composition root retains the
//! Controller; Engine supplies the exact window provider and selection clock.
const std = @import("std");
const sessions = @import("new_session.zig");
const windows = @import("ts_window_navigation.zig");
const support = @import("../phux_support.zig");
const PhuxProvider = support.PhuxProvider;

/// `remote` must come from the invoking window's exact attachment (including
/// empty-session ownership). Null means unavailable, never an ambient fallback.
pub fn capture(engine: anytype, remote: ?*PhuxProvider, selection_epoch: u64) ?sessions.Destination {
    if (comptime !support.phux_enabled) return null;
    const selected = remote orelse return null;
    const window = engine.model.active_window;
    if (!engine.model.windowOpen(window)) return null;
    if (selection_epoch == std.math.maxInt(u64)) return null;
    const destination: sessions.Destination = .{
        .window = @intCast(window),
        .window_epoch = engine.model.window_epochs[window],
        .provider = @intFromEnum(selected.providerId()),
        .provider_context = selected.context_id,
        .host_context = selected.host.context_id,
        .connection_epoch = selected.connectionEpoch(),
        .selection_epoch = selection_epoch,
        .host = selected.remoteLabel() orelse "This Mac",
    };
    return if (current(engine, destination)) destination else null;
}

/// Navigation since capture withdraws focus, not permission to finish the
/// captured write. Selection epochs are therefore checked only by didCreate.
pub fn current(engine: anytype, destination: sessions.Destination) bool {
    if (comptime !support.phux_enabled) return false;
    if (!windowCurrent(engine.model, destination)) return false;
    const remote = resolve(engine.model, destination) orelse return false;
    if (@intFromEnum(remote.providerId()) != destination.provider) return false;
    if (remote.pending_retarget != null) return false;
    return switch (remote.state()) {
        .negotiated, .attached => true,
        else => false,
    };
}

pub fn send(engine: anytype, destination: sessions.Destination, name: []const u8, keep_empty: bool) !u32 {
    if (comptime !support.phux_enabled) return error.InvalidState;
    if (!keep_empty or !current(engine, destination)) return error.InvalidState;
    const remote = resolve(engine.model, destination) orelse return error.InvalidState;
    return remote.requestCreateSession(name, true);
}

/// Returned text borrows the owning Client; Controller copies it before release.
pub fn poll(engine: anytype, destination: sessions.Destination, request_id: u32) sessions.Outcome {
    if (comptime !support.phux_enabled) return .{ .unknown = unavailable };
    const remote = resolve(engine.model, destination) orelse return .{ .unknown = unavailable };
    const result = remote.sessionCreateInfo(request_id);
    if (result.request_id != request_id) return .{ .unknown = unavailable };
    return switch (result.status) {
        .pending => .pending,
        .created => .{ .created = result.session_id },
        .refused => .{ .refused = result.message },
        .unknown_outcome => .{ .unknown = result.message },
        else => .{ .unknown = unavailable },
    };
}

/// Window closure, disconnect, or a pending retarget must not prevent retiring
/// the original Client's receipt. A replaced Client is already retired: never
/// forward the old request ID into its replacement's reset ID namespace.
pub fn release(engine: anytype, destination: sessions.Destination, request_id: u32) void {
    if (comptime !support.phux_enabled) return;
    const remote = resolve(engine.model, destination) orelse return;
    remote.releaseSessionCreate(request_id);
}

/// selectExact(engine, remote, session, window, window_epoch, fx)!void is the
/// Engine's exact-session navigation path. It must preserve other windows and
/// represent keep-empty sessions without inventing a terminal or coordinator.
pub fn didCreate(engine: anytype, destination: sessions.Destination, session: u32, selection_epoch: u64, fx: anytype, comptime selectExact: anytype) void {
    if (comptime !support.phux_enabled) return;
    if (session == 0 or selection_epoch == std.math.maxInt(u64)) return;
    if (selection_epoch != destination.selection_epoch) return;
    if (!current(engine, destination)) return;
    const remote = resolve(engine.model, destination) orelse return;
    selectExact(engine, remote, session, destination.window, destination.window_epoch, fx) catch {
        engine.intent_refused = true;
    };
}

const unavailable = "The original session creation request is unavailable. Refresh Sessions to check the outcome.";

fn windowCurrent(model: anytype, destination: sessions.Destination) bool {
    const target: windows.Target = .{ .window = destination.window, .epoch = destination.window_epoch };
    return target.validWindow(model);
}

fn resolve(model: anytype, destination: sessions.Destination) ?*PhuxProvider {
    const remote = attachment(model, destination.provider_context) orelse return null;
    if (remote.host.context_id != destination.host_context) return null;
    if (remote.connectionEpoch() != destination.connection_epoch) return null;
    return remote;
}

fn attachment(model: anytype, context: u64) ?*PhuxProvider {
    const Model = @TypeOf(model.*);
    if (comptime @hasDecl(Model, "phuxForAttachment")) return model.phuxForAttachment(context);
    // Base models predate the public attachment resolver. This compatibility
    // walk still requires an exact process-local context, never a machine ID.
    if (model.phux()) |remote| if (remote.context_id == context) return remote;
    for (model.peers.items) |entry| {
        const remote = entry.provider orelse continue;
        if (remote.context_id == context) return remote;
    }
    return null;
}
