//! Shipping raw pointer routing. Provider anchors own the selected document;
//! this adapter owns only native gesture capture and viewport coordinates.
const std = @import("std");
const sdk = @import("native_sdk");
const contract = @import("provider_contract");
const support = @import("../phux_support.zig");
const model_module = @import("../model.zig");
const pointer = @import("../pointer_input.zig");
const selection = @import("../update.zig").remote_selection;
const interaction = @import("../terminal_interaction.zig");
const Model = model_module.Model;
const Raw = sdk.platform.GpuSurfaceInputEvent;
const Point = sdk.geometry.PointF;
const Rect = sdk.geometry.RectF;
const Owner = contract.ReplicaOwner;

const Capture = struct {
    owner: Owner,
    retired: bool = false,
    window_id: sdk.platform.WindowId,
    window_index: usize,
    pointer_id: u64,
    button: i32,
    reporting: bool,
    point: Point,
    cell: contract.DocumentPoint,
    gesture_handle: u64,
    modifiers: contract.ModifierMask,
};

const ClickTarget = struct {
    owner: Owner,
    window_id: sdk.platform.WindowId,
    pointer_id: u64,
    button: i32,
};

pub const State = struct {
    captures: [model_module.max_pointer_captures]?Capture = @splat(null),
    last_click: ?ClickTarget = null,
    wheel_owner: ?Owner = null,
    wheel_mode: contract.MouseMode = .off,

    pub fn continuesClick(self: *State, model: *const Model, raw: Raw) bool {
        const ref = pointer.terminalRefAtPoint(model, raw.x, raw.y) orelse return false;
        const owner = model.terminalOwner(ref) orelse return false;
        const previous = self.last_click;
        self.last_click = .{ .owner = owner, .window_id = raw.window_id, .pointer_id = raw.pointer_id, .button = raw.button };
        const last = previous orelse return false;
        return last.owner.eql(owner) and last.window_id == raw.window_id and
            last.pointer_id == raw.pointer_id and last.button == raw.button;
    }

    /// null delegates to the local path. A stale remote capture consumes its
    /// tail rather than targeting the newly published replica underneath it.
    pub fn route(self: *State, model: *Model, raw: Raw, clicks: u8) ?bool {
        if (comptime !support.phux_enabled) return null;
        if (self.captureIndex(raw)) |index| {
            if (self.routeCaptured(model, raw, index)) |handled| return handled;
        }
        const ref = pointer.terminalRefAtPoint(model, raw.x, raw.y) orelse return null;
        if (contract.isLocal(ref)) return null;
        const state = model.remoteUi(ref) orelse return false;
        const owner = state.owner;
        if (!ready(model, owner)) return false;
        const frame = pointer.paneFrameForTerminal(model, ref) orelse return false;
        return self.uncaptured(model, raw, owner, frame, clicks);
    }

    fn routeCaptured(self: *State, model: *Model, raw: Raw, index: usize) ?bool {
        switch (raw.kind) {
            .pointer_drag, .pointer_move, .pointer_up, .pointer_cancel => return self.tail(model, raw, index),
            .pointer_down => self.finish(model, index),
            else => {},
        }
        return null;
    }

    fn uncaptured(self: *State, model: *Model, raw: Raw, owner: Owner, frame: Rect, clicks: u8) bool {
        return switch (raw.kind) {
            .pointer_down => self.press(model, raw, owner, frame, clicks),
            .scroll => self.wheelTarget(model, raw, owner, frame),
            .pointer_move => !raw.modifiers.shift and report(model, owner, .move, .none, modifiers(raw), .{ .x = raw.x, .y = raw.y }, frame),
            else => false,
        };
    }

    fn wheelTarget(self: *State, model: *Model, raw: Raw, owner: Owner, frame: Rect) bool {
        const remote = model.phuxForOwner(owner) orelse return false;
        const mode = if (raw.modifiers.shift) .off else remote.mouseMode(owner) catch return false;
        const state = interaction.stateForOwner(model, owner) orelse return false;
        const same_owner = if (self.wheel_owner) |previous| previous.eql(owner) else false;
        if (!same_owner or mode != self.wheel_mode) {
            state.wheel_accum = 0;
            state.wheel_accum_x = 0;
        }
        self.wheel_owner = owner;
        self.wheel_mode = mode;
        return wheel(model, raw, owner, frame, mode != .off);
    }

    fn captureIndex(self: *const State, raw: Raw) ?usize {
        for (self.captures, 0..) |slot, index| {
            const capture = slot orelse continue;
            if (capture.window_id == raw.window_id and capture.pointer_id == raw.pointer_id) return index;
        }
        return null;
    }

    fn freeIndex(self: *const State) ?usize {
        for (self.captures, 0..) |slot, index| if (slot == null) return index;
        return null;
    }

    pub fn cancelPointer(self: *State, model: *Model, raw: Raw) void {
        if (comptime !support.phux_enabled) return;
        const index = self.captureIndex(raw) orelse return;
        self.finish(model, index);
    }

    fn press(self: *State, model: *Model, raw: Raw, owner: Owner, frame: Rect, clicks: u8) bool {
        const point: Point = .{ .x = raw.x, .y = raw.y };
        if (raw.button <= 1) {
            if (model.selectedTree()) |tree| _ = tree.focusTerminal(owner.terminal_ref);
        }
        const cell = coordinate(model, owner, point, frame) orelse return true;
        const index = self.freeIndex() orelse return false;
        const tracking = tracksMouse(model, owner) and !raw.modifiers.shift;
        if (!startGesture(model, raw, owner, frame, cell, tracking, clicks)) return true;
        self.captures[index] = .{
            .owner = owner,
            .window_id = raw.window_id,
            .window_index = model.active_window,
            .pointer_id = raw.pointer_id,
            .button = raw.button,
            .reporting = tracking,
            .point = point,
            .cell = cell,
            .gesture_handle = if (interaction.stateForOwner(model, owner)) |state| state.gesture_handle else 0,
            .modifiers = modifiers(raw),
        };
        return true;
    }

    fn tail(self: *State, model: *Model, raw: Raw, index: usize) bool {
        if (raw.kind == .pointer_cancel) {
            self.finish(model, index);
            return true;
        }
        defer if (raw.kind == .pointer_up) self.finish(model, index);
        const captured = self.captures[index].?;
        if (!captureCurrent(model, captured)) {
            self.retire(model, index);
            return true;
        }
        const frame = pointer.paneFrameForTerminal(model, captured.owner.terminal_ref) orelse return true;
        const point: Point = .{ .x = raw.x, .y = raw.y };
        const cell = coordinate(model, captured.owner, point, frame) orelse return true;
        self.captures[index].?.point = point;
        self.captures[index].?.cell = cell;
        self.captures[index].?.modifiers = modifiers(raw);
        if (captured.reporting) {
            if (raw.kind != .pointer_up) _ = report(model, captured.owner, .move, buttonFor(captured.button), modifiers(raw), point, frame);
        } else dragSelection(model, captured, cell, point, frame);
        return true;
    }

    fn finish(self: *State, model: *Model, index: usize) void {
        const capture = self.captures[index] orelse return;
        self.captures[index] = null;
        if (!capture.retired) releaseCapture(model, capture);
    }

    /// Release the old gesture once, but retain its pointer identity so later
    /// motion cannot become an uncaptured event for a replacement attachment.
    fn retire(self: *State, model: *Model, index: usize) void {
        const capture = self.captures[index] orelse return;
        if (capture.retired) return;
        self.captures[index].?.retired = true;
        releaseCapture(model, capture);
    }

    fn releaseCapture(model: *Model, capture: Capture) void {
        if (!ready(model, capture.owner)) {
            retireSelection(model, capture.owner);
            return;
        }
        if (capture.reporting) {
            _ = sendCell(model, capture.owner, .release, buttonFor(capture.button), capture.modifiers, capture.cell);
        } else finishSelection(model, capture);
    }

    pub fn cancelAll(self: *State, model: *Model) void {
        if (comptime !support.phux_enabled) return;
        for (0..self.captures.len) |index| self.finish(model, index);
    }

    pub fn autoscrollActive(self: *const State, model: *const Model) bool {
        for (self.captures) |slot| {
            const capture = slot orelse continue;
            if (autoscrollDirection(model, capture) != 0) return true;
        }
        return false;
    }

    pub fn autoscroll(self: *State, model: *Model) void {
        if (comptime !support.phux_enabled) return;
        for (self.captures, 0..) |slot, index| {
            const capture = slot orelse continue;
            if (!captureCurrent(model, capture)) {
                self.retire(model, index);
                continue;
            }
            scrollSelection(model, capture);
        }
    }
};

