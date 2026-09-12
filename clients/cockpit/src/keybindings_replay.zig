//! Journal the admission decision at the original input callback, not a later
//! host response. Raw keys and generation IDs are evidence, never replay-time
//! authority: only the SDK's recorded effect result can admit a replay command.
//!
//! A dedicated Effects channel is consumed synchronously. It cannot drain the
//! app's unrelated completions, defer a command to another focused window, or
//! introduce nested platform events (which the SDK records innermost-first).
const std = @import("std");
const bindings = @import("config/keybindings.zig");

pub fn Journal(comptime sdk: type) type {
    return struct {
        const Self = @This();
        const Fx = sdk.Effects(Delivery);
        pub const Origin = struct {
            window_id: sdk.platform.WindowId,
            view_label: []const u8,
        };
        // SDK session_journal.encodeEvent(.shortcut): tag, two length-prefixed
        // strings, five modifier booleans, and window ID. Reuse that encoding.
        const max_shortcut_input = 1 + 4 + sdk.platform.max_shortcut_id_bytes + 4 + sdk.platform.max_shortcut_key_bytes + 5 + 8;
        // session_journal.encodeEvent(.gpu_surface_input), key_down subset:
        // tag/window/label/kind/time/pointer, six 32-bit fields, key/text,
        // absent composition cursor, five modifiers, and scale. Text is empty.
        const max_fallback_input = 1 + 8 + 4 + sdk.platform.max_view_label_bytes + 1 + 8 + 8 + 6 * 4 + 4 + sdk.platform.max_shortcut_key_bytes + 4 + 1 + 5 + 4;
        const max_input = @max(max_shortcut_input, max_fallback_input);
        const max_payload: usize = 2 + @as(usize, bindings.max_id_bytes) + max_input;
        const Delivery = struct {
            bytes: [max_payload]u8 = @splat(0),
            len: usize = 0,
            valid: bool = false,
        };

        effects: ?*Fx = null,
        allocator: ?std.mem.Allocator = null,
        handle: ?sdk.ChannelHandle = null,
        key: u64 = 0,
        replaying: bool = false,
        diverged: bool = false,
        command: bindings.Text(bindings.max_id_bytes) = .{},

        /// Call from ReplayControl.arm BEFORE start. No platform service is
        /// ever bound to this channel, including during a live recording.
        pub fn armReplay(self: *Self) !void {
            if (self.effects != null) return error.AdmissionAlreadyStarted;
            self.replaying = true;
        }

        /// The key is reserved by the embedding app alongside its other effect
        /// keys. Registry and channel creation precede all user input records.
        pub fn start(self: *Self, allocator: std.mem.Allocator, key: u64, recorder: ?*sdk.runtime.SessionRecorder) !void {
            if (self.effects != null) return error.AdmissionAlreadyStarted;
            const effects = try allocator.create(Fx);
            effects.* = Fx.init(allocator);
            errdefer allocator.destroy(effects);
            errdefer effects.deinit();
            if (self.replaying) effects.armReplay();
            if (recorder) |value| effects.bindJournal(value.effectJournal());
            const handle = effects.openChannel(.{ .key = key, .on_event = delivered, .max_pending = 1 });
            if (!self.replaying and !handle.live()) return error.AdmissionChannelUnavailable;
            self.effects = effects;
            self.allocator = allocator;
            self.key = key;
            self.handle = handle;
        }

        /// Keep this journal alive through ReplayControl.finish: the SDK sends
        /// that control AFTER app_shutdown. Release it in the outer teardown.
        pub fn deinit(self: *Self) void {
            const effects = self.effects orelse return;
            effects.deinit();
            self.allocator.?.destroy(effects);
            self.* = .{};
        }

        /// Return true only for this channel's feed. Parent forwards every
        /// other ReplayControl.feed unchanged to the UiApp's effects executor.
        pub fn feed(self: *Self, record: sdk.runtime.EffectResultRecord) !bool {
            if (record.key != self.key) return false;
            if (!self.replaying or record.kind != .channel) return self.refuse();
            const effects = self.effects orelse return self.refuse();
            try effects.feedChannelEvent(record.key, record.channel_kind, record.payload, record.dropped, record.channel_dropped_total);
            return true;
        }

        pub fn finishReplay(self: *Self) !void {
            if (self.diverged) return error.ReplayAdmissionMismatch;
            const effects = self.effects orelse return;
            try effects.settleReplayFeeds();
            try effects.finishReplay();
        }

        /// Live generation validation remains mandatory. Replay never consults
        /// keys.applied (host requests are parked and live config is irrelevant).
        /// Rejections are recorded too, so missing decisions fail explicitly.
        pub fn shortcut(self: *Self, keys: anytype, event: sdk.platform.ShortcutEvent) !?[]const u8 {
            return self.shortcutWithPermission(keys, event, true);
        }

        /// A host registration-repair failure refuses command admission, not
        /// ordinary typing. Journal that refusal without altering installed state.
        pub fn shortcutWithPermission(self: *Self, keys: anytype, event: sdk.platform.ShortcutEvent, allowed: bool) !?[]const u8 {
            const candidate = if (self.replaying or !allowed) null else keys.commandForShortcut(event);
            return self.exchange(&keys.registry, .{ .shortcut = event }, candidate);
        }

        /// Call at the same onKey fallback boundary on live and replay paths.
        /// Scope origin from the enclosing canvas_widget_keyboard event around
        /// UiApp.event and restore it on return. on_key alone omits the origin;
        /// replay must prove the callback still belongs to BOTH its window/view.
        pub fn fallback(self: *Self, keys: anytype, event: sdk.canvas.WidgetKeyboardEvent, origin: Origin) !?[]const u8 {
            return self.fallbackWithPermission(keys, event, origin, true);
        }

        pub fn fallbackWithPermission(self: *Self, keys: anytype, event: sdk.canvas.WidgetKeyboardEvent, origin: Origin, allowed: bool) !?[]const u8 {
            if (!isFallbackKey(event)) return null;
            if (!validOrigin(origin)) return self.refuse();
            const candidate = if (self.replaying or !allowed) null else keys.commandForEvent(event);
            return self.exchange(&keys.registry, .{ .gpu_surface_input = .{
                .window_id = origin.window_id,
                .label = origin.view_label,
                .kind = .key_down,
                .key = event.key,
                .modifiers = .{ .command = event.modifiers.super, .control = event.modifiers.control, .option = event.modifiers.alt, .shift = event.modifiers.shift },
            } }, candidate);
        }

        fn isFallbackKey(event: sdk.canvas.WidgetKeyboardEvent) bool {
            return event.phase == .key_down and event.modifiers.super and event.key.len <= sdk.platform.max_shortcut_key_bytes;
        }

        fn validOrigin(origin: Origin) bool {
            return origin.window_id != 0 and origin.view_label.len > 0 and origin.view_label.len <= sdk.platform.max_view_label_bytes;
        }

        fn exchange(self: *Self, registry: *const bindings.Registry, event: sdk.platform.Event, candidate: ?[]const u8) !?[]const u8 {
            if (self.diverged) return error.ReplayAdmissionMismatch;
            const effects = self.effects orelse return error.AdmissionNotStarted;
            var input_buffer: [max_input]u8 = undefined;
            const input = try sdk.runtime.session_journal.encodeEvent(event, &input_buffer);
            if (!self.replaying) try self.post(input, candidate);
            return self.takeDecision(effects, registry, input);
        }

        fn takeDecision(self: *Self, effects: *Fx, registry: *const bindings.Registry, input: []const u8) !?[]const u8 {
            const result = effects.takeMsg() orelse return self.refuse();
            self.decode(registry, input, result) catch return self.refuse();
            if (self.command.len == 0) return null;
            return self.command.slice();
        }

        fn post(self: *Self, input: []const u8, candidate: ?[]const u8) !void {
            const command = candidate orelse "";
            if (command.len > bindings.max_id_bytes) return error.AdmissionCommandTooLong;
            var bytes: [max_payload]u8 = undefined;
            bytes[0] = 1;
            bytes[1] = @intCast(command.len);
            @memcpy(bytes[2..][0..command.len], command);
            @memcpy(bytes[2 + command.len ..][0..input.len], input);
            const handle = self.handle orelse return error.AdmissionNotStarted;
            if (handle.post(bytes[0 .. 2 + command.len + input.len]) != .accepted) return error.AdmissionChannelUnavailable;
        }

        fn decode(self: *Self, registry: *const bindings.Registry, input: []const u8, result: Delivery) !void {
            const payload = try decodePayload(&result);
            if (!std.mem.eql(u8, input, payload.input)) return error.InvalidAdmission;
            if (payload.command.len > 0 and registry.indexOf(payload.command) == null) return error.InvalidAdmission;
            self.command = try bindings.Text(bindings.max_id_bytes).init(payload.command);
        }

        fn decodePayload(result: *const Delivery) !struct { command: []const u8, input: []const u8 } {
            if (!result.valid or result.len < 2) return error.InvalidAdmission;
            if (result.bytes[0] != 1) return error.InvalidAdmission;
            const command_end = 2 + @as(usize, result.bytes[1]);
            if (command_end > result.len) return error.InvalidAdmission;
            return .{ .command = result.bytes[2..command_end], .input = result.bytes[command_end..result.len] };
        }

        fn refuse(self: *Self) error{ReplayAdmissionMismatch} {
            self.diverged = true;
            self.command = .{};
            return error.ReplayAdmissionMismatch;
        }

        fn delivered(event: sdk.EffectChannelEvent) Delivery {
            if (event.kind != .data) return .{};
            if (event.dropped_pending != 0 or event.dropped_total != 0) return .{};
            if (event.bytes.len > max_payload) return .{};
            var result: Delivery = .{ .len = event.bytes.len, .valid = true };
            @memcpy(result.bytes[0..event.bytes.len], event.bytes);
            return result;
        }
    };
}
