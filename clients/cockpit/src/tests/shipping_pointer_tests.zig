const std = @import("std");
const sdk = @import("native_sdk");
const cockpit = @import("cockpit_engine");
const Engine = cockpit.engine.Engine;
const NoShells = cockpit.engine.NoShells;

const Fixture = struct {
    engine: *Engine,
    ref: cockpit.TerminalRef,

    fn start() !Fixture {
        if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
        const engine = try Engine.create(std.testing.allocator, std.testing.io);
        errdefer engine.destroy();
        engine.model.ws().surface_size = .{ .width = 1100, .height = 640 };
        var config = cockpit.startup.resolvePhuxConfig(.{}, .{ .socket = "/unused-pointer-fixture.sock", .session = "fixture" });
        const remote = (try cockpit.startup.createPhuxProviderFromConfig(std.testing.allocator, std.testing.io, &config)).?;
        cockpit.attachPhuxProvider(engine.model, remote);
        try remote.host.start("shipping-pointer-fixture");
        try std.testing.expect(remote.bridge.incoming.stage(@embedFile("fixtures/hello.bin")));
        _ = try remote.drainReadiness();
        remote.bridge.outgoing.reset();
        const attached = @embedFile("fixtures/attached.bin");
        var offset: usize = 0;
        while (offset < attached.len) {
            const size = 4 + std.mem.readInt(u32, attached[offset..][0..4], .big);
            try std.testing.expect(remote.bridge.incoming.stage(attached[offset..][0..size]));
            offset += size;
        }
        _ = try remote.drainReadiness();
        engine.model.reconcileRemoteTerminals();
        try std.testing.expect(engine.model.admitAndSelectCurrentRemoteTerminal());
        const ref = engine.model.focusedTerminalRef().?;
        try std.testing.expectEqual(.live, engine.model.remotePresentation(ref).?.phase);
        remote.bridge.outgoing.reset();
        return .{ .engine = engine, .ref = ref };
    }

    fn frame(self: Fixture) sdk.geometry.RectF {
        return cockpit.projection.paneFrameFor(self.engine.model, self.engine.model.wsConst().surface_size, self.ref).?;
    }

    fn event(self: Fixture, kind: sdk.platform.GpuSurfaceInputKind, col: f32, row: f32) sdk.platform.GpuSurfaceInputEvent {
        const rect = self.frame();
        return .{
            .window_id = 1,
            .label = cockpit.scene.canvas_label,
            .kind = kind,
            .pointer_id = 7,
            .x = rect.x + (col + 0.25) * rect.width / 80,
            .y = rect.y + (row + 0.25) * rect.height / 24,
            .timestamp_ns = 1,
        };
    }

    fn selectionText(self: Fixture) ![]u8 {
        const remote = self.engine.model.phux().?;
        return remote.selectionText(self.engine.model.terminalOwner(self.ref).?, std.testing.allocator);
    }

    /// TERMINAL_OUTPUT TLVs follow appendix-encoding and wire/field.rs. The
    /// production FFI decoder validates these before any gesture is exercised.
    fn output(self: Fixture, text: []const u8, seq: u64) !void {
        var storage: [4096]u8 = undefined;
        var writer: std.Io.Writer = .fixed(&storage);
        try writer.writeAll(&.{ 0, 0, 0, 0, 0x90, 1, 4, 5, 0, 0, 0, 0, 7, 2, 4, 8 });
        try writer.writeInt(u64, seq, .big);
        try writer.writeAll(&.{ 3, 4 });
        var remaining = text.len;
        while (remaining >= 128) : (remaining >>= 7) try writer.writeByte(@as(u8, @intCast(remaining & 0x7f)) | 0x80);
        try writer.writeByte(@intCast(remaining));
        try writer.writeAll(text);
        try writer.writeAll(&.{ 4, 4, 8 });
        try writer.writeInt(u64, 7, .big);
        try writer.writeAll(&.{ 5, 4, 8 });
        try writer.writeInt(u64, 1, .big);
        const bytes = writer.buffered();
        std.mem.writeInt(u32, bytes[0..4], @intCast(bytes.len - 4), .big);
        const remote = self.engine.model.phux().?;
        try std.testing.expect(remote.bridge.incoming.stage(bytes));
        _ = try remote.drainReadiness();
    }
};

// GUARD: ts-remote-pointer
test "shipping raw remote drag uses provider selection and fences a frozen capture" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    const local_ref = engine.model.tabTerminal(0).?;
    engine.model.dropTab(0);
    const tree = engine.model.selectedTree().?;
    _ = try tree.split(tree.focus, .horizontal, local_ref);
    try std.testing.expect(engine.model.focusedTerminalRef().?.eql(local_ref));
    try std.testing.expect(!engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0)));
    try std.testing.expect(engine.model.focusedTerminalRef().?.eql(fixture.ref));
    try std.testing.expect(!engine.model.provider.terminalConst(local_ref).?.session.selectionActive());
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, fixture.event(.pointer_drag, 6, 0)));
    const text = try fixture.selectionText();
    defer std.testing.allocator.free(text);
    try std.testing.expectEqualStrings("COCKPIT", text);
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, fixture.event(.pointer_drag, 6, -1)));
    try std.testing.expect(engine.selectionAutoscrollActive());
    engine.model.phux().?.host.freezePublished();
    try std.testing.expect(!engine.selectionAutoscrollActive());
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, fixture.event(.pointer_up, 6, 0)));
    try std.testing.expectEqual(.ignored, engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0)));
}