fn ready(model: *const Model, owner: Owner) bool {
    if (!model.ownerIsCurrent(owner)) return false;
    const presentation = interaction.presentationForOwner(model, owner) orelse return false;
    return presentation.phase == .live;
}

fn captureCurrent(model: *const Model, capture: Capture) bool {
    if (capture.retired) return false;
    if (!model.focused or !ready(model, capture.owner)) return false;
    if (model.active_window != capture.window_index) return false;
    if (!ownerInSelectedTree(model, capture.owner)) return false;
    if (capture.reporting) return true;
    const state = interaction.stateForOwnerConst(model, capture.owner) orelse return false;
    return state.gesture_handle != 0 and state.gesture_handle == capture.gesture_handle;
}

fn ownerInSelectedTree(model: *const Model, owner: Owner) bool {
    const tree = model.selectedTreeConst() orelse return false;
    if (tree.find(owner.terminal_ref) == null) return false;
    const remote = model.phuxForTreeConst(tree) orelse return false;
    const visible_owner = remote.owner(owner.terminal_ref) orelse return false;
    return visible_owner.eql(owner);
}

fn retireSelection(model: *Model, owner: Owner) void {
    for (&model.remote_ui) |*state| {
        if (state.owner.eql(owner)) selection.clear(model, state);
    }
}

