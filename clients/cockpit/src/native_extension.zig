//! Cockpit's TypeScript runner extension: the native side of the seam.
//!
//! The generated runner calls the three entry points at the bottom. Between
//! them this file installs one host-call binding for the `cockpit.` namespace
//! and keeps one `Engine` alive for the process. The core sends intents with
//! `Cmd.host("cockpit.intent")`, asks for state with
//! `Cmd.request("cockpit.snapshot")`, and hears that state moved on the
//! channel it opened under `protocol.event_channel_key`.
//!
//! Every crossing happens on the effects loop thread. A snapshot request is
//! answered through the binding's poll seam rather than by feeding the result
//! from inside the request callback, because that seam is the one the runtime
//! documents for completions and the one that copies bytes on delivery.

const std = @import("std");
const result_wire = cockpit.command_results;
test {
    _ = @import("tests/shipping_pointer_tests.zig");
}
const native_sdk = @import("native_sdk");
const core = @import("core");
const cockpit = @import("cockpit_engine");

const Engine = cockpit.Engine;
const protocol = cockpit.protocol;
const Adapter = native_sdk.TsUiApp(core);
const Effects = Adapter.Effects;
const canvas = native_sdk.canvas;
const canvas_label = "phux-cockpit-canvas";

/// The effects the engine drives, with this graph's own result constructors
/// filled in: the engine names a clipboard verb and a key, and the answer
/// comes back as a core Msg this file builds. Everything else forwards.
const EngineFx = struct {
    effects: *Effects,

    pub fn hostSend(self: EngineFx, name: []const u8, payload: []const u8) void {
        self.effects.hostSend(name, payload);
    }
    pub fn ptySpawn(self: EngineFx, options: anytype) void {
        self.effects.ptySpawn(.{
            .key = options.key,
            .argv = options.argv,
            .cols = options.cols,
            .rows = options.rows,
            .on_event = options.on_event,
        });
    }
    pub fn ptyWrite(self: EngineFx, key: u64, bytes: []const u8) bool {
        return self.effects.ptyWrite(key, bytes);
    }
    pub fn ptyResize(self: EngineFx, key: u64, cols: u16, rows: u16) void {
        self.effects.ptyResize(key, cols, rows);
    }
    pub fn ptyKill(self: EngineFx, key: u64) void {
        self.effects.ptyKill(key);
    }
    pub fn cancel(self: EngineFx, key: u64) void {
        self.effects.cancel(key);
    }
    pub fn closeWindow(self: EngineFx, label: []const u8) void {
        self.effects.closeWindow(label);
    }
    pub fn quitApp(self: EngineFx) void {
        self.effects.quitApp();
    }
    pub fn showNotification(self: EngineFx, options: native_sdk.platform.NotificationOptions) void {
        self.effects.showNotification(options);
    }
    pub fn openUrl(self: EngineFx, url: []const u8) void {
        self.effects.openUrl(url);
    }
    pub fn toggleFullscreenWindow(self: EngineFx, label: []const u8) void {
        self.effects.toggleFullscreenWindow(label);
    }
    pub fn minimizeWindow(self: EngineFx, label: []const u8) void {
        self.effects.minimizeWindow(label);
    }
    pub fn writeClipboard(self: EngineFx, options: struct { key: u64, text: []const u8 }) void {
        self.effects.writeClipboard(.{ .key = options.key, .text = options.text, .on_result = clipboardWritten });
    }
    pub fn readClipboard(self: EngineFx, options: struct { key: u64 }) void {
        self.effects.readClipboard(.{ .key = options.key, .on_result = clipboardRead });
    }
    pub fn startTimer(self: EngineFx, options: anytype) void {
        self.effects.startTimer(.{
            .key = options.key,
            .interval_ms = options.interval_ms,
            .mode = options.mode,
            .on_fire = options.on_fire,
        });
    }
    pub fn writeFile(self: EngineFx, options: anytype) void {
        self.effects.writeFile(.{
            .key = options.key,
            .path = options.path,
            .bytes = options.bytes,
            .on_result = options.on_result,
        });
    }
    pub fn openChannel(self: EngineFx, options: anytype) native_sdk.ChannelHandle {
        return self.effects.openChannel(.{
            .key = options.key,
            .on_event = options.on_event,
            .max_pending = options.max_pending,
        });
    }
    pub fn closeChannel(self: EngineFx, key: u64) void {
        self.effects.closeChannel(key);
    }
    pub fn showWindow(self: EngineFx, label: []const u8) void {
        self.effects.showWindow(label);
    }
    pub fn phuxChannelLive(self: EngineFx) bool {
        const handle = self.effects.channelHandle(cockpit.phux_channel_key) orelse return false;
        return handle.live();
    }
    pub fn restartPhux(self: EngineFx, engine: *Engine) bool {
        return engine.restartNavigationConnection(self, phuxChannel);
    }
};

fn engineFx() ?EngineFx {
    const effects = bridge.effects orelse return null;
    return .{ .effects = effects };
}

const HostChannelBinding = channelBindingType();

fn channelBindingType() type {
    const field = @FieldType(native_sdk.HostCallBinding, "bind_channels_fn");
    const function = @typeInfo(@typeInfo(field).optional.child).pointer.child;
    return @typeInfo(function).@"fn".params[1].type.?;
}

const Bridge = struct {
    const InteractionMode = enum { terminal, palette, settings };
    /// Mirrors committed core modality, never the most recently painted view.
    /// The core's modal is app-wide even when presented in a secondary window.
    interaction_mode: InteractionMode = .terminal,
    engine: ?*Engine = null,
    channels: ?HostChannelBinding = null,
    /// The adapter's effects, known once the runner has built the app. Until
    /// then there is nothing to spawn shells through, and `spawnShells` is
    /// idempotent so the first frame catches up.
    effects: ?*Effects = null,
    /// Tests that want no child processes clear this before starting.
    shells: bool = true,
    /// The one snapshot completion in flight. A newer request overwrites an
    /// unpolled older one; the runtime cancels the replaced key itself.
    pending: bool = false,
    pending_key: u64 = 0,
    pending_ok: bool = false,
    pending_len: usize = 0,
    buffer: [cockpit.snapshot.max_bytes]u8 = undefined,
    navigation_pending: bool = false,
    navigation_key: u64 = 0,
    navigation_ok: bool = false,
    navigation_len: usize = 0,
    navigation_buffer: [cockpit.engine.navigation.max_bytes]u8 = undefined,
    /// The core serializes commands until this exact receipt is consumed.
    /// Snapshot/navigation requests have their own independently polled slots.
    command_pending: bool = false,
    command_key: u64 = 0,
    command_buffer: [cockpit.engine.tab_commands.receipt_len]u8 = undefined,
    result_pending: bool = false,
    result_key: u64 = 0,
    result_ok: bool = false,
    result_len: usize = 0,
    result_buffer: [result_wire.max_bytes]u8 = undefined,
    appearance: cockpit.engine.appearance.State = .{},
    appearance_pending: bool = false,
    appearance_key: u64 = 0,
    appearance_len: usize = 0,
    appearance_buffer: [cockpit.engine.appearance.max_bytes]u8 = undefined,
    /// Connect to Host (`cockpit.remote`): applied synchronously, answered
    /// through its own completion slot like navigation.
    remote_pending: bool = false,
    remote_key: u64 = 0,
    remote_ok: bool = false,
    remote_len: usize = 0,
    remote_buffer: [cockpit.remote_hosts.max_bytes]u8 = undefined,
    /// Kept for tests: the post outcomes the runtime handed back.
    posts_accepted: usize = 0,
    posts_unroutable: usize = 0,

    fn binding(self: *Bridge) native_sdk.HostCallBinding {
        return .{
            .context = self,
            .send_fn = send,
            .request_fn = request,
            .cancel_fn = cancel,
            .poll_fn = poll,
            .pending_fn = hasPending,
            .bind_channels_fn = bindChannels,
        };
    }

    fn send(context: *anyopaque, name: []const u8, payload: []const u8) void {
        const self: *Bridge = @ptrCast(@alignCast(context));
        if (std.mem.eql(u8, name, "cockpit.committed")) {
            self.commitInteraction(Adapter.Host.model());
            return;
        }
        if (!std.mem.eql(u8, name, protocol.intent_command)) return;
        const engine = self.engine orelse return;
        if (engineFx()) |fx| {
            _ = engine.applyIntent(payload, fx);
            self.spawnShells(engine, fx);
            engine.noteTopologyChange(fx, topologyTimer);
        } else {
            _ = engine.applyIntent(payload, &cockpit.NoShells{});
        }
        self.announce(engine);
    }

    /// The SDK commits Host.model before walking the returned command batch.
    /// Core modality transitions place this marker before their other effects;
    /// app_state.model is still the OLD mirror here and must never be read.
    fn commitInteraction(self: *Bridge, model: *const core.Model) void {
        self.interaction_mode = interactionMode(model);
        const engine = self.engine orelse return;
        const fx = engineFx() orelse return;
        engine.setInputSuspended(fx, self.interaction_mode != .terminal);
    }

    fn interactionMode(model: *const core.Model) InteractionMode {
        // Connect to Host takes text like the switcher; neither may type
        // into a terminal behind it.
        return if (model.paletteOpen or model.hostOpen) .palette else if (model.settingsOpen) .settings else .terminal;
    }

    /// Host sends are intentionally suppressed by the SDK during replay.
    /// Read the committed mode for replayed fallback keys, but never call the
    /// live engine's focus/capture/provider effects from this recovery path.
    fn replayInteraction(self: *Bridge) bool {
        const effects = self.effects orelse return false;
        if (!effects.replayArmed()) return false;
        self.interaction_mode = interactionMode(Adapter.Host.model());
        return true;
    }

    fn spawnShells(self: *Bridge, engine: *Engine, fx: EngineFx) void {
        if (!self.shells) return;
        engine.spawnShells(fx, shellEvent);
    }

    /// Tell the core that state moved. Silence when the channel is not open
    /// yet is correct: the core requests a snapshot at boot regardless, and
    /// an intent it sent before opening the channel cannot exist.
    fn announce(self: *Bridge, engine: *const Engine) void {
        const channels = self.channels orelse {
            self.posts_unroutable += 1;
            return;
        };
        const handle = channels.acquire_fn(channels.context, protocol.event_channel_key) orelse {
            self.posts_unroutable += 1;
            return;
        };
        const bytes = engine.invalidation();
        switch (handle.post(&bytes)) {
            .accepted => self.posts_accepted += 1,
            else => self.posts_unroutable += 1,
        }
    }

    fn request(context: *anyopaque, name: []const u8, key: u64, payload: []const u8) void {
        if (std.mem.eql(u8, name, result_wire.request_name)) {
            const result_bridge: *Bridge = @ptrCast(@alignCast(context));
            return result_bridge.requestResults(key, payload);
        }
        if (std.mem.eql(u8, name, cockpit.engine.appearance.request_name)) {
            const appearance_bridge: *Bridge = @ptrCast(@alignCast(context));
            return appearance_bridge.requestAppearance(key, payload);
        }
        if (std.mem.eql(u8, name, cockpit.engine.tab_commands.request_name)) {
            const command_bridge: *Bridge = @ptrCast(@alignCast(context));
            return command_bridge.requestTabCommand(key, payload);
        }
        if (std.mem.eql(u8, name, cockpit.remote_hosts.request_name)) {
            const remote_bridge: *Bridge = @ptrCast(@alignCast(context));
            return remote_bridge.requestRemote(key, payload);
        }
        if (std.mem.eql(u8, name, cockpit.engine.navigation.request_name)) {
            const navigation_bridge: *Bridge = @ptrCast(@alignCast(context));
            return navigation_bridge.requestNavigation(key, payload);
        }
        const self: *Bridge = @ptrCast(@alignCast(context));
        self.pending = true;
        self.pending_key = key;
        if (!std.mem.eql(u8, name, protocol.snapshot_request)) {
            self.pending_ok = false;
            self.pending_len = copyInto(&self.buffer, "unknown cockpit request");
            return;
        }
        const engine = self.engine orelse {
            self.pending_ok = false;
            self.pending_len = copyInto(&self.buffer, "engine unavailable");
            return;
        };
        const bytes = engine.snapshot(&self.buffer) catch {
            self.pending_ok = false;
            self.pending_len = copyInto(&self.buffer, "snapshot too large");
            return;
        };
        self.pending_ok = true;
        self.pending_len = bytes.len;
    }

    fn requestNavigation(self: *Bridge, key: u64, payload: []const u8) void {
        self.navigation_pending = true;
        self.navigation_key = key;
        self.navigation_ok = false;
        const engine = self.engine orelse {
            self.navigation_len = copyInto(&self.navigation_buffer, "engine unavailable");
            return;
        };
        const bytes = engine.navigationSnapshot(payload, &self.navigation_buffer) catch |err| {
            self.navigation_len = copyInto(&self.navigation_buffer, @errorName(err));
            return;
        };
        self.navigation_ok = true;
        self.navigation_len = bytes.len;
    }

    fn requestRemote(self: *Bridge, key: u64, payload: []const u8) void {
        self.remote_pending = true;
        self.remote_key = key;
        self.remote_ok = false;
        const engine = self.engine orelse {
            self.remote_len = copyInto(&self.remote_buffer, "engine unavailable");
            return;
        };
        const answered = if (engineFx()) |fx|
            cockpit.remote_hosts.handle(engine, fx, payload, &self.remote_buffer)
        else
            cockpit.remote_hosts.handle(engine, &cockpit.NoShells{}, payload, &self.remote_buffer);
        const bytes = answered catch |err| {
            self.remote_len = copyInto(&self.remote_buffer, @errorName(err));
            return;
        };
        self.remote_ok = true;
        self.remote_len = bytes.len;
        // A connect or return-to-local retargets the provider, and the core
        // resyncs from the snapshot. A status read moves nothing; announcing
        // it would buy a snapshot whose arrival asks for status again.
        const decoded = cockpit.remote_hosts.decode(payload) catch return;
        if (decoded.kind != .status) self.announce(engine);
    }

    fn requestTabCommand(self: *Bridge, key: u64, payload: []const u8) void {
        // A broken caller cannot overwrite an unconsumed result or apply a
        // second command. The shipping core keeps exactly one request in flight.
        if (self.command_pending) return;
        self.command_pending = true;
        self.command_key = key;
        const engine = self.engine orelse {
            const parsed = cockpit.engine.tab_commands.decode(payload);
            self.command_buffer = (cockpit.engine.tab_commands.Receipt{
                .id = if (parsed) |command| command.id else 0,
                .reason = .unavailable,
                .status = .rejected,
            }).encode();
            return;
        };
        self.command_buffer = if (engineFx()) |fx|
            engine.applySelectionCommand(payload, fx).encode()
        else
            engine.applyTabCommand(payload).encode();
        if (engineFx()) |fx| {
            self.spawnShells(engine, fx);
            engine.noteTopologyChange(fx, topologyTimer);
        }
        self.announce(engine);
    }

    fn requestResults(self: *Bridge, key: u64, payload: []const u8) void {
        if (self.result_pending) return;
        self.result_pending = true;
        self.result_key = key;
        self.result_ok = false;
        const acknowledgement = result_wire.decodeAck(payload) catch {
            self.result_len = copyInto(&self.result_buffer, "invalid result acknowledgement");
            return;
        };
        const engine = self.engine orelse {
            self.result_len = copyInto(&self.result_buffer, "engine unavailable");
            return;
        };
        if (acknowledgement) |ack| self.ackResult(engine, ack);
        self.result_ok = true;
        if (engine.creation.peekCompletion()) |result| {
            self.result_len = result_wire.encodeResult(.creation, result, &self.result_buffer).len;
            return;
        }
        if (engine.model.shared_mutations.peekCompletion()) |result| {
            const source: result_wire.Source = if (result.origin == .native) .native_shared else .shared;
            self.result_len = result_wire.encodeResult(source, result, &self.result_buffer).len;
            return;
        }
        self.result_len = copyInto(&self.result_buffer, &result_wire.empty);
    }

    fn ackResult(_: *Bridge, engine: *Engine, ack: result_wire.Ack) void {
        switch (ack.source) {
            .creation => _ = engine.creation.ackCompletion(ack.command_id),
            .shared => _ = engine.model.shared_mutations.ackCompletion(ack.command_id),
            .native_shared => _ = engine.model.shared_mutations.ackNativeCompletion(ack.command_id),
        }
    }

    fn copyInto(buffer: []u8, text: []const u8) usize {
        @memcpy(buffer[0..text.len], text);
        return text.len;
    }

    fn requestAppearance(self: *Bridge, key: u64, payload: []const u8) void {
        self.appearance_pending = true;
        self.appearance_key = key;
        self.appearance_len = 0;
        const engine = self.engine orelse return;
        // Probe inside Begin. A separate revision-fenced host command races
        // the request's revision increment in the effects batch.
        if (std.mem.eql(u8, payload, &.{ 1, 0, 0 })) _ = engine.probeConfig();
        self.appearance.apply(engine.model, payload);
        self.appearance_len = self.appearance.encode(engine.model, &self.appearance_buffer).len;
        engine.sequence +%= 1;
        engine.revision +%= 1;
        self.announce(engine);
    }

    fn cancel(context: *anyopaque, key: u64) void {
        const self: *Bridge = @ptrCast(@alignCast(context));
        if (self.pending and self.pending_key == key) self.pending = false;
        if (self.navigation_pending and self.navigation_key == key) self.navigation_pending = false;
        if (self.command_pending and self.command_key == key) self.command_pending = false;
        if (self.result_pending and self.result_key == key) self.result_pending = false;
        if (self.appearance_pending and self.appearance_key == key) self.appearance_pending = false;
        if (self.remote_pending and self.remote_key == key) self.remote_pending = false;
    }

    fn poll(context: *anyopaque) ?native_sdk.HostCallCompletion {
        const self: *Bridge = @ptrCast(@alignCast(context));
        if (self.appearance_pending) {
            self.appearance_pending = false;
            return .{ .key = self.appearance_key, .ok = self.appearance_len != 0, .bytes = self.appearance_buffer[0..self.appearance_len] };
        }
        if (self.command_pending) {
            self.command_pending = false;
            return .{ .key = self.command_key, .ok = true, .bytes = &self.command_buffer };
        }
        if (self.result_pending) {
            self.result_pending = false;
            return .{ .key = self.result_key, .ok = self.result_ok, .bytes = self.result_buffer[0..self.result_len] };
        }
        if (!self.pending) return self.pollNavigation();
        self.pending = false;
        return .{ .key = self.pending_key, .ok = self.pending_ok, .bytes = self.buffer[0..self.pending_len] };
    }

    fn pollNavigation(self: *Bridge) ?native_sdk.HostCallCompletion {
        if (!self.navigation_pending) return self.pollRemote();
        self.navigation_pending = false;
        return .{ .key = self.navigation_key, .ok = self.navigation_ok, .bytes = self.navigation_buffer[0..self.navigation_len] };
    }

    /// Last, so every pre-existing completion keeps its delivery order: the
    /// core's remote-status request is additive to the seam, never ahead of it.
    fn pollRemote(self: *Bridge) ?native_sdk.HostCallCompletion {
        if (!self.remote_pending) return null;
        self.remote_pending = false;
        return .{ .key = self.remote_key, .ok = self.remote_ok, .bytes = self.remote_buffer[0..self.remote_len] };
    }

    fn hasPending(context: *anyopaque) bool {
        const self: *Bridge = @ptrCast(@alignCast(context));
        return self.pending or self.navigation_pending or self.command_pending or self.result_pending or self.appearance_pending or self.remote_pending;
    }

    fn bindChannels(context: *anyopaque, channels: HostChannelBinding) void {
        const self: *Bridge = @ptrCast(@alignCast(context));
        self.channels = channels;
    }
};

var bridge = Bridge{};

test "appearance preview is reversible and preserves unnamed themes and explicit overrides" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const model = engine.model;
    model.config.background = .{ .r = 22, .g = 33, .b = 44 };
    model.font_size_offset = 2;
    const initial_size = model.fontSize();
    const initial_topology = model.topologyFingerprint();
    var state: cockpit.engine.appearance.State = .{};
    state.apply(model, &.{ 1, 0, 0 });
    state.apply(model, &.{ 1, 1, 2 });
    state.apply(model, &.{ 1, 2, 0 });
    state.apply(model, &.{ 1, 4, 1 });
    state.apply(model, &.{ 1, 5, 1 });
    try std.testing.expect(model.config.theme.slice().len > 0);
    try std.testing.expect(model.fontSize() > initial_size);
    try std.testing.expectEqual(.side, model.tab_placement);
    try std.testing.expectEqual(initial_topology, model.topologyFingerprint());
    try std.testing.expectEqual(.top, (try model.topologySnapshot()).tab_placement);
    const pane = model.provider.terminal(model.focusedTerminalRef().?).?;
    try std.testing.expectEqual(.bar, pane.session.term.cursor.default_style);
    state.apply(model, &.{ 1, 0, 0 }); // repeated open cannot move the rollback point
    state.apply(model, &.{ 1, 6, 0 });
    try std.testing.expectEqualStrings("", model.config.theme.slice());
    try std.testing.expectEqual(initial_size, model.fontSize());
    try std.testing.expectEqual(.block, model.config.cursor_style);
    try std.testing.expectEqual(.block, pane.session.term.cursor.default_style);
    try std.testing.expectEqual(.top, model.tab_placement);
    try std.testing.expectEqual(@as(u8, 22), model.config.background.?.r);
    try std.testing.expectEqual(.canceled, state.outcome);
}

test "appearance save edits only changed keys and keeps a failed save cancellable" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.testing.io;
    const original = "# my theme\nbackground = #010203\ntheme = nord\ncustom-future-key = untouched\n";
    try tmp.dir.writeFile(io, .{ .sub_path = "config", .data = original });
    const path = try tmp.dir.realPathFileAlloc(io, "config", std.testing.allocator);
    defer std.testing.allocator.free(path);
    const model = engine.model;
    model.config_file.setPath(path);
    var state: cockpit.engine.appearance.State = .{};
    state.apply(model, &.{ 1, 0, 0 });
    state.apply(model, &.{ 1, 2, 0 });
    var file = try tmp.dir.openFile(io, "config", .{});
    var buffer: [1024]u8 = undefined;
    var length = try file.readPositionalAll(io, &buffer, 0);
    file.close(io);
    try std.testing.expectEqualStrings(original, buffer[0..length]);
    state.apply(model, &.{ 1, 7, 0 });
    try std.testing.expectEqual(.saved, state.outcome);
    try std.testing.expectEqual(@as(f32, 14), model.config.font_size);
    try std.testing.expectEqual(@as(f32, 0), model.font_size_offset);
    file = try tmp.dir.openFile(io, "config", .{});
    length = try file.readPositionalAll(io, &buffer, 0);
    file.close(io);
    try std.testing.expect(std.mem.startsWith(u8, buffer[0..length], original));
    try std.testing.expect(std.mem.indexOf(u8, buffer[0..length], "font-size = 14") != null);
    model.config_file.setPath("/dev/null/impossible/config");
    state.apply(model, &.{ 1, 0, 0 });
    state.apply(model, &.{ 1, 2, 0 });
    state.apply(model, &.{ 1, 7, 0 });
    try std.testing.expectEqual(.refused, state.outcome);
    try std.testing.expect(state.initial != null);
    state.apply(model, &.{ 1, 6, 0 });
    try std.testing.expectEqual(@as(f32, 14), model.fontSize());
}