// GUARD: ts-remote-drop
test "shipping remote file drop quotes paths and refuses unavailable owners" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    const remote = engine.model.phux().?;
    const drop: sdk.platform.FileDropEvent = .{
        .window_id = 1,
        .view_label = cockpit.scene.canvas_label,
        .paths = &.{ "/a b", "/it's" },
        .point = .{ .x = fixture.frame().x + 1, .y = fixture.frame().y + 1 },
    };
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    try std.testing.expect(engine.onDrop(&fx, drop));
    const bytes = remote.bridge.outgoing.take() orelse return error.TestExpectedPaste;
    defer remote.bridge.outgoing.release(bytes);
    try std.testing.expectEqual(@as(u8, 0x11), bytes[4]);
    try std.testing.expect(std.mem.indexOf(u8, bytes, "'/a b' '/it'\\''s'") != null);
    remote.bridge.outgoing.reset();
    remote.host.freezePublished();
    try std.testing.expect(!engine.onDrop(&fx, drop));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

// GUARD: ts-remote-wheel
test "shipping remote wheel scrolls history and emits bounded mouse reports" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    const remote = engine.model.phux().?;
    try fixture.output("\r\nrow" ** 40, 1);
    try std.testing.expectEqual(@as(u21, 'r'), engine.model.remotePresentation(fixture.ref).?.grid.rows[0].cells[0].cp);
    var wheel = fixture.event(.scroll, 0, 0);
    wheel.delta_y = fixture.frame().height / 24 * 64;
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, wheel));
    try std.testing.expectEqual(@as(u21, 'C'), engine.model.remotePresentation(fixture.ref).?.grid.rows[0].cells[0].cp);
    try fixture.output("\x1b[?1003h\x1b[?1006h", 2);
    try std.testing.expect(try remote.mouseTracking(engine.model.terminalOwner(fixture.ref).?));
    remote.bridge.outgoing.reset();
    wheel.delta_y = fixture.frame().height / 24 * 3;
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, wheel));
    var count: usize = 0;
    while (remote.bridge.outgoing.take()) |bytes| {
        defer remote.bridge.outgoing.release(bytes);
        try std.testing.expectEqual(@as(u8, 0x12), bytes[4]);
        count += 1;
    }
    try std.testing.expectEqual(@as(usize, 3), count);
    wheel.delta_y = std.math.nan(f32);
    try std.testing.expectEqual(.ignored, engine.onPointer(&fx, wheel));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

fn expectMouse(fixture: Fixture, action: u32) !void {
    const remote = fixture.engine.model.phux().?;
    const bytes = remote.bridge.outgoing.take() orelse return error.TestExpectedMouse;
    defer remote.bridge.outgoing.release(bytes);
    try std.testing.expectEqual(@as(u8, 0x12), bytes[4]);
    // terminal_id field: local wire terminal 7; mouse event field starts at 16.
    try std.testing.expectEqualSlices(u8, &.{ 1, 4, 5, 0, 0, 0, 0, 7 }, bytes[5..13]);
    try std.testing.expectEqual(action, std.mem.readInt(u32, bytes[16..20], .big));
}

// GUARD: ts-remote-capture-release
test "shipping remote captures balance reports and Shift retains native selection" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    const remote = engine.model.phux().?;
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0)));
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, fixture.event(.pointer_up, 0, 0)));
    try std.testing.expect(!engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
    try fixture.output("\x1b[?1003h\x1b[?1006h", 1);
    remote.bridge.outgoing.reset();
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0));
    try expectMouse(fixture, 0);
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 6, 0));
    try expectMouse(fixture, 2);
    var release = fixture.event(.pointer_up, 6, 0);
    release.x = std.math.nan(f32);
    _ = engine.onPointer(&fx, release);
    try expectMouse(fixture, 1);
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0));
    try expectMouse(fixture, 0);
    engine.setFocused(&fx, false);
    try expectMouse(fixture, 1);
    remote.bridge.outgoing.reset();
    engine.setFocused(&fx, true);
    remote.bridge.outgoing.reset();
    var press = fixture.event(.pointer_down, 0, 0);
    press.modifiers.shift = true;
    _ = engine.onPointer(&fx, press);
    var drag = fixture.event(.pointer_drag, 6, 0);
    drag.modifiers.shift = true;
    _ = engine.onPointer(&fx, drag);
    _ = engine.onPointer(&fx, fixture.event(.pointer_cancel, 6, 0));
    const text = try fixture.selectionText();
    defer std.testing.allocator.free(text);
    try std.testing.expectEqualStrings("COCKPIT", text);
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0));
    try expectMouse(fixture, 0);
    try std.testing.expect(!engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
    _ = engine.onPointer(&fx, fixture.event(.pointer_cancel, 0, 0));
    try expectMouse(fixture, 1);
}