fn autoscrollDirection(model: *const Model, capture: Capture) i64 {
    if (capture.reporting or !captureCurrent(model, capture)) return 0;
    const frame = pointer.paneFrameForTerminal(model, capture.owner.terminal_ref) orelse return 0;
    if (capture.point.y < frame.y) return -1;
    if (capture.point.y >= frame.y + frame.height) return 1;
    return 0;
}

fn scrollSelection(model: *Model, capture: Capture) void {
    const direction = autoscrollDirection(model, capture);
    if (direction == 0) return;
    const remote = model.phuxForOwner(capture.owner) orelse return;
    remote.scrollViewport(capture.owner, .{ .kind = .delta, .value = direction }) catch return;
    const frame = pointer.paneFrameForTerminal(model, capture.owner.terminal_ref) orelse return;
    const cell = coordinate(model, capture.owner, capture.point, frame) orelse return;
    dragSelection(model, capture, cell, capture.point, frame);
}

fn modifiers(raw: Raw) contract.ModifierMask {
    return .{ .shift = raw.modifiers.shift, .control = raw.modifiers.control, .alt = raw.modifiers.option, .super = raw.modifiers.command };
}

fn buttonFor(button: i32) contract.MouseButton {
    if (button < 0) return .none;
    return pointer.pointerButton(@intCast(button));
}

fn coordinate(model: *const Model, owner: Owner, point: Point, frame: Rect) ?contract.DocumentPoint {
    if (!validGeometry(point, frame)) return null;
    const presentation = interaction.presentationForOwner(model, owner) orelse return null;
    if (presentation.cols == 0 or presentation.rows == 0) return null;
    const measured = presentation.measured_cell orelse return null;
    if (!validCellExtent(measured.width) or !validCellExtent(measured.height)) return null;
    return .{
        .space = .viewport,
        .column = cellAt(point.x - frame.x, measured.width, presentation.cols),
        .row = cellAt(point.y - frame.y, measured.height, presentation.rows),
    };
}

fn validCellExtent(value: f32) bool {
    return std.math.isFinite(value) and value > 0;
}

fn validGeometry(point: Point, frame: Rect) bool {
    return std.math.isFinite(point.x) and std.math.isFinite(point.y) and
        std.math.isFinite(frame.x) and std.math.isFinite(frame.y) and
        std.math.isFinite(frame.width) and std.math.isFinite(frame.height) and
        frame.width > 0 and frame.height > 0;
}

fn cellAt(value: f32, extent: f32, cells: u16) u16 {
    return @intFromFloat(std.math.clamp(@floor(value / extent), 0, @as(f32, @floatFromInt(cells - 1))));
}

fn tracksMouse(model: *Model, owner: Owner) bool {
    const remote = model.phuxForOwner(owner) orelse return false;
    return remote.mouseTracking(owner) catch false;
}

fn report(model: *Model, owner: Owner, action: contract.MouseAction, button: contract.MouseButton, mods: contract.ModifierMask, point: Point, frame: Rect) bool {
    const cell = coordinate(model, owner, point, frame) orelse return false;
    return sendCell(model, owner, action, button, mods, cell);
}