test "appearance refuses malformed configs and dangling links without replacing them" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.testing.io;
    const original = "font-size = not-a-number\n";
    try tmp.dir.writeFile(io, .{ .sub_path = "config", .data = original });
    const path = try tmp.dir.realPathFileAlloc(io, "config", std.testing.allocator);
    defer std.testing.allocator.free(path);
    engine.model.config_file.setPath(path);
    var state: cockpit.engine.appearance.State = .{};
    state.apply(engine.model, &.{ 1, 0, 0 });
    state.apply(engine.model, &.{ 1, 2, 0 });
    state.apply(engine.model, &.{ 1, 7, 0 });
    try std.testing.expectEqual(.refused, state.outcome);
    const file = try tmp.dir.openFile(io, "config", .{});
    var buffer: [128]u8 = undefined;
    const length = try file.readPositionalAll(io, &buffer, 0);
    file.close(io);
    try std.testing.expectEqualStrings(original, buffer[0..length]);
    try tmp.dir.deleteFile(io, "config");
    try tmp.dir.symLink(io, "missing-target", "config", .{});
    state.apply(engine.model, &.{ 1, 7, 0 });
    try std.testing.expectEqual(.refused, state.outcome);
    const link_length = try tmp.dir.readLink(io, "config", &buffer);
    try std.testing.expectEqualStrings("missing-target", buffer[0..link_length]);
    state.apply(engine.model, &.{ 1, 6, 0 });
    try std.testing.expectEqual(@as(f32, 13), engine.model.fontSize());
}

test "appearance creates an absent configuration and restores system-following mode on cancel" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const io = std.testing.io;
    const parent = try tmp.dir.realPathFileAlloc(io, ".", std.testing.allocator);
    defer std.testing.allocator.free(parent);
    const path = try std.fs.path.join(std.testing.allocator, &.{ parent, "nested/config" });
    defer std.testing.allocator.free(path);
    engine.model.config_file.setPath(path);
    engine.model.config.follow_system_theme = true;
    var state: cockpit.engine.appearance.State = .{};
    state.apply(engine.model, &.{ 1, 0, 0 });
    state.apply(engine.model, &.{ 1, 1, 3 });
    try std.testing.expect(!engine.model.config.follow_system_theme);
    state.apply(engine.model, &.{ 1, 6, 0 });
    try std.testing.expect(engine.model.config.follow_system_theme);
    state.apply(engine.model, &.{ 1, 0, 0 });
    state.apply(engine.model, &.{ 1, 1, 3 });
    state.apply(engine.model, &.{ 1, 7, 0 });
    try std.testing.expectEqual(.saved, state.outcome);
    const file = try tmp.dir.openFile(io, "nested/config", .{});
    defer file.close(io);
    var buffer: [128]u8 = undefined;
    const length = try file.readPositionalAll(io, &buffer, 0);
    try std.testing.expectEqualStrings("theme = nord\n", buffer[0..length]);
}

// ------------------------------------------------- native seams (no TS)

/// The pty event constructor the adapter's effects call for every shell
/// event. The bytes are consumed here, into the pane's emulator, and the
/// core receives only a void wake so it re-renders; no terminal byte enters
/// the compiled core.
fn shellEvent(event: native_sdk.EffectPtyEvent) core.Msg {
    if (bridge.engine) |engine| {
        if (engineFx()) |fx| {
            if (engine.onShellEvent(fx, event)) bridge.announce(engine);
            engine.noteTopologyChange(fx, topologyTimer);
        }
    }
    return .engine_wake;
}

fn topologyTimer(_: native_sdk.EffectTimer) core.Msg {
    if (bridge.engine) |engine| {
        if (engineFx()) |fx| engine.persistTopology(fx, topologyWritten);
    }
    return .engine_wake;
}

fn topologyWritten(result: native_sdk.EffectFileResult) core.Msg {
    if (bridge.engine) |engine| {
        const before = engine.beginPublication();
        if (engineFx()) |fx| engine.topologyPersisted(result, fx, topologyTimer);
        if (engine.finishPublication(before)) bridge.announce(engine);
    }
    return .engine_wake;
}

fn clipboardWritten(event: native_sdk.EffectClipboardResult) core.Msg {
    if (bridge.engine) |engine| engine.onClipboardWritten(event.outcome == .ok);
    return .engine_wake;
}

fn clipboardRead(event: native_sdk.EffectClipboardResult) core.Msg {
    if (bridge.engine) |engine| {
        if (engineFx()) |fx| engine.onClipboardRead(fx, event.outcome == .ok, event.text);
    }
    return .engine_wake;
}

fn phuxChannel(event: native_sdk.EffectChannelEvent) core.Msg {
    if (bridge.engine) |engine| {
        if (engineFx()) |fx| {
            if (engine.onPhuxChannel(fx, event, phuxChannel)) bridge.announce(engine);
            engine.noteTopologyChange(fx, topologyTimer);
        }
    }
    return .engine_wake;
}

fn pointerChannel(event: native_sdk.EffectChannelEvent) core.Msg {
    if (bridge.engine) |engine| {
        if (engineFx()) |fx| engine.onPointerChannel(fx, event, pointerChannel);
    }
    return .engine_wake;
}

/// The app's activation is the terminal's focus: a bell that rings while
/// deactivated notifies, and deactivation strands every capture.
fn onLifecycle(event: native_sdk.LifecycleEvent) ?core.Msg {
    if (bridge.replayInteraction()) {
        registerReplayChannels(event);
        return null;
    }
    const engine = bridge.engine orelse return null;
    const fx = engineFx() orelse return null;
    switch (event) {
        .start => engine.startProviderChannels(fx, phuxChannel, pointerChannel),
        .activate => engine.setFocused(fx, true),
        .deactivate => engine.setFocused(fx, false),
        .stop => {
            engine.model.writeWorkspaceState(engine.model.provider.io);
            engine.stopProviderChannels(fx);
        },
        .frame => {},
    }
    return null;
}

/// Replay needs effect slots to consume recorded results, but must not start
/// the provider transport which normally follows channel registration.
fn registerReplayChannels(event: native_sdk.LifecycleEvent) void {
    if (event != .start) return;
    const engine = bridge.engine orelse return;
    if (engine.model.phux() == null) return;
    const fx = engineFx() orelse return;
    _ = fx.openChannel(.{ .key = cockpit.phux_channel_key, .on_event = phuxChannel, .max_pending = 1 });
}

/// Keys no markup widget claimed. The palette and settings surfaces are the
/// core's, so while either is open the shell must not see typing meant for
/// them; the bridge's committed interaction projection says which.
/// Keys the overlays answer to that no widget of theirs claims: Escape
/// dismisses, the arrows move the highlight, Enter commits the settings
/// surface (the switcher's Enter is its input's own on-submit). Delivered
/// as core Msgs, because the overlays are the core's; the shell never sees
/// them while an overlay owns the keyboard.
fn overlayKey(event: canvas.WidgetKeyboardEvent) ?core.Msg {
    if (event.phase == .key_up) return null;
    const key = event.key;
    const palette = bridge.interaction_mode == .palette;
    if (std.mem.eql(u8, key, "Escape")) return if (palette) .palette_close else .settings_close;
    if (std.mem.eql(u8, key, "ArrowDown")) return if (palette) .{ .palette_move = 1 } else .{ .settings_move = 1 };
    if (std.mem.eql(u8, key, "ArrowUp")) return if (palette) .{ .palette_move = -1 } else .{ .settings_move = -1 };
    if (std.mem.eql(u8, key, "Enter") and !palette) return .settings_commit;
    return null;
}

/// `native automate widget-key` enters through the canvas fallback rather
/// than the platform shortcut registrar. Map the product chords to the same
/// core messages as menus/real shortcuts so the driven and physical paths are
/// indistinguishable after this boundary.
fn primaryChord(event: canvas.WidgetKeyboardEvent) ?core.Msg {
    if (event.phase == .key_up or !event.modifiers.super) return null;
    const key = event.key;
    const shift = event.modifiers.shift;
    const control = event.modifiers.control;
    const alt = event.modifiers.alt;
    if (!control and !alt and !shift and std.ascii.eqlIgnoreCase(key, "t")) return core.commandMsg("terminal.new");
    if (!control and !alt and !shift and std.ascii.eqlIgnoreCase(key, "n")) return core.commandMsg("window.new");
    if (!control and !alt and !shift and std.ascii.eqlIgnoreCase(key, "w")) return core.commandMsg("terminal.close");
    if (!control and !alt and !shift and std.ascii.eqlIgnoreCase(key, "d")) return core.commandMsg("pane.split-right");
    if (!control and !alt and shift and std.ascii.eqlIgnoreCase(key, "d")) return core.commandMsg("pane.split-down");
    if (!control and !alt and !shift and std.ascii.eqlIgnoreCase(key, "f")) return core.commandMsg("terminal.find");
    if (!control and !alt and !shift and std.mem.eql(u8, key, ",")) return core.commandMsg("settings.open");
    if (!control and !alt and shift and std.ascii.eqlIgnoreCase(key, "p")) return core.commandMsg("tabs.palette");
    if (control and !alt and !shift and std.ascii.eqlIgnoreCase(key, "f")) return core.commandMsg("window.fullscreen");
    return null;
}

fn onKey(event: canvas.WidgetKeyboardEvent) ?core.Msg {
    const replaying = bridge.replayInteraction();
    if (bridge.interaction_mode != .terminal) return overlayKey(event);
    if (primaryChord(event)) |msg| return msg;
    if (replaying) return null;
    const engine = bridge.engine orelse return null;
    const fx = engineFx() orelse return null;
    engine.onKey(fx, event);
    return null;
}

fn onText(event: canvas.WidgetKeyboardEvent) ?core.Msg {
    if (bridge.replayInteraction()) return null;
    if (bridge.interaction_mode != .terminal) return null;
    const engine = bridge.engine orelse return null;
    const fx = engineFx() orelse return null;
    engine.onText(fx, event);
    return null;
}

fn emptyInteraction(ui: *Adapter.Ui) Adapter.Ui.Node {
    return ui.el(.stack, .{ .grow = 1, .semantics = .{ .hidden = true } }, .{});
}

fn paneInteraction(
    ui: *Adapter.Ui,
    engine: *const Engine,
    workspace: anytype,
    node: cockpit.layout.NodeId,
) Adapter.Ui.Node {
    const current = workspace.selectedTreeConst() orelse return emptyInteraction(ui);
    const entry = current.node(node);
    switch (entry.kind) {
        .free => return emptyInteraction(ui),
        .leaf => {
            const terminal = entry.terminal orelse return emptyInteraction(ui);
            var title_room: [cockpit.snapshot.max_title_bytes]u8 = undefined;
            const title = cockpit.projection.terminalTitleInto(engine.model, terminal, &title_room);
            const screen = if (engine.model.provider.terminalConst(terminal)) |pane|
                pane.session.screenText()
            else if (engine.model.remotePresentation(terminal)) |presentation|
                presentation.grid.screen_text
            else
                "";
            return ui.el(.stack, .{
                // Focus belongs to the selected terminal, not the toolbar
                // control that created it. A provider-qualified key gives a
                // newly selected leaf its own mount/autofocus edge; keeping
                // the flag held does not steal focus on output-only rebuilds.
                .key = .{ .int = terminal.hash() },
                .autofocus = current.focus == node,
                .grow = 1,
                .min_width = cockpit.projection.split_pane_min_width,
                .min_height = cockpit.projection.split_pane_min_height,
                .opacity = 0,
                .text = screen,
                .semantics = .{
                    .role = .textbox,
                    .label = ui.fmt("{s}", .{title}),
                    .focusable = true,
                },
            }, .{});
        },
        .branch => {
            const first = paneInteraction(ui, engine, workspace, entry.first);
            const second = paneInteraction(ui, engine, workspace, entry.second);
            return ui.split(.{
                .grow = 1,
                .min_width = cockpit.projection.split_pane_min_width,
                .min_height = cockpit.projection.split_pane_min_height,
                .gap = cockpit.projection.split_divider_width,
                .split_axis = switch (entry.orientation) {
                    .horizontal => .horizontal,
                    .vertical => .vertical,
                },
                .value = entry.fraction,
                .opacity = 0,
                .semantics = .{ .label = "Terminal split" },
            }, .{ first, second });
        },
    }
}

/// Transparent native interaction leaves occupy exactly the rectangles the
/// authoritative projection gives the grid painter. Compiled `.native`
/// markup owns all visible chrome; this layer contributes pane semantics and
/// split topology over the app-owned libghostty surfaces beneath it.
fn terminalInteraction(ui: *Adapter.Ui, engine: *const Engine, window_index: usize) Adapter.Ui.Node {
    const workspace = engine.model.wsAtConst(window_index) orelse return emptyInteraction(ui);
    const chrome = cockpit.projection.workspaceChromeIn(engine.model, workspace, workspace.surface_size);
    const panes = if (workspace.selectedTreeConst()) |tree|
        paneInteraction(ui, engine, workspace, tree.root)
    else
        emptyInteraction(ui);
    const content = ui.row(.{ .height = chrome.content.height }, .{
        ui.el(.stack, .{ .width = chrome.content.x, .semantics = .{ .hidden = true } }, .{}),
        ui.el(.stack, .{ .width = chrome.content.width, .height = chrome.content.height }, .{panes}),
        ui.el(.stack, .{ .grow = 1, .semantics = .{ .hidden = true } }, .{}),
    });
    const search = if (chrome.search.height > 0)
        ui.el(.stack, .{
            .height = chrome.search.height,
            .opacity = 0,
            .semantics = .{ .role = .group, .label = "Scrollback search" },
        }, .{})
    else
        ui.el(.stack, .{ .height = 0, .semantics = .{ .hidden = true } }, .{});
    return ui.column(.{ .grow = 1 }, .{
        ui.el(.stack, .{ .height = chrome.search.y, .semantics = .{ .hidden = true } }, .{}),
        search,
        content,
        ui.el(.stack, .{ .grow = 1, .semantics = .{ .hidden = true } }, .{}),
    });
}

fn composeView(ui: *Adapter.Ui, model: *const core.Model, markup: Adapter.Ui.Node, window_index: usize) Adapter.Ui.Node {
    const engine = bridge.engine orelse return markup;
    if (engine.model.wsAtConst(window_index)) |workspace|
        syncTerminalSpace(model, window_index, workspace.surface_size, cockpit.projection.cockpitTokens(engine.model));
    if (model.paletteOpen or model.settingsOpen) return markup;
    return ui.el(.stack, .{ .grow = 1 }, .{
        terminalInteraction(ui, engine, window_index),
        markup,
    });
}

fn compiledWindow(ui: *Adapter.Ui, model: *const core.Model, window_index: usize) Adapter.Ui.Node {
    return switch (window_index) {
        0 => CompiledChrome.build(ui, model),
        1 => WindowView1.build(ui, model),
        2 => WindowView2.build(ui, model),
        3 => WindowView3.build(ui, model),
        else => WindowView4.build(ui, model),
    };
}

var terminal_space_measurements: usize = 0;

/// Only compiled markup enters this pass, never terminalInteraction: measuring
/// the slot therefore cannot feed terminal sizing back into chrome layout.
const ChromeSpace = struct {
    terminal: native_sdk.geometry.RectF,
    tab_strip_width: f32,
};

fn measureTerminalSpace(allocator: std.mem.Allocator, model: *const core.Model, window_index: usize, size: native_sdk.geometry.SizeF, tokens: canvas.DesignTokens) !ChromeSpace {
    if (@import("builtin").is_test) terminal_space_measurements += 1;
    var arena = std.heap.ArenaAllocator.init(allocator);
    defer arena.deinit();
    var ui = Adapter.Ui.init(arena.allocator());
    const tree = try ui.finalizeWithTokens(compiledWindow(&ui, model, window_index), tokens);
    const nodes = try arena.allocator().alloc(canvas.WidgetLayoutNode, canvas.max_layout_audit_nodes);
    const measured = try canvas.layoutWidgetTreeWithTokens(tree.root, .init(0, 0, size.width, size.height), tokens, nodes);
    var terminal: ?native_sdk.geometry.RectF = null;
    var tab_strip_width: f32 = 0;
    for (measured.nodes) |entry| {
        if (std.mem.eql(u8, entry.widget.semantics.label, "phux-terminal-space")) terminal = entry.frame;
        if (std.mem.eql(u8, entry.widget.semantics.label, "Terminal tabs")) tab_strip_width = entry.frame.width;
    }
    return .{ .terminal = terminal orelse return error.MissingTerminalSpace, .tab_strip_width = tab_strip_width };
}

fn syncTerminalSpace(model: *const core.Model, window_index: usize, size: native_sdk.geometry.SizeF, tokens: canvas.DesignTokens) void {
    const engine = bridge.engine orelse return;
    const workspace = engine.model.wsAt(window_index) orelse return;
    const space = measureTerminalSpace(std.heap.page_allocator, model, window_index, size, tokens) catch {
        workspace.shipping_terminal_space = .init(0, 0, 0, 0);
        workspace.shipping_terminal_size = .{};
        workspace.shipping_tab_strip_width = 0;
        return;
    };
    workspace.shipping_terminal_space = space.terminal;
    workspace.shipping_tab_strip_width = space.tab_strip_width;
    workspace.shipping_terminal_size = size;
}

/// composeView refreshes geometry whenever the committed chrome rebuilds.
/// GPU-only frames need a new measurement only when their physical size moves.
fn ensureTerminalSpace(model: *const core.Model, window_index: usize, size: native_sdk.geometry.SizeF, tokens: canvas.DesignTokens) void {
    const engine = bridge.engine orelse return;
    const workspace = engine.model.wsAtConst(window_index) orelse return;
    if (workspace.shipping_terminal_space != null and std.meta.eql(workspace.shipping_terminal_size, size)) return;
    syncTerminalSpace(model, window_index, size, tokens);
}

fn mainView(ui: *Adapter.Ui, model: *const core.Model) Adapter.Ui.Node {
    return composeView(ui, model, CompiledChrome.build(ui, model), 0);
}

fn windowView(ui: *Adapter.Ui, model: *const core.Model, label: []const u8) Adapter.Ui.Node {
    if (std.mem.eql(u8, label, "phux-window-1")) return composeView(ui, model, WindowView1.build(ui, model), 1);
    if (std.mem.eql(u8, label, "phux-window-2")) return composeView(ui, model, WindowView2.build(ui, model), 2);
    if (std.mem.eql(u8, label, "phux-window-3")) return composeView(ui, model, WindowView3.build(ui, model), 3);
    return composeView(ui, model, WindowView4.build(ui, model), 4);
}

fn onFrame(model: *const core.Model, frame: native_sdk.platform.GpuFrame) ?core.Msg {
    const engine = bridge.engine orelse return null;
    if (Engine.windowIndexForCanvas(frame.label)) |index| ensureTerminalSpace(model, index, frame.size, cockpit.projection.cockpitTokens(engine.model));
    const fx = engineFx() orelse return null;
    bridge.spawnShells(engine, fx);
    // The replay executor still needs PTY slots for recorded output. Only the
    // live viewport/provider pump is omitted after those slots are registered.
    if (fx.effects.replayArmed()) return null;
    // Every window's frame pumps its own workspace; the label says which.
    engine.pumpViewports(fx, frame);
    // A frame that moved the run (a resize, the first frame after a
    // placement flip) is announced like an intent: the core's chrome is
    // wrong until it resyncs, and only the engine knows.
    if (engine.refreshRun()) bridge.announce(engine);
    return null;
}

fn paintChrome(model: *const core.Model, builder: *canvas.Builder, size: native_sdk.geometry.SizeF, tokens: canvas.DesignTokens) anyerror!void {
    const engine = bridge.engine orelse return;
    // Chrome may be speculative while a fenced intent awaits its snapshot.
    // Measurement consumes that presentation; canonical placement is changed
    // only by the engine's intent/configuration paths.
    syncTerminalSpace(model, 0, size, tokens);
    return engine.paint(builder, size, tokens);
}

/// The per-window painter the runtime prefers when it is set: it alone says
/// WHICH window is being painted, and for an app whose terminals are chrome
/// commands the difference is a second window full of live cells versus a
/// tab strip over an empty canvas (the same note as the Zig app's).
fn paintChromeWindow(model: *const core.Model, builder: *canvas.Builder, context: Adapter.App.ChromeContext) anyerror!void {
    if (context.is_main) return paintChrome(model, builder, context.size, context.tokens);
    const engine = bridge.engine orelse return;
    if (Engine.windowIndexForCanvas(context.canvas_label)) |index| syncTerminalSpace(model, index, context.size, context.tokens);
    return engine.paintWindow(builder, context.canvas_label, context.window_id, context.size, context.tokens);
}

fn installEngine(options: *Adapter.CoreOptions, gpa: std.mem.Allocator, io: std.Io) void {
    bridge = .{};
    bridge.engine = Engine.create(gpa, io) catch null;
    options.host_calls = bridge.binding();
}

pub fn configureCoreOptions(options: *Adapter.CoreOptions, init: std.process.Init) void {
    bridge = .{};
    bridge.engine = Engine.createConfigured(std.heap.page_allocator, init) catch null;
    options.host_calls = bridge.binding();
}

fn configureOptionsValue(options: *Adapter.Options) void {
    options.view = mainView;
    options.markup = null;
    options.window_view = windowView;
    options.tokens_fn = struct {
        fn tokens(_: *const core.Model) canvas.DesignTokens {
            return cockpit.projection.baseTokens();
        }
    }.tokens;
    // Register the complete terminal family before the first frame. The TS
    // runner has no Zig host phase that can add the weighted faces later.
    options.fonts = &cockpit.scene.cockpit_fonts;
    options.chrome = .{
        .prefix_commands = cockpit.projection.chrome_command_envelope,
        .variable_prefix = true,
        .build = paintChrome,
        .build_window = paintChromeWindow,
    };
    options.on_key = onKey;
    options.key_release_events = true;
    options.on_text = onText;
    options.on_frame = onFrame;
    options.on_lifecycle = onLifecycle;
}

pub fn configureOptions(options: *Adapter.Options, init: std.process.Init) void {
    configureOptionsValue(options);
    options.fragment_watch = .{ .fragments = &compiled_fragments, .io = init.io };
}

/// Wraps the adapter's app to see the raw surface input before it: the
/// pointer kinds over a pane's frame are the engine's (selection, mouse
/// reporting, wheel scrollback, link hover), the way CockpitHost routes the
/// widget-routed ones. Chrome never lies under a pane frame, and an open
/// overlay keeps the pointer for the core. Every event still reaches the
/// inner app afterwards.
const PointerHost = struct {
    const workspace_timer_id = std.hash.Wyhash.hash(0, "phux-workspace-refresh");
    inner: native_sdk.App = undefined,
    selection_autoscroll_timer_active: bool = false,

    fn wrap(self: *PointerHost, inner: native_sdk.App) native_sdk.App {
        self.inner = inner;
        return .{
            .context = self,
            .name = inner.name,
            .source = inner.source,
            .source_fn = if (inner.source_fn != null) source else null,
            .scene_fn = if (inner.scene_fn != null) scene else null,
            // The adapter starts through its event callback; our timer still
            // needs its own start hook even when the inner hook is absent.
            .start_fn = start,
            .event_fn = if (inner.event_fn != null) event else null,
            .stop_fn = if (inner.stop_fn != null) stop else null,
            .replay_fn = if (inner.replay_fn != null) replay else null,
        };
    }
    fn source(context: *anyopaque) anyerror!native_sdk.WebViewSource {
        const self: *PointerHost = @ptrCast(@alignCast(context));
        return self.inner.webViewSource();
    }
    fn scene(context: *anyopaque) anyerror!native_sdk.ShellConfig {
        const self: *PointerHost = @ptrCast(@alignCast(context));
        return (try self.inner.scene()) orelse error.SceneUnavailable;
    }
    fn start(context: *anyopaque, runtime: *native_sdk.Runtime) anyerror!void {
        const self: *PointerHost = @ptrCast(@alignCast(context));
        try self.inner.start(runtime);
        if (comptime cockpit.phux_enabled) try runtime.startTimer(workspace_timer_id, std.time.ns_per_s, true);
    }
    fn event(context: *anyopaque, runtime: *native_sdk.Runtime, value: native_sdk.Event) anyerror!void {
        const self: *PointerHost = @ptrCast(@alignCast(context));
        defer _ = bridge.replayInteraction();
        defer self.syncSelectionAutoscrollTimer(runtime) catch {};
        const engine = bridge.engine;
        const before = if (engine) |current| current.beginPublication() else null;
        if (value == .timer and value.timer.id == workspace_timer_id and !bridge.replayInteraction()) {
            if (engine) |current| current.refreshWorkspace();
        }
        routeNativeInput(value);
        try self.inner.event(runtime, value);
        if (value == .canvas_widget_pointer) try self.focusTerminalAfterTabClick(runtime, value.canvas_widget_pointer);
        if (engine) |current| {
            if (current.finishPublication(before.?)) bridge.announce(current);
        }
    }
    fn stop(context: *anyopaque, runtime: *native_sdk.Runtime) anyerror!void {
        const self: *PointerHost = @ptrCast(@alignCast(context));
        if (comptime cockpit.phux_enabled) runtime.cancelTimer(workspace_timer_id) catch {};
        if (self.selection_autoscroll_timer_active) {
            runtime.cancelTimer(cockpit.selection_autoscroll_timer_id) catch {};
            self.selection_autoscroll_timer_active = false;
        }
        try self.inner.stop(runtime);
    }

    fn focusTerminalAfterTabClick(self: *PointerHost, runtime: *native_sdk.Runtime, routed: native_sdk.runtime.CanvasWidgetPointerEvent) !void {
        if (!activatedTab(routed)) return;
        // The SDK has already focused the clicked control and dispatched its
        // handler. Reselecting the same terminal has no autofocus edge, so
        // explicitly hand the keyboard back through its public focus action.
        // Overlay trees contain no terminal leaf and cannot lose editor focus.
        const layout = try runtime.canvasWidgetLayout(routed.window_id, routed.view_label);
        for (layout.nodes) |node| {
            if (node.widget.kind != .stack or node.widget.semantics.role != .textbox or !node.widget.autofocus) continue;
            _ = try runtime.dispatchCanvasWidgetAccessibilityAction(self.inner, routed.window_id, routed.view_label, .{
                .id = node.widget.id,
                .action = .focus,
            });
            return;
        }
    }
    fn replay(context: *anyopaque, control: native_sdk.runtime.ReplayControl) anyerror!void {
        const self: *PointerHost = @ptrCast(@alignCast(context));
        try self.inner.replayControl(control);
        _ = bridge.replayInteraction();
    }

    fn syncSelectionAutoscrollTimer(self: *PointerHost, runtime: *native_sdk.Runtime) !void {
        const engine = bridge.engine orelse return;
        const needed = engine.selectionAutoscrollActive();
        if (needed == self.selection_autoscroll_timer_active) return;
        if (needed) {
            try runtime.startTimer(cockpit.selection_autoscroll_timer_id, cockpit.selection_autoscroll_interval_ns, true);
        } else {
            try runtime.cancelTimer(cockpit.selection_autoscroll_timer_id);
        }
        self.selection_autoscroll_timer_active = needed;
    }
};

