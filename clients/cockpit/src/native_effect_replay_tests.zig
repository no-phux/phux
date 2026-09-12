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

/// allocateKey must use the production allocator (or a fresh Engine fixture
/// which allocates through it), never reset a counter or return replay's key.
pub fn check(comptime sdk: type, comptime module: type, comptime allocateKey: anytype) !void {
    const Fixture = Driver(sdk, module, allocateKey);
    const gpa = std.testing.allocator;
    const recorder = try gpa.create(sdk.runtime.SessionRecorder);
    defer gpa.destroy(recorder);
    var buffer: Buffer = .{};
    recorder.* = sdk.runtime.SessionRecorder.init(.{ .context = &buffer, .write_fn = Buffer.write });
    recorder.begin(.{ .platform_name = "test", .app_name = "native-replay", .window_width = 400, .window_height = 300 });
    const live = try Fixture.create();
    defer live.destroy();
    live.harness.runtime.options.session_recorder = recorder;
    const first_key = live.native_key;
    try live.harness.start(live.app());
    try live.harness.runtime.dispatchPlatformEvent(live.app(), .{ .gpu_surface_frame = .{ .label = "canvas", .size = .{ .width = 400, .height = 300 }, .scale_factor = 1, .frame_index = 1, .timestamp_ns = 1 } });
    try live.postNative();
    try live.publish("one");
    try live.wake();
    try live.reply("ONE");
    try live.fireRetry();
    try live.fireRetry();
    try live.rejectRetry();
    live.ui.effects.closeChannel(live.native_key);
    try live.wake(); // Closed delivery readmits the mux under another real key.
    try std.testing.expect(live.native_key != first_key);
    try live.postNative();
    try live.publish("two");
    try live.wake();
    try live.reply("TWO");
    try live.harness.runtime.dispatchPlatformEvent(live.app(), .frame_requested);
    try live.harness.runtime.dispatchPlatformEvent(live.app(), .app_shutdown);
    try std.testing.expectEqual(@as(usize, 2), live.native_opens);
    try std.testing.expectEqual(@as(usize, 2), live.retry_fires);
    try std.testing.expectEqual(@as(usize, 1), live.retry_rejections);
    try std.testing.expectEqual(@as(u32, 2), live.ui.model.notices);
    try std.testing.expectEqual(@as(u32, 2), live.ui.model.replies);
    try std.testing.expectEqualStrings("TWO", &live.ui.model.snapshot);

    _ = try allocateKey(); // Advance the ORIGINAL process-global allocator.
    const fresh = try Fixture.create();
    defer fresh.destroy();
    try std.testing.expect(fresh.native_key != first_key);
    try std.testing.expect(fresh.native_key != live.native_key);
    const report = try sdk.runtime.replaySession(&fresh.harness.runtime, fresh.app(), buffer.bytes[0..buffer.len], .{ .require_same_platform = false });
    try std.testing.expectEqual(@as(usize, 0), report.mismatch_count);
    try std.testing.expectEqual(@as(u64, 1), report.effects_skipped);
    try std.testing.expectEqualDeep(live.ui.model, fresh.ui.model);
    try std.testing.expectEqual(live.harness.runtime.sessionStateFingerprint(), fresh.harness.runtime.sessionStateFingerprint());
    try std.testing.expectEqual(@as(usize, 0), fresh.native_opens);
    try std.testing.expectEqual(@as(usize, 0), fresh.retry_fires);
    try std.testing.expectEqual(@as(usize, 0), fresh.native_callbacks);
    try std.testing.expectEqual(@as(usize, 5), fresh.sink.delivered);
    try std.testing.expectEqual(@as(usize, 0), Fixture.timer_services);
    try std.testing.expect(fresh.sink.channels.contains(first_key));
    try std.testing.expect(!fresh.sink.channels.contains(fresh.native_key));
}