fn sendCell(model: *Model, owner: Owner, action: contract.MouseAction, button: contract.MouseButton, mods: contract.ModifierMask, cell: contract.DocumentPoint) bool {
    if (!ready(model, owner)) return false;
    const remote = model.phuxForOwner(owner) orelse return false;
    const mode = remote.mouseMode(owner) catch return false;
    if (!reportsAction(mode, action, button)) return false;
    remote.sendMouse(owner, &.{ .action = action, .button = button, .modifiers = mods, .x = @floatFromInt(cell.column), .y = @floatFromInt(cell.row) }) catch return false;
    return true;
}

fn reportsAction(mode: contract.MouseMode, action: contract.MouseAction, button: contract.MouseButton) bool {
    return switch (mode) {
        .off => false,
        .x10 => action == .press,
        .normal => action != .move,
        .button => action != .move or button != .none,
        .any_motion => true,
    };
}

fn startGesture(model: *Model, raw: Raw, owner: Owner, frame: Rect, cell: contract.DocumentPoint, tracking: bool, clicks: u8) bool {
    if (tracking) return reportPress(model, raw, owner, frame);
    if (raw.button != 0) return false;
    return beginSelection(model, owner, cell, clicks, .{ .x = raw.x, .y = raw.y }, frame);
}

fn reportPress(model: *Model, raw: Raw, owner: Owner, frame: Rect) bool {
    const button = buttonFor(raw.button);
    if (button == .none) return false;
    if (!report(model, owner, .press, button, modifiers(raw), .{ .x = raw.x, .y = raw.y }, frame)) return false;
    if (raw.button != 1) clearSelection(model, owner);
    return true;
}

fn clearSelection(model: *Model, owner: Owner) void {
    const state = interaction.stateForOwner(model, owner) orelse return;
    selection.clear(model, state);
}

fn beginSelection(model: *Model, owner: Owner, cell: contract.DocumentPoint, clicks: u8, point: Point, frame: Rect) bool {
    const state = interaction.stateForOwner(model, owner) orelse return false;
    selection.clear(model, state);
    return applyGesture(model, owner, .press, clicks, cell, point, frame);
}

fn dragSelection(model: *Model, capture: Capture, cell: contract.DocumentPoint, point: Point, frame: Rect) void {
    const state = interaction.stateForOwner(model, capture.owner) orelse return;
    if (state.gesture_handle != capture.gesture_handle or state.gesture_handle == 0) return;
    _ = applyGesture(model, capture.owner, .drag, 1, cell, point, frame);
}

fn finishSelection(model: *Model, capture: Capture) void {
    const state = interaction.stateForOwner(model, capture.owner) orelse return;
    if (state.gesture_handle != capture.gesture_handle or state.gesture_handle == 0) return;
    const remote = model.phuxForOwner(capture.owner) orelse return;
    _ = remote.selectionGesture(capture.owner, .{
        .phase = .release,
        .handle = capture.gesture_handle,
        .cell = capture.cell,
        .x = 0,
        .y = 0,
        .columns = 1,
        .cell_width = 1,
        .screen_height = 1,
    }) catch {};
    state.gesture_handle = 0;
}

fn applyGesture(model: *Model, owner: Owner, gesture_phase: @FieldType(contract.SelectionGesture, "phase"), clicks: u8, cell: contract.DocumentPoint, point: Point, frame: Rect) bool {
    const state = interaction.stateForOwner(model, owner) orelse return false;
    const remote = model.phuxForOwner(owner) orelse return false;
    const presentation = interaction.presentationForOwner(model, owner) orelse return false;
    const measured = presentation.measured_cell orelse return false;
    const result = remote.selectionGesture(owner, .{
        .phase = gesture_phase,
        .handle = state.gesture_handle,
        .clicks = clicks,
        .cell = cell,
        .x = point.x - frame.x,
        .y = point.y - frame.y,
        .columns = presentation.cols,
        .cell_width = measured.width,
        .screen_height = frame.height,
        .rectangle = state.rectangle,
    }) catch {
        selection.clear(model, state);
        return false;
    };
    state.gesture_handle = result.handle;
    if (state.start_anchor != 0) remote.releaseAnchor(owner, .{ .opaque_id = state.start_anchor });
    if (state.end_anchor != 0 and state.end_anchor != state.start_anchor) remote.releaseAnchor(owner, .{ .opaque_id = state.end_anchor });
    state.start_anchor = result.start;
    state.end_anchor = result.end;
    state.head_x = cell.column;
    state.head_y = cell.row;
    return true;
}