fn routeNativeInput(value: native_sdk.Event) void {
    if (bridge.replayInteraction()) return;
    const engine = bridge.engine orelse return;
    const fx = engineFx() orelse return;
    switch (value) {
        .command => |command| engine.adoptFocusedWindow(command.window_id),
        // A routed key names the window it was typed in: adopt it before the
        // key fallback resolves the focused pane, as CockpitHost adopts a
        // routed event's window. The raw echo is left to the pointer kinds.
        .canvas_widget_keyboard => |routed| {
            const index = Engine.windowIndexForCanvas(routed.view_label) orelse return;
            if (engine.model.windowOpen(index)) engine.model.active_window = index;
        },
        .gpu_surface_input => |raw| {
            switch (raw.kind) {
                .pointer_down, .pointer_up, .pointer_cancel, .pointer_move, .pointer_drag, .scroll => {},
                else => return,
            }
            const index = Engine.windowIndexForCanvas(raw.label) orelse return;
            if (bridge.interaction_mode != .terminal) return;
            if (!engine.model.windowOpen(index)) return;
            // A press in any window's grid makes that window the active one
            // before the pane under it is resolved, as CockpitHost adopts
            // the routed event's window.
            engine.model.active_window = index;
            if (engine.onPointer(fx, raw) == .geometry_changed) bridge.announce(engine);
            engine.noteTopologyChange(fx, topologyTimer);
        },
        .files_dropped => |drop| _ = engine.onDrop(fx, drop),
        .timer => |timer| if (timer.id == cockpit.selection_autoscroll_timer_id) {
            engine.selectionAutoscroll(fx);
        },
        else => {},
    }
}

var pointer_host = PointerHost{};

fn activatedTab(routed: native_sdk.runtime.CanvasWidgetPointerEvent) bool {
    const target = routed.press_target orelse return false;
    return routed.pointer.phase == .up and routed.pointer.button == 0 and
        routed.pointer.captured_id == target.id and target.role == .tab and
        target.bounds.normalized().containsPoint(routed.pointer.point);
}

pub fn app(app_state: *Adapter.App) native_sdk.App {
    bridge.effects = &app_state.effects;
    return pointer_host.wrap(app_state.app());
}

// ------------------------------------------------------------------ tests

test "shared workspace refresh timer starts with the shipping event-only adapter" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    const timer = rig.harness.null_platform.startedTimer(PointerHost.workspace_timer_id) orelse return error.TestExpectedWorkspaceTimer;
    try std.testing.expectEqual(std.time.ns_per_s, timer.interval_ns);
    try std.testing.expect(timer.repeats);
}

const test_views = [_]native_sdk.ShellView{
    .{ .label = canvas_label, .kind = .gpu_surface, .fill = true, .gpu_backend = .metal },
};
const test_windows = [_]native_sdk.ShellWindow{.{
    .label = "main",
    .title = "Phux Cockpit TS",
    .width = 1100,
    .height = 640,
    .views = &test_views,
}};
const test_scene: native_sdk.ShellConfig = .{ .windows = &test_windows };

test "shipping TypeScript graph registers the terminal family and Cockpit token ids" {
    var options: Adapter.Options = .{
        .name = "phux-cockpit",
        .scene = test_scene,
        .canvas_label = canvas_label,
        .markup = .{ .source = @embedFile("app.native") },
    };
    configureOptionsValue(&options);
    try std.testing.expectEqual(@as(usize, 4), options.fonts.len);
    const expected = [_]canvas.FontId{
        cockpit.scene.terminal_font_id,
        cockpit.scene.terminal_bold_font_id,
        cockpit.scene.terminal_italic_font_id,
        cockpit.scene.terminal_bold_italic_font_id,
    };
    for (expected, options.fonts) |id, registration| {
        try std.testing.expectEqual(id, registration.id);
        try std.testing.expect(registration.ttf.len > 4);
        try std.testing.expectEqualSlices(u8, &.{ 0, 1, 0, 0 }, registration.ttf[0..4]);
    }
    var unused_model: core.Model = undefined;
    const tokens = options.tokens_fn.?(&unused_model);
    try std.testing.expectEqual(cockpit.scene.terminal_font_id, tokens.typography.mono_font_id);
    try std.testing.expectEqual(cockpit.scene.terminal_bold_font_id, tokens.typography.mono_bold_font_id);
    try std.testing.expectEqual(cockpit.scene.terminal_italic_font_id, tokens.typography.mono_italic_font_id);
    try std.testing.expectEqual(cockpit.scene.terminal_bold_italic_font_id, tokens.typography.mono_bold_italic_font_id);
}

/// What the generated runner's src/windows registry does, for the rig: each
/// declared window label builds its own compiled markup over the core model.
const window_sources = [_]canvas.ui_markup.SourceFile{
    .{ .path = "components/cockpit-window.native", .source = @embedFile("windows/components/cockpit-window.native") },
    .{ .path = "phux-window-1.native", .source = @embedFile("windows/phux-window-1.native") },
    .{ .path = "phux-window-2.native", .source = @embedFile("windows/phux-window-2.native") },
    .{ .path = "phux-window-3.native", .source = @embedFile("windows/phux-window-3.native") },
    .{ .path = "phux-window-4.native", .source = @embedFile("windows/phux-window-4.native") },
};
const WindowView1 = canvas.CompiledMarkupImports(core.Model, core.Msg, "phux-window-1.native", &window_sources);
const WindowView2 = canvas.CompiledMarkupImports(core.Model, core.Msg, "phux-window-2.native", &window_sources);
const WindowView3 = canvas.CompiledMarkupImports(core.Model, core.Msg, "phux-window-3.native", &window_sources);
const WindowView4 = canvas.CompiledMarkupImports(core.Model, core.Msg, "phux-window-4.native", &window_sources);

test "secondary fragments build through the live markup interpreter" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    inline for (.{ WindowView1, WindowView2, WindowView3, WindowView4 }) |View| {
        var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
        defer arena.deinit();
        var ui = Adapter.Ui.init(arena.allocator());
        var view = canvas.MarkupView(core.Model, core.Msg).fromDocument(View.document);
        const root = try view.build(&ui, &rig.app_state.model);
        _ = try ui.finalize(root);
    }
}

fn testWindowView(ui: *Adapter.Ui, model: *const core.Model, label: []const u8) Adapter.Ui.Node {
    if (std.mem.eql(u8, label, "phux-window-1")) return WindowView1.build(ui, model);
    if (std.mem.eql(u8, label, "phux-window-2")) return WindowView2.build(ui, model);
    if (std.mem.eql(u8, label, "phux-window-3")) return WindowView3.build(ui, model);
    return WindowView4.build(ui, model);
}

const Rig = struct {
    app_state: *Adapter.App,
    decorated: native_sdk.App,
    harness: *native_sdk.TestHarness(),
    frame_index: u64 = 1,

    fn attachFixture(self: *Rig) !cockpit.TerminalRef {
        if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
        const engine = bridge.engine.?;
        var config = cockpit.startup.resolvePhuxConfig(.{}, .{ .socket = "/unused-fixture.sock", .session = "fixture" });
        const remote = (try cockpit.startup.createPhuxProviderFromConfig(std.testing.allocator, std.testing.io, &config)).?;
        cockpit.attachPhuxProvider(engine.model, remote);
        try remote.host.start("shipping-fixture");
        try std.testing.expect(remote.bridge.incoming.stage(@embedFile("tests/fixtures/hello.bin")));
        _ = phuxChannel(.{ .key = cockpit.phux_channel_key, .kind = .data, .bytes = &.{1} });
        remote.bridge.outgoing.reset();
        const attached = @embedFile("tests/fixtures/attached.bin");
        var offset: usize = 0;
        while (offset < attached.len) {
            const size = 4 + std.mem.readInt(u32, attached[offset..][0..4], .big);
            try std.testing.expect(remote.bridge.incoming.stage(attached[offset..][0..size]));
            offset += size;
        }
        _ = phuxChannel(.{ .key = cockpit.phux_channel_key, .kind = .data, .bytes = &.{1} });
        try std.testing.expectEqual(.attached, remote.state());
        try std.testing.expectEqual(@as(u32, 0), engine.model.shared_workspace.session);
        try std.testing.expect(remote.bridge.incoming.stage(@embedFile("providers/phux/fixtures/workspace_initial_metadata.bin")));
        try std.testing.expect(remote.bridge.incoming.stage(@embedFile("providers/phux/fixtures/workspace_initial_state.bin")));
        _ = phuxChannel(.{ .key = cockpit.phux_channel_key, .kind = .data, .bytes = &.{1} });
        try std.testing.expectEqual(@as(usize, 1), engine.model.remote_inventory_count);
        const ref = engine.model.focusedTerminalRef().?;
        try std.testing.expectEqual(.phux, ref.provider_id);
        try std.testing.expectEqual(.live, remote.presentation(ref).?.phase);
        remote.bridge.outgoing.reset();
        _ = self;
        return ref;
    }

    fn start() !Rig {
        return startWithPhux(false);
    }

    fn startWithPhux(want_phux: bool) !Rig {
        return startWithReplay(want_phux, false);
    }

    fn startWithReplay(want_phux: bool, replaying: bool) !Rig {
        var core_options: Adapter.CoreOptions = .{};
        installEngine(&core_options, std.testing.allocator, std.testing.io);
        try std.testing.expect(bridge.engine != null);
        if (want_phux) {
            if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
            var config = cockpit.startup.resolvePhuxConfig(.{}, .{
                .socket = "/tmp/phux-typescript-lifecycle-test.sock",
                .session = "configured-session",
            });
            const remote = (try cockpit.startup.createPhuxProviderFromConfig(
                std.testing.allocator,
                std.testing.io,
                &config,
            )) orelse return error.TestExpectedPhuxProvider;
            cockpit.attachPhuxProvider(bridge.engine.?.model, remote);
        }
        bridge.shells = replaying;
        var options: Adapter.Options = .{
            .name = "phux-cockpit",
            .scene = test_scene,
            .canvas_label = canvas_label,
            .markup = .{ .source = @embedFile("app.native") },
            .window_view = testWindowView,
            // The generated runner wires the core's exported commandMsg; a
            // quit-close window command must map through it.
            .on_command = core.commandMsg,
        };
        configureOptionsValue(&options);
        const app_state = try Adapter.create(std.heap.page_allocator, core_options, options);
        errdefer app_state.destroy();
        const decorated = app(app_state);
        if (replaying) try decorated.replayControl(.arm);
        const harness = try native_sdk.TestHarness().create(std.testing.allocator, .{
            .size = native_sdk.geometry.SizeF.init(1100, 640),
        });
        errdefer harness.destroy(std.testing.allocator);
        harness.null_platform.gpu_surfaces = true;
        try harness.start(decorated);
        try harness.runtime.dispatchPlatformEvent(decorated, .{ .gpu_surface_frame = .{
            .label = canvas_label,
            .size = native_sdk.geometry.SizeF.init(1100, 640),
            .scale_factor = 1,
            .frame_index = 1,
            .timestamp_ns = 1,
        } });
        // The frame request is what commits the widget tree; input routed
        // before it has no tree to fall through and reaches nothing.
        try harness.runtime.dispatchPlatformEvent(decorated, .frame_requested);
        // A press on the grid gives the surface keyboard focus, as a person's
        // first click does; unfocused input routes nowhere.
        try harness.runtime.dispatchPlatformEvent(decorated, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = canvas_label,
            .kind = .pointer_down,
            .x = 400,
            .y = 300,
        } });
        return .{ .app_state = app_state, .decorated = decorated, .harness = harness };
    }

    fn stop(self: *Rig) void {
        self.app_state.destroy();
        self.harness.destroy(std.testing.allocator);
        if (bridge.engine) |engine| engine.destroy();
        bridge = .{};
    }

    /// Drain the effects loop until the core has heard engine announcement
    /// `sequence` and shows `status`, or give up after a bounded number of
    /// wakes. Each seam crossing (completion, channel event, re-request)
    /// needs its own drain, and the status alone cannot be waited on: it
    /// already reads READY before a fresh announcement has been drained.
    fn settle(self: *Rig, sequence: i64, status: []const u8) !void {
        var wakes: usize = 0;
        while (wakes < 8) : (wakes += 1) {
            const model = self.app_state.model;
            if (model.engineSequence.lo == sequence and std.mem.eql(u8, model.status, status)) return;
            try self.harness.runtime.dispatchPlatformEvent(self.decorated, .wake);
        }
        std.debug.print("settled at sequence {d} status '{s}', wanted {d} '{s}'\n", .{
            self.app_state.model.engineSequence.lo,
            self.app_state.model.status,
            sequence,
            status,
        });
        return error.TestUnexpectedResult;
    }

    fn dispatch(self: *Rig, msg: core.Msg) !void {
        self.app_state.dispatch(&self.harness.runtime, 1, msg) catch |err| {
            std.debug.print("dispatch of {s} failed: {s}\n", .{ @tagName(msg), @errorName(err) });
            return err;
        };
    }

    fn settleNavigation(self: *Rig) !void {
        for (0..8) |_| {
            if (!self.app_state.model.paletteLoading) return;
            try self.harness.runtime.dispatchPlatformEvent(self.decorated, .wake);
        }
        return error.TestNavigationDidNotComplete;
    }

    fn settleAppearance(self: *Rig) !void {
        for (0..8) |_| {
            const model = self.app_state.model;
            if (!model.appearanceBusy and model.engineConnected and model.engineSequence.lo == bridge.engine.?.sequence) return;
            try self.harness.runtime.dispatchPlatformEvent(self.decorated, .wake);
        }
        return error.TestAppearanceDidNotComplete;
    }

    /// Present a frame at `size`; the engine re-derives the run and, when it
    /// moved, announces so the core resyncs. Settled either way.
    fn resize(self: *Rig, size: native_sdk.geometry.SizeF) !void {
        self.frame_index += 1;
        const before = self.app_state.model.engineSequence.lo;
        const engine = bridge.engine.?;
        const was = engine.currentRun();
        try self.harness.runtime.dispatchPlatformEvent(self.decorated, .{ .gpu_surface_frame = .{
            .label = canvas_label,
            .size = size,
            .scale_factor = 1,
            .frame_index = self.frame_index,
            .timestamp_ns = self.frame_index * 16_000_000,
        } });
        const now = engine.currentRun();
        const moved = was.first != now.first or was.count != now.count or was.extent != now.extent;
        try self.settle(if (moved) before + 1 else before, "READY");
    }

    /// Drive the real core and engine into `state`: the tab count through
    /// intents and resync, the placement and overlays through their Msgs.
    fn reach(self: *Rig, state: ChromeState) !void {
        if (self.app_state.model.settingsOpen) {
            try self.dispatch(.settings_close);
            try self.settleAppearance();
        }
        const engine = bridge.engine.?;
        while (engine.model.wsConst().tab_count < state.tabs) {
            const before = self.app_state.model.engineSequence.lo;
            try self.dispatch(.new_terminal);
            try self.settle(before + 1, "READY");
        }
        while (engine.model.wsConst().tab_count > state.tabs) {
            const before = self.app_state.model.engineSequence.lo;
            try self.dispatch(.close_selected_tab);
            try self.settle(before + 1, "READY");
        }
        try self.dispatch(.{ .select_tab = 0 });
        const before_select = self.app_state.model.engineSequence.lo;
        try self.settle(before_select + 1, "READY");
        if ((self.app_state.model.tabPlacement == .side) != (state.placement == .side)) {
            const before = self.app_state.model.engineSequence.lo;
            try self.dispatch(.toggle_tab_placement);
            try self.settle(before + 1, "READY");
        }
        // The overlays are mutually exclusive in the core as in the shipping
        // app; settings is asked for last so a "both" state ends on it.
        // Opening settings probes the config file through the seam, which
        // is one announcement to wait for.
        try self.dispatch(if (state.palette) .palette_open else .palette_close);
        if (state.palette) {
            try self.settleNavigation();
            if (state.tabs >= 5) {
                try std.testing.expectEqual(@as(usize, 4), self.app_state.model.paletteRows.len);
                try std.testing.expect(self.app_state.model.paletteNext);
            }
        }
        if (state.settings != self.app_state.model.settingsOpen) {
            try self.dispatch(if (state.settings) .settings_open else .settings_close);
            try self.settleAppearance();
        }
        if (state.settings) try self.dispatch(.{ .settings_section = state.settings_section });
        try std.testing.expectEqual(state.tabs, self.app_state.model.tabs.len);
    }
};

test "the core boots from the engine's snapshot, not from its own defaults" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const model = rig.app_state.model;
    try std.testing.expect(model.engineConnected);
    try std.testing.expectEqual(@as(usize, 1), model.tabs.len);
    try std.testing.expect(model.tabs[0].id != 0);
    try std.testing.expectEqual(@as(i64, 1), model.engineRevision.lo);
}

test "configured Phux attachment starts native provider lifecycle without replacing the ephemeral local terminal" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.startWithPhux(true);
    defer rig.stop();
    const engine = bridge.engine.?;

    try std.testing.expect(engine.model.phux() != null);
    try std.testing.expect(engine.model.pointer_state != null);
    // The test executor may refuse the socket worker; either a live channel or
    // the engine's explicit unavailable state proves startup attempted the
    // native source instead of leaving an attached provider inert.
    try std.testing.expect(
        rig.app_state.effects.channelHandle(cockpit.phux_channel_key) != null or
            engine.model.phux_connection_unavailable or
            engine.model.phux().?.state() != .new,
    );
    // Installing the global pointer monitor may be refused by the test host's
    // process permissions; its model state still exists and the production
    // start path attempts the native channel without exposing it to TS.
    // Attaching durable Phux work is discovery, not implicit focus or an
    // attempt to make the direct local PTY durable. Until ATTACH_READY admits
    // a remote identity, the initial terminal remains the local provider's.
    try std.testing.expectEqual(.local, engine.model.focusedTerminalRef().?.provider_id);
    try std.testing.expectEqual(@as(usize, 1), engine.model.provider.activeCount());
    try std.testing.expectEqual(@as(usize, 0), engine.model.remote_inventory_count);
}

test "TypeScript engine startup restores topology and cwd while applying config and tab precedence" {
    const root = ".zig-cache/ts-configured-startup-test";
    const state_path = root ++ "/workspace.state";
    const config_path = root ++ "/config";
    const cwd = std.Io.Dir.cwd();
    cwd.deleteTree(std.testing.io, root) catch {};
    defer cwd.deleteTree(std.testing.io, root) catch {};
    try cwd.createDirPath(std.testing.io, root);

    const seed = try Engine.create(std.testing.allocator, std.testing.io);
    seed.model.state.setPath(state_path);
    const split = protocol.encodeIntent(.{
        .kind = .native_command,
        .expected_revision = 1,
        .argument = 4,
        .window = 0,
    });
    try std.testing.expect(seed.applyIntent(&split, &cockpit.NoShells{}));
    seed.model.tab_placement = .side;
    const cwd_pane = seed.model.provider.terminal(seed.model.focusedTerminalRef().?).?;
    cwd_pane.session.feed("\x1b]7;file://host/tmp/restored-right\x1b\\");
    seed.model.writeWorkspaceState(std.testing.io);
    seed.destroy();

    const config = cockpit.startup.parseConfig(
        "command = tmux attach\n" ++
            "scrollback-limit = 1048576\n" ++
            "tab-placement = top\n",
    );
    const restored = try Engine.createResolvedConfigured(
        std.testing.allocator,
        std.testing.io,
        config,
        config_path,
        state_path,
        null,
    );
    defer restored.destroy();
    try std.testing.expectEqual(@as(usize, 2), restored.model.provider.activeCount());
    // Persisted placement beats config when no debug override exists.
    try std.testing.expectEqual(.side, restored.model.tab_placement);
    try std.testing.expectEqual(@as(usize, 1048576), restored.model.provider.max_scrollback_bytes);
    try std.testing.expectEqualStrings(config_path, restored.model.config_file.path());
    const plain = restored.model.provider.slot(0).argv;
    try std.testing.expectEqualStrings("exec tmux attach", plain[plain.len - 1]);
    const with_cwd = restored.model.provider.slot(1).argv;
    try std.testing.expect(std.mem.indexOf(u8, with_cwd[with_cwd.len - 1], "/tmp/restored-right") != null);

    const overridden = try Engine.createResolvedConfigured(
        std.testing.allocator,
        std.testing.io,
        config,
        config_path,
        state_path,
        "top",
    );
    defer overridden.destroy();
    // PHUX_COCKPIT_TABS is the final debug precedence edge.
    try std.testing.expectEqual(.top, overridden.model.tab_placement);
}

test "an intent moves the engine and the core resyncs to the new revision" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const before = bridge.posts_accepted;

    try rig.dispatch(.new_terminal);
    try std.testing.expectEqual(before + 1, bridge.posts_accepted);
    try std.testing.expectEqual(@as(usize, 2), bridge.engine.?.model.wsConst().tab_count);

    // The announcement, the re-request and the completion each ride one
    // drain; the status walks SYNCING back to READY as they land.
    try rig.settle(1, "READY");
    const model = rig.app_state.model;
    try std.testing.expectEqual(@as(usize, 2), model.tabs.len);
    try std.testing.expectEqual(@as(i64, 1), model.selectedTab);
    try std.testing.expectEqual(@as(i64, 2), model.engineRevision.lo);
    try std.testing.expect(model.tabs[0].id != model.tabs[1].id);
}

test "native menu commands split and close the focused pane through the engine seam" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;

    try std.testing.expect(core.commandMsg("terminal.new") != null);
    try std.testing.expect(core.commandMsg("terminal.find") != null);
    try std.testing.expect(core.commandMsg("window.fullscreen") != null);

    try rig.dispatch(core.commandMsg("pane.split-right").?);
    try rig.settle(1, "READY");
    try std.testing.expectEqual(@as(usize, 2), engine.model.provider.activeCount());

    try rig.dispatch(core.commandMsg("terminal.close").?);
    try rig.settle(2, "READY");
    try std.testing.expectEqual(@as(usize, 1), engine.model.provider.activeCount());
}

