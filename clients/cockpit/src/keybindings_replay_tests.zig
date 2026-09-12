const std = @import("std");

const Buffer = struct {
    bytes: [64 * 1024]u8 = undefined,
    len: usize = 0,
    fn write(context: *anyopaque, bytes: []const u8) !void {
        const self: *Buffer = @ptrCast(@alignCast(context));
        if (bytes.len > self.bytes.len - self.len) return error.NoSpaceLeft;
        @memcpy(self.bytes[self.len..][0..bytes.len], bytes);
        self.len += bytes.len;
    }
};

/// Real SDK dispatchPlatformEvent, SessionRecorder and replaySession driver.
/// The replay registry is intentionally NEVER installed or updated: admissions
/// must reproduce the remapped/disabled timeline without a platform mutation.
pub fn check(comptime sdk: type, comptime runtime: type, comptime replay: type) !void {
    const gpa = std.testing.allocator;
    const recorder = try gpa.create(sdk.runtime.SessionRecorder);
    defer gpa.destroy(recorder);
    var buffer: Buffer = .{};
    recorder.* = sdk.runtime.SessionRecorder.init(.{ .context = &buffer, .write_fn = Buffer.write });
    recorder.begin(.{ .platform_name = "test", .app_name = "keybinding-admission", .window_width = 400, .window_height = 300 });
    const shortcuts = [_]sdk.platform.Shortcut{.{ .id = "terminal.new", .key = "t", .modifiers = .{ .primary = true } }};
    var state = try runtime.State(sdk.platform).init(&shortcuts, &.{});
    var platform = sdk.platform.NullPlatform.init(.{});
    var overrides: runtime.bindings.Overrides = .{};
    var live: replay.Journal(sdk) = .{};
    defer live.deinit();
    const harness = try sdk.runtime.TestHarness().create(gpa, .{});
    defer harness.destroy(gpa);
    harness.runtime.options.session_recorder = recorder;
    var live_app: Driver(sdk, runtime, replay) = .{ .keys = &state, .admission = &live };
    try harness.runtime.dispatchPlatformEvent(live_app.app(), .app_start);
    try state.sync(platform.platform().services, &overrides, true);
    const first_id = try runtime.bindings.Text(64).init(platform.configuredShortcuts()[0].id);
    const first: sdk.platform.ShortcutEvent = .{ .id = first_id.slice(), .key = "t", .modifiers = .{ .command = true }, .window_id = 7 };
    try harness.runtime.dispatchPlatformEvent(live_app.app(), .{ .shortcut = first });

    try overrides.set("terminal.new", "Cmd+r");
    try state.sync(platform.platform().services, &overrides, true);
    const remapped_id = try runtime.bindings.Text(64).init(platform.configuredShortcuts()[0].id);
    const remapped: sdk.platform.ShortcutEvent = .{ .id = remapped_id.slice(), .key = "r", .modifiers = .{ .command = true }, .window_id = 7 };
    try harness.runtime.dispatchPlatformEvent(live_app.app(), .{ .shortcut = first });
    try harness.runtime.dispatchPlatformEvent(live_app.app(), .{ .shortcut = remapped });
    try state.sync(platform.platform().services, &overrides, false);
    try harness.runtime.dispatchPlatformEvent(live_app.app(), .{ .shortcut = remapped });
    try state.sync(platform.platform().services, &overrides, true);
    try harness.runtime.dispatchPlatformEvent(live_app.app(), .{ .shortcut = remapped });
    const restored = platform.configuredShortcuts()[0];
    try harness.runtime.dispatchPlatformEvent(live_app.app(), .{ .shortcut = .{ .id = restored.id, .key = restored.key, .modifiers = restored.modifiers, .window_id = 7 } });
    try harness.runtime.dispatchPlatformEvent(live_app.app(), .app_shutdown);

    var replayed: replay.Journal(sdk) = .{};
    defer replayed.deinit();
    var initial_state = try runtime.State(sdk.platform).init(&shortcuts, &.{});
    const replay_harness = try sdk.runtime.TestHarness().create(gpa, .{});
    defer replay_harness.destroy(gpa);
    Driver(sdk, runtime, replay).registrations = 0;
    replay_harness.runtime.options.platform.services.configure_shortcuts_fn = Driver(sdk, runtime, replay).forbidShortcuts;
    replay_harness.runtime.options.platform.services.configure_menus_fn = Driver(sdk, runtime, replay).forbidMenus;
    var replay_app: Driver(sdk, runtime, replay) = .{ .keys = &initial_state, .admission = &replayed };
    const report = try sdk.runtime.replaySession(&replay_harness.runtime, replay_app.app(), buffer.bytes[0..buffer.len], .{ .require_same_platform = false });
    const expected = [_]bool{ true, false, true, false, false, true };
    try std.testing.expectEqualSlices(bool, &expected, live_app.decisions[0..live_app.count]);
    try std.testing.expectEqualSlices(bool, &expected, replay_app.decisions[0..replay_app.count]);
    try std.testing.expectEqual(@as(u64, 8), report.events_replayed);
    try std.testing.expectEqual(@as(usize, 3), replay_app.commands);
    try std.testing.expectEqual(@as(usize, 6), replay_app.preliminary);
    try std.testing.expectEqual(@as(usize, 0), replay_harness.null_platform.configuredShortcuts().len);
    try std.testing.expectEqual(@as(usize, 0), Driver(sdk, runtime, replay).registrations);
    try std.testing.expect(!initial_state.installed);
    try std.testing.expectEqual(@as(u64, 0), initial_state.generation);
    try replayed.finishReplay();
}