fn wheel(model: *Model, raw: Raw, owner: Owner, frame: Rect, reporting: bool) bool {
    if (!validWheelDelta(raw)) return false;
    const quantum = wheelQuantum(model, owner, frame) orelse return false;
    const state = interaction.stateForOwner(model, owner) orelse return false;
    const rows = wheelRows(&state.wheel_accum, raw.delta_y, quantum);
    if (reporting) return horizontalWheel(model, raw, owner, frame, rows);
    if (rows == 0) return true;
    const remote = model.phuxForOwner(owner) orelse return false;
    remote.scrollViewport(owner, .{ .kind = .delta, .value = -rows }) catch return false;
    return true;
}

fn validWheelDelta(raw: Raw) bool {
    return std.math.isFinite(raw.delta_x) and std.math.isFinite(raw.delta_y) and
        (raw.delta_x != 0 or raw.delta_y != 0);
}

fn horizontalWheel(model: *Model, raw: Raw, owner: Owner, frame: Rect, rows: i64) bool {
    const presentation = interaction.presentationForOwner(model, owner) orelse return false;
    const measured = presentation.measured_cell orelse return false;
    const state = interaction.stateForOwner(model, owner) orelse return false;
    const columns = wheelRows(&state.wheel_accum_x, raw.delta_x, measured.width);
    return reportWheel(model, raw, owner, frame, rows, columns);
}

fn wheelQuantum(model: *const Model, owner: Owner, frame: Rect) ?f32 {
    _ = frame;
    const presentation = interaction.presentationForOwner(model, owner) orelse return null;
    if (presentation.rows == 0) return null;
    const quantum = (presentation.measured_cell orelse return null).height;
    if (!std.math.isFinite(quantum) or quantum <= 0) return null;
    return quantum;
}

fn wheelRows(accum: *f32, delta: f32, quantum: f32) i64 {
    if (!std.math.isFinite(accum.*)) accum.* = 0;
    const bound = quantum * 64;
    accum.* = std.math.clamp(accum.* + std.math.clamp(delta, -bound, bound), -bound, bound);
    const rows: i64 = @intFromFloat(@trunc(accum.* / quantum));
    accum.* -= @as(f32, @floatFromInt(rows)) * quantum;
    return rows;
}

fn reportWheel(model: *Model, raw: Raw, owner: Owner, frame: Rect, rows: i64, columns: i64) bool {
    if (rows != 0 or columns != 0) clearSelection(model, owner);
    var count: u64 = @abs(rows);
    while (count > 0) : (count -= 1) {
        _ = report(model, owner, .press, if (rows > 0) .button_4 else .button_5, modifiers(raw), .{ .x = raw.x, .y = raw.y }, frame);
    }
    count = @abs(columns);
    while (count > 0) : (count -= 1) {
        _ = report(model, owner, .press, if (columns > 0) .button_6 else .button_7, modifiers(raw), .{ .x = raw.x, .y = raw.y }, frame);
    }
    return true;
}

pub fn pasteDrop(model: *Model, terminal: contract.TerminalRef, text: []const u8) bool {
    if (comptime !support.phux_enabled) return false;
    const owner = model.terminalOwner(terminal) orelse return false;
    if (!ready(model, owner)) return false;
    const remote = model.phuxForOwner(owner) orelse return false;
    remote.sendPaste(owner, text, false) catch return false;
    if (model.selectedTree()) |tree| _ = tree.focusTerminal(terminal);
    return true;
}

pub fn phase(raw: Raw) ?sdk.canvas.WidgetPointerPhase {
    return switch (raw.kind) {
        .pointer_down => .down,
        .pointer_up => .up,
        .pointer_cancel => .cancel,
        .pointer_move => .hover,
        .pointer_drag => .move,
        .scroll => .wheel,
        else => null,
    };
}

pub fn cancelLocal(model: *Model, fx: anytype, raw: Raw) void {
    const previous = pointer.pointerCaptureFor(model, raw.window_id, raw.pointer_id) orelse return;
    pointer.handleTerminalPointer(model, fx, .{
        .window_id = previous.window_id,
        .terminal_id = previous.terminal_id,
        .generation = previous.generation,
        .phase = .cancel,
        .pointer_id = previous.pointer_id,
        .button = previous.button,
        .point = previous.last_point,
        .frame = previous.frame,
        .modifiers = previous.modifiers,
    });
}