test "TypeScript topology changes use the shipping debounce and file effect" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const executor = rig.app_state.effects.executor;
    defer rig.app_state.effects.executor = executor;
    engine.model.state.setPath("/tmp/phux-cockpit-tests/ts-workspace.state");
    defer engine.model.state.setPath(null);

    rig.app_state.effects.executor = .fake;
    try rig.dispatch(.new_terminal);
    // Fake request effects park rather than invoke the bridge. Deliver the
    // actual compiled packet while keeping timer/file effects deterministic.
    const request = rig.app_state.effects.pendingHostAt(0).?;
    try std.testing.expectEqualStrings(cockpit.engine.tab_commands.request_name, request.name);
    Bridge.request(&bridge, request.name, request.key, request.payload);
    rig.app_state.effects.executor = executor;
    try rig.settle(1, "READY");
    try std.testing.expect(engine.model.state.pending);
    try std.testing.expectEqual(@as(usize, 1), rig.app_state.effects.pendingTimerCount());
    const timer = rig.app_state.effects.pendingTimerAt(0).?;
    try std.testing.expectEqual(cockpit.topology_persist_timer_key, timer.key);
    try std.testing.expectEqual(cockpit.topology_persist_debounce_ms, timer.interval_ms);

    try rig.app_state.effects.fireTimer(cockpit.topology_persist_timer_key);
    rig.app_state.effects.executor = .fake;
    try rig.app_state.drainEffects(&rig.harness.runtime);
    rig.app_state.effects.executor = executor;
    try std.testing.expect(engine.model.state.inflight);
    try std.testing.expectEqual(@as(usize, 1), rig.app_state.effects.pendingFileCount());
    const write = rig.app_state.effects.pendingFileAt(0).?;
    try std.testing.expectEqual(cockpit.topology_state_file_key, write.key);
    try std.testing.expectEqualStrings(engine.model.state.path(), write.path);
    try std.testing.expect(write.bytes.len > 0);

    try rig.app_state.effects.feedFileResult(cockpit.topology_state_file_key, .ok, "");
    try rig.app_state.drainEffects(&rig.harness.runtime);
    try std.testing.expect(!engine.model.state.inflight);
    try std.testing.expect(!engine.model.state.pending);
}

fn firstPaintedTabMessage(node: Adapter.Ui.Node) ?core.Msg {
    if (node.widget.semantics.role == .tab) {
        if (node.on_toggle) |msg| return msg;
        if (node.on_press) |msg| return msg;
    }
    for (node.nodes) |child| {
        if (firstPaintedTabMessage(child)) |msg| return msg;
    }
    return null;
}

fn firstPaintedCatalogMessage(node: Adapter.Ui.Node) ?core.Msg {
    if (node.on_press) |msg| {
        if (msg == .palette_pick) return msg;
    }
    for (node.nodes) |child| {
        if (firstPaintedCatalogMessage(child)) |msg| return msg;
    }
    return null;
}

test "navigation held painted catalog target follows metadata filtering and window movement" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const original = engine.model.focusedTerminalRef().?;
    for (0..4) |_| {
        try rig.dispatch(.new_terminal);
        try rig.settle(@intCast(engine.sequence), "READY");
    }
    try rig.dispatch(.palette_open);
    try rig.settleNavigation();
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    var ui = Adapter.Ui.init(arena.allocator());
    var held = firstPaintedCatalogMessage(mainView(&ui, &rig.app_state.model)).?;
    held.palette_pick = try arena.allocator().dupe(u8, held.palette_pick);
    // Replace the visible page; a held event still owns its original identity.
    try rig.dispatch(.palette_next);
    try rig.settleNavigation();
    try std.testing.expectEqual(@as(i64, 4), rig.app_state.model.paletteOffset);
    _ = shellEvent(.{ .key = engine.model.provider.terminal(original).?.pty_key, .kind = .output, .bytes = "\x1b]2;renamed\x07" });
    const second = engine.model.openWindow(1).?;
    engine.model.primary.dropTab(engine.model.primary.tabOfTerminal(original).?);
    try std.testing.expect(second.admitTab(original));
    engine.model.active_window = 0;
    engine.revision += 1;
    engine.sequence += 1;
    bridge.announce(engine);
    try rig.settle(@intCast(engine.sequence), "READY");
    try std.testing.expect(!engine.model.focusedTerminalRef().?.eql(original));
    const expected_command = rig.app_state.model.tabCommands.nextId.lo;
    try rig.dispatch(held);
    try std.testing.expectEqual(@as(usize, 1), engine.model.active_window);
    try std.testing.expect(engine.model.focusedTerminalRef().?.eql(original));
    try rig.settle(@intCast(engine.sequence), "READY");
    try std.testing.expectEqual(@as(i64, 2), rig.app_state.model.tabCommands.outcome);
    try std.testing.expectEqual(expected_command, rig.app_state.model.tabCommands.lastId.lo);
}

fn catalogCommandBytes(target: []const u8, id: u64, out: []u8) []const u8 {
    out[0] = 1;
    out[1] = 2;
    std.mem.writeInt(u64, out[2..10], id, .little);
    @memcpy(out[10..][0..target.len], target);
    return out[0 .. 10 + target.len];
}

test "navigation rejected captured identity preserves focus pending selection and receipt slot" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const targets = cockpit.engine.navigation.targets;
    const original = engine.model.focusedTerminalRef().?;
    const target = targets.capture(engine.model, .{ .available_terminal = original }).?;
    var target_buffer: [targets.max_len]u8 = undefined;
    var buffer: [10 + targets.max_len]u8 = undefined;
    const id: u64 = 0xfedc_ba98_7654_3210;
    const request = catalogCommandBytes(target.encode(&target_buffer), id, &buffer);
    var service: Bridge = .{ .engine = engine };
    const sentinel = original;
    engine.model.shared_workspace.desired_terminal = sentinel;
    // Provider lifetime replacement with exactly the same TerminalRef bits.
    const context = engine.model.provider.context_id;
    engine.model.provider.context_id = context + 1;
    const before = engine.revision;
    Bridge.request(&service, cockpit.engine.tab_commands.request_name, 7001, request);
    Bridge.request(&service, protocol.snapshot_request, 7002, "");
    const query = navigationRequestBytes(engine.revision);
    Bridge.request(&service, cockpit.engine.navigation.request_name, 7003, &query);
    const result = Bridge.poll(&service).?;
    try std.testing.expectEqual(@as(u64, 7001), result.key);
    try std.testing.expectEqual(@as(u8, 2), result.bytes[1]);
    try std.testing.expectEqual(@as(u8, 2), result.bytes[2]);
    try std.testing.expectEqual(id, std.mem.readInt(u64, result.bytes[3..11], .little));
    try std.testing.expectEqual(before, engine.revision);
    try std.testing.expect(engine.model.focusedTerminalRef().?.eql(original));
    try std.testing.expect(engine.model.shared_workspace.desired_terminal.?.eql(sentinel));
    try std.testing.expectEqual(@as(u64, 7002), Bridge.poll(&service).?.key);
    try std.testing.expectEqual(@as(u64, 7003), Bridge.poll(&service).?.key);
    engine.model.provider.context_id = context;
    try std.testing.expect(engine.model.provider.destroyTerminal(original));
    const retired = engine.applyTabCommand(request);
    try std.testing.expectEqual(.stale_target, retired.reason);
    try std.testing.expectEqual(.rejected, retired.status);
    try std.testing.expect(engine.model.shared_workspace.desired_terminal.?.eql(sentinel));
}

test "retained outcome survives canceled delivery and is consumed only by exact core acknowledgement" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const command_id: u64 = 0xfedcba9876543210;
    engine.creation.pending[0] = .{
        .command_id = command_id,
        .window = 0,
        .window_epoch = engine.model.window_epochs[0],
        .kind = .tab,
        .origin = null,
        .completion = .{
            .command_id = command_id,
            .connection_epoch = 0xfedcba9876543211,
            .request_id = 99,
            .operation = .success,
            .placement = .destination_lost,
            .focus = .superseded,
            .reason = .destination_lost,
            .terminal_ref = engine.model.focusedTerminalRef(),
        },
    };
    Bridge.request(&bridge, result_wire.request_name, 801, &result_wire.empty);
    Bridge.cancel(&bridge, 801);
    try std.testing.expect(engine.creation.peekCompletion() != null);
    Bridge.request(&bridge, result_wire.request_name, 802, &result_wire.empty);
    Bridge.request(&bridge, "cockpit.snapshot", 803, "");
    const result = Bridge.poll(&bridge).?;
    try std.testing.expectEqual(@as(u64, 802), result.key);
    var owned: [result_wire.max_bytes]u8 = undefined;
    @memcpy(owned[0..result.bytes.len], result.bytes);
    try std.testing.expect(engine.creation.peekCompletion() != null);
    try rig.dispatch(.{ .command_result_loaded = owned[0..result.bytes.len] });
    try std.testing.expect(engine.creation.peekCompletion() == null);
    try std.testing.expectEqual(@as(usize, 1), rig.app_state.model.commandResults.recent.len);
    const retained = rig.app_state.model.commandResults.recent[0];
    try std.testing.expectEqual(@as(i64, 0xfedcba98), retained.id.hi);
    try std.testing.expectEqual(@as(i64, 0x76543210), retained.id.lo);
    try std.testing.expectEqual(@as(i64, 1), retained.operation);
    try std.testing.expectEqual(@as(i64, 3), retained.placement);
    try std.testing.expectEqual(@as(i64, 2), retained.focus);
    try std.testing.expect(std.mem.indexOf(u8, rig.app_state.model.commandNotice, "succeeded") != null);
}

test "held painted tab action follows identity after metadata and reorder" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const first = engine.model.focusedTerminalRef().?;
    try rig.dispatch(.new_terminal);
    try rig.settle(1, "READY");
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    var ui = Adapter.Ui.init(arena.allocator());
    const tree = mainView(&ui, &rig.app_state.model);
    var held = firstPaintedTabMessage(tree).?;
    // Like the runtime's held-message storage, own the captured byte payload
    // across replacement of the model that produced the painted tree.
    if (held == .select_target) held.select_target = try arena.allocator().dupe(u8, held.select_target);
    _ = shellEvent(.{ .key = engine.model.provider.terminal(first).?.pty_key, .kind = .output, .bytes = "\x1b]2;renamed\x07" });
    try rig.settle(@intCast(engine.sequence), "READY");
    try rig.dispatch(core.commandMsg("tab.move-left").?);
    try rig.settle(@intCast(engine.sequence), "READY");
    try std.testing.expectEqual(@as(usize, 1), engine.model.wsConst().tabOfTerminal(first).?);
    const expected_command = rig.app_state.model.tabCommands.nextId.lo;
    try rig.dispatch(held);
    try std.testing.expect(first.eql(engine.model.focusedTerminalRef().?));
    try rig.settle(@intCast(engine.sequence), "READY");
    try std.testing.expectEqual(@as(i64, 2), rig.app_state.model.tabCommands.outcome);
    try std.testing.expectEqual(expected_command, rig.app_state.model.tabCommands.lastId.lo);
}

test "tab command receipt survives snapshot and navigation requests and rejects reused windows" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    const engine = bridge.engine.?;
    const commands = cockpit.engine.tab_commands;
    const target = commands.capture(engine.model, 1, 0).?.encode();
    var request: [commands.request_len]u8 = undefined;
    request[0] = 1;
    request[1] = 1;
    const id: u64 = 0xfedc_ba98_ffff_ffff;
    std.mem.writeInt(u64, request[2..10], id, .little);
    @memcpy(request[10..], &target);
    const host = bridge.binding();
    host.request_fn(host.context, commands.request_name, 9001, &request);
    host.request_fn(host.context, protocol.snapshot_request, 9002, "");
    host.request_fn(host.context, cockpit.engine.navigation.request_name, 9003, "bad request");
    const result = host.poll_fn.?(host.context).?;
    try std.testing.expectEqual(@as(u64, 9001), result.key);
    try std.testing.expectEqual(@as(u8, 1), result.bytes[1]);
    try std.testing.expectEqual(id, std.mem.readInt(u64, result.bytes[3..11], .little));
    try std.testing.expectEqual(@as(u64, 9002), host.poll_fn.?(host.context).?.key);
    try std.testing.expectEqual(@as(u64, 9003), host.poll_fn.?(host.context).?.key);

    engine.model.closeWindow(1);
    const reopened = engine.model.openWindow(1).?;
    const pane = try engine.model.provider.createTerminal();
    try std.testing.expect(reopened.admitTab(pane.id));
    try std.testing.expectEqual(@as(u32, 1), reopened.tabId(0).?);
    engine.model.active_window = 0;
    const before = engine.model.focusedTerminalRef().?;
    const refused = engine.applyTabCommand(&request);
    try std.testing.expectEqual(.stale_target, refused.reason);
    try std.testing.expectEqual(id, refused.id);
    try std.testing.expectEqual(@as(usize, 0), engine.model.active_window);
    try std.testing.expect(before.eql(engine.model.focusedTerminalRef().?));
}

test "retired tab targets cannot alias reused IDs after allocation rollover" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const commands = cockpit.engine.tab_commands;
    const original = commands.capture(engine.model, 0, 0).?;
    const workspace = engine.model.ws();
    workspace.dropTab(0);
    workspace.next_tab_id = std.math.maxInt(u32);
    const first = try engine.model.provider.createTerminal();
    try std.testing.expect(workspace.admitTab(first.id));
    const reused = try engine.model.provider.createTerminal();
    try std.testing.expect(workspace.admitTab(reused.id));
    try std.testing.expectEqual(original.tab_id, workspace.tabId(1).?);
    try std.testing.expect(original.resolve(engine.model) == null);
    try std.testing.expectEqual(@as(u8, 1), commands.capture(engine.model, 0, 1).?.resolve(engine.model).?);
    workspace.tab_generation = std.math.maxInt(u64);
    try std.testing.expect(commands.capture(engine.model, 0, 1).?.resolve(engine.model) == null);
    engine.model.window_epochs[0] = std.math.maxInt(u64);
    engine.model.closeWindow(0);
    try std.testing.expectEqual(std.math.maxInt(u64), engine.model.window_epochs[0]);
}

test "compiled core serializes captured window selections across independent projection traffic" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    var ui = Adapter.Ui.init(arena.allocator());
    const main = firstPaintedTabMessage(mainView(&ui, &rig.app_state.model)).?;
    const other = firstPaintedTabMessage(compiledWindow(&ui, &rig.app_state.model, 1)).?;
    const first = try arena.allocator().dupe(u8, main.select_target);
    const second = try arena.allocator().dupe(u8, other.select_target);
    const first_command = rig.app_state.model.tabCommands.nextId.lo;
    try rig.dispatch(.{ .select_target = first });
    try std.testing.expectEqual(@as(usize, 0), bridge.engine.?.model.active_window);
    try rig.dispatch(.{ .select_target = second });
    try std.testing.expectEqual(@as(usize, 2), rig.app_state.model.tabCommands.queue.len);
    // The queued second click has not executed. Its receipt owns that step.
    try std.testing.expectEqual(@as(usize, 0), bridge.engine.?.model.active_window);
    try rig.dispatch(.palette_open);
    for (0..8) |_| {
        if (rig.app_state.model.tabCommands.queue.len == 0) break;
        try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .wake);
    }
    try std.testing.expectEqual(@as(usize, 0), rig.app_state.model.tabCommands.queue.len);
    try std.testing.expectEqual(first_command + 1, rig.app_state.model.tabCommands.lastId.lo);
    try std.testing.expectEqual(@as(i64, 2), rig.app_state.model.tabCommands.outcome);
    try std.testing.expectEqual(@as(usize, 1), bridge.engine.?.model.active_window);
}

test "persistence failure and recovery publish without a later command" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const state = &engine.model.state;
    state.inflight = true;
    state.inflight_fingerprint = state.fingerprint;
    state.retry_count = 3;
    const revision = engine.revision;
    const sequence = engine.sequence;
    _ = topologyWritten(.{ .key = cockpit.topology_state_file_key, .outcome = .io_failed });
    try std.testing.expect(state.write_failed);
    try rig.settle(@intCast(sequence + 1), "ACTION REFUSED");
    // Persistence feedback does not change any positional command target.
    try std.testing.expectEqual(revision, engine.revision);
    state.inflight = true;
    state.pending = false;
    _ = topologyWritten(.{ .key = cockpit.topology_state_file_key, .outcome = .ok });
    try rig.settle(@intCast(sequence + 2), "READY");
    try std.testing.expectEqual(revision, engine.revision);
}

test "quiet split pointer focus publishes chrome without terminal output" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const left = engine.model.focusedTerminalRef().?;
    try rig.dispatch(core.commandMsg("pane.split-right").?);
    try rig.settle(1, "READY");
    const right = engine.model.focusedTerminalRef().?;
    const left_key = engine.model.provider.terminal(left).?.pty_key;
    const right_key = engine.model.provider.terminal(right).?.pty_key;
    _ = shellEvent(.{ .key = left_key, .kind = .output, .bytes = "\x1b]2;quiet-left\x07" });
    _ = shellEvent(.{ .key = right_key, .kind = .output, .bytes = "\x1b]2;quiet-right\x07" });
    try rig.settle(@intCast(engine.sequence), "READY");
    try std.testing.expectEqualStrings("quiet-right", rig.app_state.model.tabs[0].title);
    const before = bridge.posts_accepted;
    const revision = engine.revision;
    const frame = cockpit.projection.paneFrameFor(engine.model, .init(1100, 640), left).?;
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .pointer_down,
        .x = frame.x + 10,
        .y = frame.y + 10,
    } });
    try std.testing.expect(left.eql(engine.model.focusedTerminalRef().?));
    try std.testing.expectEqual(before + 1, bridge.posts_accepted);
    try std.testing.expectEqual(revision + 1, engine.revision);
    try rig.settle(@intCast(engine.sequence), "READY");
    try std.testing.expectEqualStrings("quiet-left", rig.app_state.model.tabs[0].title);
    const published = bridge.posts_accepted;
    _ = shellEvent(.{ .key = left_key, .kind = .output, .bytes = "ordinary output" });
    try std.testing.expectEqual(published, bridge.posts_accepted);
}

test "refused command window adoption still fences ambient targets" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    try rig.dispatch(.{ .select_tab = 0 });
    try rig.settle(2, "READY");
    const engine = bridge.engine.?;
    engine.model.wsAt(0).?.window_id = 1;
    engine.model.wsAt(1).?.window_id = 42;
    const revision = engine.revision;
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .native_command = .{
        .name = "pane.focus-left",
        .window_id = 42,
    } });
    try std.testing.expectEqual(@as(usize, 1), engine.model.active_window);
    try std.testing.expect(engine.intent_refused);
    try std.testing.expectEqual(revision + 1, engine.revision);
    try rig.settle(@intCast(engine.sequence), "ACTION REFUSED");
    const stale = protocol.encodeIntent(.{ .kind = .new_terminal, .argument = 0, .window = 255, .expected_revision = revision });
    const host = bridge.binding();
    host.send_fn(host.context, protocol.intent_command, &stale);
    try std.testing.expect(engine.intent_refused);
    try std.testing.expectEqual(@as(usize, 1), engine.model.wsConst().tab_count);
    try rig.settle(@intCast(engine.sequence), "ACTION REFUSED");

    const posts = bridge.posts_accepted;
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .native_command = .{
        .name = "terminal.new",
        .window_id = 1,
    } });
    // A successful command already fenced/published the adoption and new view.
    try std.testing.expectEqual(posts + 1, bridge.posts_accepted);
    try rig.settle(@intCast(engine.sequence), "READY");
}

test "shipping close detaches a Phux pane without destroying a local terminal" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const ref: cockpit.TerminalRef = .{
        .provider_id = .phux,
        .terminal_id = .{ .phux = try cockpit.RemoteResourceId.fromPhux(0, 7, "") },
    };
    try std.testing.expect(engine.model.admitTab(ref));
    try std.testing.expect(engine.model.selectTerminal(ref));
    try rig.dispatch(core.commandMsg("terminal.close").?);
    try std.testing.expect(engine.model.locateTerminal(ref) == null);
    try std.testing.expectEqual(@as(usize, 1), engine.model.provider.activeCount());
    try std.testing.expectEqual(@as(usize, 1), engine.model.primary.tab_count);
}

test "shipping final pane close retires main while a secondary keeps running" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    engine.model.active_window = 0;
    const intent = protocol.encodeIntent(.{
        .kind = .native_command,
        .expected_revision = engine.revision,
        .argument = @intFromEnum(protocol.NativeCommand.close_focused_pane),
        .window = 0,
    });
    try std.testing.expect(engine.applyIntent(&intent, &cockpit.NoShells{}));
    try std.testing.expect(!engine.model.primary_open);
    try std.testing.expect(engine.model.windowOpen(1));
    try std.testing.expectEqual(@as(usize, 1), engine.model.active_window);
    try std.testing.expectEqual(@as(usize, 1), engine.model.provider.activeCount());
}

test "shipping clipboard completion belongs to its requesting replica after focus moves" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const source_ref = engine.model.focusedTerminalRef().?;
    const source = engine.model.provider.terminal(source_ref).?;
    source.phase = .live;
    source.session.feed("copy from source");
    try std.testing.expect(source.session.selectAllHistory());
    source.selecting = true;
    const copy = protocol.encodeIntent(.{
        .kind = .native_command,
        .expected_revision = engine.revision,
        .argument = @intFromEnum(protocol.NativeCommand.copy),
    });
    try std.testing.expect(engine.applyIntent(&copy, &cockpit.NoShells{}));
    try std.testing.expect(engine.model.copy_inflight);
    const create = protocol.encodeIntent(.{ .kind = .new_terminal, .expected_revision = engine.revision, .argument = 0 });
    try std.testing.expect(engine.applyIntent(&create, &cockpit.NoShells{}));
    const other = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    other.selecting = true;
    engine.onClipboardWritten(true);
    try std.testing.expect(!source.selecting);
    try std.testing.expect(other.selecting);
}

test "shipping Phux callbacks emit structured key text paste and focus frames" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    _ = onText(.{ .phase = .text_input, .text = "z", .key = "z" });
    try expectOutgoingTag(remote, 0x10);
    _ = onKey(.{ .phase = .key_down, .key = "enter" });
    try expectOutgoingTag(remote, 0x10);
    _ = onKey(.{ .phase = .key_down, .key = "v", .modifiers = .{ .super = true } });
    try std.testing.expect(engine.model.paste_inflight);
    try std.testing.expect(engine.model.paste_owner.terminal_ref.eql(ref));
    engine.onClipboardRead(EngineFx{ .effects = &rig.app_state.effects }, true, "hello paste");
    try expectOutgoingTag(remote, 0x11);
    engine.setFocused(EngineFx{ .effects = &rig.app_state.effects }, false);
    try expectOutgoingTag(remote, 0x14);
    engine.setFocused(EngineFx{ .effects = &rig.app_state.effects }, true);
    try expectOutgoingTag(remote, 0x14);
}

fn expectOutgoingTag(remote: anytype, tag: u8) !void {
    const frame = remote.bridge.outgoing.take() orelse return error.TestExpectedOutgoingFrame;
    defer remote.bridge.outgoing.release(frame);
    try std.testing.expect(frame.len > 4);
    try std.testing.expectEqual(tag, frame[4]);
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

/// Decode the small KEY fixture's positional event inside its two TLV fields.
/// Layout derives from wire/frame/kind.rs::encode_input_key and codec.rs::encode_key_event.
fn expectOutgoingKey(remote: anytype, physical: u32, modifiers: u16) !void {
    const frame = remote.bridge.outgoing.take() orelse return error.TestExpectedOutgoingKey;
    defer remote.bridge.outgoing.release(frame);
    try std.testing.expect(frame.len >= 28);
    try std.testing.expectEqual(@as(u8, 0x10), frame[4]);
    try std.testing.expectEqualSlices(u8, &.{ 1, 4, 5, 0, 0, 0, 0, 7, 2, 4 }, frame[5..15]);
    try std.testing.expectEqual(frame.len - 16, frame[15]);
    try std.testing.expectEqual(physical, std.mem.readInt(u32, frame[20..24], .big));
    try std.testing.expectEqual(modifiers, std.mem.readInt(u16, frame[24..26], .big));
}

test "shipping Phux control keys remain terminal input" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    _ = try rig.attachFixture();
    const remote = bridge.engine.?.model.phux().?;
    _ = onKey(.{ .phase = .key_down, .key = "c", .modifiers = .{ .control = true, .super = true } });
    try expectOutgoingKey(remote, 22, 2);
    _ = onKey(.{ .phase = .key_down, .key = "v", .modifiers = .{ .control = true, .super = true } });
    try expectOutgoingKey(remote, 41, 2);
    try std.testing.expect(!bridge.engine.?.model.paste_inflight);
}