fn Driver(comptime sdk: type, comptime runtime: type, comptime replay: type) type {
    return struct {
        const Self = @This();
        var registrations: usize = 0;
        keys: *runtime.State(sdk.platform),
        admission: *replay.Journal(sdk),
        decisions: [6]bool = @splat(false),
        count: usize = 0,
        commands: usize = 0,
        preliminary: usize = 0,
        fn forbidShortcuts(_: ?*anyopaque, _: []const sdk.platform.Shortcut) !void {
            registrations += 1;
            return error.ReplayTouchedPlatform;
        }
        fn forbidMenus(_: ?*anyopaque, _: []const sdk.platform.Menu) !void {
            registrations += 1;
            return error.ReplayTouchedPlatform;
        }
        fn app(self: *Self) sdk.App {
            return .{ .context = self, .name = "keybinding-admission", .start_fn = start, .event_fn = event, .replay_fn = control };
        }
        fn start(context: *anyopaque, rt: *sdk.Runtime) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            try self.admission.start(std.testing.allocator, 7101, rt.options.session_recorder);
        }
        fn event(context: *anyopaque, _: *sdk.Runtime, value: sdk.Event) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            if (value == .command and value.command.source == .shortcut) {
                self.preliminary += 1;
                return;
            }
            if (value != .shortcut) return;
            const command = try self.admission.shortcut(self.keys, value.shortcut);
            self.decisions[self.count] = command != null;
            self.count += 1;
            const name = command orelse return;
            try std.testing.expectEqualStrings("terminal.new", name);
            try std.testing.expectEqual(@as(u64, 7), value.shortcut.window_id);
            self.commands += 1;
        }
        fn control(context: *anyopaque, value: sdk.runtime.ReplayControl) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            switch (value) {
                .arm => try self.admission.armReplay(),
                .feed => |record| try std.testing.expect(try self.admission.feed(record)),
                .finish => try self.admission.finishReplay(),
            }
        }
    };
}

fn recordShortcut(comptime sdk: type, recorder: *sdk.runtime.SessionRecorder, admission: anytype, state: anytype, event: sdk.platform.ShortcutEvent, expected: ?[]const u8) !void {
    recorder.stageEvent(.{ .shortcut = event });
    const command = try admission.shortcut(state, event);
    if (expected) |name| {
        try std.testing.expectEqualStrings(name, command.?);
    } else try std.testing.expect(command == null);
    recorder.commitEvent();
}