fn Driver(comptime sdk: type, comptime module: type, comptime allocateKey: anytype) type {
    return struct {
        const Self = @This();
        const Model = struct { notices: u32 = 0, replies: u32 = 0, snapshot: [3]u8 = @splat(0) };
        const Msg = union(enum) { engine_wake, notice: sdk.EffectChannelEvent, reply: sdk.EffectHostResult };
        const UiApp = sdk.UiApp(Model, Msg);
        const Sink = module.Replay(sdk);
        const metadata_key: u64 = 7001;
        const ui_channel: u64 = 50;
        const host_key: u64 = 77;
        var active: ?*Self = null;
        var timer_services: usize = 0;
        harness: *sdk.runtime.TestHarness(),
        ui: *UiApp,
        sink: Sink,
        native_key: u64,
        native_handle: sdk.ChannelHandle = .{},
        retry_key: u64 = 0,
        native_opens: usize = 0,
        native_callbacks: usize = 0,
        retry_fires: usize = 0,
        retry_rejections: usize = 0,
        retry_starts: usize = 0,
        callback_error: ?anyerror = null,

        fn create() !*Self {
            const self = try std.testing.allocator.create(Self);
            errdefer std.testing.allocator.destroy(self);
            const harness = try sdk.runtime.TestHarness().create(std.testing.allocator, .{ .size = .{ .width = 400, .height = 300 } });
            errdefer harness.destroy(std.testing.allocator);
            harness.null_platform.gpu_surfaces = true;
            timer_services = 0;
            harness.runtime.options.platform.services.start_timer_fn = observeTimerService;
            const ui = try UiApp.create(std.heap.page_allocator, .{
                .name = "native-replay",
                .scene = .{ .windows = &.{.{ .label = "main", .width = 400, .height = 300, .views = &.{.{ .label = "canvas", .kind = .gpu_surface, .fill = true, .gpu_backend = .metal }} }} },
                .canvas_label = "canvas",
                .view = view,
                .update_fx = update,
                .init_fx = initFx,
            });
            errdefer ui.destroy();
            ui.effects.executor = .fake;
            self.* = .{ .harness = harness, .ui = ui, .sink = Sink.init(std.testing.allocator, metadata_key, policy(module)), .native_key = try allocateKey() };
            return self;
        }
        fn destroy(self: *Self) void {
            self.sink.deinit();
            self.ui.destroy();
            self.harness.destroy(std.testing.allocator);
            std.testing.allocator.destroy(self);
        }
        fn observeTimerService(_: ?*anyopaque, _: u64, _: u64, _: bool) !void {
            timer_services += 1;
            return error.UnexpectedTimerService;
        }
        fn app(self: *Self) sdk.App {
            return .{ .context = self, .name = "native-replay", .scene_fn = scene, .start_fn = start, .stop_fn = stop, .event_fn = event, .replay_fn = control };
        }
        fn scene(context: *anyopaque) !sdk.app_manifest.ShellConfig {
            const self: *Self = @ptrCast(@alignCast(context));
            return (try self.ui.app().scene()).?;
        }
        fn start(context: *anyopaque, rt: *sdk.Runtime) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            try self.ui.app().start(rt);
            self.sink.bindRecorder(rt.options.session_recorder);
            if (!self.sink.replaying) try self.openNative();
        }
        fn stop(context: *anyopaque, rt: *sdk.Runtime) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            try self.ui.app().stop(rt);
        }
        fn event(context: *anyopaque, rt: *sdk.Runtime, value: sdk.Event) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            const previous = active;
            active = self;
            defer active = previous;
            if (try self.sink.event(value, &self.ui.effects, .{ .installed = self.ui.installed, .primary_canvas_label = self.ui.options.canvas_label })) return;
            try self.ui.app().event(rt, value);
            if (self.callback_error) |err| return err;
        }
        fn control(context: *anyopaque, value: sdk.runtime.ReplayControl) !void {
            const self: *Self = @ptrCast(@alignCast(context));
            switch (value) {
                .arm => self.sink.armReplay(),
                .feed => |result| if (try self.sink.feed(result)) return,
                .finish => try self.sink.finish(),
            }
            try self.ui.app().replayControl(value);
        }
        fn initFx(_: *Model, fx: *UiApp.Effects) void {
            _ = fx.openChannel(.{ .key = ui_channel, .on_event = UiApp.Effects.channelMsg(.notice) });
        }
        fn update(model: *Model, msg: Msg, fx: *UiApp.Effects) void {
            switch (msg) {
                .engine_wake => {}, // Shipping core: copy model, no effects.
                .notice => {
                    model.notices += 1;
                    fx.hostRequest(.{ .key = host_key, .name = "snapshot", .on_result = UiApp.Effects.hostMsg(.reply) });
                },
                .reply => |result| {
                    model.replies += 1;
                    @memcpy(&model.snapshot, result.bytes[0..3]);
                },
            }
        }
        fn view(ui: *UiApp.Ui, _: *const Model) UiApp.Ui.Node {
            return ui.text(.{}, "Native replay");
        }
        fn openNative(self: *Self) !void {
            self.native_handle = self.ui.effects.openChannel(.{ .key = self.native_key, .on_event = nativeEvent, .max_pending = 1 });
            try self.sink.noteChannelOpen(self.native_key, self.native_handle.live());
            self.native_opens += 1;
        }
        fn nativeEvent(value: sdk.EffectChannelEvent) Msg {
            const self = active.?;
            self.native_callbacks += 1;
            self.nativeDelivery(value) catch |err| {
                self.callback_error = err;
            };
            return .engine_wake;
        }
        fn nativeDelivery(self: *Self, value: sdk.EffectChannelEvent) !void {
            if (value.kind == .closed) {
                self.native_key = try allocateKey();
                return self.openNative();
            }
            if (self.retry_starts >= 2) return;
            self.retry_key = try allocateKey();
            self.ui.effects.startTimer(.{ .key = self.retry_key, .interval_ms = 1, .mode = .one_shot, .on_fire = retry });
            try self.sink.noteTimer(&self.ui.effects, self.retry_key);
            self.retry_starts += 1;
        }
        fn retry(value: sdk.EffectTimer) Msg {
            const self = active.?;
            if (value.outcome == .rejected) {
                self.retry_rejections += 1;
                return .engine_wake;
            }
            self.retry_fires += 1;
            self.postNative() catch |err| {
                self.callback_error = err;
            };
            return .engine_wake;
        }
        fn postNative(self: *Self) !void {
            try std.testing.expectEqual(.accepted, self.native_handle.post(&.{1}));
        }
        fn publish(self: *Self, bytes: []const u8) !void {
            try std.testing.expectEqual(.accepted, self.ui.effects.channelHandle(ui_channel).?.post(bytes));
        }
        fn wake(self: *Self) !void {
            try self.harness.runtime.dispatchPlatformEvent(self.app(), .wake);
        }
        fn reply(self: *Self, bytes: []const u8) !void {
            try self.ui.effects.feedHostResult(host_key, true, bytes);
            try self.wake();
        }
        fn fireRetry(self: *Self) !void {
            try self.harness.runtime.dispatchPlatformEvent(self.app(), .{ .timer = .{ .id = sdk.runtime.effect_timer_platform_id_base, .timestamp_ns = self.retry_starts } });
            try self.wake();
        }
        fn rejectRetry(self: *Self) !void {
            const key = try allocateKey();
            self.ui.effects.startTimer(.{ .key = key, .interval_ms = 0, .mode = .one_shot, .on_fire = retry });
            try self.sink.noteTimer(&self.ui.effects, key);
            try self.wake();
        }
    };
}