test "shipping Phux committed text consumes composition modifiers" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    _ = try rig.attachFixture();
    _ = onText(.{ .phase = .text_input, .text = "ƒ", .key = "f", .modifiers = .{ .alt = true } });
    try expectOutgoingKey(bridge.engine.?.model.phux().?, 0, 0);
}

test "shipping Control F is terminal input rather than a fullscreen shortcut" {
    try std.testing.expect(primaryChord(.{ .phase = .key_down, .key = "f", .modifiers = .{ .control = true } }) == null);
    try std.testing.expect(primaryChord(.{ .phase = .key_down, .key = "f", .modifiers = .{ .control = true, .super = true } }) != null);
}

test "shipping overlay commit suspends remote focus and input before a frame" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    _ = try rig.attachFixture();
    const remote = bridge.engine.?.model.phux().?;
    // A real core transition must retire terminal input ownership immediately,
    // even if no GPU frame is delivered between this message and the next key.
    try rig.dispatch(.settings_open);
    try std.testing.expect(bridge.engine.?.input_suspended);
    try expectOutgoingTag(remote, 0x14);
    bridge.engine.?.onText(engineFx().?, .{ .phase = .text_input, .key = "a", .text = "a" });
    _ = onKey(.{ .phase = .key_up, .key = "a" });
    try std.testing.expectEqual(.ignored, bridge.engine.?.onPointer(engineFx().?, .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .pointer_down,
        .x = 400,
        .y = 300,
    }));
    try std.testing.expect(!bridge.engine.?.onDrop(engineFx().?, .{
        .window_id = 1,
        .view_label = canvas_label,
        .paths = &.{"/blocked"},
    }));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    try rig.dispatch(.settings_close);
    try std.testing.expect(bridge.engine.?.input_suspended);
    try rig.settleAppearance();
    try std.testing.expect(!bridge.engine.?.input_suspended);
    try expectOutgoingTag(remote, 0x14);
}

test "shipping Phux macOS editing gestures target word and line bindings" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    _ = try rig.attachFixture();
    const remote = bridge.engine.?.model.phux().?;
    const cases = .{
        .{ "arrowleft", true, @as(u32, 21), @as(u16, 4) },
        .{ "arrowright", true, @as(u32, 25), @as(u16, 4) },
        .{ "arrowleft", false, @as(u32, 20), @as(u16, 2) },
        .{ "arrowright", false, @as(u32, 24), @as(u16, 2) },
        .{ "backspace", false, @as(u32, 40), @as(u16, 2) },
    };
    inline for (cases) |binding| {
        _ = onKey(.{ .phase = .key_down, .key = binding[0], .modifiers = .{ .alt = binding[1], .super = !binding[1] } });
        try expectOutgoingKey(remote, binding[2], binding[3]);
        // The modifier may be released before the navigation key.
        _ = onKey(.{ .phase = .key_up, .key = binding[0] });
        try std.testing.expect(!remote.bridge.outgoing.hasPending());
    }
    _ = onKey(.{ .phase = .key_down, .key = "arrowleft", .modifiers = .{ .alt = true } });
    try expectOutgoingKey(remote, 21, 4);
    // An ordinary repeat after Option-up supersedes the natural-key latch.
    _ = onKey(.{ .phase = .key_down, .key = "arrowleft" });
    try expectOutgoingKey(remote, 76, 0);
    _ = onKey(.{ .phase = .key_up, .key = "arrowleft" });
    try expectOutgoingKey(remote, 76, 0);
}

test "committed palette owns input even when an older model is painted" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const stale = rig.app_state.model;
    try rig.dispatch(.palette_open);
    try std.testing.expect(engine.input_suspended);

    const commands = try std.testing.allocator.alloc(canvas.CanvasCommand, cockpit.projection.chrome_command_envelope);
    defer std.testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try paintChrome(&stale, &builder, .init(1100, 640), cockpit.projection.cockpitTokens(engine.model));
    try std.testing.expect(engine.input_suspended);
    const escape = onKey(.{ .phase = .key_down, .key = "Escape" }) orelse return error.TestExpectedOverlayKey;
    try std.testing.expectEqual(core.Msg.palette_close, escape);
    try rig.dispatch(escape);
    try std.testing.expect(!engine.input_suspended);
}

test "replayed modality routes fallback keys without live terminal effects" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    _ = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    try rig.decorated.replayControl(.arm);
    try rig.dispatch(.settings_open);
    // The marker is suppressed. A replayed event recovers the committed mode
    // without invoking the live provider's focus or capture cleanup.
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .frame_requested);
    try std.testing.expectEqual(.settings, bridge.interaction_mode);
    try std.testing.expectEqual(core.Msg.settings_close, onKey(.{ .phase = .key_down, .key = "Escape" }).?);
    try rig.dispatch(.palette_open);
    // Replay supplies the recorded rollback acknowledgement, not a live disk
    // or provider effect. Until that record arrives Settings owns the keys.
    try std.testing.expectEqual(core.Msg.settings_close, onKey(.{ .phase = .key_down, .key = "Escape" }).?);
    try rig.dispatch(.{ .appearance_loaded = &.{ 1, 0, 2, 0, 255, 0, 0, 0, 0, 0 } });
    try std.testing.expectEqual(core.Msg.palette_close, onKey(.{ .phase = .key_down, .key = "Escape" }).?);
    _ = onText(.{ .phase = .text_input, .key = "a", .text = "a" });
    routeNativeInput(.{ .files_dropped = .{ .window_id = 1, .view_label = canvas_label, .paths = &.{"/blocked"} } });
    const before = engine.model.active_window;
    routeNativeInput(.{ .gpu_surface_input = .{ .window_id = 1, .label = canvas_label, .kind = .pointer_down, .x = 400, .y = 300 } });
    try std.testing.expectEqual(before, engine.model.active_window);
    try rig.dispatch(.palette_close);
    _ = onKey(.{ .phase = .key_down, .key = "Enter" });
    try std.testing.expectEqual(.terminal, bridge.interaction_mode);
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

test "committed app modal blocks raw pointers across native windows" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    const engine = bridge.engine.?;
    try rig.dispatch(.settings_open);
    const active = engine.model.active_window;
    const focus = engine.model.focusedTerminalRef().?;
    for ([_][]const u8{ canvas_label, "phux-cockpit-canvas-1" }) |label| {
        try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = label,
            .kind = .pointer_down,
            .x = 400,
            .y = 300,
        } });
        try std.testing.expectEqual(active, engine.model.active_window);
        try std.testing.expect(focus.eql(engine.model.focusedTerminalRef().?));
        try std.testing.expect(engine.input_suspended);
    }
}

test "cold replay registers provider and PTY results without live startup" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.startWithReplay(true, true);
    defer rig.stop();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    const key = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?.pty_key;
    // Rig's installing frame precedes the first widget-tree commit. The next
    // presented frame is the normal native surface pump entry.
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_frame = .{
        .label = canvas_label,
        .size = .init(1100, 640),
        .scale_factor = 1,
        .frame_index = 2,
        .timestamp_ns = 2,
    } });
    try rig.app_state.effects.feedPtyOutput(key, "replayed output");
    try rig.app_state.effects.feedChannelEvent(cockpit.phux_channel_key, .data, &.{1}, 0, 0);
    try std.testing.expectEqual(.new, remote.state());
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .timer = .{ .id = PointerHost.workspace_timer_id } });
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

test "shipping search text cannot leak a key release after search closes" {
    const engine = try engineWithText("\x1b[>3u");
    defer engine.destroy();
    var fx = Recorder{};
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    pane.phase = .live;
    engine.onKey(&fx, .{ .phase = .key_down, .key = "f", .modifiers = .{ .super = true } });
    try std.testing.expect(pane.session.search.open);
    engine.onText(&fx, .{ .phase = .text_input, .key = "a", .text = "a" });
    engine.onKey(&fx, .{ .phase = .key_down, .key = "escape" });
    try std.testing.expect(!pane.session.search.open);
    engine.onKey(&fx, .{ .phase = .key_up, .key = "a" });
    try std.testing.expectEqual(@as(usize, 0), pane.outbound_len);
}

test "shipping exit of superseded scratch shell preserves confirmed Phux focus" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const scratch = bridge.engine.?.model.provider.terminal(bridge.engine.?.model.focusedTerminalRef().?).?;
    const scratch_key = scratch.pty_key;
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    const fx = EngineFx{ .effects = &rig.app_state.effects };
    try std.testing.expect(engine.model.focusedTerminalRef().?.eql(ref));
    _ = engine.onShellEvent(fx, .{ .key = scratch_key, .kind = .exit, .code = 0 });
    try std.testing.expect(engine.model.focusedTerminalRef().?.eql(ref));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

test "shipping frame resizes a published Phux viewport once" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    const before = remote.lastViewport(ref);
    const frame: native_sdk.platform.GpuFrame = .{
        .label = canvas_label,
        .size = .{ .width = 900, .height = 500 },
        .scale_factor = 1,
        .frame_index = 2,
        .timestamp_ns = 2,
    };
    _ = onFrame(&rig.app_state.model, frame);
    const after = remote.lastViewport(ref) orelse return error.TestExpectedRemoteViewport;
    if (before) |old| try std.testing.expect(!old.eql(after));
    try expectOutgoingTag(remote, 0x23);
    _ = onFrame(&rig.app_state.model, frame);
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

test "pending attachment cannot propose a viewport for a reused remote identity" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const model = bridge.engine.?.model;
    const size = native_sdk.geometry.SizeF{ .width = 900, .height = 500 };
    const before = cockpit.projection.proposedViewportsIn(model, model.ws(), size);
    try std.testing.expectEqual(@as(usize, 1), before.slice().len);
    model.rejectAttachmentContext();
    try std.testing.expect(model.attachmentPending(ref));
    const after = cockpit.projection.proposedViewportsIn(model, model.ws(), size);
    try std.testing.expectEqual(@as(usize, 0), after.slice().len);
}

test "pending attachment hides the published grid of a reused remote identity" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    _ = try rig.attachFixture();
    const engine = bridge.engine.?;
    const size = native_sdk.geometry.SizeF.init(900, 500);
    const tokens = cockpit.projection.cockpitTokens(engine.model);
    const commands = try std.testing.allocator.alloc(canvas.CanvasCommand, cockpit.projection.chrome_command_envelope);
    defer std.testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try engine.paint(&builder, size, tokens);
    const visible = builder.displayList().commands.len;
    engine.model.rejectAttachmentContext();
    builder.reset();
    try engine.paint(&builder, size, tokens);
    try std.testing.expect(visible > builder.displayList().commands.len);
}

test "native divider drag updates engine geometry without crossing the TypeScript seam" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;

    try rig.dispatch(core.commandMsg("pane.split-right").?);
    try rig.settle(1, "READY");
    const workspace = engine.model.ws();
    const tree = workspace.selectedTree().?;
    const chrome = cockpit.projection.workspaceChromeIn(engine.model, workspace, workspace.surface_size);
    var dividers: [cockpit.layout.max_panes - 1]cockpit.layout.Divider = undefined;
    const count = tree.dividers(
        chrome.content,
        cockpit.projection.split_divider_width,
        cockpit.projection.split_pane_min_width,
        cockpit.projection.split_pane_min_height,
        &dividers,
    );
    try std.testing.expectEqual(@as(usize, 1), count);
    const divider = dividers[0];
    const y = divider.rect.y + divider.rect.height / 2;
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .pointer_down,
        .pointer_id = 7,
        .x = divider.rect.x + divider.rect.width / 2,
        .y = y,
    } });
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .pointer_drag,
        .pointer_id = 7,
        .x = divider.bounds.x + (divider.bounds.width - cockpit.projection.split_divider_width) * 0.7,
        .y = y,
    } });
    try rig.settle(2, "READY");
    try std.testing.expectApproxEqAbs(@as(f32, 0.7), tree.node(divider.node).fraction, 0.001);
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .pointer_up,
        .pointer_id = 7,
        .x = divider.rect.x,
        .y = y,
    } });
}

test "painting speculative placement cannot bypass a refused intent" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    try rig.dispatch(.new_terminal);
    try rig.settle(1, "READY");

    const stale = protocol.encodeIntent(.{ .kind = .set_tab_placement, .expected_revision = 1, .argument = 1 });
    const host = bridge.binding();
    host.send_fn(host.context, protocol.intent_command, &stale);
    try std.testing.expect(engine.intent_refused);
    try std.testing.expectEqual(.top, engine.model.tab_placement);
    const revision = engine.revision;
    const sequence = engine.sequence;

    var speculative = rig.app_state.model;
    speculative.tabPlacement = .side;
    const commands = try std.testing.allocator.alloc(canvas.CanvasCommand, cockpit.projection.chrome_command_envelope);
    defer std.testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try paintChrome(&speculative, &builder, .init(1100, 640), cockpit.projection.cockpitTokens(engine.model));
    try std.testing.expectEqual(.top, engine.model.tab_placement);
    try std.testing.expectEqual(revision, engine.revision);
    try std.testing.expectEqual(sequence, engine.sequence);
}

test "a stale intent is refused, announced, and surfaced instead of applied" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const host = bridge.binding();

    // Two tabs, so a close has something to do: against one tab the last-tab
    // rule refuses it for its own reason and the fence is never exercised.
    try rig.dispatch(.new_terminal);
    try rig.settle(1, "READY");
    try std.testing.expectEqual(@as(usize, 2), engine.model.wsConst().tab_count);
    try std.testing.expectEqual(@as(u64, 2), engine.revision);

    // A close computed against revision 1, after the engine moved to 2: the
    // tab at index 0 may no longer be the one that close meant.
    const stale = protocol.encodeIntent(.{ .kind = .close_tab, .expected_revision = 1, .argument = 0 });
    host.send_fn(host.context, protocol.intent_command, &stale);
    try std.testing.expect(engine.intent_refused);
    try std.testing.expectEqual(@as(usize, 2), engine.model.wsConst().tab_count);
    try std.testing.expectEqual(@as(u64, 2), engine.revision);

    try rig.settle(2, "ACTION REFUSED");
    try std.testing.expectEqual(@as(usize, 2), rig.app_state.model.tabs.len);

    // The next well-fenced intent clears the refusal.
    try rig.dispatch(.toggle_tab_placement);
    try std.testing.expect(!engine.intent_refused);
    try rig.settle(3, "READY");
    try std.testing.expectEqual(core.TabPlacement.side, rig.app_state.model.tabPlacement);
}

test "new terminal click hands typing and Enter to the created pane" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.resize(native_sdk.geometry.SizeF.init(1100, 640));
    const engine = bridge.engine.?;
    const original = engine.model.focusedTerminalRef().?;
    const layout = try rig.harness.runtime.canvasWidgetLayout(1, canvas_label);
    var button: ?native_sdk.geometry.RectF = null;
    for (layout.nodes) |node| {
        if (std.mem.eql(u8, node.widget.semantics.label, "New terminal")) button = node.frame;
    }
    const frame = button orelse return error.TestExpectedButton;
    inline for (.{ .pointer_down, .pointer_up }) |kind| {
        try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = canvas_label,
            .kind = kind,
            .x = frame.x + frame.width / 2,
            .y = frame.y + frame.height / 2,
        } });
    }
    try rig.settle(@intCast(engine.sequence), "READY");
    const created = engine.model.focusedTerminalRef().?;
    try std.testing.expect(!created.eql(original));
    try std.testing.expectEqual(@as(usize, 2), engine.model.wsConst().tab_count);
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .text_input,
        .text = "ls",
    } });
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .key_down,
        .key = "Enter",
    } });
    // The focused toolbar button used to consume Enter as another creation.
    const pane = engine.model.provider.terminal(created).?;
    try std.testing.expectEqual(@as(usize, 3), pane.outbound_len);
    try std.testing.expectEqual(@as(usize, 0), engine.model.provider.terminal(original).?.outbound_len);
    try std.testing.expectEqual(@as(usize, 2), engine.model.wsConst().tab_count);
}

test "changing placement preserves the adopted secondary window" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    const engine = bridge.engine.?;
    try std.testing.expectEqual(@as(usize, 1), engine.model.active_window);
    try rig.dispatch(.toggle_tab_placement);
    try std.testing.expectEqual(@as(usize, 1), engine.model.active_window);
    try rig.settle(2, "READY");
    try std.testing.expectEqual(core.TabPlacement.side, rig.app_state.model.tabPlacement);
}

test "clicking the selected tab returns Enter to its focused split pane in strip and rail" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(core.commandMsg("pane.split-right").?);
    try rig.settle(1, "READY");
    const engine = bridge.engine.?;
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    inline for (.{ core.TabPlacement.top, core.TabPlacement.side }) |placement| {
        if (placement == .side) {
            const before = rig.app_state.model.engineSequence.lo;
            try rig.dispatch(.toggle_tab_placement);
            try rig.settle(before + 1, "READY");
        }
        try rig.resize(native_sdk.geometry.SizeF.init(1100, 640));
        const layout = try rig.harness.runtime.canvasWidgetLayout(1, canvas_label);
        var tab: ?native_sdk.geometry.RectF = null;
        for (layout.nodes) |node| {
            if (node.widget.state.selected and (node.widget.kind == .toggle_button or node.widget.kind == .list_item)) tab = node.frame;
        }
        const frame = tab orelse return error.TestExpectedTab;
        inline for (.{ .pointer_down, .pointer_up }) |kind| {
            try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
                .window_id = 1,
                .label = canvas_label,
                .kind = kind,
                .x = frame.x + frame.width / 2,
                .y = frame.y + frame.height / 2,
            } });
        }
        try rig.settle(@intCast(engine.sequence), "READY");
        const bytes_before = pane.outbound_len;
        try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = canvas_label,
            .kind = .text_input,
            .text = "ls",
        } });
        try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = canvas_label,
            .kind = .key_down,
            .key = "Enter",
        } });
        try std.testing.expectEqual(bytes_before + 3, pane.outbound_len);
        try std.testing.expectEqual(@as(usize, 1), engine.model.wsConst().tab_count);
    }
}

test "unclaimed keys and text reach the focused pane's outbound ring and never the core" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    try std.testing.expectEqual(@as(usize, 0), pane.outbound_len);

    // Through the platform, so the SDK's own widget-precedence routing is
    // what hands the input to the extension: nothing in the markup claims
    // typing, so committed text and a bare Enter fall through. No shell is
    // live in the rig, so the encoded bytes stay queued in the ring, which
    // is exactly where the shipping app parks them too.
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .text_input,
        .text = "ls",
    } });
    try std.testing.expectEqual(@as(usize, 2), pane.outbound_len);
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .key_down,
        .key = "Enter",
    } });
    try std.testing.expectEqual(@as(usize, 3), pane.outbound_len);

    // With an overlay open the same input is the core's, not the shell's.
    try rig.dispatch(.palette_open);
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .text_input,
        .text = "x",
    } });
    try std.testing.expectEqual(@as(usize, 3), pane.outbound_len);
}

test "every registered pane gets exactly one shell request and a closed tab kills its own" {
    const SpawnRecorder = struct {
        pub fn cancel(_: *@This(), _: u64) void {}
        pub fn closeWindow(_: *@This(), _: []const u8) void {}
        pub fn quitApp(_: *@This()) void {}
        spawned: usize = 0,
        killed: usize = 0,
        last_killed: u64 = 0,
        pub fn hostSend(_: *@This(), _: []const u8, _: []const u8) void {}
        pub fn showNotification(_: *@This(), _: anytype) void {}
        pub fn writeClipboard(_: *@This(), _: anytype) void {}
        pub fn readClipboard(_: *@This(), _: anytype) void {}
        pub fn openUrl(_: *@This(), _: []const u8) void {}
        pub fn toggleFullscreenWindow(_: *@This(), _: []const u8) void {}
        pub fn minimizeWindow(_: *@This(), _: []const u8) void {}
        pub fn ptySpawn(self: *@This(), _: anytype) void {
            self.spawned += 1;
        }
        pub fn ptyWrite(_: *@This(), _: u64, _: []const u8) bool {
            return false;
        }
        pub fn ptyResize(_: *@This(), _: u64, _: u16, _: u16) void {}
        pub fn ptyKill(self: *@This(), key: u64) void {
            self.killed += 1;
            self.last_killed = key;
        }
    };
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var fx = SpawnRecorder{};

    engine.spawnShells(&fx, shellEvent);
    engine.spawnShells(&fx, shellEvent);
    try std.testing.expectEqual(@as(usize, 1), fx.spawned);

    const open = protocol.encodeIntent(.{ .kind = .new_terminal, .expected_revision = 1, .argument = 0 });
    try std.testing.expect(engine.applyIntent(&open, &fx));
    engine.spawnShells(&fx, shellEvent);
    try std.testing.expectEqual(@as(usize, 2), fx.spawned);

    const second = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    const second_key = second.pty_key;
    const close = protocol.encodeIntent(.{ .kind = .close_tab, .expected_revision = 2, .argument = 1 });
    try std.testing.expect(engine.applyIntent(&close, &fx));
    try std.testing.expectEqual(@as(usize, 1), fx.killed);
    try std.testing.expectEqual(second_key, fx.last_killed);
    engine.spawnShells(&fx, shellEvent);
    try std.testing.expectEqual(@as(usize, 2), fx.spawned);

    // A split creates a real provider pane, not only a visual branch. The
    // next idempotent spawn pass must start exactly that pane's PTY.
    const split = protocol.encodeIntent(.{ .kind = .native_command, .expected_revision = 3, .argument = 4, .window = 0 });
    try std.testing.expect(engine.applyIntent(&split, &fx));
    engine.spawnShells(&fx, shellEvent);
    try std.testing.expectEqual(@as(usize, 3), fx.spawned);
    try std.testing.expectEqual(@as(usize, 2), engine.model.provider.activeCount());
}

// MEASURED: the cost of the route docs/DECISIONS.md chose by reuse. A full
// 80x24 grid of text painted as the chrome prefix, on the engine's model, the
// way every frame paints it. Print with:
//
//   zig build test -Dplatform=null -Dmeasure=true
//
// The number a media-surface leaf would have to beat is the per-paint time
// below plus the display-list decode it saves; the leaf route is not built,
// so this is the baseline half of that comparison, not the comparison.
test "MEASURED: the chrome-prefix paint of a full grid on the engine model" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    var line: [81]u8 = undefined;
    for (0..24) |row| {
        for (0..80) |col| line[col] = @intCast('!' + ((row * 7 + col) % 90));
        line[80] = '\n';
        pane.session.feed(&line);
    }
    pane.session.refreshScreenText();
    const size = native_sdk.geometry.SizeF.init(1100, 640);
    engine.model.ws().surface_size = size;
    engine.model.ws().surface_scale_factor = 2;
    const tokens = cockpit.projection.cockpitTokens(engine.model);

    const commands = try std.testing.allocator.alloc(canvas.CanvasCommand, cockpit.projection.chrome_command_envelope);
    defer std.testing.allocator.free(commands);
    // One warm paint measures the cell box and primes the painter's caches.
    var builder = canvas.Builder.init(commands);
    try engine.paint(&builder, size, tokens);
    const first = builder.displayList().commands.len;
    try std.testing.expect(first > 0);

    const iterations: usize = 200;
    const started = std.Io.Clock.awake.now(std.testing.io);
    for (0..iterations) |_| {
        builder.reset();
        try engine.paint(&builder, size, tokens);
    }
    const finished = std.Io.Clock.awake.now(std.testing.io);
    const total_ns: u64 = @intCast(finished.nanoseconds - started.nanoseconds);
    cockpit.measured.print(
        "\nMEASURED chrome_prefix_paint: grid=80x24 size=1100x640 scale=2 commands={d} paints={d} per_paint_us={d}\n",
        .{ first, iterations, total_ns / iterations / std.time.ns_per_us },
    );
}

