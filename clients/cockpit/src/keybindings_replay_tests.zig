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
    const shortcuts = [_]sdk.platform.Shortcut{.{ .id = "terminal.new", .key = "t", .modifiers = .{ .primary = true } }};
    var state = try runtime.State(sdk.platform).init(&shortcuts, &.{});
    var platform = sdk.platform.NullPlatform.init(.{});
    var overrides: runtime.bindings.Overrides = .{};
    try overrides.set("terminal.new", "Cmd+Shift+r");
    try state.sync(platform.platform().services, &overrides, true);
    var live: replay.Journal(sdk) = .{};
    defer live.deinit();
    try live.start(gpa, 7101, recorder);
    const events = [_]sdk.canvas.WidgetKeyboardEvent{
        .{ .phase = .key_down, .key = "t", .modifiers = .{ .super = true } },
        .{ .phase = .key_down, .key = "R", .modifiers = .{ .super = true, .shift = true } },
        .{ .phase = .text_input, .key = "R", .modifiers = .{ .super = true, .shift = true } },
        .{ .phase = .key_up, .key = "R", .modifiers = .{ .super = true, .shift = true } },
        .{ .phase = .key_down, .key = "r", .modifiers = .{ .control = true } },
    };
    const expected = [_]bool{ false, true, false, false, false };
    for (events, expected) |value, accepted| {
        recorder.stageEvent(.wake);
        try std.testing.expectEqual(accepted, (try live.fallback(&state, value)) != null);
        recorder.commitEvent();
    }
    recorder.finish();
    var replayed: replay.Journal(sdk) = .{};
    defer replayed.deinit();
    try replayed.armReplay();
    try replayed.start(gpa, 7101, null);
    var initial_state = try runtime.State(sdk.platform).init(&shortcuts, &.{});
    var reader = try sdk.runtime.session_journal.Reader.init(buffer.bytes[0..buffer.len]);
    var count: usize = 0;
    while (try reader.next()) |record| {
        switch (record) {
            .effect => |value| try std.testing.expect(try replayed.feed(value)),
            .event => {
                try std.testing.expectEqual(expected[count], (try replayed.fallback(&initial_state, events[count])) != null);
                count += 1;
            },
            else => {},
        }
    }
    try std.testing.expectEqual(events.len, count);
    try replayed.finishReplay();
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