fn policy(comptime module: type) module.Policy {
    // Actual support.allocatePeerHandle range: lower bound through MAX-1.
    return .{ .channels = &.{ 102, 103 }, .timers = &.{200}, .dynamic_first = 0x5046_0000_0000_0000, .dynamic_limit = std.math.maxInt(u64), .reserved = &.{ 50, 77, 7001, 0xfefe_0000_0000_0001 } };
}

pub fn checkOwnership(comptime sdk: type, comptime module: type, comptime allocateKey: anytype) !void {
    const Sink = module.Replay(sdk);
    const Fx = sdk.Effects(union(enum) { timer: sdk.EffectTimer });
    const gpa = std.testing.allocator;
    const recorder = try gpa.create(sdk.runtime.SessionRecorder);
    defer gpa.destroy(recorder);
    var buffer: Buffer = .{};
    recorder.* = sdk.runtime.SessionRecorder.init(.{ .context = &buffer, .write_fn = Buffer.write });
    recorder.begin(.{ .platform_name = "test", .app_name = "ownership", .window_width = 400, .window_height = 300 });
    var live = Sink.init(gpa, 7001, policy(module));
    defer live.deinit();
    live.bindRecorder(recorder);
    const channel_key = try allocateKey();
    const timer_key = try allocateKey();
    try live.noteChannelOpen(channel_key, true);
    const effects = try gpa.create(Fx);
    defer gpa.destroy(effects);
    effects.* = Fx.init(gpa);
    defer effects.deinit();
    effects.executor = .fake;
    effects.startTimer(.{ .key = timer_key, .interval_ms = 1, .mode = .one_shot, .on_fire = Fx.timerMsg(.timer) });
    try live.noteTimer(effects, timer_key);
    effects.cancelTimer(timer_key);
    recorder.finish();

    var sink = Sink.init(gpa, 7001, policy(module));
    defer sink.deinit();
    var timer_sink = Sink.init(gpa, 7001, policy(module));
    defer timer_sink.deinit();
    var reader = try sdk.runtime.session_journal.Reader.init(buffer.bytes[0..buffer.len]);
    while (try reader.next()) |record| {
        if (record != .effect) continue;
        try std.testing.expect(try sink.feed(record.effect));
        try std.testing.expect(try timer_sink.feed(record.effect));
    }
    sink.armReplay();
    timer_sink.armReplay();
    const installed: Sink.DrainContext = .{ .installed = true, .primary_canvas_label = "canvas" };
    const uninstalled: Sink.DrainContext = .{ .installed = false, .primary_canvas_label = "canvas" };
    const data: sdk.runtime.EffectResultRecord = .{ .kind = .channel, .key = channel_key, .payload = &.{1} };
    try std.testing.expect(try sink.feed(data));
    try std.testing.expectEqual(@as(usize, 0), sink.delivered);
    try std.testing.expectError(error.NativeReplayMismatch, sink.finish());
    for ([_][]const u8{ "secondary", "unknown" }) |label| {
        try std.testing.expect(!try sink.event(.{ .gpu_surface_frame = .{ .label = label, .size = .{ .width = 400, .height = 300 } } }, effects, installed));
        try std.testing.expectError(error.NativeReplayMismatch, sink.finish());
    }
    const primary: sdk.Event = .{ .gpu_surface_frame = .{ .label = "canvas", .size = .{ .width = 400, .height = 300 } } };
    try std.testing.expect(!try sink.event(primary, effects, uninstalled));
    try std.testing.expectError(error.NativeReplayMismatch, sink.finish());
    try std.testing.expect(!try sink.event(.effects_wake, effects, uninstalled));
    try std.testing.expectError(error.NativeReplayMismatch, sink.finish());
    try std.testing.expect(!try sink.event(.{ .lifecycle = .activate }, effects, installed));
    try std.testing.expectError(error.NativeReplayMismatch, sink.finish());
    try std.testing.expect(!try sink.event(.effects_wake, effects, installed));
    try sink.finish();
    try std.testing.expectEqual(@as(usize, 1), sink.delivered);

    try std.testing.expect(!try sink.feed(.{ .kind = .channel, .key = 50, .payload = "core notification" }));
    try std.testing.expect(!try sink.feed(.{ .kind = .channel, .key = 0xfefe_0000_0000_0001, .payload = "another native extension" }));
    try std.testing.expect(!try sink.feed(.{ .kind = .host, .key = 77, .payload = "snapshot" }));
    try std.testing.expect(!try sink.feed(.{ .kind = .file, .key = channel_key }));
    try std.testing.expect(try sink.feed(.{ .kind = .channel, .key = channel_key, .channel_kind = .closed }));
    try std.testing.expect(!try sink.event(primary, effects, installed));
    try sink.finish();
    try std.testing.expectError(error.NativeReplayMismatch, sink.feed(data));
    try std.testing.expectError(error.NativeReplayMismatch, sink.finish());

    const base = sdk.runtime.effect_timer_platform_id_base;
    try std.testing.expect(try timer_sink.event(.{ .timer = .{ .id = base } }, effects, installed));
    try std.testing.expect(!try timer_sink.event(.{ .timer = .{ .id = base + 1 } }, effects, installed));
    effects.startTimer(.{ .key = 7, .interval_ms = 1, .mode = .one_shot, .on_fire = Fx.timerMsg(.timer) });
    // A core timer shifted into the omitted native slot must not be silently
    // lost even when the next recorded timer ID itself is unowned.
    try std.testing.expectError(error.NativeReplayMismatch, timer_sink.event(.{ .timer = .{ .id = base + 1 } }, effects, installed));
    try std.testing.expectError(error.NativeReplayMismatch, timer_sink.finish());

    var unknown = Sink.init(gpa, 7001, policy(module));
    defer unknown.deinit();
    unknown.armReplay();
    try std.testing.expectError(error.NativeReplayMismatch, unknown.feed(.{ .kind = .channel, .key = try allocateKey(), .payload = &.{1} }));
    try std.testing.expectError(error.NativeReplayMismatch, unknown.finish());
}