pub fn checkFallback(comptime sdk: type, comptime runtime: type, comptime replay: type) !void {
    const gpa = std.testing.allocator;
    const recorder = try gpa.create(sdk.runtime.SessionRecorder);
    defer gpa.destroy(recorder);
    var buffer: Buffer = .{};
    recorder.* = sdk.runtime.SessionRecorder.init(.{ .context = &buffer, .write_fn = Buffer.write });
    recorder.begin(.{ .platform_name = "test", .app_name = "fallback", .window_width = 400, .window_height = 300 });
    const Fixture = FallbackDriver(sdk, runtime, replay);
    const live = try Fixture.create();
    defer live.destroy();
    live.harness.runtime.options.session_recorder = recorder;
    try live.harness.start(live.app());
    try live.installWindows();
    try live.driveKeys();
    try live.harness.runtime.dispatchPlatformEvent(live.app(), .app_shutdown);
    try std.testing.expectEqualSlices(u32, &.{ 1, 1 }, &live.ui.model.commands);
    try std.testing.expectEqual(@as(usize, 8), live.callbacks);

    const replayed = try Fixture.create();
    defer replayed.destroy();
    _ = try sdk.runtime.replaySession(&replayed.harness.runtime, replayed.app(), buffer.bytes[0..buffer.len], .{ .require_same_platform = false });
    try std.testing.expectEqualSlices(u32, &live.ui.model.commands, &replayed.ui.model.commands);
    try std.testing.expectEqual(live.callbacks, replayed.callbacks);
    try std.testing.expect(!replayed.keys.installed);
    try std.testing.expect(replayed.origin == null);

    for ([_]Fixture.Retarget{ .window, .view }) |target| {
        const wrong = try Fixture.create();
        defer wrong.destroy();
        wrong.retarget = target;
        try std.testing.expectError(error.ReplayAdmissionMismatch, sdk.runtime.replaySession(&wrong.harness.runtime, wrong.app(), buffer.bytes[0..buffer.len], .{ .require_same_platform = false }));
        try std.testing.expectEqualSlices(u32, &.{ 0, 0 }, &wrong.ui.model.commands);
        try std.testing.expectError(error.ReplayAdmissionMismatch, wrong.admission.finishReplay());
        try std.testing.expect(wrong.origin == null);
    }
}

