const std = @import("std");
const native_sdk = @import("native_sdk");
const support = @import("../phux_support.zig");
const provider_contract = @import("provider_contract");
const model_module = @import("../model.zig");
const layout = @import("../layout.zig");
const app_types = @import("../app_types.zig");
const pointer_input = @import("../pointer_input.zig");
const update_module = @import("../update.zig");
const scene_module = @import("scene.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const Model = model_module.Model;
const Msg = app_types.Msg;
const TerminalApp = app_types.TerminalApp;
const LocalResourceId = support.LocalResourceId;
const TerminalRef = support.TerminalRef;
const canvas_label = scene_module.canvas_label;
const webview_label = scene_module.webview_label;
const localRef = support.localRef;
const selection_autoscroll_timer_id = app_types.selection_autoscroll_timer_id;
const selection_autoscroll_interval_ns: u64 = 15 * std.time.ns_per_ms;
const appShortcutKeyMask = update_module.appShortcutKeyMask;
const commandShortcutKeyMask = update_module.commandShortcutKeyMask;
const pointerCaptureFor = pointer_input.pointerCaptureFor;
const modelHasSelectionAutoscroll = pointer_input.modelHasSelectionAutoscroll;
const webview_layer: i32 = 20;

fn registerDeferredFonts(runtime: *native_sdk.Runtime, model: *Model) !void {
    const fonts = scene_module.cockpit_fonts[1..];
    while (@as(usize, model.deferred_font_count) < fonts.len) {
        const index: usize = model.deferred_font_count;
        const registration = fonts[index];
        try runtime.registerCanvasFont(registration.id, registration.ttf);
        model.deferred_font_count += 1;
    }
}

fn materializeWebView(runtime: *native_sdk.Runtime, model: *Model, window_id: native_sdk.platform.WindowId) !void {
    if (model.webview_materialized) return;
    _ = try runtime.createView(.{
        .window_id = window_id,
        .label = webview_label,
        .kind = .webview,
        .parent = canvas_label,
        .frame = geometry.RectF.init(0, 0, scene_module.webkit_parking_extent, scene_module.webkit_parking_extent),
        .layer = webview_layer,
        .url = model.browser_page.url(),
    });
    model.webview_materialized = true;
}

/// Install work that is necessary after launch but not before useful canvas
/// pixels. The regular face already rendered the first frame; late registration
/// replaces transient bold/italic synthesis on the next rebuild. WebKit keeps
/// the same parent, URL, and parking geometry it had as a scene child without
/// putting its process startup in front of the first synchronous canvas flush.
pub fn installPostPresentResources(runtime: *native_sdk.Runtime, model: *Model, window_id: native_sdk.platform.WindowId) !void {
    try registerDeferredFonts(runtime, model);
    try materializeWebView(runtime, model, window_id);
}

/// UiApp owns the deterministic model/view/effects loop. This host adds the
/// one imperative operation native surface switching requires: moving the OS
/// first responder after a global tab shortcut. Without it, a parked WebKit
/// view can keep consuming text after the model has returned to a terminal.
pub const CockpitHost = struct {
    inner: TerminalApp = undefined,
    inner_app: native_sdk.App = undefined,
    /// Global AppKit shortcuts and canvas key delivery can describe the same
    /// physical edge. Suppress only the duplicate canvas edge; the model's
    /// latch remains reserved for shortcuts originating on the canvas itself.
    suppressed_canvas_shortcuts: u64 = 0,
    /// Platform shortcut callbacks have no key phase. Hold the physical key
    /// until a canvas release or a different shortcut edge so repeated
    /// callbacks for one edge execute the command exactly once.
    global_shortcut_keys_held: u64 = 0,
    selection_autoscroll_timer_active: bool = false,

    pub fn init(self: *CockpitHost, allocator: std.mem.Allocator, model: Model, options: TerminalApp.Options) void {
        var host_options = options;
        // Production wheel input is routed by the retained terminal overlay
        // below; disable the app-global geometry fallback to avoid a duplicate.
        host_options.on_wheel = null;
        self.inner = TerminalApp.init(allocator, model, host_options);
        self.inner_app = self.inner.app();
        self.suppressed_canvas_shortcuts = 0;
        self.global_shortcut_keys_held = 0;
        self.selection_autoscroll_timer_active = false;
    }

    pub fn deinit(self: *CockpitHost) void {
        self.inner.deinit();
    }

    pub fn app(self: *CockpitHost) native_sdk.App {
        return .{
            .context = self,
            .name = self.inner_app.name,
            .source = self.inner_app.source,
            .source_fn = if (self.inner_app.source_fn != null) source else null,
            .scene_fn = if (self.inner_app.scene_fn != null) scene else null,
            .start_fn = if (self.inner_app.start_fn != null) start else null,
            .event_fn = if (self.inner_app.event_fn != null) event else null,
            .stop_fn = if (self.inner_app.stop_fn != null) stop else null,
            .replay_fn = if (self.inner_app.replay_fn != null) replay else null,
        };
    }

    fn source(context: *anyopaque) anyerror!native_sdk.platform.WebViewSource {
        const self: *CockpitHost = @ptrCast(@alignCast(context));
        return self.inner_app.webViewSource();
    }

    fn scene(context: *anyopaque) anyerror!native_sdk.ShellConfig {
        const self: *CockpitHost = @ptrCast(@alignCast(context));
        return (try self.inner_app.scene()) orelse return error.SceneUnavailable;
    }

    fn start(context: *anyopaque, runtime: *native_sdk.Runtime) anyerror!void {
        const self: *CockpitHost = @ptrCast(@alignCast(context));
        try self.inner_app.start(runtime);
    }

    /// Point the model at the window an event came from, BEFORE that event is
    /// handled.
    ///
    /// Every message that reshapes a workspace acts on `active_window`, so a
    /// cmd+T typed in the second window has to find the second window already
    /// selected. Canvas-carrying events name their window by LABEL; a shortcut
    /// carries only a platform window id, which each workspace learns from its
    /// own frame events (`Workspace.window_id`).
    ///
    /// Deliberately INPUT only, and deliberately the ROUTED half of it.
    ///
    /// A presented frame is not a focus signal — every open window presents
    /// continuously, so adopting frames made the active window flap between
    /// them dozens of times a second.
    ///
    /// A key arrives TWICE: once as `canvas_widget_keyboard` (routed, first,
    /// and the delivery the model's `on_key` actually acts on) and once as the
    /// raw `gpu_surface_input` echo. Adopting the echo as well undoes a focus
    /// change the routed event's own message just made — cmd+N opens a window
    /// and makes it active, and then the echo of that same keystroke, still
    /// labelled with the window the chord was typed in, handed the next message
    /// straight back to it. That was observed in the running app: a cmd+D
    /// addressed to the second window's canvas split the FIRST window. So key
    /// kinds are taken from the routed event only, and the raw channel is left
    /// to the pointer kinds it is the sole carrier of.
    fn adoptEventWindow(self: *CockpitHost, runtime: *native_sdk.Runtime, event_value: native_sdk.Event) !void {
        const index: ?usize = switch (event_value) {
            .gpu_surface_input => |input| switch (input.kind) {
                .key_down, .key_up, .text_input => null,
                else => scene_module.windowIndexForCanvas(input.label),
            },
            .canvas_widget_keyboard => |routed| scene_module.windowIndexForCanvas(routed.view_label),
            .canvas_widget_pointer => |routed| scene_module.windowIndexForCanvas(routed.view_label),
            .shortcut => |shortcut| self.windowIndexForId(shortcut.window_id),
            .command => |command| self.windowIndexForId(command.window_id),
            else => null,
        };
        const resolved = index orelse return;
        if (resolved == self.inner.model.active_window) return;
        if (!self.inner.model.windowOpen(resolved)) return;
        try self.inner.dispatch(runtime, self.windowIdFor(resolved), .{ .focus_window = @intCast(resolved) });
    }

    /// The window index behind a platform window id, or null when no
    /// workspace has been rendered into it yet.
    fn windowIndexForId(self: *const CockpitHost, window_id: native_sdk.platform.WindowId) ?usize {
        for (0..model_module.max_windows) |index| {
            const workspace = self.inner.model.wsAtConst(index) orelse continue;
            if (workspace.window_id == window_id) return index;
        }
        return null;
    }

    fn windowIdFor(self: *const CockpitHost, index: usize) native_sdk.platform.WindowId {
        const workspace = self.inner.model.wsAtConst(index) orelse return 1;
        return if (workspace.window_id == 0) 1 else workspace.window_id;
    }

    fn logPostPresentFailure(resource: []const u8, err: anyerror) void {
        std.log.warn("post-present {s} initialization failed: {s}", .{ resource, @errorName(err) });
    }

    fn attemptPostPresentResources(self: *CockpitHost, runtime: *native_sdk.Runtime, window_id: native_sdk.platform.WindowId) void {
        registerDeferredFonts(runtime, &self.inner.model) catch |err| logPostPresentFailure("font", err);
        materializeWebView(runtime, &self.inner.model, window_id) catch |err| logPostPresentFailure("WebView", err);
    }

    fn installResourcesAfterFirstPresent(self: *CockpitHost, runtime: *native_sdk.Runtime, event_value: native_sdk.Event) void {
        const frame = switch (event_value) {
            .gpu_surface_frame => |value| value,
            else => return,
        };
        if (!frame.nonblank or !std.mem.eql(u8, frame.label, canvas_label)) return;
        if (self.inner.model.post_present_resources_attempted) return;
        self.inner.model.post_present_resources_attempted = true;
        self.attemptPostPresentResources(runtime, frame.window_id);
        if (self.inner.model.webview_materialized and self.inner.model.selectedTerminalRef() == null) {
            runtime.focusView(frame.window_id, webview_label) catch |err| logPostPresentFailure("WebView focus", err);
        }
    }

    fn resourceRetryEvent(event_value: native_sdk.Event) bool {
        return switch (event_value) {
            .command, .shortcut, .canvas_widget_keyboard, .canvas_widget_pointer => true,
            else => false,
        };
    }

    fn recoverResourcesOnDemand(self: *CockpitHost, runtime: *native_sdk.Runtime, event_value: native_sdk.Event) void {
        if (!self.inner.model.post_present_resources_attempted) return;
        if (!resourceRetryEvent(event_value)) return;
        if (self.inner.model.selectedTerminalRef() != null) return;
        if (self.inner.model.webview_materialized and
            @as(usize, self.inner.model.deferred_font_count) == scene_module.cockpit_fonts.len - 1) return;
        self.attemptPostPresentResources(runtime, 1);
        if (self.inner.model.webview_materialized) {
            runtime.focusView(1, webview_label) catch |err| logPostPresentFailure("WebView focus", err);
        }
    }

    fn installingMainCanvas(self: *const CockpitHost, event_value: native_sdk.Event) bool {
        if (self.inner.installed) return false;
        return switch (event_value) {
            .gpu_surface_frame => |frame| std.mem.eql(u8, frame.label, canvas_label),
            else => false,
        };
    }

    fn event(context: *anyopaque, runtime: *native_sdk.Runtime, event_value: native_sdk.Event) anyerror!void {
        const self: *CockpitHost = @ptrCast(@alignCast(context));
        defer self.syncSelectionAutoscrollTimer(runtime) catch {};
        self.installResourcesAfterFirstPresent(runtime, event_value);
        try self.adoptEventWindow(runtime, event_value);
        switch (event_value) {
            .command => |command| {
                if (command.source == .shortcut) {
                    const mask = commandShortcutKeyMask(command.name);
                    if (mask != 0 and
                        ((self.inner.model.consumed_shortcut_keys_held & mask) != 0 or
                            (self.global_shortcut_keys_held & mask) != 0)) return;
                    // Preserve a still-held older shortcut until its canvas
                    // release arrives. A new global edge for this key clears
                    // stale duplicate suppression left by WebKit focus.
                    self.suppressed_canvas_shortcuts &= ~mask;
                    self.suppressed_canvas_shortcuts |= self.global_shortcut_keys_held;
                    self.global_shortcut_keys_held = mask;
                }
            },
            .gpu_surface_input => |input| {
                const mask = appShortcutKeyMask(input.key);
                const global_owned = mask != 0 and (self.global_shortcut_keys_held & mask) != 0;
                if (input.kind == .key_up) self.global_shortcut_keys_held &= ~mask;
                if (global_owned) {
                    if (input.kind == .key_up) self.suppressed_canvas_shortcuts &= ~mask;
                    return;
                }
                if (mask != 0 and (self.suppressed_canvas_shortcuts & mask) != 0) {
                    if (input.kind == .key_up) self.suppressed_canvas_shortcuts &= ~mask;
                    return;
                }
            },
            .canvas_widget_pointer => |pointer_event| {
                if (try self.routeTerminalPointer(runtime, pointer_event)) return;
            },
            .shortcut => |shortcut| {
                const mask = appShortcutKeyMask(shortcut.key);
                if (mask != 0 and (self.inner.model.consumed_shortcut_keys_held & mask) != 0) {
                    // Canvas handled key-down first. Do not execute the global
                    // copy, but own its eventual canvas release after focus.
                    if (self.inner.model.selectedTerminalRef() == null) {
                        self.inner.model.consumed_shortcut_keys_held &= ~mask;
                    } else {
                        self.suppressed_canvas_shortcuts |= mask;
                    }
                    try self.focusSelectedContent(runtime, shortcut.window_id);
                    return;
                }
                if (mask != 0 and (self.global_shortcut_keys_held & mask) != 0) {
                    try self.focusSelectedContent(runtime, shortcut.window_id);
                    return;
                }
            },
            .lifecycle => |lifecycle| if (lifecycle == .deactivate) {
                self.suppressed_canvas_shortcuts = 0;
                self.global_shortcut_keys_held = 0;
            },
            else => {},
        }
        const selected_before = self.inner.model.selectedSurface();
        // UiApp's first install publishes AppKit accessibility synchronously
        // after emitting the display list and before planning/presenting it.
        // Reuse the SDK's documented "present follows immediately" batch seam:
        // the display list still builds now, while only the platform a11y
        // publication rides RuntimeGpuSurfaceEvents' post-present flush.
        var startup_batch_active = self.installingMainCanvas(event_value);
        if (startup_batch_active) runtime.beginCanvasWidgetDisplayListRefreshBatch();
        defer if (startup_batch_active) runtime.cancelCanvasWidgetDisplayListRefreshBatch();
        try self.inner_app.event(runtime, event_value);
        if (startup_batch_active) {
            try runtime.endCanvasWidgetDisplayListRefreshBatch();
            startup_batch_active = false;
        }
        self.recoverResourcesOnDemand(runtime, event_value);
        if (!selected_before.eql(self.inner.model.selectedSurface())) {
            const window_id = switch (event_value) {
                .shortcut => |shortcut| shortcut.window_id,
                .gpu_surface_input => |input| input.window_id,
                else => 1,
            };
            try self.focusSelectedContent(runtime, window_id);
            return;
        }
        switch (event_value) {
            .shortcut => |shortcut| {
                if (!std.mem.startsWith(u8, shortcut.id, "tab.") and
                    !std.mem.startsWith(u8, shortcut.id, "layout.") and
                    !std.mem.startsWith(u8, shortcut.id, "pane.")) return;
                try self.focusSelectedContent(runtime, shortcut.window_id);
                if (self.inner.model.selectedTerminalRef() != null) {
                    self.suppressed_canvas_shortcuts |= appShortcutKeyMask(shortcut.key);
                }
            },
            .gpu_surface_input => |input| {
                if (input.kind != .pointer_up) return;
                if (scene_module.windowIndexForCanvas(input.label) == null) return;
                if (canvasActionControlFocused(runtime, input.window_id, input.label)) {
                    try self.focusSelectedContent(runtime, input.window_id);
                }
            },
            else => {},
        }
    }

    /// Move the OS first responder onto whatever the window's workspace has
    /// selected. Only the MAIN window hosts the webview, so a secondary
    /// window's answer is always its own canvas.
    fn focusSelectedContent(self: *CockpitHost, runtime: *native_sdk.Runtime, window_id: native_sdk.platform.WindowId) !void {
        const index = self.windowIndexForId(window_id) orelse self.inner.model.active_window;
        const surface = scene_module.canvasLabelFor(index);
        const on_web = index == 0 and self.inner.model.selectedTerminalRef() == null;
        if (on_web and !self.inner.model.webview_materialized) {
            // An early Web selection must not reintroduce launch work or try to
            // focus a view that does not exist. Keep the canvas first responder;
            // the first successful post-present install transfers focus when
            // the model still selects Web. Explicit later input is handled by
            // recoverResourcesOnDemand above.
            try runtime.focusView(window_id, surface);
            return;
        }
        try runtime.focusView(window_id, if (on_web) webview_label else surface);
        if (!on_web) clearCanvasWidgetFocus(runtime, window_id, surface);
    }

    fn canvasActionControlFocused(runtime: *native_sdk.Runtime, window_id: native_sdk.platform.WindowId, surface: []const u8) bool {
        for (runtime.views[0..runtime.view_count]) |*runtime_view| {
            if (runtime_view.window_id != window_id or !std.mem.eql(u8, runtime_view.label, surface)) continue;
            const focused = runtime_view.canvas_widget_focused_id;
            if (focused == 0) return false;
            const node = runtime_view.widgetLayoutTree().findById(focused) orelse return false;
            // Role, not just kind: a tab trigger is a plain container now
            // (a segmented control cannot host a close affordance), and a
            // click that leaves canvas widget focus parked on it is a click
            // after which the keyboard no longer reaches the terminal.
            if (node.widget.semantics.role == .tab) return true;
            return node.widget.kind == .segmented_control or
                node.widget.kind == .toggle_button or
                node.widget.kind == .button;
        }
        return false;
    }

    fn clearCanvasWidgetFocus(runtime: *native_sdk.Runtime, window_id: native_sdk.platform.WindowId, surface: []const u8) void {
        for (runtime.views[0..runtime.view_count]) |*runtime_view| {
            if (runtime_view.window_id != window_id or !std.mem.eql(u8, runtime_view.label, surface)) continue;
            runtime_view.canvas_widget_focused_id = 0;
            runtime_view.canvas_widget_focus_visible_id = 0;
            runtime_view.canvas_widget_focus_visible_keyboard = false;
            return;
        }
    }

    /// Narrow v0.7.1 adapter: consume the SDK's already-routed/captured widget
    /// event exactly once. Custom widgets cannot bind a continuous pointer Msg,
    /// and the SDK keeps only one pressed-widget register per canvas, so model
    /// capture below supplies the missing per-window/per-pointer ownership.
    fn routeTerminalPointer(
        self: *CockpitHost,
        runtime: *native_sdk.Runtime,
        routed: native_sdk.runtime.CanvasWidgetPointerEvent,
    ) !bool {
        if (scene_module.windowIndexForCanvas(routed.view_label) == null) return false;
        const pointer = routed.pointer;
        // A new down supersedes only this physical pointer's old capture,
        // even when the new target is chrome rather than another terminal.
        if (pointer.phase == .down) {
            if (pointerCaptureFor(&self.inner.model, routed.window_id, pointer.pointer_id)) |previous| {
                try self.inner.dispatch(runtime, routed.window_id, .{ .pointer = .{
                    .window_id = previous.window_id,
                    .terminal_id = previous.terminal_id,
                    .generation = previous.generation,
                    .phase = .cancel,
                    .pointer_id = previous.pointer_id,
                    .button = previous.button,
                    .point = previous.last_point,
                    .frame = previous.frame,
                    .modifiers = previous.modifiers,
                } });
            }
        }
        const capture = switch (pointer.phase) {
            .move, .up, .cancel => pointerCaptureFor(&self.inner.model, routed.window_id, pointer.pointer_id),
            .hover, .down, .wheel => null,
        };

        var terminal_id: LocalResourceId = undefined;
        var generation: u64 = 0;
        var frame: geometry.RectF = .{};
        if (capture) |owned| {
            terminal_id = owned.terminal_id;
            generation = owned.generation;
            frame = if (routed.target) |target|
                if (target.id == terminalInteractionWidgetId(owned.terminal_id)) target.bounds else terminalInteractionFrame(runtime, routed.window_id, routed.view_label, owned.terminal_id) orelse owned.frame
            else
                terminalInteractionFrame(runtime, routed.window_id, routed.view_label, owned.terminal_id) orelse owned.frame;
        } else {
            if (pointer.phase == .up or pointer.phase == .cancel or pointer.phase == .move) {
                // A stale/unrelated terminal edge cannot borrow the SDK's
                // canvas-global pressed id and disturb another pointer.
                if (routed.target) |target| {
                    if (terminalIdForInteractionWidget(&self.inner.model, target.id) != null) return true;
                }
                return false;
            }
            const target = routed.target orelse return false;
            terminal_id = terminalIdForInteractionWidget(&self.inner.model, target.id) orelse return false;
            const pane = self.inner.model.provider.terminal(localRef(terminal_id)) orelse return true;
            generation = pane.session_generation;
            frame = target.bounds;
        }

        try self.inner.dispatch(runtime, routed.window_id, .{ .pointer = .{
            .window_id = routed.window_id,
            .terminal_id = terminal_id,
            .generation = generation,
            .phase = pointer.phase,
            .pointer_id = pointer.pointer_id,
            .button = pointer.button,
            .click_count = pointer.click_count,
            .point = pointer.point,
            .frame = frame,
            .delta = pointer.delta,
            .modifiers = .{
                .shift = pointer.modifiers.shift,
                .control = pointer.modifiers.control,
                .alt = pointer.modifiers.alt,
                .super = pointer.modifiers.super,
            },
        } });
        if (pointer.phase == .down) try self.focusSelectedContent(runtime, routed.window_id);
        return true;
    }

    fn syncSelectionAutoscrollTimer(self: *CockpitHost, runtime: *native_sdk.Runtime) !void {
        const needed = modelHasSelectionAutoscroll(&self.inner.model);
        if (needed == self.selection_autoscroll_timer_active) return;
        if (needed) {
            try runtime.startTimer(selection_autoscroll_timer_id, selection_autoscroll_interval_ns, true);
        } else {
            try runtime.cancelTimer(selection_autoscroll_timer_id);
        }
        self.selection_autoscroll_timer_active = needed;
    }

    fn stop(context: *anyopaque, runtime: *native_sdk.Runtime) anyerror!void {
        const self: *CockpitHost = @ptrCast(@alignCast(context));
        if (self.selection_autoscroll_timer_active) {
            runtime.cancelTimer(selection_autoscroll_timer_id) catch {};
            self.selection_autoscroll_timer_active = false;
        }
        try self.inner_app.stop(runtime);
    }

    fn replay(context: *anyopaque, control: native_sdk.runtime.ReplayControl) anyerror!void {
        const self: *CockpitHost = @ptrCast(@alignCast(context));
        try self.inner_app.replayControl(control);
    }
};

fn terminalInteractionWidgetId(id: LocalResourceId) canvas.ObjectId {
    return canvas.globalWidgetId(.terminal, .{ .index = @intCast(@intFromEnum(id)) });
}

/// Routed widget pointer input reaches LOCAL terminals only: the remote path
/// runs through the NSEvent monitor, which carries its own hit testing.
fn terminalIdForInteractionWidget(model: *const Model, widget_id: canvas.ObjectId) ?LocalResourceId {
    // Every pane of every tab of every WINDOW, not just one tab's: a widget id
    // survives a tab switch and a window switch alike, and routing must
    // resolve it against the whole session.
    for (0..model_module.max_windows) |window_index| {
        const workspace = model.wsAtConst(window_index) orelse continue;
        for (0..workspace.tab_count) |tab_index| {
            const tree = workspace.treeConst(tab_index) orelse continue;
            var refs: [layout.max_panes]TerminalRef = undefined;
            const count = tree.terminals(&refs);
            for (refs[0..count]) |id| {
                const local = provider_contract.localId(id) orelse continue;
                if (terminalInteractionWidgetId(local) == widget_id) return local;
            }
        }
    }
    return null;
}

fn terminalInteractionFrame(
    runtime: *native_sdk.Runtime,
    window_id: native_sdk.platform.WindowId,
    surface: []const u8,
    id: LocalResourceId,
) ?geometry.RectF {
    const widget_id = terminalInteractionWidgetId(id);
    for (runtime.views[0..runtime.view_count]) |*runtime_view| {
        if (runtime_view.window_id != window_id or !std.mem.eql(u8, runtime_view.label, surface)) continue;
        const node = runtime_view.widgetLayoutTree().findById(widget_id) orelse return null;
        if (node.widget.kind != .terminal) return null;
        return node.frame;
    }
    return null;
}
