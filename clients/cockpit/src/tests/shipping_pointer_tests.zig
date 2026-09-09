const std = @import("std");
const sdk = @import("native_sdk");
const cockpit = @import("cockpit_engine");
const Engine = cockpit.engine.Engine;
const NoShells = cockpit.engine.NoShells;

const Fixture = struct {
    engine: *Engine,
    ref: cockpit.TerminalRef,
    local_ref: cockpit.TerminalRef,

    fn start() !Fixture {
        if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
        const engine = try Engine.create(std.testing.allocator, std.testing.io);
        errdefer engine.destroy();
        const local_ref = engine.model.tabTerminal(0).?;
        engine.model.ws().surface_size = .{ .width = 1100, .height = 640 };
        var config = cockpit.startup.resolvePhuxConfig(.{}, .{ .socket = "/unused-pointer-fixture.sock", .session = "fixture" });
        const remote = (try cockpit.startup.createPhuxProviderFromConfig(std.testing.allocator, std.testing.io, &config)).?;
        cockpit.attachPhuxProvider(engine.model, remote);
        try @TypeOf(remote.*).test_support.attachHost(remote.host);
        engine.model.reconcileRemoteTerminals();
        try std.testing.expect(try engine.model.shared_workspace.apply(engine.model, remote.workspaceSnapshot(), remote.connectionEpoch()));
        const ref = engine.model.focusedTerminalRef().?;
        try std.testing.expectEqual(.live, engine.model.remotePresentation(ref).?.phase);
        remote.bridge.outgoing.reset();
        const fixture = Fixture{ .engine = engine, .ref = ref, .local_ref = local_ref };
        try fixture.paint();
        const fx = NoShells{};
        engine.setFocused(&fx, false);
        engine.setFocused(&fx, true);
        remote.bridge.outgoing.reset();
        return fixture;
    }

    fn frame(self: Fixture) sdk.geometry.RectF {
        return cockpit.projection.paneFrameFor(self.engine.model, self.engine.model.wsConst().surface_size, self.ref).?;
    }

    fn measure(_: ?*anyopaque, _: sdk.canvas.FontId, _: f32, text: []const u8) f32 {
        return @as(f32, @floatFromInt(text.len)) * 10.25;
    }

    fn paint(self: Fixture) !void {
        const provider = sdk.canvas.TextMeasureProvider{ .measure_fn = measure };
        var tokens = cockpit.projection.cockpitTokens(self.engine.model);
        tokens.text_measure = &provider;
        const commands = try std.testing.allocator.alloc(sdk.canvas.CanvasCommand, cockpit.projection.chrome_command_envelope);
        defer std.testing.allocator.free(commands);
        var builder = sdk.canvas.Builder.init(commands);
        try self.engine.paint(&builder, self.engine.model.wsConst().surface_size, tokens);
    }

    fn metrics(self: Fixture) struct { width: f32, height: f32 } {
        const cell = self.engine.model.remotePresentation(self.ref).?.measured_cell.?;
        return .{ .width = cell.width, .height = cell.height };
    }

    fn event(self: Fixture, kind: sdk.platform.GpuSurfaceInputKind, col: f32, row: f32) sdk.platform.GpuSurfaceInputEvent {
        const rect = self.frame();
        return .{
            .window_id = 1,
            .label = cockpit.scene.canvas_label,
            .kind = kind,
            .pointer_id = 7,
            .x = rect.x + (col + 0.25) * self.metrics().width,
            .y = rect.y + (row + 0.25) * self.metrics().height,
            .timestamp_ns = 1,
        };
    }

    fn selectionText(self: Fixture) ![]u8 {
        const remote = self.engine.model.phux().?;
        return remote.selectionText(self.engine.model.terminalOwner(self.ref).?, std.testing.allocator);
    }

    fn click(self: Fixture, col: f32, row: f32, time: u64) void {
        const fx = NoShells{};
        var raw = self.event(.pointer_down, col, row);
        raw.timestamp_ns = time;
        _ = self.engine.onPointer(&fx, raw);
        raw.kind = .pointer_up;
        _ = self.engine.onPointer(&fx, raw);
    }

    fn expectSelection(self: Fixture, expected: []const u8) !void {
        const text = try self.selectionText();
        defer std.testing.allocator.free(text);
        try std.testing.expectEqualStrings(expected, text);
    }

    /// RESOURCE_OUTPUT TLVs follow appendix-encoding and wire/field.rs. The
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

test "shipping raw remote drag uses provider selection and fences a frozen capture" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    const local_ref = fixture.local_ref;
    const tree = engine.model.selectedTree().?;
    _ = try tree.split(tree.focus, .horizontal, local_ref);
    try std.testing.expect(engine.model.focusedTerminalRef().?.eql(local_ref));
    try std.testing.expect(!engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0)));
    try std.testing.expect(engine.model.focusedTerminalRef().?.eql(fixture.ref));
    try std.testing.expect(!engine.model.provider.terminalConst(local_ref).?.session.selectionActive());
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, fixture.event(.pointer_drag, 6.5, 0)));
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
    wheel.delta_y = fixture.metrics().height * 64;
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, wheel));
    try std.testing.expectEqual(@as(u21, 'C'), engine.model.remotePresentation(fixture.ref).?.grid.rows[0].cells[0].cp);
    try fixture.output("\x1b[?1003h\x1b[?1006h", 2);
    try std.testing.expect(try remote.mouseTracking(engine.model.terminalOwner(fixture.ref).?));
    remote.bridge.outgoing.reset();
    wheel.delta_y = fixture.metrics().height * 3;
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
    var drag = fixture.event(.pointer_drag, 6.5, 0);
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