/// Two real UiApp canvases receive journal-decoded gpu_surface_input events.
/// The wrapper scopes the on_key origin from the actual derived keyboard event,
/// exactly as the parent host must. Retarget changes ONLY the callback routing,
/// leaving both the recorded input and its admission result intact.
fn FallbackDriver(comptime sdk: type, comptime runtime: type, comptime replay: type) type {
    return struct {
        const Self = @This();
        const Model = struct { commands: [2]u32 = .{ 0, 0 } };
        const Msg = union(enum) { window: sdk.platform.WindowId };
        const UiApp = sdk.UiApp(Model, Msg);
        const Origin = struct { window_id: sdk.platform.WindowId, view_label: []const u8 };
        const Retarget = enum { none, window, view };
        const main_label = "fallback-main";
        const secondary_label = "fallback-secondary";
        var active: ?*Self = null;
        harness: *sdk.runtime.TestHarness(),
        ui: *UiApp,
        keys: runtime.State(sdk.platform),
        admission: replay.Journal(sdk) = .{},
        origin: ?Origin = null,
        retarget: Retarget = .none,
        callback_error: ?anyerror = null,
        callbacks: usize = 0,

        fn create() !*Self {
            const self = try std.testing.allocator.create(Self);
            errdefer std.testing.allocator.destroy(self);
            const harness = try sdk.runtime.TestHarness().create(std.testing.allocator, .{ .size = .{ .width = 400, .height = 300 } });
            errdefer harness.destroy(std.testing.allocator);
            harness.null_platform.gpu_surfaces = true;
            self.* = .{
                .harness = harness,
                .ui = try UiApp.create(std.heap.page_allocator, .{
                    .name = "fallback",
                    .scene = .{ .windows = &.{.{ .label = "main", .title = "Fallback", .width = 400, .height = 300, .views = &.{.{ .label = main_label, .kind = .gpu_surface, .fill = true, .gpu_backend = .metal }} }} },
                    .canvas_label = main_label,
                    .view = view,
                    .update = update,
                    .on_key = onKey,
                    .key_release_events = true,
                    .windows_fn = windows,
                    .window_view = windowView,
                }),
                .keys = try runtime.State(sdk.platform).init(&.{.{ .id = "terminal.new", .key = "t", .modifiers = .{ .primary = true } }}, &.{}),
            };
            return self;
        }
        fn destroy(self: *Self) void {
            self.admission.deinit();
            self.ui.destroy();
            self.harness.destroy(std.testing.allocator);
            std.testing.allocator.destroy(self);
        }
        fn app(self: *Self) sdk.App {
            return .{ .context = self, .name = "fallback", .start_fn = start, .scene_fn = scene, .event_fn = event, .stop_fn = stop, .replay_fn = control };
        }
        fn scene(context: *anyopaque) !sdk.app_manifest.ShellConfig {
            const self: *Self = @ptrCast(@alignCast(context));
            return (try self.ui.app().scene()).?;
        }
        fn start(context: *anyopaque, rt: *sdk.Runtime) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            try self.ui.app().start(rt);
            try self.admission.start(std.testing.allocator, 7101, rt.options.session_recorder);
            if (self.admission.replaying) return;
            var overrides: runtime.bindings.Overrides = .{};
            try overrides.set("terminal.new", "Cmd+Shift+r");
            try self.keys.sync(rt.options.platform.services, &overrides, true);
        }
        fn stop(context: *anyopaque, rt: *sdk.Runtime) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            try self.ui.app().stop(rt);
        }
        fn event(context: *anyopaque, rt: *sdk.Runtime, value: sdk.Event) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            var routed = value;
            if (routed == .canvas_widget_keyboard) self.retargetKeyboard(&routed.canvas_widget_keyboard);
            const previous = self.origin;
            const previous_active = active;
            defer self.origin = previous;
            defer active = previous_active;
            active = self;
            if (routed == .canvas_widget_keyboard) self.origin = .{ .window_id = routed.canvas_widget_keyboard.window_id, .view_label = routed.canvas_widget_keyboard.view_label };
            try self.ui.app().event(rt, routed);
            if (self.callback_error) |err| return err;
        }
        fn retargetKeyboard(self: *Self, keyboard: *sdk.runtime.CanvasWidgetKeyboardEvent) void {
            // Retarget the ADMITTED remapped key, not the removed default or
            // a key-up/control event which has no admission to consume.
            if (keyboard.keyboard.phase != .key_down) return;
            if (!std.mem.eql(u8, keyboard.keyboard.key, "R")) return;
            switch (self.retarget) {
                .none => {},
                .window => keyboard.window_id = 2,
                .view => keyboard.view_label = secondary_label,
            }
        }
        fn onKey(keyboard: sdk.canvas.WidgetKeyboardEvent) ?Msg {
            const self = active.?;
            const origin = self.origin.?;
            self.callbacks += 1;
            const command = self.admit(keyboard, origin) catch |err| {
                self.callback_error = err;
                return null;
            };
            if (command == null) return null;
            return .{ .window = origin.window_id };
        }
        fn admit(self: *Self, keyboard: sdk.canvas.WidgetKeyboardEvent, origin: Origin) !?[]const u8 {
            return self.admission.fallback(&self.keys, keyboard, .{ .window_id = origin.window_id, .view_label = origin.view_label });
        }
        fn control(context: *anyopaque, value: sdk.runtime.ReplayControl) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            switch (value) {
                .arm => try self.admission.armReplay(),
                .feed => |record| if (try self.admission.feed(record)) return,
                .finish => try self.admission.finishReplay(),
            }
            try self.ui.app().replayControl(value);
        }
        fn update(model: *Model, msg: Msg) void {
            model.commands[@intCast(msg.window - 1)] += 1;
        }
        fn view(ui: *UiApp.Ui, _: *const Model) UiApp.Ui.Node {
            return ui.text(.{}, "Fallback");
        }
        fn windows(_: *const Model, scratch: *UiApp.WindowsScratch) []const UiApp.WindowDescriptor {
            scratch.windows[0] = .{ .label = "secondary", .canvas_label = secondary_label, .width = 400, .height = 300 };
            return scratch.windows[0..1];
        }
        fn windowView(ui: *UiApp.Ui, model: *const Model, _: []const u8) UiApp.Ui.Node {
            return view(ui, model);
        }
        fn installWindows(self: *Self) !void {
            for ([_][]const u8{ main_label, secondary_label }, 1..) |label, id| {
                try self.harness.runtime.dispatchPlatformEvent(self.app(), .{ .gpu_surface_frame = .{ .window_id = id, .label = label, .size = .{ .width = 400, .height = 300 }, .scale_factor = 1, .frame_index = 1, .timestamp_ns = id, .nonblank = true } });
                try self.harness.runtime.dispatchPlatformEvent(self.app(), .{ .view_focused = .{ .window_id = id, .label = label } });
            }
        }
        fn driveKeys(self: *Self) !void {
            for ([_][]const u8{ main_label, secondary_label }, 1..) |label, id| {
                try self.harness.runtime.dispatchPlatformEvent(self.app(), .{ .gpu_surface_input = .{ .window_id = id, .label = label, .kind = .key_down, .key = "t", .modifiers = .{ .command = true } } });
                try self.harness.runtime.dispatchPlatformEvent(self.app(), .{ .gpu_surface_input = .{ .window_id = id, .label = label, .kind = .key_down, .key = "R", .modifiers = .{ .command = true, .shift = true } } });
                try self.harness.runtime.dispatchPlatformEvent(self.app(), .{ .gpu_surface_input = .{ .window_id = id, .label = label, .kind = .key_up, .key = "R", .modifiers = .{ .command = true, .shift = true } } });
                try self.harness.runtime.dispatchPlatformEvent(self.app(), .{ .gpu_surface_input = .{ .window_id = id, .label = label, .kind = .text_input, .text = "R", .modifiers = .{ .command = true, .shift = true } } });
                try self.harness.runtime.dispatchPlatformEvent(self.app(), .{ .gpu_surface_input = .{ .window_id = id, .label = label, .kind = .key_down, .key = "r", .modifiers = .{ .control = true } } });
            }
        }
    };
}