pub fn localCaptured(model: *const Model, raw: Raw) bool {
    return switch (raw.kind) {
        .pointer_drag, .pointer_up, .pointer_cancel => pointer.pointerCaptureFor(model, raw.window_id, raw.pointer_id) != null,
        else => false,
    };
}

const LocalTarget = struct { id: support.LocalResourceId, generation: u64, frame: Rect };

fn localHit(model: *Model, raw: Raw) ?LocalTarget {
    const ref = pointer.terminalRefAtPoint(model, raw.x, raw.y) orelse return null;
    const pane = model.provider.terminal(ref) orelse return null;
    return .{
        .id = contract.localId(ref) orelse return null,
        .generation = pane.session_generation,
        .frame = pointer.paneFrameForTerminal(model, ref) orelse return null,
    };
}

fn localTarget(model: *Model, raw: Raw) ?LocalTarget {
    switch (raw.kind) {
        .pointer_drag, .pointer_up, .pointer_cancel => {
            const capture = pointer.pointerCaptureFor(model, raw.window_id, raw.pointer_id) orelse return null;
            return .{
                .id = capture.terminal_id,
                .generation = capture.generation,
                .frame = pointer.paneFrameForTerminal(model, support.localRef(capture.terminal_id)) orelse capture.frame,
            };
        },
        else => return localHit(model, raw),
    }
}

pub fn dispatchLocal(model: *Model, fx: anytype, raw: Raw, clicks: u8) bool {
    const target = localTarget(model, raw) orelse return false;
    pointer.handleTerminalPointer(model, fx, .{
        .window_id = raw.window_id,
        .terminal_id = target.id,
        .generation = target.generation,
        .phase = phase(raw) orelse return false,
        .pointer_id = raw.pointer_id,
        .button = raw.button,
        .click_count = clicks,
        .point = .{ .x = raw.x, .y = raw.y },
        .frame = target.frame,
        .delta = .{ .dx = raw.delta_x, .dy = raw.delta_y },
        .modifiers = .{ .shift = raw.modifiers.shift, .control = raw.modifiers.control, .alt = raw.modifiers.option, .super = raw.modifiers.command },
    });
    return true;
}

const SourceFixture = @import("remote_presentation_commands.zig").test_support;

fn sourcePointerLayout(fixture: SourceFixture, both: bool) void {
    const model = fixture.engine.model;
    fixture.seedUi();
    model.focused = true;
    model.primary.web_selected = false;
    model.primary.selected_tab = 0;
    model.primary.surface_size = .{ .width = 1100, .height = 640 };
    model.primary.tabs[0] = @import("../layout.zig").Tree.initLeaf(fixture.owner_a.terminal_ref);
    model.primary.tabs[0].attachment_id = fixture.a.context_id;
    model.primary.tabs[1] = @import("../layout.zig").Tree.initLeaf(fixture.owner_b.terminal_ref);
    model.primary.tabs[1].attachment_id = fixture.b.context_id;
    model.primary.tab_count = if (both) 2 else 1;
}

fn enableSourceMotion(remote: *support.PhuxProvider, owner: Owner) !void {
    // RESOURCE_OUTPUT uses the same production-decoded TLVs as the shipping
    // pointer fixtures: resource 7, stream 7, bootstrap 1, first output frame.
    const text = "\x1b[?1003h\x1b[?1006h";
    var storage: [128]u8 = undefined;
    var writer: std.Io.Writer = .fixed(&storage);
    try writer.writeAll(&.{ 0, 0, 0, 0, 0x90, 1, 4, 5, 0, 0, 0, 0, 7, 2, 4, 8 });
    try writer.writeInt(u64, 1, .big);
    try writer.writeAll(&.{ 3, 4, text.len });
    try writer.writeAll(text);
    try writer.writeAll(&.{ 4, 4, 8 });
    try writer.writeInt(u64, 7, .big);
    try writer.writeAll(&.{ 5, 4, 8 });
    try writer.writeInt(u64, 1, .big);
    const bytes = writer.buffered();
    std.mem.writeInt(u32, bytes[0..4], @intCast(bytes.len - 4), .big);
    try std.testing.expect(remote.bridge.incoming.stage(bytes));
    _ = try remote.drainReadiness();
    try std.testing.expectEqual(contract.MouseMode.any_motion, try remote.mouseMode(owner));
    remote.host.recordMeasuredCell(owner, .{ .width = 8, .height = 16 });
    remote.bridge.outgoing.reset();
}