test "shipping remote word and line gestures retain Ghostty drag semantics" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const fx = NoShells{};
    try fixture.output("\x1b[2J\x1b[Hhello world next", 1);
    fixture.click(2, 0, 1);
    try std.testing.expect(!fixture.engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
    var press = fixture.event(.pointer_down, 2, 0);
    press.timestamp_ns = 100 * std.time.ns_per_ms;
    _ = fixture.engine.onPointer(&fx, press);
    try fixture.expectSelection("hello");
    _ = fixture.engine.onPointer(&fx, fixture.event(.pointer_drag, 8, 0));
    try fixture.expectSelection("hello world");
    _ = fixture.engine.onPointer(&fx, fixture.event(.pointer_up, 8, 0));
    fixture.click(2, 0, 200 * std.time.ns_per_ms);
    try fixture.expectSelection("hello world next");
}

test "shipping remote copy unwraps provider selected Unicode words" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    // Ghostty owns Unicode/wide-cell boundaries, including a word spanning
    // a soft wrap. The native adapter never reads projected text to find it.
    try fixture.output("\x1b[2J\x1b[H" ++ " " ** 77 ++ "界éword", 1);
    fixture.click(2, 1, std.time.ns_per_s);
    fixture.click(2, 1, std.time.ns_per_s + 100 * std.time.ns_per_ms);
    try fixture.expectSelection("éword");
    fixture.click(77, 0, 2 * std.time.ns_per_s);
    fixture.click(77, 0, 2 * std.time.ns_per_s + 100 * std.time.ns_per_ms);
    try fixture.expectSelection("界");
}

test "shipping remote pointer maps the canvas measured fractional cell pitch" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const fx = NoShells{};
    try std.testing.expectEqual(@as(f32, 10.25), fixture.metrics().width);
    try std.testing.expect(fixture.metrics().width != fixture.frame().width / 80);
    _ = fixture.engine.onPointer(&fx, fixture.event(.pointer_down, 1, 0));
    _ = fixture.engine.onPointer(&fx, fixture.event(.pointer_drag, 6.75, 0));
    try fixture.expectSelection("OCKPIT");
}

test "shipping remote full mouse modes gate hover and drag reports" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const remote = engine.model.phux().?;
    const fx = NoShells{};
    try fixture.output("\x1b[?1000h", 1);
    remote.bridge.outgoing.reset();
    try std.testing.expectEqual(.ignored, engine.onPointer(&fx, fixture.event(.pointer_move, 1, 0)));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 1, 0));
    try expectMouse(fixture, 0);
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 2, 0));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    _ = engine.onPointer(&fx, fixture.event(.pointer_up, 2, 0));
    try expectMouse(fixture, 1);
    try fixture.output("\x1b[?1000l\x1b[?1002h", 2);
    remote.bridge.outgoing.reset();
    _ = engine.onPointer(&fx, fixture.event(.pointer_move, 1, 0));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 1, 0));
    try expectMouse(fixture, 0);
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 2, 0));
    try expectMouse(fixture, 2);
    _ = engine.onPointer(&fx, fixture.event(.pointer_up, 2, 0));
    try expectMouse(fixture, 1);
    try fixture.output("\x1b[?1002l\x1b[?1003h", 3);
    remote.bridge.outgoing.reset();
    _ = engine.onPointer(&fx, fixture.event(.pointer_move, 3, 0));
    try expectMouse(fixture, 2);
    try fixture.output("\x1b[?1003l\x1b[?9h", 4);
    remote.bridge.outgoing.reset();
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 1, 0));
    try expectMouse(fixture, 0);
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 2, 0));
    _ = engine.onPointer(&fx, fixture.event(.pointer_up, 2, 0));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