pub fn checkDivergence(comptime sdk: type, comptime runtime: type, comptime replay: type) !void {
    const gpa = std.testing.allocator;
    const recorder = try gpa.create(sdk.runtime.SessionRecorder);
    defer gpa.destroy(recorder);
    var buffer: Buffer = .{};
    recorder.* = sdk.runtime.SessionRecorder.init(.{ .context = &buffer, .write_fn = Buffer.write });
    recorder.begin(.{ .platform_name = "test", .app_name = "divergence", .window_width = 400, .window_height = 300 });
    const shortcuts = [_]sdk.platform.Shortcut{.{ .id = "terminal.new", .key = "t", .modifiers = .{ .primary = true } }};
    var state = try runtime.State(sdk.platform).init(&shortcuts, &.{});
    var platform = sdk.platform.NullPlatform.init(.{});
    const overrides: runtime.bindings.Overrides = .{};
    try state.sync(platform.platform().services, &overrides, true);
    try checkOriginLimits(sdk, replay, &state);
    const registered = platform.configuredShortcuts()[0];
    const event: sdk.platform.ShortcutEvent = .{ .id = registered.id, .key = registered.key, .modifiers = registered.modifiers, .window_id = 2 };
    var live: replay.Journal(sdk) = .{};
    defer live.deinit();
    try live.start(gpa, 7101, recorder);
    try recordShortcut(sdk, recorder, &live, &state, event, "terminal.new");
    recorder.finish();
    var reader = try sdk.runtime.session_journal.Reader.init(buffer.bytes[0..buffer.len]);
    while (try reader.next()) |record| {
        if (record != .effect) continue;
        try rejectChangedInput(sdk, runtime, replay, &state, event, record.effect);
        break;
    }
}

