//! Shipping disconnect/paint recovery evidence; no desktop automation.
const std = @import("std");
const testing = std.testing;
const sdk = @import("native_sdk");
const engine_module = @import("native/ts_engine.zig");
const support = @import("phux_support.zig");
const projection = @import("native/workspace_projection.zig");
const fixture = if (support.phux_enabled) support.PhuxProvider.test_support else struct {};
const ChannelFx = struct {
    pub fn openChannel(_: *const @This(), _: anytype) sdk.ChannelHandle { return .{}; }
    pub fn closeChannel(_: *const @This(), _: u64) void {}
};

fn disconnect(engine: *engine_module.Engine) void {
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .closed }, null);
}

/// Paint the shipping display list and reconstruct the visible terminal text
/// from the row cell grids (the terminal paints a packed cell_grid per row,
/// never draw_text runs). This is the actual glass a user sees.
fn expectPaint(engine: *engine_module.Engine, present: []const u8, absent: []const u8) !void {
    const commands = try testing.allocator.alloc(sdk.canvas.CanvasCommand, projection.chrome_command_envelope);
    defer testing.allocator.free(commands);
    var builder = sdk.canvas.Builder.init(commands);
    try engine.paint(&builder, engine.model.wsConst().surface_size, projection.cockpitTokens(engine.model));
    var text: std.ArrayList(u8) = .empty;
    defer text.deinit(testing.allocator);
    for (builder.displayList().commands) |command| switch (command) {
        .cell_grid => |grid| {
            for (grid.cells) |cell| {
                if (cell.text_len == 0) continue;
                try text.appendSlice(testing.allocator, grid.text[cell.text_offset..][0..cell.text_len]);
            }
        },
        else => {},
    };
    try testing.expect(std.mem.indexOf(u8, text.items, present) != null);
    try testing.expect(std.mem.indexOf(u8, text.items, absent) == null);
}

fn stageReplaced(remote: *support.PhuxProvider, name: []const u8, old: []const u8, new: []const u8) !void {
    const bytes = try fixture.readFixture(name);
    defer testing.allocator.free(bytes);
    try testing.expectEqual(old.len, new.len);
    const index = std.mem.indexOf(u8, bytes, old) orelse return error.MissingMarker;
    @memcpy(bytes[index..][0..new.len], new);
    var offset: usize = 0;
    while (offset < bytes.len) try fixture.stageFrames(remote.bridge, bytes, &offset, 1);
}

fn reconnect(engine: *engine_module.Engine, wrong: bool) !void {
    const remote = engine.model.phux().?;
    try remote.host.reconnect("frozen-test");
    remote.attach_queued = true;
    if (wrong) {
        try stageReplaced(remote, "hello.bin", "cockpit-fixture", "foreign-server!");
    } else try fixture.stageFixture(remote.bridge, "hello.bin");
    _ = try remote.host.drainReadiness();
    try remote.host.attachSessionId(1, .{ .cols = 80, .rows = 24 });
    try stageReplaced(remote, "attached.bin", "COCKPIT FIXTURE", if (wrong) "UNPROVEN GRID!!" else "REPLACED GRID!!");
    _ = engine.onPhuxChannel(&ChannelFx{}, .{ .key = support.phux_channel_key, .kind = .data }, null);
    remote.bridge.outgoing.reset();
}

fn expectFenced(engine: *engine_module.Engine, ref: support.TerminalRef) !void {
    const model = engine.model;
    try testing.expect(model.attachmentPending(ref));
    try testing.expect(model.remotePresentation(ref) == null);
    try testing.expect(model.terminalOwner(ref) == null);
    const remote = model.phux().?;
    remote.bridge.outgoing.reset();
    engine.onText(&engine_module.NoShells{}, .{ .phase = .text_input, .key = "x", .text = "x" });
    const proposals = projection.proposedViewportsIn(model, model.wsConst(), .{ .width = 1400, .height = 900 });
    for (proposals.slice()) |proposal| try testing.expect(!proposal.terminal.eql(ref));
    try testing.expect(!remote.bridge.outgoing.hasPending());
}

pub fn recovery() !void {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const engine = try @import("durable_creation_tests.zig").start();
    defer engine.destroy();
    const model = engine.model;
    model.ws().surface_size = .{ .width = 1100, .height = 640 };
    const ref = model.focusedTerminalRef().?;
    const owner = model.remotePresentation(ref).?.owner;
    try expectPaint(engine, "COCKPIT FIXTURE", "UNPROVEN GRID!");
    disconnect(engine);
    try expectFenced(engine, ref);
    // This is the regression: paint the shipping display list, not merely a
    // provider canvas that the shipping pending guard makes unreachable.
    try expectPaint(engine, "COCKPIT FIXTURE", "UNPROVEN GRID!");
    const frozen = model.remotePaintPresentation(ref).?;
    try testing.expect(owner.eql(frozen.owner));
    try testing.expectEqual(.frozen, frozen.phase);
    try testing.expect(!frozen.grid.running);

    try reconnect(engine, true);
    try testing.expect(std.mem.indexOf(u8, model.phux().?.presentation(ref).?.grid.screen_text, "UNPROVEN GRID!") != null);
    try expectFenced(engine, ref);
    try expectPaint(engine, "COCKPIT FIXTURE", "UNPROVEN GRID!");
    try testing.expectEqual(frozen.grid.rows.ptr, model.remotePaintPresentation(ref).?.grid.rows.ptr);
    disconnect(engine);
    try reconnect(engine, false);
    try testing.expect(!model.attachmentPending(ref));
    try expectPaint(engine, "REPLACED GRID!", "COCKPIT FIXTURE");
    for (model.frozen_paint) |entry| try testing.expect(entry == null);

    disconnect(engine);
    try testing.expect(model.remotePaintPresentation(ref) != null);
    model.dropTab(model.ws().selected_tab);
    try testing.expect(model.remotePaintPresentation(ref) == null);
    for (model.frozen_paint) |entry| try testing.expect(entry == null);
}
