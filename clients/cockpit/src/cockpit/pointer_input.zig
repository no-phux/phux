const std = @import("std");
const native_sdk = @import("native_sdk");
const vt = @import("ghostty-vt");
const provider_contract = @import("provider_contract");
const support = @import("phux_support.zig");
const local_terminal = @import("../providers/local/provider.zig");
const model_module = @import("model.zig");
const topology = @import("topology.zig");
const app_types = @import("app_types.zig");
const runtime = @import("terminal_runtime.zig");
const projection = @import("native/workspace_projection.zig");
const grid = @import("../terminal/grid.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const Model = model_module.Model;
const Pane = local_terminal.Pane;
const Fx = app_types.Fx;
const TerminalRef = support.TerminalRef;
const ReplicaOwner = support.ReplicaOwner;
const MouseAction = support.MouseAction;
const MouseButton = support.MouseButton;
const ModifierMask = support.ModifierMask;
const PointerCapture = model_module.PointerCapture;
const PointerModifiers = model_module.PointerModifiers;
const TerminalPointerEvent = model_module.TerminalPointerEvent;
const max_mouse_reports_per_event: usize = 64;
const max_mouse_coordinate: f32 = 1_000_000;
const phux_enabled = support.phux_enabled;
const providerKind = support.providerKind;
const localRef = support.localRef;
const cockpitTokens = projection.cockpitTokens;
const enqueueTransient = runtime.enqueueTransient;

pub fn paneFrameForTerminal(model: *const Model, id: TerminalRef) ?geometry.RectF {
    const frame = projection.paneFrameFor(model, model.wsConst().surface_size, id) orelse return null;
    return if (frame.isEmpty()) null else frame;
}

/// The provider-qualified terminal whose rect contains a view point, or null
/// when the point stands over chrome, a gutter, or outside the panes. This is
/// `tree.paneAt` — the SAME resolve the painter and the widget tree consume,
/// so a hit target can never sit somewhere text does not.
pub fn terminalRefAtPoint(model: *const Model, x: f32, y: f32) ?TerminalRef {
    if (!std.math.isFinite(x) or !std.math.isFinite(y)) return null;
    const pane = projection.paneAtPoint(model, model.wsConst().surface_size, x, y) orelse return null;
    return pane.terminal;
}

fn terminalFrame(model: *const Model, terminal_ref: TerminalRef) ?geometry.RectF {
    return projection.paneFrameFor(model, model.wsConst().surface_size, terminal_ref);
}

pub fn pointerButton(button: u32) MouseButton {
    return switch (button) {
        0 => .left,
        1 => .right,
        2 => .middle,
        3 => .button_4,
        4 => .button_5,
        else => .none,
    };
}

fn sendPointerToOwner(
    model: *Model,
    owner: ReplicaOwner,
    action: MouseAction,
    button: MouseButton,
    modifiers: ModifierMask,
    x: f64,
    y: f64,
) bool {
    if (comptime !phux_enabled) return false;
    if (!model.ownerIsCurrent(owner)) return false;
    const frame = terminalFrame(model, owner.terminal_ref) orelse return false;
    const presentation = model.remotePresentation(owner.terminal_ref) orelse return false;
    if (frame.width <= 0 or frame.height <= 0 or presentation.cols == 0 or presentation.rows == 0)
        return false;
    const remote = model.phuxForRef(owner.terminal_ref) orelse return false;
    if (!(remote.mouseTracking(owner) catch return false)) return false;

    const local_x = @max(0, @min(
        @as(f64, @floatCast(frame.width)) - 1,
        x - @as(f64, @floatCast(frame.x)),
    ));
    const local_y = @max(0, @min(
        @as(f64, @floatCast(frame.height)) - 1,
        y - @as(f64, @floatCast(frame.y)),
    ));
    const cell_x = @min(
        @as(u16, @intFromFloat(@floor(
            local_x * @as(f64, @floatFromInt(presentation.cols)) /
                @as(f64, @floatCast(frame.width)),
        ))),
        presentation.cols - 1,
    );
    const cell_y = @min(
        @as(u16, @intFromFloat(@floor(
            local_y * @as(f64, @floatFromInt(presentation.rows)) /
                @as(f64, @floatCast(frame.height)),
        ))),
        presentation.rows - 1,
    );
    remote.sendMouse(owner, &.{
        .action = action,
        .button = button,
        .modifiers = modifiers,
        .x = @floatFromInt(cell_x),
        .y = @floatFromInt(cell_y),
    }) catch return false;
    return true;
}

fn dispatchPointerEvent(model: *Model, event: anytype) void {
    if (comptime !phux_enabled) return;
    const pointer_state = model.pointer_state orelse return;
    pointer_state.last_x = event.x;
    pointer_state.last_y = event.y;
    const action: MouseAction = switch (event.eventKind() orelse return) {
        .button_down => .press,
        .button_up => .release,
        .motion => .move,
    };
    const button = pointerButton(event.button);

    var owner = if (pointer_state.capture) |capture|
        capture.owner
    else blk: {
        const terminal_ref = terminalRefAtPoint(
            model,
            @floatCast(event.x),
            @floatCast(event.y),
        ) orelse return;
        if (providerKind(terminal_ref) != .phux) return;
        break :blk model.terminalOwner(terminal_ref) orelse return;
    };
    if (!model.ownerIsCurrent(owner)) {
        pointer_state.capture = null;
        const terminal_ref = terminalRefAtPoint(
            model,
            @floatCast(event.x),
            @floatCast(event.y),
        ) orelse return;
        if (providerKind(terminal_ref) != .phux) return;
        owner = model.terminalOwner(terminal_ref) orelse return;
    }

    const sent = sendPointerToOwner(
        model,
        owner,
        action,
        button,
        @bitCast(event.modifiers),
        event.x,
        event.y,
    );
    if (action == .press and sent and button != .none) {
        pointer_state.capture = .{ .owner = owner, .button = button };
    } else if (action == .release) {
        pointer_state.capture = null;
    }
}

fn releasePointerCapture(model: *Model) void {
    if (comptime !phux_enabled) return;
    const pointer_state = model.pointer_state orelse return;
    const capture = pointer_state.capture orelse return;
    _ = sendPointerToOwner(
        model,
        capture.owner,
        .release,
        capture.button,
        .{},
        pointer_state.last_x,
        pointer_state.last_y,
    );
    pointer_state.capture = null;
}

pub fn drainPointerEvents(model: *Model) void {
    if (comptime !phux_enabled) return;
    const pointer_state = model.pointer_state orelse return;
    while (pointer_state.queue.take()) |event| dispatchPointerEvent(model, event);
    if (pointer_state.queue.takeOverflow()) releasePointerCapture(model);
}

fn pointerCaptureIndex(model: *const Model, window_id: native_sdk.platform.WindowId, pointer_id: u64) ?usize {
    for (model.pointer_captures, 0..) |capture, index| {
        if (capture.active and capture.window_id == window_id and capture.pointer_id == pointer_id) return index;
    }
    return null;
}

pub fn pointerCaptureFor(model: *const Model, window_id: native_sdk.platform.WindowId, pointer_id: u64) ?PointerCapture {
    const index = pointerCaptureIndex(model, window_id, pointer_id) orelse return null;
    return model.pointer_captures[index];
}

fn freePointerCaptureIndex(model: *const Model) ?usize {
    for (model.pointer_captures, 0..) |capture, index| if (!capture.active) return index;
    return null;
}

/// A terminal is visible when it holds a pane of the SELECTED tab. Panes of
/// other tabs, and every terminal while the web surface is up, are hidden.
fn terminalVisible(model: *const Model, id: TerminalRef) bool {
    const current = model.selectedTreeConst() orelse return false;
    return current.find(id) != null;
}

pub fn paneReportsMouse(pane: *const Pane) bool {
    return pane.acceptsInput() and pane.session.term.flags.mouse_event != .none;
}

fn validPointerGeometry(event: TerminalPointerEvent) bool {
    return std.math.isFinite(event.point.x) and std.math.isFinite(event.point.y) and
        std.math.isFinite(event.frame.x) and std.math.isFinite(event.frame.y) and
        std.math.isFinite(event.frame.width) and std.math.isFinite(event.frame.height) and
        event.frame.width > 0 and event.frame.height > 0;
}

/// Track target-preview hover and arm the Cmd underline on the pane under the
/// pointer, disarming every other one.
///
/// The chord is the SAME one that opens a link (Cmd, without Shift), which is
/// the whole point of the affordance: what underlines is what a click would
/// follow, so the gesture is discoverable by holding the key rather than by
/// already knowing it exists. Shift is excluded because it is the
/// override-the-TUI selection modifier — a Shift-drag is selecting text, not
/// aiming at a link.
///
/// Every OTHER pane is disarmed here because there is no pointer-exit event to
/// hang that on: a pointer that leaves a terminal produces no further events
/// for the terminal it left, so the pane holding an underline is cleared by
/// whichever pane the pointer arrives at next. A pointer that leaves the panes
/// entirely (chrome, another window) is covered by the press and blur paths.
fn updateHoverLink(model: *Model, pane: *Pane, event: TerminalPointerEvent) void {
    const armed = event.modifiers.super and !event.modifiers.shift;
    const local = geometry.PointF.init(event.point.x - event.frame.x, event.point.y - event.frame.y);
    for (0..model.provider.states.len) |index| {
        if (model.provider.states[index] != .active) continue;
        const other = model.provider.slot(index);
        if (other == pane) continue;
        _ = other.session.setHoverPoint(null);
        _ = other.session.setPointerPoint(null);
    }
    _ = pane.session.setPointerPoint(.{ .x = local.x, .y = local.y });
    _ = pane.session.setHoverPoint(if (armed) .{ .x = local.x, .y = local.y } else null);
}

/// Disarm every pane's hover underline. For the paths where the pointer stops
/// being over a link without another pane hearing about it: a press that
/// consumed the hover, and window blur.
pub fn clearHoverLinks(model: *Model) void {
    for (0..model.provider.states.len) |index| {
        if (model.provider.states[index] != .active) continue;
        const session = model.provider.slot(index).session;
        _ = session.setHoverPoint(null);
        _ = session.setPointerPoint(null);
    }
}

pub fn handleTerminalPointer(model: *Model, fx: anytype, event: TerminalPointerEvent) void {
    switch (event.phase) {
        .down => {
            // A second down for the same physical pointer terminates its old
            // lease first. Other pointers remain completely independent.
            if (pointerCaptureIndex(model, event.window_id, event.pointer_id)) |index| endPointerCapture(model, fx, index, true);
            const pane = model.provider.terminal(localRef(event.terminal_id)) orelse return;
            if (pane.session_generation != event.generation or !terminalVisible(model, pane.id)) return;
            if (!validPointerGeometry(event)) return;
            syncMouseProtocol(pane);
            // A press moves pane focus inside the selected tab. There is no
            // second selection to update: the tree's focus IS the selection.
            if (event.button == 0 or event.button == 1) {
                if (model.selectedTree()) |current| _ = current.focusTerminal(pane.id);
            }

            const local = geometry.PointF.init(event.point.x - event.frame.x, event.point.y - event.frame.y);
            // Cmd+click opens a link, the chord Ghostty uses and the one that
            // cannot be confused with a selection drag.
            //
            // It sits ABOVE both the selection and the mouse-report arms on
            // purpose: a TUI that has asked for mouse reporting still prints
            // URLs, and a chord that works in one pane and silently does
            // nothing in the next is worse than not having it. It consumes the
            // press entirely — no capture is taken, so there is no drag to
            // leave dangling and no selection to half-start.
            //
            // Only when a URL is actually under the pointer; a cmd+click on
            // ordinary text falls through to the normal gesture rather than
            // being swallowed.
            if (event.button == 0 and event.modifiers.super and !event.modifiers.shift) {
                if (pane.session.urlAtPoint(local.x, local.y)) |link| {
                    if (model.recordOpenedUrl(link)) fx.openUrl(model.openedUrl());
                    // The link is being followed, so the affordance that
                    // advertised it has done its job. Leaving it armed would
                    // underline a link under a pointer that is no longer
                    // hovering it — the press is the end of the hover.
                    clearHoverLinks(model);
                    return;
                }
            }
            clearHoverLinks(model);

            const slot = freePointerCaptureIndex(model) orelse return;
            // An ended child cannot own pointer input. Its last mode flags may
            // still say mouse reporting, but the static viewport must revert to
            // ordinary terminal selection without requiring Shift.
            const mouse_reporting = paneReportsMouse(pane);
            const local_selection = event.button == 0 and (!mouse_reporting or event.modifiers.shift);
            if (local_selection) {
                pane.selecting = false;
                _ = pane.session.pointerSelection(.{
                    .phase = .down,
                    .x = local.x,
                    .y = local.y,
                    .width = event.frame.width,
                    .height = event.frame.height,
                    .click_count = event.click_count,
                });
                model.pointer_captures[slot] = .{
                    .active = true,
                    .window_id = event.window_id,
                    .terminal_id = event.terminal_id,
                    .generation = pane.session_generation,
                    .pointer_id = event.pointer_id,
                    .button = event.button,
                    .mode = .local_selection,
                    .frame = event.frame,
                    .last_point = event.point,
                    .modifiers = event.modifiers,
                };
                return;
            }

            const button = terminalMouseButton(event.button) orelse return;
            if (!mouse_reporting or !pane.acceptsInput()) return;
            pane.selecting = false;
            // A secondary report belongs to the TUI but does not revoke a
            // Shift-created local selection; Cmd+C remains available while
            // mouse reporting owns the native context-click gesture.
            if (event.button != 1) pane.session.clearSelection();
            model.pointer_captures[slot] = .{
                .active = true,
                .window_id = event.window_id,
                .terminal_id = event.terminal_id,
                .generation = pane.session_generation,
                .pointer_id = event.pointer_id,
                .button = event.button,
                .mode = .mouse_report,
                .mouse_protocol_fingerprint = pane.mouse_protocol_fingerprint,
                .frame = event.frame,
                .last_point = event.point,
                .modifiers = event.modifiers,
            };
            if (!encodeMouseReport(model, pane, fx, .press, button, local, event.frame, event.modifiers)) {
                model.pointer_captures[slot].active = false;
            }
        },
        .move => {
            const capture_index = pointerCaptureIndex(model, event.window_id, event.pointer_id);
            if (capture_index) |index| {
                const capture = model.pointer_captures[index];
                const pane = model.provider.terminal(localRef(capture.terminal_id)) orelse {
                    endPointerCapture(model, fx, index, true);
                    return;
                };
                if (pane.session_generation != capture.generation or !terminalVisible(model, pane.id) or
                    (capture.mode == .mouse_report and
                        (!pane.acceptsInput() or !mouseCaptureProtocolMatches(pane, capture))))
                {
                    endPointerCapture(model, fx, index, true);
                    return;
                }
                if (!validPointerGeometry(event)) return;
                model.pointer_captures[index].frame = event.frame;
                model.pointer_captures[index].last_point = event.point;
                model.pointer_captures[index].modifiers = event.modifiers;
                const local = geometry.PointF.init(event.point.x - event.frame.x, event.point.y - event.frame.y);
                switch (capture.mode) {
                    .local_selection => _ = pane.session.pointerSelection(.{
                        .phase = .move,
                        .x = local.x,
                        .y = local.y,
                        .width = event.frame.width,
                        .height = event.frame.height,
                        .click_count = event.click_count,
                    }),
                    .mouse_report => _ = encodeMouseReport(
                        model,
                        pane,
                        fx,
                        .motion,
                        terminalMouseButton(capture.button),
                        local,
                        event.frame,
                        event.modifiers,
                    ),
                }
                return;
            }
            const pane = model.provider.terminal(localRef(event.terminal_id)) orelse return;
            if (pane.session_generation != event.generation or !terminalVisible(model, pane.id) or
                !validPointerGeometry(event)) return;
            // Before the `acceptsInput` gate: an ended session still shows the
            // URLs its child printed, and Cmd+click still opens them, so the
            // affordance that advertises the gesture has to survive the child.
            updateHoverLink(model, pane, event);
            if (!pane.acceptsInput()) return;
            syncMouseProtocol(pane);
            if (pane.session.term.flags.mouse_event == .none or event.modifiers.shift) return;
            const local = geometry.PointF.init(event.point.x - event.frame.x, event.point.y - event.frame.y);
            _ = encodeMouseReport(model, pane, fx, .motion, null, local, event.frame, event.modifiers);
        },
        .up, .cancel => {
            const index = pointerCaptureIndex(model, event.window_id, event.pointer_id) orelse return;
            if (validPointerGeometry(event)) {
                model.pointer_captures[index].frame = event.frame;
                model.pointer_captures[index].last_point = event.point;
                model.pointer_captures[index].modifiers = event.modifiers;
            }
            endPointerCapture(model, fx, index, event.phase == .cancel);
        },
        .wheel => {
            const pane = model.provider.terminal(localRef(event.terminal_id)) orelse return;
            if (pane.session_generation != event.generation or !terminalVisible(model, pane.id) or !validPointerGeometry(event)) return;
            const finite_x = std.math.isFinite(event.delta.dx);
            const finite_y = std.math.isFinite(event.delta.dy);
            if ((!finite_x or event.delta.dx == 0) and (!finite_y or event.delta.dy == 0)) return;
            syncMouseProtocol(pane);
            // A wheel over a pane nothing has painted yet has no row height to
            // quantize against. Dropping the event is right: the pane is not
            // on screen for the user to have aimed at.
            const cell = pane.session.measuredCell() orelse return;
            const local = geometry.PointF.init(event.point.x - event.frame.x, event.point.y - event.frame.y);
            if (pane.acceptsInput() and pane.session.term.flags.mouse_event != .none and !event.modifiers.shift) {
                pane.selecting = false;
                pane.session.clearSelection();
                accumulateWheel(&pane.mouse_wheel_y_accum, if (finite_y) event.delta.dy else 0, cell.height);
                accumulateWheel(&pane.mouse_wheel_x_accum, if (finite_x) event.delta.dx else 0, cell.width);
                flushMouseWheels(model, pane, fx, cell, local, event.frame, event.modifiers);
                return;
            }
            if (pane.selecting or !finite_y) return;
            const cell_h = finiteQuantum(cell.height);
            accumulateWheel(&pane.scrollback_wheel_accum, event.delta.dy, cell_h);
            const report_bound: f32 = @floatFromInt(max_mouse_reports_per_event);
            const rows = std.math.clamp(@trunc(pane.scrollback_wheel_accum / cell_h), -report_bound, report_bound);
            if (rows != 0) {
                pane.scrollback_wheel_accum -= rows * cell_h;
                pane.session.scrollLines(-@as(isize, @intFromFloat(rows)));
            }
        },
        .hover => {
            const pane = model.provider.terminal(localRef(event.terminal_id)) orelse return;
            if (pane.session_generation != event.generation or !terminalVisible(model, pane.id) or
                !validPointerGeometry(event)) return;
            updateHoverLink(model, pane, event);
            if (!pane.acceptsInput()) return;
            syncMouseProtocol(pane);
            if (pane.session.term.flags.mouse_event != .any or event.modifiers.shift) return;
            const local = geometry.PointF.init(event.point.x - event.frame.x, event.point.y - event.frame.y);
            _ = encodeMouseReport(model, pane, fx, .motion, null, local, event.frame, event.modifiers);
        },
    }
}

fn endPointerCapture(model: *Model, fx: anytype, index: usize, cancelled: bool) void {
    if (index >= model.pointer_captures.len or !model.pointer_captures[index].active) return;
    const capture = model.pointer_captures[index];
    model.pointer_captures[index].active = false;
    const pane = model.provider.terminal(localRef(capture.terminal_id)) orelse return;
    if (pane.session_generation != capture.generation) return;
    const local = geometry.PointF.init(capture.last_point.x - capture.frame.x, capture.last_point.y - capture.frame.y);
    switch (capture.mode) {
        .local_selection => _ = pane.session.pointerSelection(.{
            .phase = if (cancelled) .cancel else .up,
            .x = local.x,
            .y = local.y,
            .width = capture.frame.width,
            .height = capture.frame.height,
        }),
        .mouse_report => if (pane.acceptsInput() and mouseCaptureProtocolMatches(pane, capture)) {
            _ = encodeMouseReport(model, pane, fx, .release, terminalMouseButton(capture.button), local, capture.frame, capture.modifiers);
        },
    }
}

pub fn endCapturesForTerminal(model: *Model, fx: anytype, id: TerminalRef) void {
    const local = provider_contract.localId(id) orelse return;
    for (0..model.pointer_captures.len) |index| {
        if (model.pointer_captures[index].active and model.pointer_captures[index].terminal_id == local) {
            endPointerCapture(model, fx, index, true);
        }
    }
}

pub fn endMismatchedMouseCaptures(model: *Model, fx: anytype, pane: *Pane) void {
    for (0..model.pointer_captures.len) |index| {
        const capture = model.pointer_captures[index];
        if (capture.active and capture.mode == .mouse_report and
            localRef(capture.terminal_id).eql(pane.id) and capture.generation == pane.session_generation and
            capture.mouse_protocol_fingerprint != pane.mouse_protocol_fingerprint)
        {
            endPointerCapture(model, fx, index, true);
        }
    }
}

pub fn endAllCaptures(model: *Model, fx: anytype) void {
    for (0..model.pointer_captures.len) |index| endPointerCapture(model, fx, index, true);
}

pub fn endHiddenCaptures(model: *Model, fx: anytype) void {
    for (0..model.pointer_captures.len) |index| {
        if (model.pointer_captures[index].active and !terminalVisible(model, localRef(model.pointer_captures[index].terminal_id))) {
            endPointerCapture(model, fx, index, true);
        }
    }
}

pub fn modelHasSelectionAutoscroll(model: *const Model) bool {
    for (model.pointer_captures) |capture| {
        if (!capture.active or capture.mode != .local_selection) continue;
        const pane = model.provider.terminalConst(localRef(capture.terminal_id)) orelse continue;
        if (pane.session_generation == capture.generation and
            terminalVisible(model, pane.id) and pane.session.pointerAutoscrollActive()) return true;
    }
    return false;
}

pub fn handleSelectionAutoscroll(model: *Model, fx: anytype) void {
    for (0..model.pointer_captures.len) |index| {
        const capture = model.pointer_captures[index];
        if (!capture.active or capture.mode != .local_selection) continue;
        const pane = model.provider.terminal(localRef(capture.terminal_id)) orelse {
            endPointerCapture(model, fx, index, true);
            continue;
        };
        if (pane.session_generation != capture.generation or !terminalVisible(model, pane.id)) {
            endPointerCapture(model, fx, index, true);
            continue;
        }
        _ = pane.session.pointerAutoscroll(.{
            .x = capture.last_point.x - capture.frame.x,
            .y = capture.last_point.y - capture.frame.y,
            .width = capture.frame.width,
            .height = capture.frame.height,
        });
    }
}

fn terminalMouseButton(button: i32) ?vt.input.MouseButton {
    return switch (button) {
        0 => .left,
        1 => .right,
        2 => .middle,
        3 => .four,
        4 => .five,
        5 => .six,
        6 => .seven,
        7 => .eight,
        8 => .nine,
        else => null,
    };
}

fn encodeMouseReport(
    model: *const Model,
    pane: *Pane,
    fx: anytype,
    action: vt.input.MouseAction,
    button: ?vt.input.MouseButton,
    local: geometry.PointF,
    frame: geometry.RectF,
    modifiers: PointerModifiers,
) bool {
    const session = pane.session;
    syncMouseProtocol(pane);
    if (session.term.flags.mouse_event == .none or !validScale(model.wsConst().surface_scale_factor) or
        !std.math.isFinite(local.x) or !std.math.isFinite(local.y) or
        !std.math.isFinite(frame.width) or !std.math.isFinite(frame.height) or
        frame.width <= 0 or frame.height <= 0) return false;

    const pixel_protocol = session.term.flags.mouse_format == .sgr_pixels;
    const scaled_width = frame.width * model.wsConst().surface_scale_factor;
    const scaled_height = frame.height * model.wsConst().surface_scale_factor;
    const scaled_x = local.x * model.wsConst().surface_scale_factor;
    const scaled_y = local.y * model.wsConst().surface_scale_factor;
    if (pixel_protocol and (!std.math.isFinite(scaled_width) or !std.math.isFinite(scaled_height) or
        !std.math.isFinite(scaled_x) or !std.math.isFinite(scaled_y))) return false;
    const screen_width: u32 = if (pixel_protocol)
        boundedMouseDimension(scaled_width)
    else
        session.cols();
    const screen_height: u32 = if (pixel_protocol)
        boundedMouseDimension(scaled_height)
    else
        session.rows();
    // Only the CELL protocol needs a cell box; `sgr_pixels` reports device
    // pixels and must keep working on a pane that has not been measured, so
    // the measurement is required inside the branch that divides by it and
    // nowhere else.
    const x = if (pixel_protocol)
        boundedMouseCoordinate(scaled_x)
    else
        boundedMouseCoordinate(local.x / (session.measuredCell() orelse return false).width);
    const y = if (pixel_protocol)
        boundedMouseCoordinate(scaled_y)
    else
        boundedMouseCoordinate(local.y / (session.measuredCell() orelse return false).height);

    var options: vt.input.MouseEncodeOptions = .{
        .event = session.term.flags.mouse_event,
        .format = session.term.flags.mouse_format,
        .size = .{
            .screen = .{ .width = screen_width, .height = screen_height },
            .cell = .{ .width = 1, .height = 1 },
            .padding = .{},
        },
        .any_button_pressed = paneHasReportingCapture(model, pane.id, pane.session_generation),
        .last_cell = if (pixel_protocol) null else &pane.mouse_last_cell,
    };
    if (action == .release) options.any_button_pressed = false;

    var bytes: [128]u8 = undefined;
    var writer: std.Io.Writer = .fixed(&bytes);
    vt.input.encodeMouse(&writer, .{
        .action = action,
        .button = button,
        .mods = .{
            .shift = modifiers.shift,
            .ctrl = modifiers.control,
            .alt = modifiers.alt,
            .super = modifiers.super,
        },
        .pos = .{ .x = x, .y = y },
    }, options) catch return false;
    const encoded = writer.buffered();
    if (encoded.len == 0) return false;
    enqueueTransient(pane, fx, encoded);
    return true;
}

fn paneHasReportingCapture(model: *const Model, id: TerminalRef, generation: u64) bool {
    const local = provider_contract.localId(id) orelse return false;
    for (model.pointer_captures) |capture| {
        if (capture.active and capture.mode == .mouse_report and capture.terminal_id == local and capture.generation == generation) return true;
    }
    return false;
}

fn mouseCaptureProtocolMatches(pane: *Pane, capture: PointerCapture) bool {
    syncMouseProtocol(pane);
    return capture.mouse_protocol_fingerprint != 0 and
        capture.mouse_protocol_fingerprint == pane.mouse_protocol_fingerprint and
        pane.session.term.flags.mouse_event != .none;
}

pub fn validScale(scale: f32) bool {
    return std.math.isFinite(scale) and scale > 0;
}

fn finiteQuantum(value: f32) f32 {
    return if (std.math.isFinite(value) and value >= 1)
        @min(value, max_mouse_coordinate)
    else
        1;
}

fn boundedMouseCoordinate(value: f32) f32 {
    if (!std.math.isFinite(value)) return 0;
    return std.math.clamp(value, -max_mouse_coordinate, max_mouse_coordinate);
}

fn boundedMouseDimension(value: f32) u32 {
    return @intFromFloat(@max(1, @ceil(std.math.clamp(value, 1, max_mouse_coordinate))));
}

fn accumulateWheel(accum: *f32, delta: f32, quantum_value: f32) void {
    if (!std.math.isFinite(accum.*)) accum.* = 0;
    if (!std.math.isFinite(delta) or delta == 0) return;
    const quantum = finiteQuantum(quantum_value);
    const bound = quantum * @as(f32, max_mouse_reports_per_event);
    accum.* = std.math.clamp(accum.* + std.math.clamp(delta, -bound, bound), -bound, bound);
}

fn flushMouseWheelOnce(
    model: *const Model,
    pane: *Pane,
    fx: anytype,
    accum: *f32,
    positive: vt.input.MouseButton,
    negative: vt.input.MouseButton,
    quantum_value: f32,
    local: geometry.PointF,
    frame: geometry.RectF,
    modifiers: PointerModifiers,
) bool {
    const quantum = finiteQuantum(quantum_value);
    if (@abs(accum.*) < quantum) return false;
    const button = if (accum.* > 0) positive else negative;
    _ = encodeMouseReport(model, pane, fx, .press, button, local, frame, modifiers);
    accum.* += if (accum.* > 0) -quantum else quantum;
    return true;
}

/// `cell` is passed in rather than re-read from the session so the whole flush
/// quantizes against the SAME box the accumulation used. Re-reading would be
/// harmless today and wrong the first time a repaint lands mid-flush.
fn flushMouseWheels(
    model: *const Model,
    pane: *Pane,
    fx: anytype,
    cell: grid.CellBox,
    local: geometry.PointF,
    frame: geometry.RectF,
    modifiers: PointerModifiers,
) void {
    var budget = max_mouse_reports_per_event;
    while (budget > 0) : (budget -= 1) {
        const horizontal_first = pane.mouse_wheel_next_horizontal;
        const first = if (horizontal_first)
            flushMouseWheelOnce(model, pane, fx, &pane.mouse_wheel_x_accum, .six, .seven, cell.width, local, frame, modifiers)
        else
            flushMouseWheelOnce(model, pane, fx, &pane.mouse_wheel_y_accum, .four, .five, cell.height, local, frame, modifiers);
        const second = if (first)
            false
        else if (horizontal_first)
            flushMouseWheelOnce(model, pane, fx, &pane.mouse_wheel_y_accum, .four, .five, cell.height, local, frame, modifiers)
        else
            flushMouseWheelOnce(model, pane, fx, &pane.mouse_wheel_x_accum, .six, .seven, cell.width, local, frame, modifiers);
        if (!first and !second) return;
        pane.mouse_wheel_next_horizontal = !horizontal_first;
    }
}

fn mouseProtocolFingerprint(pane: *const Pane) u8 {
    return 1 + (@as(u8, @intCast(@intFromEnum(pane.session.term.flags.mouse_event))) << 4) +
        @as(u8, @intCast(@intFromEnum(pane.session.term.flags.mouse_format)));
}

pub fn syncMouseProtocol(pane: *Pane) void {
    const fingerprint = mouseProtocolFingerprint(pane);
    if (pane.mouse_protocol_fingerprint == fingerprint) return;
    pane.mouse_protocol_fingerprint = fingerprint;
    pane.mouse_last_cell = null;
    pane.mouse_wheel_y_accum = 0;
    pane.mouse_wheel_x_accum = 0;
    pane.mouse_wheel_next_horizontal = false;
}