fn checkOriginLimits(comptime sdk: type, comptime replay: type, keys: anytype) !void {
    const Journal = replay.Journal(sdk);
    const longest_label = "v" ** sdk.platform.max_view_label_bytes;
    const key: sdk.canvas.WidgetKeyboardEvent = .{ .phase = .key_down, .key = "t", .modifiers = .{ .super = true } };
    const valid: Journal.Origin = .{ .window_id = 2, .view_label = longest_label };
    var live: Journal = .{};
    defer live.deinit();
    try live.start(std.testing.allocator, 7101, null);
    try std.testing.expectEqualStrings("terminal.new", (try live.fallback(keys, key, valid)).?);
    var longest_key = key;
    longest_key.key = "x" ** sdk.platform.max_shortcut_key_bytes;
    try std.testing.expect((try live.fallback(keys, longest_key, valid)) == null);
    for ([_]Journal.Origin{
        .{ .window_id = 0, .view_label = "canvas" },
        .{ .window_id = 2, .view_label = "" },
        .{ .window_id = 2, .view_label = longest_label ++ "x" },
    }) |invalid| {
        var refused: Journal = .{};
        defer refused.deinit();
        try refused.start(std.testing.allocator, 7101, null);
        try std.testing.expectError(error.ReplayAdmissionMismatch, refused.fallback(keys, key, invalid));
        try std.testing.expectError(error.ReplayAdmissionMismatch, refused.fallback(keys, key, valid));
        try std.testing.expectError(error.ReplayAdmissionMismatch, refused.finishReplay());
    }
}

fn rejectChangedInput(comptime sdk: type, comptime runtime: type, comptime replay: type, state: *runtime.State(sdk.platform), event: sdk.platform.ShortcutEvent, effect: sdk.runtime.EffectResultRecord) !void {
    // Even a valid live installation cannot admit a replay without its feed.
    var missing: replay.Journal(sdk) = .{};
    defer missing.deinit();
    try missing.armReplay();
    try missing.start(std.testing.allocator, 7101, null);
    try std.testing.expectError(error.ReplayAdmissionMismatch, missing.shortcut(state, event));
    try std.testing.expectError(error.ReplayAdmissionMismatch, missing.finishReplay());

    const changed = [_]sdk.platform.ShortcutEvent{
        .{ .id = "kb.0.0", .key = event.key, .modifiers = event.modifiers, .window_id = event.window_id },
        .{ .id = event.id, .key = event.key, .modifiers = event.modifiers, .window_id = 99 },
        .{ .id = event.id, .key = "x", .modifiers = event.modifiers, .window_id = event.window_id },
        .{ .id = event.id, .key = event.key, .modifiers = .{ .command = true, .shift = true }, .window_id = event.window_id },
    };
    for (changed) |value| {
        var mismatch: replay.Journal(sdk) = .{};
        defer mismatch.deinit();
        try mismatch.armReplay();
        try mismatch.start(std.testing.allocator, 7101, null);
        try std.testing.expect(try mismatch.feed(effect));
        try std.testing.expectError(error.ReplayAdmissionMismatch, mismatch.shortcut(state, value));
        try std.testing.expectError(error.ReplayAdmissionMismatch, mismatch.finishReplay());
    }
    var leftover: replay.Journal(sdk) = .{};
    defer leftover.deinit();
    try leftover.armReplay();
    try leftover.start(std.testing.allocator, 7101, null);
    var unrelated = effect;
    unrelated.key = 7102;
    try std.testing.expect(!try leftover.feed(unrelated));
    try std.testing.expect(try leftover.feed(effect));
    try std.testing.expectError(error.ReplayDivergence, leftover.finishReplay());
}