// ------------------------------------------------------ parity harness
test "shipping durable tab waits for exact publication and keeps its original window" {
    try cockpit.durable_tests.tabPublication();
}

test "shipping durable split does not follow a different selected tab" {
    try cockpit.durable_tests.splitDestination();
}

test "shipping durable completion cannot acquire a reopened window slot" {
    try cockpit.durable_tests.windowEpoch();
}

test "shipping disconnected creation stays unknown and never retries as a local shell" {
    try cockpit.durable_tests.unknownOutcome();
}

test "shipping attachment recovery resolves only the saved coordinator incarnation" {
    try cockpit.durable_tests.incarnationRecovery();
}

test "shipping restored subscription waits for both exact bootstrap and command acceptance" {
    try cockpit.durable_tests.restoredSubscription();
}

test "shipping proven display survives disconnect without admitting a reused incarnation" {
    try cockpit.durable_tests.frozenPaintRecovery();
}

test "shipping pending spawns reserve destination tab capacity" {
    try cockpit.durable_tests.destinationReservations();
}

test "shipping pending spawns reserve split capacity" {
    try cockpit.durable_tests.splitReservations();
}

test "shipping refused creation retires its still-empty reserved window" {
    try cockpit.durable_tests.windowRefusal();
}

test "shipping empty persisted Phux workspace does not spawn a synthetic local shell" {
    try cockpit.durable_tests.restoredEmptyWorkspace();
}

test "shipping failed reconnect publishes the retired pending window" {
    try cockpit.durable_tests.reconnectClosePublishes();
}

// ------------------------------------------------------ parity harness
//
// What chrome_register_tests.zig is for the Zig chrome, for the markup tree:
// the compiled app.native is solved at every declared window size and
// density, in every chrome state the core can be in, and audited with the
// same toolkit audit the Zig ladder answers to. A finding is a real defect
// (overlap, a target under the WCAG floor, a widget off its grid), printed
// the way the Zig audit prints it. The engine's grids are painted beneath
// the composite tree; its transparent pane leaves carry accessibility and
// consume the exact geometry the shipping painter uses.

const main_sources = [_]canvas.ui_markup.SourceFile{
    .{ .path = "app.native", .source = @embedFile("app.native") },
    .{ .path = "windows/components/cockpit-window.native", .source = @embedFile("windows/components/cockpit-window.native") },
};
const CompiledChrome = canvas.CompiledMarkupImports(core.Model, core.Msg, "app.native", &main_sources);
const compiled_fragments = [_]canvas.MarkupFragment{
    CompiledChrome.fragment("src/app.native"),
    WindowView1.fragment("src/windows/phux-window-1.native"),
    WindowView2.fragment("src/windows/phux-window-2.native"),
    WindowView3.fragment("src/windows/phux-window-3.native"),
    WindowView4.fragment("src/windows/phux-window-4.native"),
};

const parity_sizes = [_]native_sdk.geometry.SizeF{
    native_sdk.geometry.SizeF.init(900, 420),
    native_sdk.geometry.SizeF.init(1100, 640),
    native_sdk.geometry.SizeF.init(1680, 1000),
};

const ChromeState = struct {
    label: []const u8,
    placement: core.TabPlacement = .top,
    tabs: usize = 1,
    palette: bool = false,
    settings: bool = false,
    settings_section: i64 = 0,
};

const parity_states = [_]ChromeState{
    .{ .label = "one tab, strip" },
    .{ .label = "one tab, rail", .placement = .side },
    .{ .label = "full strip", .tabs = 16 },
    .{ .label = "full rail", .tabs = 16, .placement = .side },
    .{ .label = "palette over strip", .palette = true },
    .{ .label = "full palette with paging", .palette = true, .tabs = 5 },
    .{ .label = "settings over rail", .settings = true, .placement = .side },
    .{ .label = "workspace settings", .settings = true, .settings_section = 1 },
    .{ .label = "connection settings", .settings = true, .settings_section = 2 },
    .{ .label = "both overlays, full strip", .tabs = 16, .palette = true, .settings = true },
};

fn auditChromeAt(model: *const core.Model, size: native_sdk.geometry.SizeF, density: canvas.Density, label: []const u8) !usize {
    const arena_bytes = try std.testing.allocator.alloc(u8, 1 << 20);
    defer std.testing.allocator.free(arena_bytes);
    var fixed = std.heap.FixedBufferAllocator.init(arena_bytes);
    var ui = Adapter.Ui.init(fixed.allocator());

    var tokens = cockpit.projection.cockpitTokens(bridge.engine.?.model);
    tokens.density = density;
    const node = mainView(&ui, model);
    const tree = try ui.finalizeWithTokens(node, tokens);

    const nodes = try std.testing.allocator.alloc(canvas.WidgetLayoutNode, canvas.max_layout_audit_nodes);
    defer std.testing.allocator.free(nodes);
    const bounds = native_sdk.geometry.RectF.init(0, 0, size.width, size.height);
    const layout = try canvas.layoutWidgetTreeWithTokens(tree.root, bounds, tokens, nodes);

    var storage: [canvas.max_layout_audit_findings]canvas.LayoutAuditFinding = undefined;
    const issues = canvas.auditWidgetLayout(layout, bounds, tokens, &storage);
    if (issues.total == 0) return 0;
    std.debug.print(
        "\nmarkup layout audit: {d} finding(s) in \"{s}\" at {d:.0}x{d:.0}, {s} density\n",
        .{ issues.total, label, size.width, size.height, @tagName(density) },
    );
    // The first three findings of a state say what is wrong; the rest of a
    // sixteen-tab overflow say it again.
    for (issues.findings, 0..) |finding, index| {
        if (index == 3) {
            std.debug.print("  - ... {d} more\n", .{issues.findings.len - 3});
            break;
        }
        var message: [1400]u8 = undefined;
        var writer = std.Io.Writer.fixed(&message);
        canvas.formatLayoutAuditFinding(layout, finding, &writer) catch {};
        std.debug.print("  - {s}\n", .{writer.buffered()});
    }
    return issues.total;
}

test "the markup chrome passes the layout audit at every declared size, density and state" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    var total: usize = 0;
    for (parity_states) |state| {
        try rig.reach(state);
        for (parity_sizes) |size| {
            try rig.resize(size);
            for ([_]canvas.Density{ .compact, .regular, .spacious }) |density| {
                total += try auditChromeAt(&rig.app_state.model, size, density, state.label);
            }
        }
    }
    try std.testing.expectEqual(@as(usize, 0), total);
}

test "crowded tab strip keeps every tab and overflow cue inside its allocated chrome slot" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.reach(.{ .label = "crowded strip", .tabs = 16 });
    const before = rig.app_state.model.engineSequence.lo;
    try rig.dispatch(.{ .select_tab = 15 });
    try rig.settle(before + 1, "READY");
    for (parity_sizes) |size| {
        try rig.resize(size);
        const layout = try rig.harness.runtime.canvasWidgetLayout(1, canvas_label);
        var strip: ?native_sdk.geometry.RectF = null;
        for (layout.nodes) |node| {
            if (std.mem.eql(u8, node.widget.semantics.label, "Terminal tabs")) strip = node.frame;
        }
        const frame = strip orelse return error.TestExpectedTabStrip;
        var seen: usize = 0;
        for (layout.nodes) |node| {
            if (node.widget.semantics.role != .tab and !std.mem.eql(u8, node.widget.semantics.label, "Tabs not shown")) continue;
            seen += 1;
            try std.testing.expect(node.frame.x >= frame.x);
            try std.testing.expect(node.frame.x + node.frame.width <= frame.x + frame.width);
        }
        try std.testing.expect(seen > 1);
    }
}

test "secondary windows expose every tab in the shared workspace rail" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    for (1..16) |_| {
        const before = rig.app_state.model.engineSequence.lo;
        try rig.dispatch(.new_terminal);
        try rig.settle(before + 1, "READY");
    }
    const engine = bridge.engine.?;
    const workspace = engine.model.wsAt(1).?;
    engine.model.tab_placement = .side;
    rig.app_state.model.tabPlacement = .side;
    const size = native_sdk.geometry.SizeF.init(1100, 640);
    workspace.surface_size = size;
    syncTerminalSpace(&rig.app_state.model, 1, size, cockpit.projection.cockpitTokens(engine.model));
    const run = engine.currentRuns()[1];
    try std.testing.expectEqual(workspace.tab_count, run.count);
    try std.testing.expectEqual(@as(u8, 0), run.first);
    try std.testing.expect(workspace.shipping_terminal_space.?.x >= 224);
}

test "the switcher filters the engine's tabs by position or title and selects through the seam" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.reach(.{ .label = "three tabs", .tabs = 3 });
    const engine = bridge.engine.?;

    try rig.dispatch(.palette_open);
    try rig.settleNavigation();
    try std.testing.expectEqual(@as(usize, 3), rig.app_state.model.paletteRows.len);
    try rig.dispatch(.{ .palette_edit = .{ .insert_text = "3" } });
    try rig.settleNavigation();
    try std.testing.expectEqual(@as(usize, 1), rig.app_state.model.paletteRows.len);
    try std.testing.expectEqual(@as(i64, 2), rig.app_state.model.paletteRows[0].index);

    // Enter on the input is its own on-submit; the core sends a fenced
    // select intent and the engine's selection moves.
    const before = rig.app_state.model.engineSequence.lo;
    try rig.dispatch(.palette_submit);
    try std.testing.expect(!rig.app_state.model.paletteOpen);
    try rig.settle(before + 1, "READY");
    try std.testing.expectEqual(@as(usize, 2), engine.model.wsConst().selected_tab);
    try std.testing.expectEqual(@as(i64, 2), rig.app_state.model.selectedTab);
}

test "painted navigation targets dispatch through the compiled core" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.reach(.{ .label = "navigation target", .tabs = 3, .palette = true });
    const target = rig.app_state.model.paletteRows[1].target;
    // Painted rows carry the captured provider-qualified catalog target.
    try std.testing.expect(target.len >= 38);
    try std.testing.expectEqual(@as(u8, 2), target[0]);
    try rig.dispatch(.{ .palette_pick = target });
    try std.testing.expect(!rig.app_state.model.paletteOpen);
}

test "populated navigation rows accept native pointer activation" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.resize(.init(1100, 640));
    try rig.settle(@intCast(bridge.engine.?.sequence), "READY");
    try rig.reach(.{ .label = "navigation pointer", .tabs = 3, .palette = true });
    const layout = try rig.harness.runtime.canvasWidgetLayout(1, canvas_label);
    var frame: ?native_sdk.geometry.RectF = null;
    for (layout.nodes) |node| {
        if (node.widget.kind == .list_item) frame = node.frame;
    }
    const row = frame orelse return error.MissingNavigationRow;
    inline for (.{ .pointer_down, .pointer_up }) |kind| {
        try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
            .window_id = 1,
            .label = canvas_label,
            .kind = kind,
            .x = row.x + row.width / 2,
            .y = row.y + row.height / 2,
        } });
    }
    try std.testing.expect(!rig.app_state.model.paletteOpen);
}

test "the switcher receives the focused split pane's cwd without polling terminal bytes" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;

    try rig.dispatch(core.commandMsg("pane.split-right").?);
    try rig.settle(1, "READY");
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    const wake = shellEvent(.{
        .key = pane.pty_key,
        .kind = .output,
        .bytes = "\x1b]7;file://host/tmp/right-pane\x1b\\",
    });
    try rig.dispatch(wake);
    try rig.settle(2, "READY");
    try std.testing.expectEqualStrings("/tmp/…", rig.app_state.model.tabs[0].cwd);

    try rig.dispatch(.palette_open);
    try rig.dispatch(.{ .palette_edit = .{ .insert_text = "right-pane" } });
    try rig.settleNavigation();
    try std.testing.expectEqual(@as(usize, 1), rig.app_state.model.paletteRows.len);
}

test "the settings surface shows the engine's theme catalog and saves through the seam" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;

    // Opening probes the config file once, through the seam; the catalog
    // rides every snapshot.
    try rig.dispatch(.settings_open);
    try rig.settleAppearance();
    try std.testing.expect(engine.config_probe.probed);
    try std.testing.expect(!engine.intent_refused);
    try std.testing.expectEqual(@as(usize, 6), rig.app_state.model.themes.len);
    try std.testing.expect(rig.app_state.model.configNotice.len > 0);

    try rig.dispatch(.{ .settings_pick = 3 });
    try rig.settleAppearance();
    try std.testing.expect(rig.app_state.model.themes[3].highlighted);
    try rig.dispatch(.settings_commit);
    try std.testing.expect(rig.app_state.model.settingsOpen);
    try rig.settleAppearance();
    // The harness has no config destination; preview remains cancellable.
    try std.testing.expect(rig.app_state.model.settingsOpen);
    try std.testing.expectEqual(.no_destination, bridge.appearance.outcome);
    try std.testing.expectEqualStrings("nord", engine.model.config.theme.slice());
    try std.testing.expect(rig.app_state.model.themes[3].active);
}

/// An effects recorder for the native-behaviour guards: counts what the
/// engine asked for and keeps the last clipboard text, no processes.
const Recorder = struct {
    pub fn restartPhux(self: *@This(), _: *Engine) bool {
        self.navigation_restarts += 1;
        return true;
    }
    pub fn showWindow(self: *@This(), label: []const u8) void {
        self.navigation_shown = label;
    }
    pub fn toggleFullscreenWindow(_: *@This(), _: []const u8) void {}
    pub fn minimizeWindow(_: *@This(), _: []const u8) void {}
    pub fn cancel(_: *@This(), _: u64) void {}
    pub fn closeWindow(_: *@This(), _: []const u8) void {}
    pub fn quitApp(_: *@This()) void {}
    navigation_restarts: usize = 0,
    navigation_shown: []const u8 = "",
    notifications: usize = 0,
    clipboard_writes: usize = 0,
    clipboard_reads: usize = 0,
    last_text: [256]u8 = undefined,
    last_text_len: usize = 0,
    pub fn hostSend(_: *Recorder, _: []const u8, _: []const u8) void {}
    pub fn ptySpawn(_: *Recorder, _: anytype) void {}
    pub fn ptyWrite(_: *Recorder, _: u64, _: []const u8) bool {
        return false;
    }
    pub fn ptyResize(_: *Recorder, _: u64, _: u16, _: u16) void {}
    pub fn ptyKill(_: *Recorder, _: u64) void {}
    pub fn openUrl(_: *Recorder, _: []const u8) void {}
    pub fn showNotification(self: *Recorder, _: anytype) void {
        self.notifications += 1;
    }
    pub fn writeClipboard(self: *Recorder, options: anytype) void {
        self.clipboard_writes += 1;
        self.last_text_len = @min(options.text.len, self.last_text.len);
        @memcpy(self.last_text[0..self.last_text_len], options.text[0..self.last_text_len]);
    }
    pub fn readClipboard(self: *Recorder, _: anytype) void {
        self.clipboard_reads += 1;
    }
    fn text(self: *const Recorder) []const u8 {
        return self.last_text[0..self.last_text_len];
    }
};

fn engineWithText(text: []const u8) !*Engine {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    pane.session.feed(text);
    pane.session.refreshScreenText();
    return engine;
}

test "shipping remote bell uses native notifications and owner fenced attention" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    const fixtures = @TypeOf(remote.*).test_support;
    const BellFx = struct {
        notifications: usize = 0,
        pub fn openChannel(_: *@This(), _: anytype) native_sdk.ChannelHandle {
            return .{};
        }
        pub fn closeChannel(_: *@This(), _: u64) void {}
        pub fn showNotification(self: *@This(), _: anytype) void {
            self.notifications += 1;
        }
    };
    var bells = BellFx{};
    var fx = Recorder{};
    engine.setInputSuspended(&fx, false);
    engine.setFocused(&fx, false);
    try std.testing.expect(!remote.bellRung(ref));
    for ([_][]const u8{ "remote-bell-1.bin", "remote-bell-2.bin" }) |name| {
        try fixtures.stageFixture(remote.bridge, name);
        _ = engine.onPhuxChannel(&bells, .{ .key = cockpit.phux_channel_key, .kind = .data }, null);
        try std.testing.expectEqual(@as(usize, 1), bells.notifications);
        try std.testing.expect(cockpit.projection.terminalNeedsAttention(engine.model, ref));
    }
    engine.setFocused(&fx, true);
    try std.testing.expect(!remote.bellRung(ref));
    engine.setFocused(&fx, false);
    try fixtures.stageFixture(remote.bridge, "remote-bell-3.bin");
    _ = engine.onPhuxChannel(&bells, .{ .key = cockpit.phux_channel_key, .kind = .data }, null);
    try std.testing.expectEqual(@as(usize, 2), bells.notifications);
    remote.acknowledgeBell(ref);
    engine.model.rejectAttachmentContext();
    try fixtures.stageFixture(remote.bridge, "remote-bell-4.bin");
    _ = engine.onPhuxChannel(&bells, .{ .key = cockpit.phux_channel_key, .kind = .data }, null);
    try std.testing.expectEqual(@as(usize, 2), bells.notifications);
    try std.testing.expect(!remote.bellRung(ref));
}

test "shipping snapshot exposes focused terminal history and fenced recovery" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    _ = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    var storage: [4096]u8 = undefined;
    var snapshot = try engine.snapshot(&storage);
    try rig.dispatch(.{ .snapshot_loaded = snapshot });
    try std.testing.expect(std.mem.indexOf(u8, rig.app_state.model.connectionStatus, "history") == null);
    remote.host.terminals.items[0].history_loading = true;
    snapshot = try engine.snapshot(&storage);
    try rig.dispatch(.{ .snapshot_loaded = snapshot });
    try std.testing.expect(std.mem.indexOf(u8, rig.app_state.model.connectionStatus, "Loading earlier history") != null);
    remote.host.freezePublished();
    snapshot = try engine.snapshot(&storage);
    try rig.dispatch(.{ .snapshot_loaded = snapshot });
    try std.testing.expect(std.mem.indexOf(u8, rig.app_state.model.connectionStatus, "Terminal frozen") != null);
    engine.model.rejectAttachmentContext();
    remote.host.terminals.items[0].phase = .live;
    snapshot = try engine.snapshot(&storage);
    try rig.dispatch(.{ .snapshot_loaded = snapshot });
    try std.testing.expect(std.mem.indexOf(u8, rig.app_state.model.connectionStatus, "Recovering terminal") != null);
}

test "a bell while the app is deactivated notifies once, on its rising edge" {
    const engine = try engineWithText("prompt$ ");
    defer engine.destroy();
    var fx = Recorder{};
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    const key = pane.pty_key;

    engine.setFocused(&fx, false);
    _ = engine.onShellEvent(&fx, .{ .key = key, .kind = .output, .bytes = "more\x07" });
    try std.testing.expectEqual(@as(usize, 1), fx.notifications);
    // A standing bell does not ring again until it is acknowledged.
    _ = engine.onShellEvent(&fx, .{ .key = key, .kind = .output, .bytes = "\x07" });
    try std.testing.expectEqual(@as(usize, 1), fx.notifications);

    // A focused app hears its own bell and is not notified.
    const attended = try engineWithText("prompt$ ");
    defer attended.destroy();
    var quiet = Recorder{};
    const front = attended.model.provider.terminal(attended.model.focusedTerminalRef().?).?;
    attended.setFocused(&quiet, true);
    _ = attended.onShellEvent(&quiet, .{ .key = front.pty_key, .kind = .output, .bytes = "\x07" });
    try std.testing.expectEqual(@as(usize, 0), quiet.notifications);
}

test "select all and cmd+C put the scrollback on the clipboard through the seam" {
    const engine = try engineWithText("hello world\r\n");
    defer engine.destroy();
    var fx = Recorder{};
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;

    engine.onKey(&fx, .{ .key = "a", .text = "", .phase = .key_down, .modifiers = .{ .super = true } });
    try std.testing.expect(pane.session.selectionActive());
    engine.onKey(&fx, .{ .key = "c", .text = "", .phase = .key_down, .modifiers = .{ .super = true } });
    try std.testing.expectEqual(@as(usize, 1), fx.clipboard_writes);
    try std.testing.expect(std.mem.indexOf(u8, fx.text(), "hello world") != null);
    try std.testing.expect(engine.model.copy_inflight);
    engine.onClipboardWritten(true);
    try std.testing.expect(!engine.model.copy_inflight);
}

fn remotePresentationCommand(engine: *cockpit.Engine, value: protocol.NativeCommand, fx: anytype) bool {
    const intent = protocol.encodeIntent(.{
        .kind = .native_command,
        .expected_revision = engine.revision,
        .argument = @intFromEnum(value),
    });
    return engine.applyIntent(&intent, fx);
}

test "remote presentation menu select all copies provider history without child input" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    var fx = Recorder{};
    try std.testing.expect(remotePresentationCommand(engine, .select_all, &fx));
    try std.testing.expect(remotePresentationCommand(engine, .copy, &fx));
    try std.testing.expect(std.mem.indexOf(u8, fx.text(), "COCKPIT FIXTURE") != null);
    try std.testing.expect(engine.model.copy_owner.terminal_ref.eql(ref));
    engine.onClipboardWritten(true);
    try std.testing.expect(!engine.model.remoteUi(ref).?.selecting);
    try std.testing.expect(remotePresentationCommand(engine, .clear, &fx));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

test "remote presentation search owns shipping chord text navigation clipboard and paint" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    try rig.settle(@intCast(engine.sequence), "READY");
    const before = engine.sequence;
    try rig.dispatch(onKey(.{ .phase = .key_down, .key = "f", .modifiers = .{ .super = true } }).?);
    try rig.settle(@intCast(before + 1), "READY");
    const state = engine.model.remoteUi(ref).?;
    try std.testing.expect(state.search.open);
    _ = onText(.{ .phase = .text_input, .key = "i", .text = "i" });
    try std.testing.expectEqualStrings("i", state.search.needle());
    try std.testing.expectEqual(@as(usize, 2), state.search.count);
    try std.testing.expectEqual(@as(usize, 1), state.search.index);
    _ = onKey(.{ .phase = .key_down, .key = "Enter" });
    try std.testing.expectEqual(@as(usize, 0), state.search.index);
    var fx = Recorder{};
    try std.testing.expect(remotePresentationCommand(engine, .find_previous, &fx));
    try std.testing.expectEqual(@as(usize, 1), state.search.index);
    try std.testing.expect(remotePresentationCommand(engine, .copy, &fx));
    try std.testing.expectEqualStrings("I", fx.text());
    engine.onClipboardWritten(true);
    try std.testing.expect(remotePresentationCommand(engine, .select_all, &fx));
    try std.testing.expect(remotePresentationCommand(engine, .copy, &fx));
    try std.testing.expect(std.mem.indexOf(u8, fx.text(), "COCKPIT FIXTURE") != null);
    engine.onClipboardWritten(true);
    try std.testing.expect(remotePresentationCommand(engine, .find_next, &fx));
    try std.testing.expect(remotePresentationCommand(engine, .find_previous, &fx));
    try expectSearchPaint(engine, "i", "2 of 2");
    _ = onKey(.{ .phase = .key_down, .key = "Backspace" });
    try std.testing.expectEqualStrings("", state.search.needle());
    _ = onKey(.{ .phase = .key_down, .key = "v", .modifiers = .{ .super = true } });
    engine.onClipboardRead(&fx, true, "COCKPIT\nnot shell input");
    try std.testing.expectEqualStrings("COCKPIT", state.search.needle());
    try std.testing.expectEqual(@as(usize, 1), state.search.count);
    _ = onKey(.{ .phase = .key_down, .key = "Escape" });
    try std.testing.expect(!state.search.open);
    try std.testing.expect(remote.host.search_owner == null);
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
}