test "shipping remote horizontal wheels retain fractional signs" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const remote = engine.model.phux().?;
    const fx = NoShells{};
    try fixture.output("\x1b[?1003h", 1);
    remote.bridge.outgoing.reset();
    var wheel = fixture.event(.scroll, 3, 0);
    for ([_]f32{ 1, -1 }, [_]u32{ 6, 7 }) |sign, button| {
        wheel.delta_x = sign * fixture.metrics().width / 2;
        _ = engine.onPointer(&fx, wheel);
        try std.testing.expect(!remote.bridge.outgoing.hasPending());
        _ = engine.onPointer(&fx, wheel);
        const bytes = remote.bridge.outgoing.take() orelse return error.TestExpectedMouse;
        defer remote.bridge.outgoing.release(bytes);
        try std.testing.expectEqual(button, std.mem.readInt(u32, bytes[20..24], .big));
    }
}

test "shipping remote typing releases anchors and retires held selection gestures" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0));
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 6, 0));
    const state = engine.model.remoteUi(fixture.ref).?;
    try std.testing.expect(state.start_anchor != 0);
    const old_start = state.start_anchor;
    const old_end = state.end_anchor;
    const old_handle = state.gesture_handle;
    engine.onText(&fx, .{ .phase = .text_input, .key = "x", .text = "x" });
    try std.testing.expectEqual(@as(u64, 0), state.start_anchor);
    try std.testing.expectEqual(@as(u64, 0), state.end_anchor);
    try std.testing.expectEqual(@as(u64, 0), state.gesture_handle);
    try std.testing.expectError(error.InvalidState, engine.model.phux().?.setSelection(state.owner, .{ .opaque_id = old_start }, .{ .opaque_id = old_end }, false));
    try std.testing.expectError(error.InvalidState, engine.model.phux().?.selectionGesture(state.owner, .{
        .phase = .drag,
        .handle = old_handle,
        .cell = .{ .space = .viewport, .column = 9, .row = 0 },
        .x = 90,
        .y = 0,
        .columns = 80,
        .cell_width = fixture.metrics().width,
        .screen_height = fixture.frame().height,
    }));
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 9, 0));
    try std.testing.expect(!engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
}

test "shipping input suspension gates pointer and file drops" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    engine.setInputSuspended(&fx, true);
    try std.testing.expectEqual(.ignored, engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0)));
    try std.testing.expect(!engine.onDrop(&fx, .{ .window_id = 1, .view_label = cockpit.scene.canvas_label, .paths = &.{"/blocked"} }));
    engine.setInputSuspended(&fx, false);
}

test "shipping keyboard selection releases retained pointer anchors" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    const state = engine.model.remoteUi(fixture.ref).?;
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0));
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 6, 0));
    try std.testing.expect(state.gesture_handle != 0);
    const pointer_start = state.start_anchor;
    const pointer_end = state.end_anchor;
    engine.onKey(&fx, .{ .phase = .key_down, .key = "space", .modifiers = .{ .super = true, .shift = true } });
    try std.testing.expect(state.selecting);
    try std.testing.expectEqual(@as(u64, 0), state.gesture_handle);
    try std.testing.expectError(error.InvalidState, engine.model.phux().?.setSelection(state.owner, .{ .opaque_id = pointer_start }, .{ .opaque_id = pointer_end }, false));
    _ = engine.onPointer(&fx, fixture.event(.pointer_up, 6, 0));
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0));
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 6, 0));
    engine.model.phux().?.stop();
    _ = engine.onPointer(&fx, fixture.event(.pointer_up, 6, 0));
    try std.testing.expect(!engine.selectionAutoscrollActive());
    try std.testing.expectEqual(@as(u64, 0), state.start_anchor);
    try std.testing.expectEqual(@as(u64, 0), state.gesture_handle);
}