fn sourcePointerEvent(model: *const Model, owner: Owner) !Raw {
    const frame = pointer.paneFrameForTerminal(model, owner.terminal_ref) orelse return error.MissingFrame;
    return .{ .window_id = 1, .label = "source-pointer", .kind = .pointer_down, .pointer_id = 7, .button = 0, .x = frame.x + 2, .y = frame.y + 2 };
}

test "shipping captured source uses selected tree when the global ref is ambiguous" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    const fixture = try SourceFixture.init();
    defer fixture.engine.destroy();
    sourcePointerLayout(fixture, true);
    try enableSourceMotion(fixture.a, fixture.owner_a);
    try enableSourceMotion(fixture.b, fixture.owner_b);
    const model = fixture.engine.model;
    try std.testing.expect(model.phuxForRef(fixture.owner_a.terminal_ref) == null);
    try std.testing.expect(model.ownerIsCurrent(fixture.owner_a));
    try std.testing.expect(model.ownerIsCurrent(fixture.owner_b));
    var state: State = .{};
    var raw = try sourcePointerEvent(model, fixture.owner_a);
    const frame = pointer.paneFrameForTerminal(model, fixture.owner_a.terminal_ref).?;
    try std.testing.expect(state.press(model, raw, fixture.owner_a, frame, 1));
    try std.testing.expect(fixture.a.bridge.outgoing.hasPending());
    fixture.a.bridge.outgoing.reset();
    raw.kind = .pointer_drag;
    try std.testing.expectEqual(@as(?bool, true), state.route(model, raw, 1));
    try std.testing.expect(state.captures[0] != null);
    try std.testing.expect(fixture.a.bridge.outgoing.hasPending());
    try std.testing.expect(!fixture.b.bridge.outgoing.hasPending());
    state.cancelAll(model);
}

fn retiredSourceTail(autoscroll: bool, release_first: bool) !void {
    const fixture = try SourceFixture.init();
    defer fixture.engine.destroy();
    sourcePointerLayout(fixture, false);
    try enableSourceMotion(fixture.a, fixture.owner_a);
    try enableSourceMotion(fixture.b, fixture.owner_b);
    const model = fixture.engine.model;
    var state: State = .{};
    var raw = try sourcePointerEvent(model, fixture.owner_a);
    try std.testing.expectEqual(@as(?bool, true), state.route(model, raw, 1));
    try std.testing.expect(state.captures[0] != null);
    fixture.a.host.freezePublished();
    model.primary.tabs[0] = model.primary.tabs[1];
    fixture.a.bridge.outgoing.reset();
    if (autoscroll) state.autoscroll(model);
    raw.kind = .pointer_drag;
    try std.testing.expectEqual(@as(?bool, true), state.route(model, raw, 1));
    raw.kind = .pointer_move;
    try std.testing.expectEqual(@as(?bool, true), state.route(model, raw, 1));
    try std.testing.expect(!fixture.b.bridge.outgoing.hasPending());
    if (release_first) {
        raw.kind = .pointer_up;
        try std.testing.expectEqual(@as(?bool, true), state.route(model, raw, 1));
        try std.testing.expect(state.captures[0] == null);
    }
    try std.testing.expect(!fixture.a.bridge.outgoing.hasPending());
    try std.testing.expect(!fixture.b.bridge.outgoing.hasPending());
    raw.kind = .pointer_down;
    try std.testing.expectEqual(@as(?bool, true), state.route(model, raw, 1));
    try std.testing.expect(state.captures[0].?.owner.eql(fixture.owner_b));
    try std.testing.expect(fixture.b.bridge.outgoing.hasPending());
    state.cancelAll(model);
}

test "shipping retired source consumes multiple motion events and release before a fresh press" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    try retiredSourceTail(false, true);
}

test "shipping autoscroll retirement retains the source tombstone through release" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    try retiredSourceTail(true, true);
}

test "shipping fresh press can replace a retired source tombstone before its old release" {
    if (comptime !support.phux_enabled) return error.SkipZigTest;
    try retiredSourceTail(false, false);
}