test "shipping remote Clear preserves an incomplete VT sequence" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    const fixtures = @TypeOf(remote.*).test_support;
    try fixtures.stageFixture(remote.bridge, "clear-parser-prefix.bin");
    _ = phuxChannel(.{ .key = cockpit.phux_channel_key, .kind = .data });
    var fx = Recorder{};
    remote.bridge.outgoing.reset();
    try std.testing.expect(remotePresentationCommand(engine, .clear, &fx));
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    try fixtures.stageFixture(remote.bridge, "clear-parser-suffix.bin");
    _ = phuxChannel(.{ .key = cockpit.phux_channel_key, .kind = .data });
    const text = engine.model.remotePresentation(ref).?.grid.screen_text;
    try std.testing.expectEqualStrings("X", std.mem.trim(u8, text, " \r\n"));
}

test "remote presentation Clear blanks the current replica without execution input" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    const before = engine.model.remotePresentation(ref).?;
    const owner = before.owner;
    try std.testing.expect(std.mem.indexOf(u8, before.grid.screen_text, "COCKPIT FIXTURE") != null);
    var fx = Recorder{};
    try std.testing.expect(remotePresentationCommand(engine, .find, &fx));
    engine.onText(&fx, .{ .phase = .text_input, .text = "COCKPIT" });
    try std.testing.expectEqual(@as(usize, 1), engine.model.remoteUi(ref).?.search.count);
    try rig.settle(@intCast(engine.sequence), "READY");
    const sequence = engine.sequence;
    try rig.dispatch(core.commandMsg("terminal.clear").?);
    try rig.settle(@intCast(sequence + 1), "READY");
    const after = engine.model.remotePresentation(ref).?;
    try std.testing.expectEqualStrings("", std.mem.trim(u8, after.grid.screen_text, " \r\n"));
    try std.testing.expect(after.owner.eql(owner));
    try std.testing.expectEqual(.live, after.phase);
    try std.testing.expect(!engine.model.remoteUi(ref).?.selecting);
    try std.testing.expectEqual(@as(usize, 0), engine.model.remoteUi(ref).?.search.count);
    try std.testing.expect(remote.host.search_owner == null);
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    remote.stop();
    try std.testing.expect(!remotePresentationCommand(engine, .clear, &fx));
}

fn expectSearchPaint(engine: *cockpit.Engine, needle: []const u8, status: []const u8) !void {
    const commands = try std.testing.allocator.alloc(canvas.CanvasCommand, cockpit.projection.chrome_command_envelope);
    defer std.testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try engine.paint(&builder, .{ .width = 900, .height = 500 }, cockpit.projection.cockpitTokens(engine.model));
    var found_needle = false;
    var found_status = false;
    for (builder.displayList().commands) |command| switch (command) {
        .draw_text => |text| {
            if (text.id == 0x0d01) found_needle = std.mem.eql(u8, needle, text.text);
            if (text.id == 0x0d02) found_status = std.mem.eql(u8, status, text.text);
        },
        else => {},
    };
    try std.testing.expect(found_needle);
    try std.testing.expect(found_status);
}

test "remote presentation search consumes controls and rejects stale clipboard owners" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    var fx = Recorder{};
    try std.testing.expect(remotePresentationCommand(engine, .find, &fx));
    engine.onText(&fx, .{ .phase = .text_input, .text = "é" });
    const state = engine.model.remoteUi(ref).?;
    try std.testing.expectEqualStrings("é", state.search.needle());
    engine.onKey(&fx, .{ .phase = .key_down, .key = "Backspace" });
    try std.testing.expectEqualStrings("", state.search.needle());
    engine.onText(&fx, .{ .phase = .text_input, .text = "\x03" });
    try std.testing.expectEqualStrings("", state.search.needle());
    engine.onKey(&fx, .{ .phase = .key_down, .key = "ArrowLeft", .modifiers = .{ .alt = true } });
    engine.onKey(&fx, .{ .phase = .key_up, .key = "ArrowLeft", .modifiers = .{ .alt = true } });
    engine.onKey(&fx, .{ .phase = .key_down, .key = "v", .modifiers = .{ .super = true } });
    try std.testing.expectEqual(.search_needle, engine.model.paste_target);
    engine.onKey(&fx, .{ .phase = .key_down, .key = "Escape" });
    try std.testing.expect(remotePresentationCommand(engine, .find, &fx));
    engine.onClipboardRead(&fx, true, "closed field");
    try std.testing.expectEqualStrings("", state.search.needle());
    const near_limit = [_]u8{'x'} ** 127;
    engine.onText(&fx, .{ .phase = .text_input, .text = &near_limit });
    engine.onKey(&fx, .{ .phase = .key_down, .key = "v", .modifiers = .{ .super = true } });
    engine.onClipboardRead(&fx, true, "é");
    try std.testing.expectEqual(@as(usize, 127), state.search.needle_len);
    try std.testing.expect(std.unicode.utf8ValidateSlice(state.search.needle()));
    try expectSearchPaint(engine, &near_limit, "No matches");
    engine.onKey(&fx, .{ .phase = .key_down, .key = "v", .modifiers = .{ .super = true } });
    remote.host.terminals.items[0].generation.bootstrap_id += 1;
    engine.onClipboardRead(&fx, true, "stale");
    try std.testing.expect(!engine.model.paste_inflight);
    try std.testing.expect(engine.model.remoteUi(ref).?.search.open);
    try std.testing.expectEqualStrings(&near_limit, engine.model.remoteUi(ref).?.search.needle());
    try std.testing.expect(!engine.model.remoteUi(ref).?.search.paste_pending);
    try std.testing.expect(!remote.bridge.outgoing.hasPending());
    remote.stop();
    try std.testing.expect(!remotePresentationCommand(engine, .find, &fx));
    try std.testing.expect(engine.model.remoteUi(ref).?.search.open);
}

test "remote Find survives staged resize rebootstrap and reruns only on READY" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    const fixtures = @TypeOf(remote.*).test_support;
    var fx = Recorder{};
    const frame: native_sdk.platform.GpuFrame = .{
        .label = canvas_label,
        .size = .{ .width = 1100, .height = 640 },
        .scale_factor = 1,
        .frame_index = 2,
        .timestamp_ns = 2,
    };
    engine.pumpViewports(&fx, frame);
    const closed_rows = remote.lastViewport(ref).?.rows;
    try std.testing.expect(remotePresentationCommand(engine, .find, &fx));
    engine.onText(&fx, .{ .phase = .text_input, .text = "COCKPIT" });
    const state = engine.model.remoteUi(ref).?;
    const owner = state.owner;
    try std.testing.expectEqual(@as(usize, 1), state.search.count);
    try std.testing.expect(remotePresentationCommand(engine, .select_all, &fx));
    const old_anchor = state.start_anchor;
    try std.testing.expect(old_anchor != 0);
    try std.testing.expect(remotePresentationCommand(engine, .copy, &fx));
    state.gesture_handle = 99;
    state.wheel_accum = 0.5;
    state.wheel_accum_x = -0.5;
    state.search.restore_bottom = false;
    state.search.restore_row = 19;
    engine.onKey(&fx, .{ .phase = .key_down, .key = "v", .modifiers = .{ .super = true } });
    try std.testing.expect(state.search.paste_pending);
    // The search band changes the viewport; the server replies in three wakes,
    // so a query cannot run against the partial replacement document.
    engine.pumpViewports(&fx, frame);
    const open_rows = remote.lastViewport(ref).?.rows;
    try std.testing.expect(open_rows < closed_rows);
    const frames = try fixtures.readFixture("search-resize.bin");
    defer std.testing.allocator.free(frames);
    var offset: usize = 0;
    try fixtures.stageFrames(remote.bridge, frames, &offset, 1);
    _ = phuxChannel(.{ .key = cockpit.phux_channel_key, .kind = .data, .bytes = &.{1} });
    try std.testing.expect(state.search.open);
    try std.testing.expect(state.owner.eql(owner));
    try expectSearchPaint(engine, "COCKPIT", "1 of 1");
    engine.pumpViewports(&fx, frame);
    try std.testing.expectEqual(open_rows, remote.lastViewport(ref).?.rows);
    try fixtures.stageFrames(remote.bridge, frames, &offset, 1);
    _ = phuxChannel(.{ .key = cockpit.phux_channel_key, .kind = .data, .bytes = &.{1} });
    try std.testing.expect(state.owner.eql(owner));
    try std.testing.expectEqual(@as(usize, 1), state.search.count);
    try expectSearchPaint(engine, "COCKPIT", "1 of 1");
    engine.pumpViewports(&fx, frame);
    try std.testing.expectEqual(open_rows, remote.lastViewport(ref).?.rows);
    try fixtures.stageFrames(remote.bridge, frames, &offset, 1);
    _ = phuxChannel(.{ .key = cockpit.phux_channel_key, .kind = .data, .bytes = &.{1} });
    try std.testing.expect(state.search.open);
    try std.testing.expectEqualStrings("COCKPIT", state.search.needle());
    try std.testing.expect(!state.owner.eql(owner));
    try std.testing.expectEqual(@as(usize, 3), state.search.count);
    try std.testing.expectEqual(@as(usize, 2), state.search.index);
    try std.testing.expectEqual(@as(u64, 0), state.start_anchor);
    try std.testing.expectEqual(@as(u64, 0), state.end_anchor);
    try std.testing.expectEqual(@as(u64, 0), state.gesture_handle);
    try std.testing.expectEqual(@as(f32, 0), state.wheel_accum);
    try std.testing.expectEqual(@as(f32, 0), state.wheel_accum_x);
    try std.testing.expectEqual(@as(u64, 0), state.search.restore_row);
    try std.testing.expect(state.search.restore_bottom);
    try std.testing.expect(!state.search.paste_pending);
    try std.testing.expect(!state.search.refresh_pending);
    try std.testing.expect(remote.host.search_owner.?.eql(state.owner));
    engine.onClipboardRead(&fx, true, "stale clipboard");
    engine.onClipboardWritten(false);
    try std.testing.expect(!state.copy_failed);
    try std.testing.expectEqualStrings("COCKPIT", state.search.needle());
    try expectSearchPaint(engine, "COCKPIT", "3 of 3");
    engine.pumpViewports(&fx, frame);
    try std.testing.expectEqual(open_rows, remote.lastViewport(ref).?.rows);
    try std.testing.expect(remotePresentationCommand(engine, .copy, &fx));
    try std.testing.expectEqualStrings("COCKPIT", fx.text());
    engine.onClipboardWritten(true);
    engine.onKey(&fx, .{ .phase = .key_down, .key = "Escape" });
    try std.testing.expect(!state.search.open);
}

test "remote Find inheritance requires matching nonempty durable attachment evidence" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const model = bridge.engine.?.model;
    const remote = model.phux().?;
    const Context = @TypeOf(model.attachment_context);
    const current = model.attachment_context;
    const cases = [_]Context{
        try .init("/other.sock", current.server_id.slice(), current.session_id),
        try .init(current.endpoint.slice(), "another-incarnation", current.session_id),
        try .init(current.endpoint.slice(), current.server_id.slice(), current.session_id + 1),
        .{},
    };
    for (cases) |context| {
        const state = model.remoteUi(ref).?;
        state.attachment_context = context;
        state.search.open = true;
        state.search.needle_buf[0] = 'x';
        state.search.needle_len = 1;
        state.search.paste_pending = true;
        remote.host.terminals.items[0].generation.bootstrap_id += 1;
        const replacement = model.remoteUi(ref).?;
        try std.testing.expect(!replacement.search.open);
        try std.testing.expectEqualStrings("", replacement.search.needle());
        try std.testing.expect(!replacement.search.paste_pending);
    }
    // A provider owner alone never admits a retained UI through a mismatching
    // saved-attachment gate, including the const paint path.
    try model.setAttachmentContext(current.endpoint.slice(), "another-incarnation", current.session_id);
    try std.testing.expect(model.attachmentPending(ref));
    try std.testing.expect(model.remoteUi(ref) == null);
    try std.testing.expect(model.remoteUiConst(ref) == null);
}

test "remote Find presentation survives frozen publication without admitting commands" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const model = engine.model;
    const remote = model.phux().?;
    var fx = Recorder{};
    try std.testing.expect(remotePresentationCommand(engine, .find, &fx));
    engine.onText(&fx, .{ .phase = .text_input, .text = "COCKPIT" });
    const owner = model.remoteUi(ref).?.owner;
    const size = native_sdk.geometry.SizeF.init(1100, 640);
    const before = cockpit.projection.workspaceChromeIn(model, model.ws(), size);
    try std.testing.expect(before.search.height > 0);
    remote.host.freezePublished();
    try std.testing.expect(!model.ownerIsCurrent(owner));
    try std.testing.expect(!remotePresentationCommand(engine, .find, &fx));
    const frozen = cockpit.projection.workspaceChromeIn(model, model.ws(), size);
    try std.testing.expectEqual(before.search.height, frozen.search.height);
    try std.testing.expectEqual(before.content.height, frozen.content.height);
    try expectSearchPaint(engine, "COCKPIT", "1 of 1");
    // Retaining a frozen field must not loosen either owner or attachment fences.
    remote.host.terminals.items[0].generation.bootstrap_id += 1;
    try std.testing.expect(model.remoteUiConst(ref) == null);
}

test "cmd+F opens the scrollback search, typing feeds the needle, Escape closes it" {
    const engine = try engineWithText("alpha\r\nbeta\r\n");
    defer engine.destroy();
    var fx = Recorder{};
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;

    engine.onKey(&fx, .{ .key = "f", .text = "", .phase = .key_down, .modifiers = .{ .super = true } });
    try std.testing.expect(pane.session.search.open);
    engine.onText(&fx, .{ .key = "", .text = "bet", .phase = .text_input });
    try std.testing.expectEqualStrings("bet", pane.session.searchNeedle());
    try std.testing.expectEqual(@as(usize, 0), pane.outbound_len);
    engine.onKey(&fx, .{ .key = "Escape", .text = "", .phase = .key_down });
    try std.testing.expect(!pane.session.search.open);
}

test "a drag across the grid through the raw surface input selects text" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    pane.session.feed("the quick brown fox jumps over the lazy dog\r\n");
    pane.session.refreshScreenText();
    try rig.resize(native_sdk.geometry.SizeF.init(1100, 640));
    const frame = cockpit.engine.pointerFrame(engine) orelse return error.TestExpectedFrame;
    try std.testing.expect(!pane.session.selectionActive());

    const y = frame.y + 12;
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{ .window_id = 1, .label = canvas_label, .kind = .pointer_down, .x = frame.x + 8, .y = y, .timestamp_ns = 1 } });
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{ .window_id = 1, .label = canvas_label, .kind = .pointer_drag, .x = frame.x + 160, .y = y, .timestamp_ns = 2 } });
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{ .window_id = 1, .label = canvas_label, .kind = .pointer_up, .x = frame.x + 160, .y = y, .timestamp_ns = 3 } });
    try std.testing.expect(pane.session.selectionActive());
}

test "Finder drops stay native and enter the focused pane as bracketed paste" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    pane.session.feed("\x1b[?2004h");
    try std.testing.expectEqual(@as(usize, 0), pane.outbound_len);

    routeNativeInput(.{ .files_dropped = .{
        .view_label = canvas_label,
        .paths = &.{ "/tmp/a b.txt", "/tmp/second" },
    } });

    const queued = pane.outbound_buffer[0..pane.outbound_len];
    try std.testing.expect(std.mem.startsWith(u8, queued, "\x1b[200~"));
    try std.testing.expect(std.mem.indexOf(u8, queued, "'/tmp/a b.txt' '/tmp/second' ") != null);
    try std.testing.expect(std.mem.endsWith(u8, queued, "\x1b[201~"));
}

test "selection edge drag autoscrolls through the TypeScript host timer" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    pane.session.reset();
    var lines: [2048]u8 = undefined;
    var writer = std.Io.Writer.fixed(&lines);
    for (0..80) |index| try writer.print("row {d}\r\n", .{index});
    pane.session.feed(writer.buffered());
    pane.session.refreshScreenText();
    try rig.resize(native_sdk.geometry.SizeF.init(1100, 640));
    const frame = cockpit.engine.pointerFrame(engine) orelse return error.TestExpectedFrame;
    const start = native_sdk.geometry.PointF.init(frame.x + 24, frame.y + 24);
    const above = native_sdk.geometry.PointF.init(start.x, frame.y - 8);

    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .pointer_down,
        .x = start.x,
        .y = start.y,
        .timestamp_ns = 1,
    } });
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .pointer_drag,
        .x = above.x,
        .y = above.y,
        .timestamp_ns = 2,
    } });
    try std.testing.expect(pointer_host.selection_autoscroll_timer_active);
    const timer = rig.harness.null_platform.startedTimer(cockpit.selection_autoscroll_timer_id) orelse return error.TestExpectedTimer;
    try std.testing.expect(timer.active and timer.repeats);
    try std.testing.expectEqual(cockpit.selection_autoscroll_interval_ns, timer.interval_ns);
    const before = pane.session.scrollbar().offset;
    const fired = rig.harness.null_platform.fireTimer(cockpit.selection_autoscroll_timer_id, 20 * std.time.ns_per_ms) orelse return error.TestExpectedTimer;
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, fired);
    try std.testing.expect(before > 0);
    try std.testing.expectEqual(before - 1, pane.session.scrollbar().offset);

    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .gpu_surface_input = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .pointer_up,
        .x = above.x,
        .y = above.y,
        .timestamp_ns = 3,
    } });
    try std.testing.expect(!pointer_host.selection_autoscroll_timer_active);
}

test "a new window opens a second workspace with its own shell and closes whole through the seam" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    // windows(model) builds its descriptors in the frame arena it is handed.
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    const frame = arena.allocator();

    var before = rig.app_state.model.engineSequence.lo;
    try rig.dispatch(.new_window);
    try rig.settle(before + 1, "READY");
    try std.testing.expect(engine.model.windowOpen(1));
    try std.testing.expectEqual(@as(usize, 1), engine.model.wsAtConst(1).?.tab_count);
    try std.testing.expect(rig.app_state.model.window1Open);
    try std.testing.expectEqual(@as(usize, 1), rig.app_state.model.window1Tabs.len);
    try std.testing.expectEqual(@as(usize, 1), rig.app_state.model.windows(frame).len);
    // The main window's own list is untouched by the second window's tab.
    try std.testing.expectEqual(@as(usize, 1), rig.app_state.model.tabs.len);

    // A tab intent from the second window's chrome names that window.
    try std.testing.expectEqual(@as(i64, 32), rig.app_state.model.window1Tabs[0].slot);

    // The OS close button arrives as the declared command; the engine
    // retires the slot and its shell, and the descriptor goes with it.
    before = rig.app_state.model.engineSequence.lo;
    try rig.dispatch(.{ .window_closed = 1 });
    try rig.settle(before + 1, "READY");
    try std.testing.expect(!engine.model.windowOpen(1));
    try std.testing.expect(!rig.app_state.model.window1Open);
    try std.testing.expectEqual(@as(usize, 0), rig.app_state.model.windows(frame).len);
}

fn compiledViewHasLabel(model: *const core.Model, window_index: usize, label: []const u8) !bool {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    var ui = Adapter.Ui.init(arena.allocator());
    const node = if (window_index == 0)
        mainView(&ui, model)
    else
        windowView(&ui, model, "phux-window-1");
    const tree = try ui.finalizeWithTokens(node, cockpit.projection.cockpitTokens(bridge.engine.?.model));
    const nodes = try arena.allocator().alloc(canvas.WidgetLayoutNode, canvas.max_layout_audit_nodes);
    const layout_tree = try canvas.layoutWidgetTreeWithTokens(
        tree.root,
        native_sdk.geometry.RectF.init(0, 0, 1100, 640),
        cockpit.projection.cockpitTokens(bridge.engine.?.model),
        nodes,
    );
    for (layout_tree.nodes) |entry| {
        if (std.mem.eql(u8, entry.widget.semantics.label, label)) return true;
    }
    return false;
}

test "healthy canvas gives the footer space to the terminal" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    var ui = Adapter.Ui.init(arena.allocator());
    const tokens = cockpit.projection.cockpitTokens(bridge.engine.?.model);
    const tree = try ui.finalizeWithTokens(mainView(&ui, &rig.app_state.model), tokens);
    const nodes = try arena.allocator().alloc(canvas.WidgetLayoutNode, canvas.max_layout_audit_nodes);
    const measured = try canvas.layoutWidgetTreeWithTokens(tree.root, .init(0, 0, 1100, 640), tokens, nodes);
    var header_bottom: f32 = 0;
    var status_top: f32 = 640;
    for (measured.nodes) |entry| {
        if (std.mem.eql(u8, entry.widget.semantics.label, "Terminal tabs")) header_bottom = entry.frame.y + entry.frame.height;
        if (entry.widget.kind == .status_bar) status_top = entry.frame.y;
    }
    try std.testing.expect(header_bottom > 0);
    try std.testing.expectEqual(@as(f32, 640), status_top);
    const content = cockpit.projection.workspaceChrome(bridge.engine.?.model, .init(1100, 640)).content;
    try std.testing.expect(content.y >= header_bottom);
    try std.testing.expect(content.y + content.height <= status_top);
}

test "shipping unchanged GPU frames reuse compiled terminal geometry" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    var frame: native_sdk.platform.GpuFrame = .{ .label = cockpit.scene.canvasLabelFor(0), .window_id = 1, .size = .init(1100, 640), .scale_factor = 1, .frame_index = 2, .timestamp_ns = 2 };
    _ = onFrame(&rig.app_state.model, frame);
    const before = terminal_space_measurements;
    for (0..8) |_| _ = onFrame(&rig.app_state.model, frame);
    try std.testing.expectEqual(before, terminal_space_measurements);
    frame.size.width += 100;
    _ = onFrame(&rig.app_state.model, frame);
    try std.testing.expectEqual(before + 1, terminal_space_measurements);
    _ = onFrame(&rig.app_state.model, frame);
    try std.testing.expectEqual(before + 1, terminal_space_measurements);
    // A chrome rebuild remains authoritative even at the same surface size.
    syncTerminalSpace(&rig.app_state.model, 0, frame.size, cockpit.projection.cockpitTokens(bridge.engine.?.model));
    try std.testing.expectEqual(before + 2, terminal_space_measurements);
    _ = onFrame(&rig.app_state.model, frame);
    try std.testing.expectEqual(before + 2, terminal_space_measurements);
}

fn expectRectInside(inner: native_sdk.geometry.RectF, outer: native_sdk.geometry.RectF) !void {
    try std.testing.expect(inner.x >= outer.x);
    try std.testing.expect(inner.y >= outer.y);
    try std.testing.expect(inner.x + inner.width <= outer.x + outer.width + 0.001);
    try std.testing.expect(inner.y + inner.height <= outer.y + outer.height + 0.001);
}

fn expectShippingWindowGeometry(rig: *Rig, index: usize, size: native_sdk.geometry.SizeF, search: bool) !void {
    const engine = bridge.engine.?;
    const workspace = engine.model.wsAt(index).?;
    const pane = engine.model.provider.terminal(workspace.focusedTerminalRef().?).?;
    pane.session.search.open = search;
    const label = cockpit.scene.canvasLabelFor(index);
    try std.testing.expectEqual(index, Engine.windowIndexForCanvas(label).?);
    const frame: native_sdk.platform.GpuFrame = .{ .label = label, .window_id = @intCast(index + 1), .size = size, .scale_factor = 1, .frame_index = 2, .timestamp_ns = 2 };
    _ = onFrame(&rig.app_state.model, frame);
    const tokens = cockpit.projection.cockpitTokens(engine.model);
    const commands = try std.testing.allocator.alloc(canvas.CanvasCommand, cockpit.projection.chrome_command_envelope);
    defer std.testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try paintChromeWindow(&rig.app_state.model, &builder, .{ .is_main = index == 0, .canvas_label = label, .window_id = frame.window_id, .size = size, .tokens = tokens });
    _ = onFrame(&rig.app_state.model, frame);
    const chrome = cockpit.projection.workspaceChromeIn(engine.model, workspace, size);
    try std.testing.expectEqual(search, chrome.search.height > 0);
    try expectRectInside(chrome.search, workspace.shipping_terminal_space.?);
    try expectRectInside(chrome.content, workspace.shipping_terminal_space.?);
    const cell = pane.session.measuredCell().?;
    try std.testing.expect(pane.session.rows() > 0);
    // First and last complete cell rows fit in the slot that markup leaves.
    try expectRectInside(.init(chrome.content.x, chrome.content.y, cell.width, cell.height), chrome.content);
    try expectRectInside(.init(chrome.content.x, chrome.content.y + @as(f32, @floatFromInt(pane.session.rows() - 1)) * cell.height, cell.width, cell.height), chrome.content);
    try expectShippingInteraction(rig, index, size, chrome.content);
}

