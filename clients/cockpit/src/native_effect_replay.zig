//! Replay ownership for native-only wake channels, timers, and topology writes.
//! Their constructors and callbacks change the live Engine but return an
//! effect-free core engine_wake. The core's
//! separately journaled notifications and host replies remain authoritative.
//! Never rerun providers, callbacks, or the process-global handle allocator here.
const std = @import("std");

pub const Policy = struct {
    channels: []const u64,
    timers: []const u64,
    files: []const u64 = &.{},
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
    fn ownsFile(self: Policy, key: u64) bool {
        return std.mem.indexOfScalar(u64, self.files, key) != null and std.mem.indexOfScalar(u64, self.reserved, key) == null;
    }
};

pub fn Replay(comptime sdk: type) type {
    return struct {
        const Self = @This();
        const Op = enum(u8) { channel = 1, timer = 2, file_write = 3, file_refused = 4 };
        const Channel = struct { active: bool = false, rejected: bool = false, opened: bool = false };
        const Declaration = struct { op: Op, key: u64, value: u64 };
        pub const DrainContext = struct {
            installed: bool,
            primary_canvas_label: []const u8,
        };
        pub const FileAttempt = struct { key: u64, op: sdk.EffectFileOp, sequence: u64 };
        // version + operation + original key + admission/timer ID/file operation.
        const metadata_bytes = 1 + 1 + 8 + 8;

        allocator: std.mem.Allocator,
        metadata_key: u64,
        policy: Policy,
        recorder: ?*sdk.runtime.SessionRecorder = null,
        replaying: bool = false,
        failed: bool = false,
        channels: std.AutoHashMapUnmanaged(u64, Channel) = .empty,
        timer_ids: std.AutoHashMapUnmanaged(u64, u64) = .empty,
        files: std.AutoHashMapUnmanaged(u64, void) = .empty,
        pending: usize = 0,
        delivered: usize = 0,

        pub fn init(allocator: std.mem.Allocator, metadata_key: u64, policy: Policy) Self {
            return .{ .allocator = allocator, .metadata_key = metadata_key, .policy = policy };
        }
        pub fn deinit(self: *Self) void {
            self.channels.deinit(self.allocator);
            self.timer_ids.deinit(self.allocator);
            self.files.deinit(self.allocator);
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
        /// Bracket the parent's actual writeFile call. Capture the pending-ring
        /// sequence BEFORE it, observe provenance AFTER it, without executing IO.
        pub fn beginFile(self: *Self, effects: anytype, key: u64, op: sdk.EffectFileOp) !FileAttempt {
            if (self.replaying or op != .write) return self.refuse();
            if (!self.owns(.file_write, key)) return self.refuse();
            return .{ .key = key, .op = op, .sequence = effects.pending_seq };
        }
        pub fn noteFile(self: *Self, effects: anytype, attempt: FileAttempt) !void {
            if (attempt.op != .write) return self.refuse();
            const op: Op = if (fileAdmissionRefused(effects, attempt)) .file_refused else .file_write;
            try self.record(op, attempt.key, @intFromEnum(attempt.op));
        }
        fn fileAdmissionRefused(effects: anytype, attempt: FileAttempt) bool {
            // Pinned SDK deliverLoopFileAdmission sets rejected_admission on
            // this loop-thread ring. External rejections set it false. Only
            // newly staged entries belong to this call; never infer from a
            // previous terminal on the reused topology key or from .rejected.
            for (0..effects.pending_exit_len) |offset| {
                const index = (effects.pending_exit_head + offset) % effects.pending_exits.len;
                if (effects.pending_exit_seqs[index] < attempt.sequence) continue;
                const pending = effects.pending_exits[index];
                if (pending != .file) continue;
                if (sameFileAttempt(pending.file.result, attempt)) return pending.file.rejected_admission;
            }
            return false;
        }
        fn sameFileAttempt(result: sdk.EffectFileResult, attempt: FileAttempt) bool {
            return result.key == attempt.key and result.op == attempt.op;
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
                .file_write, .file_refused => return self.policy.ownsFile(key),
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
            const op = resultOperation(result) orelse return false;
            if (!self.owns(op, result.key)) return false;
            try self.deliverResult(result);
            return true;
        }
        fn resultOperation(result: sdk.runtime.EffectResultRecord) ?Op {
            return switch (result.kind) {
                .channel => .channel,
                .file => .file_write,
                else => null,
            };
        }
        fn deliverResult(self: *Self, result: sdk.runtime.EffectResultRecord) !void {
            switch (result.kind) {
                .channel => try self.channelResult(result),
                .file => try self.fileResult(result),
                else => unreachable,
            }
        }
        fn metadata(self: *Self, result: sdk.runtime.EffectResultRecord) !void {
            const declaration = decodeMetadata(result) catch return self.refuse();
            if (!self.owns(declaration.op, declaration.key)) return self.refuse();
            switch (declaration.op) {
                .channel => try self.declareChannel(declaration.key, declaration.value),
                .timer => try self.declareTimer(declaration.key, declaration.value),
                .file_write, .file_refused => try self.declareFile(declaration),
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
        fn declareFile(self: *Self, declaration: Declaration) !void {
            if (declaration.value != @intFromEnum(sdk.EffectFileOp.write)) return self.refuse();
            if (declaration.op == .file_refused) return; // SDK skips this terminal.
            if (self.files.contains(declaration.key)) return self.refuse();
            try self.files.put(self.allocator, declaration.key, {});
        }
        fn fileResult(self: *Self, result: sdk.runtime.EffectResultRecord) !void {
            if (!writeTerminal(result)) return self.refuse();
            if (!self.files.remove(result.key)) return self.refuse();
            self.pending += 1;
        }
        fn writeTerminal(result: sdk.runtime.EffectResultRecord) bool {
            return result.file_op == .write and result.file_event == .terminal and result.payload.len == 0 and !result.file_rejected_admission;
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
