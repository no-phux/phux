//! Replay ownership for native-only wake channels and timers. Their constructors
//! change the live Engine but return an effect-free core engine_wake. The core's
//! separately journaled notifications and host replies remain authoritative.
//! Never rerun providers, callbacks, or the process-global handle allocator here.
const std = @import("std");

pub const Policy = struct {
    channels: []const u64,
    timers: []const u64,
    dynamic_first: u64,
    dynamic_limit: u64,
    reserved: []const u64 = &.{},

    fn dynamic(self: Policy, key: u64) bool {
        return key >= self.dynamic_first and key < self.dynamic_limit;
    }
    fn owns(self: Policy, keys: []const u64, key: u64) bool {
        if (std.mem.indexOfScalar(u64, self.reserved, key) != null) return false;
        return self.dynamic(key) or std.mem.indexOfScalar(u64, keys, key) != null;
    }
};

pub fn Replay(comptime sdk: type) type {
    return struct {
        const Self = @This();
        const Op = enum(u8) { channel = 1, timer = 2 };
        const Channel = struct { active: bool = false, rejected: bool = false, opened: bool = false };
        const Declaration = struct { op: Op, key: u64, value: u64 };
        pub const DrainContext = struct {
            installed: bool,
            primary_canvas_label: []const u8,
        };
        // version + operation + original key + admission/platform timer ID.
        const metadata_bytes = 1 + 1 + 8 + 8;

        allocator: std.mem.Allocator,
        metadata_key: u64,
        policy: Policy,
        recorder: ?*sdk.runtime.SessionRecorder = null,
        replaying: bool = false,
        failed: bool = false,
        channels: std.AutoHashMapUnmanaged(u64, Channel) = .empty,
        timer_ids: std.AutoHashMapUnmanaged(u64, u64) = .empty,
        pending: usize = 0,
        delivered: usize = 0,

        pub fn init(allocator: std.mem.Allocator, metadata_key: u64, policy: Policy) Self {
            return .{ .allocator = allocator, .metadata_key = metadata_key, .policy = policy };
        }
        pub fn deinit(self: *Self) void {
            self.channels.deinit(self.allocator);
            self.timer_ids.deinit(self.allocator);
        }
        pub fn bindRecorder(self: *Self, recorder: ?*sdk.runtime.SessionRecorder) void {
            self.recorder = recorder;
        }
        pub fn armReplay(self: *Self) void {
            // Registration metadata can precede app_start. Do not reset it.
            self.replaying = true;
        }

        /// Call AFTER the live EngineFx.openChannel, with handle.live(). A failed
        /// attempt is distinct from closing the already-open stream on that key.
        pub fn noteChannelOpen(self: *Self, key: u64, accepted: bool) !void {
            try self.record(.channel, key, @intFromBool(accepted));
        }

        /// Call AFTER live startTimer/schedulePeerRetry. The pinned SDK exposes
        /// timer_slots but no key-to-platform-ID accessor; its routing rule is
        /// effect_timer_platform_id_base + slot index (effects.zig).
        pub fn noteTimer(self: *Self, effects: anytype, key: u64) !void {
            const id = timerId(effects, key) orelse 0;
            try self.record(.timer, key, id);
        }
        fn timerId(effects: anytype, key: u64) ?u64 {
            for (effects.timer_slots, 0..) |slot, index| {
                if (slot.active and slot.key == key) return sdk.runtime.effect_timer_platform_id_base + index;
            }
            return null;
        }

        fn record(self: *Self, op: Op, key: u64, value: u64) !void {
            if (self.replaying) return self.refuse();
            if (!self.owns(op, key)) return self.refuse();
            const recorder = self.recorder orelse return;
            var bytes: [metadata_bytes]u8 = undefined;
            bytes[0] = 1;
            bytes[1] = @intFromEnum(op);
            std.mem.writeInt(u64, bytes[2..10], key, .little);
            std.mem.writeInt(u64, bytes[10..18], value, .little);
            const journal = recorder.effectJournal();
            journal.record_fn(journal.context, .{ .kind = .channel, .key = self.metadata_key, .payload = &bytes });
        }
        fn owns(self: *const Self, op: Op, key: u64) bool {
            if (key == self.metadata_key) return false;
            const keys = switch (op) {
                .channel => self.policy.channels,
                .timer => self.policy.timers,
            };
            return self.policy.owns(keys, key);
        }

        /// True means an explicitly declared native result. False MUST forward
        /// unchanged to UiApp.replayControl; never swallow unrelated failures.
        /// ReplaySession does not forward .timer result records: it regenerates
        /// rejections and routes fires through raw platform .timer events.
        pub fn feed(self: *Self, result: sdk.runtime.EffectResultRecord) !bool {
            errdefer self.failed = true;
            if (self.failed) return error.NativeReplayMismatch;
            if (result.key == self.metadata_key) {
                try self.metadata(result);
                return true;
            }
            if (result.kind != .channel or !self.owns(.channel, result.key)) return false;
            try self.channelResult(result);
            return true;
        }
        fn metadata(self: *Self, result: sdk.runtime.EffectResultRecord) !void {
            const declaration = decodeMetadata(result) catch return self.refuse();
            if (!self.owns(declaration.op, declaration.key)) return self.refuse();
            switch (declaration.op) {
                .channel => try self.declareChannel(declaration.key, declaration.value),
                .timer => try self.declareTimer(declaration.key, declaration.value),
            }
        }
        fn cleanData(result: sdk.runtime.EffectResultRecord) bool {
            return result.kind == .channel and result.channel_kind == .data and result.dropped == 0 and result.channel_dropped_total == 0;
        }
        fn decodeMetadata(result: sdk.runtime.EffectResultRecord) !Declaration {
            if (!cleanData(result)) return error.NativeReplayMismatch;
            const bytes = result.payload;
            if (bytes.len != metadata_bytes or bytes[0] != 1) return error.NativeReplayMismatch;
            const op = std.enums.fromInt(Op, bytes[1]) orelse return error.NativeReplayMismatch;
            const key = std.mem.readInt(u64, bytes[2..10], .little);
            const value = std.mem.readInt(u64, bytes[10..18], .little);
            return .{ .op = op, .key = key, .value = value };
        }
        fn declareChannel(self: *Self, key: u64, accepted: u64) !void {
            if (accepted > 1) return self.refuse();
            const entry = try self.channels.getOrPut(self.allocator, key);
            if (!entry.found_existing) entry.value_ptr.* = .{};
            const channel = entry.value_ptr;
            if (accepted == 0) {
                channel.rejected = true;
                return;
            }
            try self.activate(channel, key);
        }
        fn activate(self: *Self, channel: *Channel, key: u64) !void {
            if (channel.active) return self.refuse();
            if (channel.opened and self.policy.dynamic(key)) return self.refuse();
            channel.active = true;
            channel.opened = true;
        }
        fn declareTimer(self: *Self, key: u64, id: u64) !void {
            if (id == 0) return; // Rejected: no platform timer was installed.
            if (timerIndex(id) == null) return self.refuse();
            // Retain ownership after cancellation/fire: a queued platform event
            // may still carry this native slot ID. No native callback runs.
            try self.timer_ids.put(self.allocator, id, key);
        }
        fn channelResult(self: *Self, result: sdk.runtime.EffectResultRecord) !void {
            const channel = self.channels.getPtr(result.key) orelse return self.refuse();
            if (result.channel_kind == .rejected) {
                if (!channel.rejected) return self.refuse();
                channel.rejected = false;
            } else {
                if (!channel.active) return self.refuse();
                if (result.channel_kind == .closed) channel.active = false;
            }
            self.pending += 1;
        }

        /// Call before inner.event with the UiApp's ACTUAL entry-time installed
        /// state and primary canvas label. Its installing frame drains nothing.
        /// Skip only a recorded native timer ID; other
        /// events continue through the SDK. Native results were delivered live
        /// on these UiApp drain boundaries, never during replay feed itself.
        pub fn event(self: *Self, value: sdk.Event, effects: anytype, context: DrainContext) !bool {
            if (!self.replaying) return false;
            if (self.failed) return error.NativeReplayMismatch;
            try self.verifyTimerIsolation(effects);
            if (value == .timer) return self.timer_ids.contains(value.timer.id);
            if (drains(value, context)) {
                self.delivered += self.pending;
                self.pending = 0;
            }
            return false;
        }
        fn drains(value: sdk.Event, context: DrainContext) bool {
            if (!context.installed) return false;
            return switch (value) {
                .effects_wake => true,
                .gpu_surface_frame => |frame| std.mem.eql(u8, frame.label, context.primary_canvas_label),
                else => false,
            };
        }
        fn verifyTimerIsolation(self: *Self, effects: anytype) !void {
            if (self.timer_ids.count() == 0) return;
            // Current core declares no fx timers. Any core timer could move to
            // another slot when native registrations disappear, even if its
            // recorded ID does not collide. Refuse that unsupported graph.
            for (effects.timer_slots) |slot| {
                if (slot.active) return self.refuse();
            }
        }
        fn timerIndex(id: u64) ?usize {
            if (id < sdk.runtime.effect_timer_platform_id_base) return null;
            const index = id - sdk.runtime.effect_timer_platform_id_base;
            if (index >= sdk.runtime.max_effect_timers) return null;
            return @intCast(index);
        }
        pub fn finish(self: *Self) !void {
            if (self.failed or self.pending != 0) return error.NativeReplayMismatch;
        }
        fn refuse(self: *Self) error{NativeReplayMismatch} {
            self.failed = true;
            return error.NativeReplayMismatch;
        }
    };
}