fn expectShippingInteraction(rig: *Rig, index: usize, size: native_sdk.geometry.SizeF, content: native_sdk.geometry.RectF) !void {
    var arena = std.heap.ArenaAllocator.init(std.testing.allocator);
    defer arena.deinit();
    var ui = Adapter.Ui.init(arena.allocator());
    const tokens = cockpit.projection.cockpitTokens(bridge.engine.?.model);
    const node = composeView(&ui, &rig.app_state.model, compiledWindow(&ui, &rig.app_state.model, index), index);
    const tree = try ui.finalizeWithTokens(node, tokens);
    const nodes = try arena.allocator().alloc(canvas.WidgetLayoutNode, canvas.max_layout_audit_nodes);
    const measured = try canvas.layoutWidgetTreeWithTokens(tree.root, .init(0, 0, size.width, size.height), tokens, nodes);
    var panes: [cockpit.layout.max_panes]cockpit.layout.Pane = undefined;
    const engine = bridge.engine.?;
    const count = cockpit.projection.resolvePanesIn(engine.model, engine.model.wsAtConst(index).?, size, &panes);
    var terminals: usize = 0;
    for (measured.nodes) |entry| {
        if (entry.widget.semantics.role != .textbox) continue;
        try expectRectInside(entry.frame, content);
        try std.testing.expect(terminals < count);
        try std.testing.expectEqual(panes[terminals].rect, entry.frame);
        terminals += 1;
    }
    try std.testing.expect(count > 0);
    try std.testing.expectEqual(count, terminals);
}

test "shipping geometry follows each window markup through placement resize and search" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(core.commandMsg("pane.split-right").?);
    try rig.settle(1, "READY");
    for (1..5) |_| {
        const before = rig.app_state.model.engineSequence.lo;
        try rig.dispatch(.new_window);
        try rig.settle(before + 1, "READY");
    }
    for ([_]bool{ false, true }) |side| {
        rig.app_state.model.tabPlacement = if (side) .side else .top;
        for ([_]native_sdk.geometry.SizeF{ .init(1100, 640), .init(900, 420) }) |size| {
            for (0..5) |index| {
                try expectShippingWindowGeometry(&rig, index, size, false);
                try expectShippingWindowGeometry(&rig, index, size, true);
            }
        }
    }
}

test "shipping compiled chrome leaves remote row zero visible and selectable" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const ref = try rig.attachFixture();
    const engine = bridge.engine.?;
    const remote = engine.model.phux().?;
    const size = native_sdk.geometry.SizeF.init(1100, 640);
    const tokens = cockpit.projection.cockpitTokens(engine.model);
    const commands = try std.testing.allocator.alloc(canvas.CanvasCommand, cockpit.projection.chrome_command_envelope);
    defer std.testing.allocator.free(commands);
    var builder = canvas.Builder.init(commands);
    try paintChrome(&rig.app_state.model, &builder, size, tokens);
    const space = (try measureTerminalSpace(std.testing.allocator, &rig.app_state.model, 0, size, tokens)).terminal;
    const rect = cockpit.projection.paneFrameFor(engine.model, size, ref).?;
    try expectRectInside(rect, space);
    const cell = engine.model.remotePresentation(ref).?.measured_cell.?;
    const fx = engineFx().?;
    engine.setFocused(fx, false);
    engine.setFocused(fx, true);
    var raw: native_sdk.platform.GpuSurfaceInputEvent = .{
        .window_id = 1,
        .label = canvas_label,
        .kind = .pointer_down,
        .pointer_id = 7,
        .x = rect.x + cell.width * 0.25,
        .y = rect.y + cell.height * 0.25,
        .timestamp_ns = 1,
    };
    try std.testing.expectEqual(.consumed, engine.onPointer(fx, raw));
    raw.kind = .pointer_drag;
    raw.x = rect.x + cell.width * 6.75;
    try std.testing.expectEqual(.consumed, engine.onPointer(fx, raw));
    raw.kind = .pointer_up;
    try std.testing.expectEqual(.consumed, engine.onPointer(fx, raw));
    const text = try remote.selectionText(engine.model.terminalOwner(ref).?, std.testing.allocator);
    defer std.testing.allocator.free(text);
    try std.testing.expectEqualStrings("COCKPIT", text);
}

test "a focused secondary snapshot keeps main and secondary projections distinct" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;

    // Open the second window, then make a main-window mutation so both the
    // engine and core agree that main is active before the platform command.
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    try rig.dispatch(.{ .select_tab = 0 });
    try rig.settle(2, "READY");
    try rig.dispatch(.new_terminal);
    try rig.settle(3, "READY");
    try std.testing.expectEqual(@as(usize, 0), engine.model.active_window);
    try std.testing.expectEqual(@as(i64, 0), rig.app_state.model.activeWindow);

    // This is the stable process-local window id the native host records; it
    // never enters TypeScript. The command's projection slot does.
    engine.model.wsAt(1).?.window_id = 42;
    try rig.harness.runtime.dispatchPlatformEvent(rig.decorated, .{ .native_command = .{
        .name = "tabs.palette",
        .window_id = 42,
    } });
    try rig.settle(4, "READY");

    try std.testing.expectEqual(@as(usize, 1), engine.model.active_window);
    try std.testing.expectEqual(@as(i64, 1), rig.app_state.model.activeWindow);
    try std.testing.expectEqual(@as(usize, 2), rig.app_state.model.tabs.len);
    try std.testing.expectEqual(@as(usize, 1), rig.app_state.model.window1Tabs.len);
}

test "a secondary-window switcher is scoped to the focused window" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    try std.testing.expectEqual(@as(i64, 1), rig.app_state.model.activeWindow);

    try rig.dispatch(.palette_open);
    try std.testing.expect(rig.app_state.model.paletteOpen);
    try std.testing.expect(!rig.app_state.model.mainPaletteOpen);
    try std.testing.expect(rig.app_state.model.window1PaletteOpen);
    try std.testing.expect(!try compiledViewHasLabel(&rig.app_state.model, 0, "Find terminal or session"));
    try std.testing.expect(try compiledViewHasLabel(&rig.app_state.model, 1, "Find terminal or session"));
}

test "Connect to Host is presented in the secondary window that invoked it" {
    // The switcher's Connect to Host button is in every window's chrome, but
    // the panel used to render only in main: invoked from a secondary window
    // it opened behind the user and took the keyboard with it.
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.dispatch(.new_window);
    try rig.settle(1, "READY");
    try std.testing.expectEqual(@as(i64, 1), rig.app_state.model.activeWindow);
    try std.testing.expect(!try compiledViewHasLabel(&rig.app_state.model, 1, "Remote host"));

    try rig.dispatch(.host_open);
    try std.testing.expect(rig.app_state.model.hostOpen);
    try std.testing.expect(!rig.app_state.model.mainHostOpen);
    try std.testing.expect(rig.app_state.model.window1HostOpen);
    try std.testing.expect(!try compiledViewHasLabel(&rig.app_state.model, 0, "Remote host"));
    try std.testing.expect(try compiledViewHasLabel(&rig.app_state.model, 1, "Remote host"));
}

fn navigationRequestBytes(revision: u64) [13]u8 {
    var bytes = [_]u8{0} ** 13;
    bytes[0] = 1;
    bytes[1] = 3;
    std.mem.writeInt(u64, bytes[2..10], revision, .little);
    return bytes;
}

fn navigationIntentBytes(revision: u64, index: u16) [12]u8 {
    var bytes = [_]u8{0} ** 12;
    bytes[0] = 1;
    bytes[1] = 13;
    std.mem.writeInt(u64, bytes[2..10], revision, .little);
    std.mem.writeInt(u16, bytes[10..12], index, .little);
    return bytes;
}

test "navigation bridge preserves independently pending catalog and snapshot completions" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var service: Bridge = .{ .engine = engine };
    const query = navigationRequestBytes(engine.revision);
    Bridge.request(&service, cockpit.engine.navigation.request_name, 22, &query);
    Bridge.request(&service, protocol.snapshot_request, 11, "");
    const snapshot_reply = Bridge.poll(&service).?;
    try std.testing.expectEqual(@as(u64, 11), snapshot_reply.key);
    try std.testing.expect(snapshot_reply.ok);
    try std.testing.expectEqual(@as(u8, 2), snapshot_reply.bytes[1]);
    try std.testing.expect(Bridge.hasPending(&service));
    const catalog_reply = Bridge.poll(&service).?;
    try std.testing.expectEqual(@as(u64, 22), catalog_reply.key);
    try std.testing.expect(catalog_reply.ok);
    try std.testing.expectEqual(@as(u8, 3), catalog_reply.bytes[1]);
    try std.testing.expect(Bridge.poll(&service) == null);
    Bridge.request(&service, protocol.snapshot_request, 33, "");
    Bridge.request(&service, cockpit.engine.navigation.request_name, 44, &query);
    Bridge.cancel(&service, 44);
    try std.testing.expectEqual(@as(u64, 33), Bridge.poll(&service).?.key);
    try std.testing.expect(!Bridge.hasPending(&service));
}

test "navigation waits for snapshot commit before advancing positional fences" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const before = rig.app_state.model.engineRevision.lo;
    const bytes = protocol.encodeInvalidation(1, @intCast(before + 1));
    try rig.dispatch(.{ .engine_event = .{ .key = protocol.event_channel_key, .state = .data, .bytes = &bytes, .droppedPending = 0, .droppedTotal = 0 } });
    try std.testing.expectEqual(before, rig.app_state.model.engineRevision.lo);
    try std.testing.expect(!rig.app_state.model.engineConnected);
    try std.testing.expectEqualStrings("SYNCING", rig.app_state.model.status);
}

test "navigation selects an exact split pane across windows through the shipping bridge" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    const engine = bridge.engine.?;
    const original = engine.model.focusedTerminalRef().?;
    try rig.dispatch(core.commandMsg("pane.split-right").?);
    try rig.settle(1, "READY");
    try std.testing.expect(!engine.model.focusedTerminalRef().?.eql(original));
    try rig.dispatch(.new_window);
    try rig.settle(2, "READY");
    try rig.dispatch(.palette_open);
    try rig.settleNavigation();
    try std.testing.expectEqual(@as(usize, 3), rig.app_state.model.paletteRows.len);
    try rig.dispatch(.{ .palette_pick = rig.app_state.model.paletteRows[0].target });
    try rig.settle(3, "READY");
    try std.testing.expectEqual(@as(usize, 0), engine.model.active_window);
    try std.testing.expect(engine.model.focusedTerminalRef().?.eql(original));
    try std.testing.expect(!rig.app_state.model.paletteOpen);
    const stale = navigationIntentBytes(engine.revision - 1, 2);
    try std.testing.expect(!engine.applyIntent(&stale, &cockpit.NoShells{}));
    try std.testing.expect(engine.model.focusedTerminalRef().?.eql(original));
}

test "navigation activates available remote identity and stable session id including reconnect" {
    var recorder: Recorder = .{};
    try cockpit.durable_tests.navigationSharedAdmission(&recorder);
}

test "navigation catalog receipts distinguish shared admission and current session application" {
    var recorder: Recorder = .{};
    try cockpit.catalog_tests.currentSession(&recorder);
    try cockpit.catalog_tests.admission(&recorder);
}

test "navigation retained rows cannot acquire replacement connection authority" {
    try cockpit.catalog_tests.reconnectProvenance();
}

test "navigation catalog pending bridge receipt survives other requests" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const engine = try cockpit.catalog_tests.prepareAvailable();
    defer engine.destroy();
    const commands = cockpit.engine.tab_commands;
    var buffer: [10 + commands.catalog.max_len]u8 = undefined;
    const request = cockpit.catalog_tests.availableRequest(engine, &buffer);
    var service: Bridge = .{ .engine = engine };
    Bridge.request(&service, commands.request_name, 9001, request);
    Bridge.request(&service, protocol.snapshot_request, 9002, "");
    Bridge.request(&service, cockpit.engine.navigation.request_name, 9003, "invalid");
    const receipt = Bridge.poll(&service).?;
    try std.testing.expectEqual(@as(u64, 9001), receipt.key);
    try std.testing.expectEqual(@as(u8, 3), receipt.bytes[1]);
    try std.testing.expectEqual(@as(u8, 0), receipt.bytes[2]);
    try std.testing.expectEqual(@as(u64, 0xfedc_ba98_7654_3210), std.mem.readInt(u64, receipt.bytes[3..11], .little));
    try std.testing.expectEqual(@as(u64, 9002), Bridge.poll(&service).?.key);
    try std.testing.expectEqual(@as(u64, 9003), Bridge.poll(&service).?.key);
}

test "navigation snapshots preserve full window inventory within the host payload limit" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    for (0..2) |window| {
        if (window > 0) {
            const open = protocol.encodeIntent(.{ .kind = .new_window, .expected_revision = engine.revision, .argument = 0 });
            try std.testing.expect(engine.applyIntent(&open, &cockpit.NoShells{}));
        }
        for (1..16) |_| {
            const create = protocol.encodeIntent(.{ .kind = .new_terminal, .expected_revision = engine.revision, .argument = 0, .window = @intCast(window) });
            try std.testing.expect(engine.applyIntent(&create, &cockpit.NoShells{}));
        }
        for (0..16) |tab| {
            const ref = engine.model.wsAtConst(window).?.tabTerminal(tab).?;
            const pane = engine.model.provider.terminal(ref).?;
            pane.session.feed("\x1b]2;" ++ "T" ** 128 ++ "\x07");
            pane.session.feed("\x1b]7;file://host/" ++ "d" ** 127 ++ "\x1b\\");
            try std.testing.expectEqual(@as(usize, 128), pane.pwd().len);
        }
    }
    var buffer: [cockpit.snapshot.max_bytes]u8 = undefined;
    const bytes = try engine.snapshot(&buffer);
    try std.testing.expectEqual(@as(u8, 16), bytes[20]);
    try std.testing.expect(bytes.len <= 4096);
    const query = navigationRequestBytes(engine.revision);
    const page = try engine.navigationSnapshot(&query, &buffer);
    try std.testing.expectEqual(@as(u16, 32), std.mem.readInt(u16, page[13..15], .little));
}

test "shipping remote close removes shared presentation and navigation reattaches catalog identity" {
    try cockpit.durable_tests.sharedCloseAndCatalogAdmission();
}

test "shipping native close rehomes shared tabs while topology mutation is busy" {
    try cockpit.durable_tests.nativeCloseRehomesWhileBusy();
}

test "shipping offline shared topology edits refuse while native placement remains local" {
    try cockpit.durable_tests.offlineSharedCloseRefuses();
}

const NavigationConnectionRecorder = struct {
    pub fn hostSend(_: *@This(), _: []const u8, _: []const u8) void {}
    pub fn closeWindow(_: *@This(), _: []const u8) void {}
    pub fn showWindow(_: *@This(), _: []const u8) void {}
    pub fn toggleFullscreenWindow(_: *@This(), _: []const u8) void {}
    pub fn minimizeWindow(_: *@This(), _: []const u8) void {}
    pub fn quitApp(_: *@This()) void {}
    pub fn writeClipboard(_: *@This(), _: anytype) void {}
    pub fn readClipboard(_: *@This(), _: anytype) void {}
    pub fn ptyWrite(_: *@This(), _: u64, _: []const u8) bool {
        return false;
    }
    pub fn ptyKill(_: *@This(), _: u64) void {}
    pub fn ptyResize(_: *@This(), _: u64, _: u16, _: u16) void {}
    pub fn cancel(_: *@This(), _: u64) void {}
    live: bool,
    closed: usize = 0,
    opened: usize = 0,

    pub fn restartPhux(self: *@This(), engine: *Engine) bool {
        return engine.restartNavigationConnection(self, phuxChannel);
    }

    pub fn showNotification(_: *@This(), _: anytype) void {}
    pub fn phuxChannelLive(self: *@This()) bool {
        return self.live;
    }
    pub fn closeChannel(self: *@This(), _: u64) void {
        self.closed += 1;
    }
    pub fn openChannel(self: *@This(), _: anytype) native_sdk.ChannelHandle {
        self.opened += 1;
        // Deterministically exercise channel admission failure, without a
        // worker thread or a socket racing this lifecycle assertion.
        return .{};
    }
};

test "session command retains exact result through old close and immediate replacement failure" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    for ([_]bool{ false, true }) |live| {
        const engine = try cockpit.catalog_tests.prepareAvailable();
        defer engine.destroy();
        const commands = cockpit.engine.tab_commands;
        const target = commands.catalog.capture(engine.model, .{ .session = 2 }).?;
        var target_buffer: [commands.catalog.max_len]u8 = undefined;
        const target_bytes = target.encode(&target_buffer);
        var packet: [commands.catalog.max_len + 10]u8 = undefined;
        packet[0] = 1;
        packet[1] = 2;
        std.mem.writeInt(u64, packet[2..10], 0xfedcba9876543210, .little);
        @memcpy(packet[10..][0..target_bytes.len], target_bytes);
        var effects: NavigationConnectionRecorder = .{ .live = live };
        const receipt = engine.applySelectionCommand(packet[0 .. 10 + target_bytes.len], &effects);
        try std.testing.expectEqual(.accepted_pending, receipt.status);
        if (live) {
            try std.testing.expect(engine.creation.peekCompletion() == null);
            try std.testing.expectEqual(@as(usize, 1), engine.creation.count());
            _ = engine.onPhuxChannel(&effects, .{ .key = cockpit.phux_channel_key, .kind = .closed }, phuxChannel);
        }
        try std.testing.expectEqual(@as(usize, 1), effects.opened);
        const result = engine.creation.peekCompletion().?;
        try std.testing.expectEqual(receipt.id, result.command_id);
        try std.testing.expectEqual(@as(u32, 2), result.target_session_id);
        try std.testing.expectEqual(@as(u32, 0), result.request_id);
        try std.testing.expectEqual(.unknown, result.operation);
        try std.testing.expect(engine.session_handoff == null);
        _ = engine.onPhuxChannel(&effects, .{ .key = cockpit.phux_channel_key, .kind = .closed }, phuxChannel);
        try std.testing.expectEqual(receipt.id, engine.creation.peekCompletion().?.command_id);
        try std.testing.expect(engine.creation.ackCompletion(receipt.id));
    }
}

test "durable operation admission preserves known execution during transport teardown" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const engine = try cockpit.durable_tests.start();
    defer engine.destroy();
    const remote = engine.model.phux().?;
    const intent = cockpit.protocol.encodeIntent(.{
        .kind = .new_terminal,
        .argument = 0,
        .expected_revision = engine.revision,
        .window = 255,
    });
    var packet: [22]u8 = undefined;
    packet[0] = 1;
    packet[1] = 3;
    std.mem.writeInt(u64, packet[2..10], 0xfedcba9876543210, .little);
    @memcpy(packet[10..22], &intent);
    const admitted = engine.applyTabCommand(&packet);
    try std.testing.expectEqual(.accepted_pending, admitted.status);
    try std.testing.expectEqual(@as(usize, 1), engine.creation.count());
    try cockpit.PhuxProvider.test_support.stageFixture(remote.bridge, "spawn-local.bin");
    _ = try remote.drainReadiness(); // Provider has evidence the engine has not consumed.
    try std.testing.expect(engine.creation.peekCompletion() == null);
    var effects: NavigationConnectionRecorder = .{ .live = false };
    _ = engine.onPhuxChannel(&effects, .{ .key = cockpit.phux_channel_key, .kind = .closed }, phuxChannel);
    const result = engine.creation.peekCompletion().?;
    try std.testing.expectEqual(admitted.id, result.command_id);
    try std.testing.expectEqual(.success, result.operation);
    try std.testing.expectEqual(@as(u32, 1), result.request_id);
    try std.testing.expectEqual(@as(u32, 8), result.terminal_ref.?.terminal_id.phux.id);
    try std.testing.expectEqual(.unknown, result.placement);
}

test "shipping direct reconnect fences attachments and retires pending creation on open failure" {
    try cockpit.durable_tests.directReconnectFences();
}

test "shipping coalesced spawn publication and terminal death retire the reserved window" {
    try cockpit.durable_tests.earlyTerminalDeath();
}

test "shipping remote title-only output invalidates the chrome snapshot" {
    try cockpit.durable_tests.titleAnnouncement();
}

test "shipping empty replacement title clears the previously published remote title" {
    try cockpit.durable_tests.emptyTitleReconnect();
}

test "navigation reconnect waits for a live channel close and reopens an already closed source" {
    if (comptime !cockpit.phux_enabled) return error.SkipZigTest;
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    var config = cockpit.startup.resolvePhuxConfig(.{}, .{ .socket = "/navigation-unused.sock" });
    engine.model.phux_provider = (try cockpit.startup.createPhuxProviderFromConfig(std.testing.allocator, std.testing.io, &config)).?;
    engine.model.phux_connection_unavailable = true;
    var effects: NavigationConnectionRecorder = .{ .live = true };
    try std.testing.expect(engine.restartNavigationConnection(&effects, phuxChannel));
    try std.testing.expectEqual(@as(usize, 1), effects.closed);
    try std.testing.expectEqual(@as(usize, 0), effects.opened);
    try std.testing.expect(engine.model.phux_reconnect_after_close);
    try std.testing.expectEqual(.connecting, cockpit.engine.navigation.connection(engine.model));

    const announced = engine.onPhuxChannel(&effects, .{ .key = cockpit.phux_channel_key, .kind = .closed }, phuxChannel);
    try std.testing.expect(announced);
    effects.opened = 0;

    effects.live = false;
    try std.testing.expect(engine.restartNavigationConnection(&effects, phuxChannel));
    try std.testing.expectEqual(@as(usize, 1), effects.opened);
    try std.testing.expect(!engine.model.phux_reconnect_after_close);
    try std.testing.expect(engine.model.phux_connection_unavailable);
    try std.testing.expectEqual(.offline, cockpit.engine.navigation.connection(engine.model));
}

test "navigation retains keyboard highlight through a metadata snapshot refresh" {
    var rig = try Rig.start();
    defer rig.stop();
    try rig.settle(0, "READY");
    try rig.reach(.{ .label = "three tabs", .tabs = 3 });
    try rig.dispatch(.palette_open);
    try rig.settleNavigation();
    try rig.dispatch(.{ .palette_move = 1 });
    try std.testing.expectEqual(@as(i64, 1), rig.app_state.model.paletteCursor);
    const engine = bridge.engine.?;
    const pane = engine.model.provider.terminal(engine.model.focusedTerminalRef().?).?;
    const before = engine.sequence;
    try rig.dispatch(shellEvent(.{ .key = pane.pty_key, .kind = .output, .bytes = "\x1b]2;updated title\x07" }));
    try rig.settle(@intCast(before + 1), "READY");
    try rig.settleNavigation();
    try std.testing.expectEqual(@as(i64, 1), rig.app_state.model.paletteCursor);
    try std.testing.expect(rig.app_state.model.paletteRows[1].highlighted);
    try rig.dispatch(.{ .palette_move = 1 });
    try std.testing.expectEqual(@as(i64, 2), rig.app_state.model.paletteCursor);
    try rig.dispatch(.close_selected_tab);
    try rig.settle(@intCast(before + 2), "READY");
    try rig.settleNavigation();
    try std.testing.expectEqual(@as(usize, 2), rig.app_state.model.paletteRows.len);
    try std.testing.expectEqual(@as(i64, 1), rig.app_state.model.paletteCursor);
    try std.testing.expect(rig.app_state.model.paletteRows[1].highlighted);
}