test "shipping remote autoscroll keeps offscreen document anchors highlighted" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    try fixture.output("\r\nrow" ** 40, 1);
    try engine.model.phux().?.scrollViewport(engine.model.terminalOwner(fixture.ref).?, .{ .kind = .top });
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0));
    var drag = fixture.event(.pointer_drag, 2.5, 23);
    drag.y = fixture.frame().y + fixture.frame().height + 1;
    _ = engine.onPointer(&fx, drag);
    try std.testing.expect(engine.selectionAutoscrollActive());
    for (0..4) |_| engine.selectionAutoscroll(&fx);
    try std.testing.expect(engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
    const text = try fixture.selectionText();
    defer std.testing.allocator.free(text);
    try std.testing.expect(std.mem.startsWith(u8, text, "COCKPIT"));
    try std.testing.expect(std.mem.endsWith(u8, text, "row"));
    drag.kind = .pointer_up;
    _ = engine.onPointer(&fx, drag);
    try std.testing.expectEqual(@as(u64, 0), engine.model.remoteUi(fixture.ref).?.gesture_handle);
}

test "shipping remote provider clearing republishes the selection grid" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0));
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 6.5, 0));
    try std.testing.expect(engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
    try engine.model.phux().?.clearSelection(engine.model.terminalOwner(fixture.ref).?);
    try std.testing.expect(!engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
}

test "shipping input suspension releases provider gesture capture" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0));
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 6.5, 0));
    const state = engine.model.remoteUi(fixture.ref).?;
    try std.testing.expect(state.gesture_handle != 0);
    engine.setInputSuspended(&fx, true);
    try std.testing.expectEqual(@as(u64, 0), state.gesture_handle);
    try std.testing.expect(engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
}

test "shipping background windows accept wheel scrolling and Finder drops" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    try fixture.output("\r\nrow" ** 40, 1);
    const before = engine.model.remotePresentation(fixture.ref).?.history_viewport_offset;
    engine.setFocused(&fx, false);
    var wheel = fixture.event(.scroll, 0, 0);
    wheel.delta_y = fixture.metrics().height * 4;
    try std.testing.expectEqual(.consumed, engine.onPointer(&fx, wheel));
    try std.testing.expect(before != engine.model.remotePresentation(fixture.ref).?.history_viewport_offset);
    try std.testing.expect(engine.onDrop(&fx, .{ .window_id = 1, .view_label = cockpit.scene.canvas_label, .paths = &.{"/background drop"} }));
}

test "shipping mouse routing follows Ghostty effective mode after conflicting DEC sets" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const remote = engine.model.phux().?;
    const owner = engine.model.terminalOwner(fixture.ref).?;
    const fx = NoShells{};
    try fixture.output("\x1b[?1000h\x1b[?1003h\x1b[?1003l", 1);
    try std.testing.expectEqual(.off, try remote.mouseMode(owner));
    try std.testing.expect(!try remote.mouseTracking(owner));
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 0, 0));
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 6.5, 0));
    try fixture.expectSelection("COCKPIT");
    _ = engine.onPointer(&fx, fixture.event(.pointer_up, 6.5, 0));
    try fixture.output("\x1b[?1003h\x1b[?1000h", 2);
    try std.testing.expectEqual(.normal, try remote.mouseMode(owner));
    remote.bridge.outgoing.reset();
    try std.testing.expectEqual(.ignored, engine.onPointer(&fx, fixture.event(.pointer_move, 1, 0)));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

test "shipping remote drag back to origin clears the range without ending capture" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const fixture = try Fixture.start();
    defer fixture.engine.destroy();
    const engine = fixture.engine;
    const fx = NoShells{};
    _ = engine.onPointer(&fx, fixture.event(.pointer_down, 3, 0));
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 6.5, 0));
    try std.testing.expect(engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 3, 0));
    try std.testing.expect(!engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
    const state = engine.model.remoteUi(fixture.ref).?;
    try std.testing.expectEqual(@as(u64, 0), state.start_anchor);
    try std.testing.expectEqual(@as(u64, 0), state.end_anchor);
    try std.testing.expect(state.gesture_handle != 0);
    _ = engine.onPointer(&fx, fixture.event(.pointer_drag, 6.5, 0));
    try std.testing.expect(engine.model.remotePresentation(fixture.ref).?.grid.selection_active);
    engine.setFocused(&fx, false);
    try std.testing.expectEqual(@as(u64, 0), state.gesture_handle);
}
