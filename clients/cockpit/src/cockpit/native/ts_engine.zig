//! The native engine behind the TypeScript core's seam.
//!
//! The compiled core owns chrome presentation; this owns the durable chrome
//! STATE it presents — a real Cockpit `Model` with a real local provider —
//! and answers the three things the seam allows: apply a fenced intent,
//! serialize a snapshot, announce that state moved. Nothing else crosses.
//!
//! Two counters carry the ordering contract. `sequence` advances on every
//! announcement, applied or refused, so the core can detect a gap in what it
//! heard. `revision` advances only when state actually changed; every
//! positional intent names the revision it was computed against, and one
//! computed against an older revision is refused rather than applied to tabs
//! that may have moved underneath it. That refusal is itself state (bit 7 of
//! the snapshot flags), so the core can show it instead of guessing.

const std = @import("std");
const native_sdk = @import("native_sdk");
const model_module = @import("../model.zig");
const support = @import("../phux_support.zig");
const layout = @import("../layout.zig");
const grid = @import("../../terminal/grid.zig");
const vt = @import("ghostty-vt");
const terminal_runtime = @import("../terminal_runtime.zig");
const interaction = @import("../terminal_interaction.zig");
const remote_commands = @import("remote_presentation_commands.zig");
const lifecycle = @import("../workspace_lifecycle.zig");
const durable_creation = @import("../durable_creation.zig");
const shared_workspace = @import("../shared_workspace.zig");
const peer_edits = @import("../peer_edits.zig");
const session_attachments = @import("session_attachments.zig");
const local_tool_launch = @import("local_tool_launch.zig");
pub const new_session = @import("new_session.zig");
pub const new_session_runtime = @import("new_session_runtime.zig");
pub const machine_runtime = @import("machine_runtime.zig");

test {
    _ = @import("machine_engine_tests.zig");
}

test "projection refusal publishes newly retained completion independently from refusal flag" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    engine.creation.pending[0] = .{
        .command_id = 77,
        .operation = .success,
        .operation_request = 9,
        .projected_id = @splat(1),
        .window = 0,
        .window_epoch = engine.model.window_epochs[0],
        .kind = .tab,
        .origin = null,
    };
    try std.testing.expect(!engine.model.shared_workspace.refused);
    try std.testing.expect(engine.creation.peekCompletion() == null);
    try std.testing.expect(engine.projectionRefused());
    try std.testing.expectEqual(@as(u64, 77), engine.creation.peekCompletion().?.command_id);
    try std.testing.expectEqual(.success, engine.creation.peekCompletion().?.operation);
    try std.testing.expectEqual(.refused, engine.creation.peekCompletion().?.placement);
    try std.testing.expect(!engine.projectionRefused());
}
const pointer_input = @import("../pointer_input.zig");
const update_module = @import("../update.zig");
const provider_contract = @import("provider_contract");
const local = @import("../../providers/local/provider.zig");
const scene = @import("scene.zig");
const projection = @import("workspace_projection.zig");
const terminal_painter = @import("terminal_painter.zig");
const protocol = @import("ts_protocol.zig");
const ts_snapshot = @import("ts_snapshot.zig");
pub const navigation = @import("ts_navigation.zig");
const theme_module = @import("../../config/theme.zig");
pub const appearance = @import("ts_appearance.zig");
const startup = @import("../startup.zig");
const shell_words = @import("../shell_words.zig");
const session_state = @import("../session_state.zig");
const publication = @import("publication.zig");
pub const tab_commands = @import("tab_commands.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const platform = native_sdk.platform;
const keyIs = terminal_runtime.keyIs;
const Model = model_module.Model;
const TerminalRef = support.TerminalRef;
const max_terminals = model_module.max_terminals;

/// What the engine asks of an effects instance. Both this app's own
/// `TerminalApp.Effects` and the TypeScript adapter's satisfy it; tests
/// that want no processes pass `NoShells`.
pub const NoShells = struct {
    pub fn hostSend(_: *const NoShells, _: []const u8, _: []const u8) void {}
    pub fn ptySpawn(_: *const NoShells, _: anytype) void {}
    pub fn ptyWrite(_: *const NoShells, _: u64, _: []const u8) bool {
        return false;
    }
    pub fn ptyResize(_: *const NoShells, _: u64, _: u16, _: u16) void {}
    pub fn ptyKill(_: *const NoShells, _: u64) void {}
    pub fn cancel(_: *const NoShells, _: u64) void {}
    pub fn closeWindow(_: *const NoShells, _: []const u8) void {}
    pub fn quitApp(_: *const NoShells) void {}
    pub fn showNotification(_: *const NoShells, _: anytype) void {}
    pub fn writeClipboard(_: *const NoShells, _: anytype) void {}
    pub fn readClipboard(_: *const NoShells, _: anytype) void {}
    pub fn openUrl(_: *const NoShells, _: []const u8) void {}
    pub fn toggleFullscreenWindow(_: *const NoShells, _: []const u8) void {}
    pub fn minimizeWindow(_: *const NoShells, _: []const u8) void {}
};

pub const PointerOutcome = enum { ignored, consumed, geometry_changed };

test "external keybindings retire legacy find and copy chords while search still owns input" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const pane = engine.focusedPane().?;
    const find: canvas.WidgetKeyboardEvent = .{ .phase = .key_down, .key = "f", .modifiers = .{ .super = true } };
    try std.testing.expect(!engine.textInputOwnsKeyboard());
    engine.onKey(&NoShells{}, find);
    try std.testing.expect(engine.textInputOwnsKeyboard());
    pane.session.searchClose();
    engine.external_keybindings = true;
    engine.onKey(&NoShells{}, find);
    try std.testing.expect(!engine.textInputOwnsKeyboard());
    try std.testing.expect(!engine.localShortcut(&NoShells{}, pane, .{ .phase = .key_down, .key = "c", .modifiers = .{ .super = true } }));
    try std.testing.expect(!engine.localShortcut(&NoShells{}, pane, .{ .phase = .key_down, .key = "c", .modifiers = .{ .control = true } }));
    try std.testing.expect(!engine.localShortcut(&NoShells{}, pane, .{ .phase = .key_down, .key = "f", .modifiers = .{ .control = true } }));
    _ = engine.nativeCommand(@intFromEnum(protocol.NativeCommand.find), &NoShells{});
    try std.testing.expect(engine.textInputOwnsKeyboard());
    engine.onKey(&NoShells{}, .{ .phase = .key_down, .key = "escape" });
    try std.testing.expect(!engine.textInputOwnsKeyboard());
}

test "shared deferred selection is superseded by keyboard and routed window focus" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    const pending: TerminalRef = .{ .provider_id = .phux, .terminal_id = .{ .phux = .{ .kind = 0, .id = 900 } } };
    try std.testing.expect(engine.newTerminal());
    engine.model.shared_workspace.desired_terminal = pending;
    try std.testing.expect(engine.tabCommand(.next_tab));
    try std.testing.expect(engine.model.shared_workspace.desired_terminal == null);
    try std.testing.expect(engine.splitFocusedPane(.horizontal));
    engine.model.shared_workspace.desired_terminal = pending;
    try std.testing.expect(engine.paneFocusCommand(.next_pane));
    try std.testing.expect(engine.model.shared_workspace.desired_terminal == null);
    _ = engine.model.openWindow(1).?;
    engine.model.shared_workspace.desired_terminal = pending;
    engine.model.active_window = 1; // applyIntent has already routed this window.
    try std.testing.expect(engine.focusWindow(1));
    try std.testing.expect(engine.model.shared_workspace.desired_terminal == null);
    engine.model.shared_workspace.desired_terminal = pending;
    engine.setInputSuspended(&NoShells{}, true);
    try std.testing.expect(engine.model.shared_workspace.desired_terminal == null);
}

test "shared divider preview rolls back when an overlay suspends input" {
    const engine = try Engine.create(std.testing.allocator, std.testing.io);
    defer engine.destroy();
    try std.testing.expect(engine.splitFocusedPane(.horizontal));
    const workspace = engine.model.ws();
    const tree = workspace.selectedTree().?;
    const id: [16]u8 = @splat(1);
    workspace.shared_ids[workspace.selected_tab] = id;
    engine.model.shared_workspace.revision = 10;
    const drag: SplitDrag = .{
        .window_id = 0,
        .pointer_id = 1,
        .window_index = 0,
        .node = tree.root,
        .orientation = .horizontal,
        .bounds = .{ .x = 0, .y = 0, .width = 800, .height = 600 },
        .shared_id = id,
        .shared_revision = 10,
        .window_epoch = engine.model.window_epochs[0],
        .original_fraction = tree.node(tree.root).fraction,
    };
    engine.split_drag = drag;
    try std.testing.expect(engine.moveSplitDrag(drag, .{ .x = 600, .y = 0 }));
    try std.testing.expect(tree.node(tree.root).fraction != drag.original_fraction);
    engine.setInputSuspended(&NoShells{}, true);
    try std.testing.expectEqual(drag.original_fraction, tree.node(tree.root).fraction);
    try std.testing.expect(engine.split_drag == null);
    try std.testing.expectEqual(@as(u64, 0), engine.model.shared_mutations.next_ticket);
}

const SplitDrag = struct {
    window_id: platform.WindowId,
    pointer_id: u64,
    window_index: usize,
    node: layout.NodeId,
    orientation: layout.Orientation,
    bounds: geometry.RectF,
    shared_id: ?[16]u8 = null,
    shared_revision: u64 = 0,
    window_epoch: u64 = 0,
    local_fingerprint: u64 = 0,
    original_fraction: f32 = 0.5,
    /// The coordinator whose tab holds the divider; its projection's
    /// revision is the one `shared_revision` was taken from.
    authority: ?support.ProviderId = null,
};

/// Snapshot flag bit reserved for the engine: the last intent was refused
/// because it named a revision the engine had already moved past (or could
/// not be decoded at all). Bits 0..6 belong to `ts_snapshot.snapshotFlags`.
pub const intent_refused_flag: u8 = 1 << 7;

const LocalToolPlacement = struct {
    ticket: u64,
    provider_context: u64,
    host_context: u64,
    connection_epoch: u64,
    window: usize,
    window_epoch: u64,
    terminal_ref: TerminalRef,
};

pub const Engine = struct {
    model: *Model,
    allocator: std.mem.Allocator = std.heap.page_allocator,
    sequence: u64 = 0,
    revision: u64 = 1,
    selection_epoch: u64 = 1,
    intent_refused: bool = false,
    /// The pty key each registry slot was last spawned for, so `spawnShells`
    /// is idempotent across frames and a reused slot spawns again.
    spawned_keys: [max_terminals]u64 = [_]u64{0} ** max_terminals,
    /// The run the last snapshot carried. A frame that changes it (a resize,
    /// a placement flip) is announced like an intent, because the core's
    /// chrome is wrong until it resyncs.
    last_runs: ts_snapshot.WindowRuns = [_]ts_snapshot.TabRun{.{}} ** (1 + model_module.max_secondary_windows),
    /// The config file's state as of the last `probe_config` intent.
    config_probe: ts_snapshot.ConfigProbe = .{},
    /// Click coalescing for raw surface input, which carries no click count
    /// of its own: a down within the double-click window and radius of the
    /// last one counts up, the way the routed widget path counts for the
    /// Zig chrome.
    last_down_ns: u64 = 0,
    last_down_point: geometry.PointF = .{},
    last_click_count: u8 = 0,
    split_drag: ?SplitDrag = null,
    remote_focus_owner: ?support.ReplicaOwner = null,
    remote_natural_keys_held: u8 = 0,
    input_suspended: bool = false,
    creation: durable_creation.Creation = .{},
    session_handoff: ?u64 = null,
    last_workspace_refresh: ?std.Io.Timestamp = null,
    remote_pointer: @import("shipping_pointer.zig").State = .{},
    /// Go to Directory's listed host, fixed when the picker opens.
    directory_origin: @import("directory_picker.zig").Origin = .{},
    /// Edits of a showing peer's tabs, queued to that coordinator alone.
    peer_edits: peer_edits.Edits = .{},
    /// Shipping startup shares one wake channel across dynamically owned
    /// workers. Per-peer handles remain logical retirement/retry identities.
    peer_wake_key: u64 = 0,
    peer_wake_handle: native_sdk.ChannelHandle = .{},
    peer_background_cursor: usize = 0,
    /// The shipping native keybinding registry owns application chords.
    external_keybindings: bool = false,
    /// Each coordinator's last Rename Session, and on which of its
    /// connections (session_commands.zig): one pending rename per
    /// coordinator, so a rename on one never refuses a rename on another.
    /// Each outcome is read from its own coordinator only.
    rename_flights: @import("session_commands.zig").Flights = .{},
    /// Exact bound-spawn owner installed by the process composition root.
    /// It gets first refusal on operation results before ordinary creators.
    local_tool_sink: ?local_tool_launch.Sink = null,
    local_tool_placements: [16]?LocalToolPlacement = @splat(null),
    next_local_tool_ticket: u64 = 0x4c54_0000_0000_0001,

    const empty_session = @import("empty_session.zig");
    const peer_restore = @import("peer_restore.zig");

    /// Automatic redial of a failed listing peer: 1 s, then twice the last
    /// wait, at most 60 s, until it has stayed listed `peer_retry_stable_ms`.
    pub const peer_retry_initial_ms: u64 = 1000;
    pub const peer_retry_max_ms: u64 = 60_000;
    /// Hysteresis: how long a peer must stay listed before its next failure
    /// waits 1 s again. A host that lists and then fails at once keeps
    /// backing off instead of being redialed every second.
    pub const peer_retry_stable_ms: i64 = 30_000;
    /// One retry timer per peer slot, from this key (timer keys are their
    /// own namespace; topology persistence uses 200).
    pub const peer_retry_timer_key: u64 = 210;

    /// The model is multi-MB and lives on the heap for the process lifetime;
    /// `gpa` sizes the emulator sessions the provider mints, `io` is what the
    /// provider spawns through later. The first terminal exists from birth,
    /// exactly as the shipping app boots.
    pub fn create(gpa: std.mem.Allocator, io: std.Io) !*Engine {
        const session = try grid.Session.create(gpa, io, 80, 24);
        errdefer session.destroy();
        const model = try std.heap.page_allocator.create(Model);
        errdefer std.heap.page_allocator.destroy(model);
        model.* = try model_module.initialModelWithIo(gpa, io, session);
        const engine = try std.heap.page_allocator.create(Engine);
        engine.* = .{ .model = model, .allocator = gpa };
        return engine;
    }

    /// The process composition root: the same config, persisted topology,
    /// cwd restoration, and optional Phux provider bootstrap as the Zig app.
    pub fn createConfigured(gpa: std.mem.Allocator, init: std.process.Init) !*Engine {
        const initialized = try startup.initializeModel(gpa, init);
        return createFromInitialized(initialized);
    }

    /// Explicit-input twin of createConfigured, used by isolated launch tests
    /// and embedders that already resolved their environment. It still enters
    /// through the same startup implementation and final-storage cwd fixup.
    pub fn createResolvedConfigured(
        gpa: std.mem.Allocator,
        io: std.Io,
        config: startup.Config,
        config_path: ?[]const u8,
        state_path: ?[]const u8,
        tab_placement_override: ?[]const u8,
    ) !*Engine {
        const initialized = try startup.initializeResolvedModel(
            gpa,
            io,
            config,
            config_path,
            state_path,
            tab_placement_override,
        );
        return createFromInitialized(initialized);
    }

    /// Consume the fully resolved startup model and establish its final storage.
    pub fn createFromInitialized(initialized_value: startup.InitializedModel) !*Engine {
        var initialized = initialized_value;
        const model = initialized.model;
        errdefer std.heap.page_allocator.destroy(model);
        errdefer model_module.deinitModel(model);
        if (initialized.provenance == .restored) {
            model_module.applyRestoredWorkingDirectories(model, &initialized.restored_snapshot);
        }
        if (model.phux() != null) initializeSharedPresentation(model);
        const engine = try std.heap.page_allocator.create(Engine);
        engine.* = .{ .model = model, .allocator = model.provider.gpa };
        return engine;
    }

    fn initializeSharedPresentation(model: *Model) void {
        // Configuration chooses either Phux-backed work or the explicit local
        // scratch path. Old local layout files cannot restore competing remote
        // topology, or start hidden scratch PTYs behind a shared workspace.
        var refs: [max_terminals]TerminalRef = undefined;
        const count = model.provider.terminalRefs(&refs);
        for (refs[0..count]) |ref| _ = model.provider.destroyTerminal(ref);
        for (1..model_module.max_windows) |index| model.closeWindow(index);
        model.primary = .{};
        model.primary_open = true;
        model.active_window = 0;
        model.saved_attachments = .{};
        model.pending_attachments = @splat(false);
        model.state.setPath(null);
        const remote = model.phux().?;
        model.shared_workspace.attachment_id = remote.context_id;
        model.bindWindowAttachment(0, remote.context_id);
    }

    pub fn destroy(self: *Engine) void {
        self.peer_edits.deinit();
        self.rename_flights.deinit();
        model_module.deinitModel(self.model);
        std.heap.page_allocator.destroy(self.model);
        std.heap.page_allocator.destroy(self);
    }

    /// Start the configured provider's two native event sources. TypeScript
    /// sees only the resulting ordered snapshot invalidations; sockets,
    /// ChannelHandle values and pointer ownership stay native.
    pub fn startProviderChannels(self: *Engine, fx: anytype, phux_event: anytype, pointer_event: anytype) void {
        self.openPhuxChannel(fx, phux_event, false);
        // Shipping raw surface events already carry pointer ownership.
        _ = pointer_event;
    }

    fn openPhuxChannel(self: *Engine, fx: anytype, on_event: anytype, reconnect: bool) void {
        const remote = self.model.phux() orelse return;
        const handle = fx.openChannel(.{
            .key = support.phux_channel_key,
            .on_event = on_event,
            .max_pending = 1,
        });
        if (!handle.live()) {
            self.model.phux_connection_unavailable = true;
            self.failSessionHandoff();
            return;
        }
        if (reconnect) {
            remote.reconnect(handle) catch {
                self.model.phux_connection_unavailable = true;
                fx.closeChannel(support.phux_channel_key);
                self.failSessionHandoff();
                return;
            };
        } else {
            remote.open(handle) catch {
                self.model.phux_connection_unavailable = true;
                fx.closeChannel(support.phux_channel_key);
                self.failSessionHandoff();
                return;
            };
        }
        if (self.session_handoff) |command_id| {
            if (!self.creation.bindSessionConnection(self.model, command_id)) {
                self.failSessionHandoff();
                return;
            }
            self.session_handoff = null;
        }
    }

    fn failSessionHandoff(self: *Engine) void {
        if (self.session_handoff) |id| self.creation.failSessionAttempt(self.model, id);
        self.session_handoff = null;
    }

    fn openPointerChannel(self: *Engine, fx: anytype, on_event: anytype) void {
        if (comptime !support.phux_enabled) return;
        const pointer_state = self.model.pointer_state orelse return;
        if (pointer_state.monitor != null) return;
        const handle = fx.openChannel(.{
            .key = support.pointer_channel_key,
            .on_event = on_event,
            .max_pending = 1,
        });
        if (!handle.live()) return;
        pointer_state.monitor = support.pointer_module.Monitor.start(
            std.heap.page_allocator,
            &pointer_state.queue,
            handle,
        ) catch {
            fx.closeChannel(support.pointer_channel_key);
            return;
        };
    }

    /// Drain a provider wake and reconcile only stable provider terminal
    /// identities into Cockpit topology. The bool says the snapshot-visible
    /// model moved and therefore needs one ordered invalidation.
    pub fn onPhuxChannel(self: *Engine, fx: anytype, event: native_sdk.EffectChannelEvent, on_event: anytype) bool {
        defer self.syncRemoteFocus();
        if (event.key != support.phux_channel_key) return false;
        const model = self.model;
        const remote = model.phux() orelse return false;
        var changed = false;
        switch (event.kind) {
            .data => changed = self.drainPhux(fx),
            .closed, .rejected => {
                self.providerDisconnectedExcept(self.session_handoff);
                changed = true;
                remote.stop();
                if (model.phux_reconnect_after_close) {
                    model.phux_reconnect_after_close = false;
                    self.openPhuxChannel(fx, on_event, remote.state() != .new);
                } else {
                    model.phux_connection_unavailable = true;
                }
            },
        }
        return self.commitProviderChange(changed);
    }

    fn drainPhux(self: *Engine, fx: anytype) bool {
        const remote = self.model.phux() orelse return false;
        const delta = remote.drainReadiness() catch return self.failPhux(fx);
        if (delta.detached) return self.failPhux(fx);
        const changed = self.applyReadiness(delta);
        remote_commands.resumeReady(self.model);
        // A settled go-to-directory listing moves nothing in the snapshot,
        // but the picker reads it on the invalidation this announces.
        // A rename moves the header and the switcher, and settles the panel.
        return self.drainRemoteNotices(fx) or delta.generation_changed or delta.directory_changed or delta.sessions_renamed or changed;
    }

    /// Rename Session's session (session_commands.zig): the one whose tab
    /// holds the focused pane, on the coordinator that minted that pane's ref;
    /// with no Phux pane focused, the active coordinator's attached session.
    pub const RenameTarget = struct { provider: *support.PhuxProvider, session: u32, name: []const u8 };

    pub fn renameTarget(self: *Engine) ?RenameTarget {
        if (comptime !support.phux_enabled) return null;
        const owner = self.renameOwner() orelse return null;
        if (owner.state() != .attached) return null;
        const session = owner.selectedSessionId() orelse return null;
        for (owner.sessionCatalog()) |entry| {
            if (entry.id == session and entry.name.len != 0) return .{ .provider = owner, .session = session, .name = entry.name };
        }
        return null;
    }

    /// Routed by the id the focused ref carries, never by a name: a pane
    /// whose coordinator is no longer held has no owner here, and a listing
    /// peer's terminals cannot be on screen.
    fn renameOwner(self: *Engine) ?*support.PhuxProvider {
        const model = self.model;
        const ref = model.focusedTerminalRef() orelse return model.phux();
        if (support.providerKind(ref) != .phux) return model.phux();
        const owner = model.phuxForRef(ref) orelse return null;
        return if (owner.showing()) owner else null;
    }

    fn drainRemoteNotices(self: *Engine, fx: anytype) bool {
        if (comptime !support.phux_enabled) return false;
        return self.drainNotices(fx, self.model.phux() orelse return false);
    }

    /// One coordinator's bells, each checked against that coordinator's own
    /// current owner.
    fn drainNotices(self: *Engine, fx: anytype, remote: *support.PhuxProvider) bool {
        if (comptime !support.phux_enabled) return false;
        var changed = false;
        // Host admission is bounded to max_notices; every owned payload is freed.
        while (remote.takeNotice()) |notice| {
            defer remote.releaseNotice(notice);
            if (!notice.isBell()) continue;
            const owner: support.ReplicaOwner = .{ .terminal_ref = notice.terminal_ref, .generation = notice.generation, .source_context = remote.host.context_id };
            if (!self.model.ownerIsCurrent(owner)) continue;
            if (!remote.ringBell(owner)) continue;
            changed = true;
            self.notifyRemoteBell(fx, owner.terminal_ref);
        }
        return changed;
    }

    fn notifyRemoteBell(self: *Engine, fx: anytype, ref: TerminalRef) void {
        if (self.model.focused) return;
        var title_storage: [projection.max_terminal_title_bytes]u8 = undefined;
        const title = projection.terminalTitleInto(self.model, ref, &title_storage);
        if (!self.model.recordNotification(title)) return;
        fx.showNotification(.{ .title = title, .subtitle = "Phux Cockpit", .body = "Terminal bell" });
    }

    fn failPhux(self: *Engine, fx: anytype) bool {
        self.providerDisconnected();
        self.model.phux_connection_unavailable = true;
        if (self.model.phux()) |remote| remote.stop();
        fx.closeChannel(support.phux_channel_key);
        return true;
    }

    fn applyReadiness(self: *Engine, delta: support.SyncDelta) bool {
        const model = self.model;
        var changed = self.pumpOperations() or delta.metadata_changed;
        changed = self.creation.observeSessionAttachment(model) or changed;
        model.reconcileRemoteTerminals();
        if (delta.ready_published) {
            changed = model.phux_connection_unavailable or changed;
            model.phux_connection_unavailable = false;
        }
        if (model.phux()) |remote| {
            const workspace = remote.workspaceSnapshot();
            if (workspace.state != .unavailable) return self.synchronizeSharedWorkspace(delta.sessions_listed) or changed;
        }
        return changed;
    }

    fn pumpOperations(self: *Engine) bool {
        const model = self.model;
        const remote = model.phux() orelse return false;
        var changed = false;
        while (remote.takeOperationResult()) |result| {
            if (self.completeLocalTool(remote, result)) {
                changed = true;
                continue;
            }
            _ = self.creation.complete(model, result);
            // Subscription restoration may share the provider's deduplicated
            // attach request. Both exact owners must observe its completion.
            _ = completeSubscriptions(&model.shared_workspace, remote, result);
            changed = true;
        }
        return changed;
    }

    fn completeLocalTool(self: *Engine, remote: *support.PhuxProvider, result: support.OperationResult) bool {
        const sink = self.local_tool_sink orelse return false;
        return sink.complete(sink.context, remote, result);
    }

    fn completeSubscriptions(state: *shared_workspace.State, remote: *support.PhuxProvider, result: support.OperationResult) bool {
        if (state.attachment_id != null) return state.completeSubscriptionFrom(remote.context_id, result, remote.workspaceSnapshot().request_id);
        return state.completeSubscription(result, remote.workspaceSnapshot().request_id);
    }

    fn synchronizeSharedWorkspace(self: *Engine, catalog_listed: bool) bool {
        const model = self.model;
        const remote = model.phux() orelse return false;
        self.sharedContext() catch return self.projectionRefused();
        var changed = model.shared_mutations.pump(model);
        changed = self.creation.pump(model) or changed;
        changed = (self.projectSharedWorkspace() catch return self.projectionRefused()) or changed;
        // Bound empty New Tab on the primary provider: queue, then settle from
        // this drain's creation evidence (primary has no peer `holds` path).
        changed = empty_session.pumpAttachment(self, remote) or changed;
        changed = self.settleEmptyAttachment(remote, null, catalog_listed) or changed;
        self.admitDesiredTerminal();
        if (self.creation.count() == 0 and remote.workspaceSnapshot().status != .pending) model.shared_workspace.releaseUnused(model);
        model.shared_workspace.subscribe(model);
        return changed;
    }

    fn projectSharedWorkspace(self: *Engine) !bool {
        const model = self.model;
        const remote = model.phux().?;
        const generation = model.shared_workspace.projection_generation;
        var changed = try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch());
        if (generation != model.shared_workspace.projection_generation) self.split_drag = null;
        changed = model.shared_workspace.selectDesired(model) or changed;
        changed = self.creation.observeProjection(model) or changed;
        // A session continuation can offer its first hint only after the new
        // session projects. Consume it now; a provider wake is not guaranteed.
        if (model.shared_workspace.placement_hint != null or model.shared_workspace.desired_terminal != null) {
            changed = (try model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch())) or changed;
            changed = self.creation.observeProjection(model) or changed;
        }
        return changed;
    }

    fn admitDesiredTerminal(self: *Engine) void {
        const model = self.model;
        const ref = model.shared_workspace.desired_terminal orelse return;
        if (model.locateTerminal(ref) != null or self.creation.hasPendingTerminal(ref)) return;
        const remote = model.phux() orelse return;
        if (remote.terminalSession(ref) != remote.selectedSessionId()) return;
        self.creation.requestAttach(model, ref) catch {
            model.shared_workspace.desired_terminal = null;
            model.terminal_limit_refused = true;
        };
    }

    fn sharedContext(self: *Engine) !void {
        const model = self.model;
        const remote = model.phux() orelse return error.NoProvider;
        const server = remote.serverId() orelse return error.MissingServerIdentity;
        const session = remote.selectedSessionId() orelse return error.MissingSession;
        var remote_endpoint: [@import("../attachment_state.zig").max_endpoint_bytes]u8 = undefined;
        const endpoint = try coordinatorEndpoint(remote, &remote_endpoint);
        model.shared_workspace.authority = remote.providerId();
        model.bindSharedAttachment(remote);
        model.shared_workspace.setContext(contextHash(endpoint, server));
        try model.setAttachmentContext(endpoint, server, session);
    }

    /// Where a coordinator is reached. A registered host is identified by its
    /// registry label; the prefix keeps it disjoint from every absolute
    /// socket path, so a saved placement can never match the wrong
    /// coordinator.
    fn coordinatorEndpoint(remote: anytype, out: *[@import("../attachment_state.zig").max_endpoint_bytes]u8) ![]const u8 {
        return switch (remote.endpointDescriptor()) {
            .unix => |path| path,
            .remote => |host| std.fmt.bufPrint(out, "phux-remote:{s}", .{host.target}) catch
                return error.UnsupportedEndpoint,
            else => return error.UnsupportedEndpoint,
        };
    }

    /// Endpoint and server incarnation, never the session: numeric
    /// identities from a replacement server cannot recover old focus.
    fn contextHash(endpoint: []const u8, server: []const u8) u64 {
        var hash = std.hash.Wyhash.init(0);
        hash.update(endpoint);
        hash.update(server);
        return hash.final();
    }

    fn refuseSharedWorkspace(self: *Engine) bool {
        const changed = !self.model.shared_workspace.refused;
        self.model.shared_workspace.refused = true;
        return changed;
    }

    fn projectionRefused(self: *Engine) bool {
        var changed = self.creation.projectionFailed(self.model);
        changed = self.model.shared_mutations.projectionFailed(self.model) or changed;
        return self.refuseSharedWorkspace() or changed;
    }

    /// The real runtime timer and explicit navigation share a bounded refresh;
    /// UI invalidations cannot turn it into a request/reply spin loop.
    pub fn refreshWorkspace(self: *Engine) void {
        // A listing peer's list is refreshed with the switcher's; it
        // announces only when the list actually changed, so this cannot feed
        // itself.
        for (self.model.peers.items) |entry| if (entry.provider) |peer| peer.refreshStandby();
        const now = std.Io.Clock.awake.now(self.model.provider.io);
        if (self.last_workspace_refresh) |last| {
            if (last.durationTo(now).toMilliseconds() < 1000) return;
        }
        var requested = refreshAttached(self.model.phux());
        for (self.model.peers.items) |entry| {
            const peer = entry.provider orelse continue;
            if (peer.showing()) requested = refreshAttached(peer) or requested;
        }
        if (requested) self.last_workspace_refresh = now;
    }

    fn refreshAttached(remote: ?*support.PhuxProvider) bool {
        const value = remote orelse return false;
        if (value.state() != .attached) return false;
        const request = value.requestWorkspaceRefresh() catch return false;
        return request != null;
    }

    fn providerDisconnected(self: *Engine) void {
        self.providerDisconnectedExcept(null);
        self.session_handoff = null;
    }

    fn providerDisconnectedExcept(self: *Engine, handoff: ?u64) void {
        self.cancelSplitDrag();
        self.model.captureRemotePaint();
        if (self.model.phux()) |remote| {
            empty_session.forgetAttachment(self.model, remote.context_id, false);
            remote.stop();
            while (remote.takeOperationResult()) |result| {
                if (self.completeLocalTool(remote, result)) continue;
                _ = self.creation.completeDisconnected(self.model, result);
            }
        }
        self.model.rejectAttachmentContext();
        self.creation.disconnectExcept(self.model, handoff);
        self.model.shared_mutations.disconnect(self.model);
        self.last_workspace_refresh = null;
    }

    pub fn onPointerChannel(self: *Engine, fx: anytype, event: native_sdk.EffectChannelEvent, on_event: anytype) void {
        if (comptime !support.phux_enabled) return;
        if (event.key != support.pointer_channel_key) return;
        const pointer_state = self.model.pointer_state orelse return;
        switch (event.kind) {
            // Raw surface events own shipping gestures. The legacy monitor is
            // retained for the Zig coordinator, but must not duplicate reports.
            .data => pointer_state.queue.reset(),
            .closed, .rejected => {
                if (pointer_state.monitor) |*monitor| monitor.stop();
                pointer_state.monitor = null;
                pointer_state.queue.reset();
                pointer_state.capture = null;
                self.openPointerChannel(fx, on_event);
            },
        }
    }

    fn commitProviderChange(self: *Engine, changed: bool) bool {
        if (!changed) return false;
        self.sequence +%= 1;
        self.revision +%= 1;
        self.intent_refused = false;
        return true;
    }

    pub fn stopProviderChannels(self: *Engine, fx: anytype) void {
        if (self.model.phux()) |remote| remote.stop();
        fx.closeChannel(support.phux_channel_key);
        for (self.model.peers.items, 0..) |entry, slot| {
            if (entry.provider) |peer| peer.stop();
            entry.reopen = false;
            self.retirePeerChannel(fx, slot);
        }
        if (self.peer_wake_key != 0) fx.closeChannel(self.peer_wake_key);
        if (comptime support.phux_enabled) if (self.model.pointer_state) |pointer_state| {
            if (pointer_state.monitor) |*monitor| monitor.stop();
            pointer_state.monitor = null;
            pointer_state.queue.reset();
            pointer_state.capture = null;
        };
        fx.closeChannel(support.pointer_channel_key);
    }

    /// Edge-trigger the shipping topology persistence pipeline after any
    /// native mutation. The model fingerprint is the complete list of state
    /// transitions worth saving, so new intent kinds cannot forget to opt in.
    pub fn noteTopologyChange(self: *Engine, fx: anytype, on_fire: anytype) void {
        // After every native mutation: a peer none of whose tabs is on
        // screen any more goes back to listing.
        _ = self.settlePeers(fx);
        // Which session each remembered host shows, for the next launch
        // (ADR-0110); the file is written only when that changed.
        @import("remote_hosts.zig").rememberShown(self.model);
        const state = &self.model.state;
        const fingerprint = self.model.topologyFingerprint();
        if (fingerprint == state.fingerprint) return;
        state.fingerprint = fingerprint;
        if (!state.enabled()) return;
        // Replay claims the debounce timer and file write from the journal
        // (native_effect_replay). Arming pending here would strand a latch
        // the claimed terminal never clears.
        if (effectReplayArmed(fx)) return;
        state.pending = true;
        state.retry_count = 0;
        self.armTopologyPersist(fx, on_fire);
    }

    fn effectReplayArmed(fx: anytype) bool {
        if (!@hasField(@TypeOf(fx), "effects")) return false;
        return fx.effects.replayArmed();
    }

    fn armTopologyPersist(_: *Engine, fx: anytype, on_fire: anytype) void {
        fx.startTimer(.{
            .key = update_module.topology_persist_timer_key,
            .interval_ms = update_module.topology_persist_debounce_ms,
            .mode = .one_shot,
            .on_fire = on_fire,
        });
    }

    /// The debounce fired (including a rejected timer): post one bounded
    /// snapshot through the SDK's file seam unless an earlier write owns the
    /// key. Its completion drives the same retry accounting as the Zig graph.
    pub fn persistTopology(self: *Engine, fx: anytype, on_result: anytype) void {
        const state = &self.model.state;
        if (!state.enabled() or !state.pending or state.inflight) return;
        var bytes: [session_state.max_state_bytes]u8 = undefined;
        const topology_snapshot = self.model.topologySnapshot() catch return;
        const encoded = session_state.serialize(&topology_snapshot, &bytes) catch return;
        state.inflight = true;
        state.inflight_fingerprint = state.fingerprint;
        state.pending = false;
        fx.writeFile(.{
            .key = update_module.topology_state_file_key,
            .path = state.path(),
            .bytes = encoded,
            .on_result = on_result,
        });
    }

    pub fn topologyPersisted(self: *Engine, result: native_sdk.EffectFileResult, fx: anytype, on_fire: anytype) void {
        const state = &self.model.state;
        state.inflight = false;
        if (state.inflight_fingerprint != state.fingerprint) {
            state.pending = true;
            self.armTopologyPersist(fx, on_fire);
            return;
        }
        if (result.outcome == .ok) {
            state.retry_count = 0;
            state.write_failed = false;
        } else {
            state.pending = true;
            if (state.retry_count < 3) {
                state.retry_count += 1;
                self.armTopologyPersist(fx, on_fire);
            } else {
                state.write_failed = true;
            }
            return;
        }
        if (state.pending) self.armTopologyPersist(fx, on_fire);
    }

    /// Apply one wire intent. Returns whether state changed. Sequence always
    /// advances so the caller announces every outcome, including a refusal:
    /// a core that sent a stale intent must learn that it is stale. `fx`
    /// receives the pty consequences (a closed tab's shells are killed).
    pub fn applyIntent(self: *Engine, bytes: []const u8, fx: anytype) bool {
        defer self.syncRemoteFocus();
        self.sequence +%= 1;
        if (protocol.decodeNavigationIntent(bytes)) |intent| return self.applyNavigationIntent(intent, fx);
        const intent = protocol.decodeIntent(bytes) orelse return self.refuse();
        // A config probe reads process-wide disk state and names no positional
        // target. Title/output churn cannot make that read unsafe or retarget it.
        if (intent.kind != .probe_config and intent.expected_revision != self.revision) return self.refuse();
        // A tab intent means the window whose chrome sent it. Adopting it as
        // active first is what CockpitHost does with a routed event's window.
        // 255 means "the platform event's already-adopted focused window".
        // Markup intents carry an explicit 0..4 slot; native command mapping
        // has no window field, so the extension adopts CommandEvent.window_id
        // before the compiled core dispatches this intent.
        if (windowScoped(intent.kind) and intent.window != 255) {
            if (intent.window != 0 and !self.model.windowOpen(intent.window)) return self.refuse();
            self.model.active_window = intent.window;
        }
        if (!self.applyModelIntent(intent, fx)) return self.refuse();
        self.intent_refused = false;
        self.revision +%= 1;
        return true;
    }

    /// Process-wide settings and creation have no originating window target.
    /// Their legacy zero byte is not permission to move focus to the primary.
    fn windowScoped(kind: protocol.IntentKind) bool {
        return switch (kind) {
            .probe_config, .reveal_config, .set_theme, .set_tab_placement, .new_window => false,
            else => true,
        };
    }

    fn applyModelIntent(self: *Engine, intent: protocol.Intent, fx: anytype) bool {
        return switch (intent.kind) {
            .select_tab => self.selectTab(intent.argument),
            .new_terminal => self.newTerminal(),
            .close_tab => self.closeTab(intent.argument, fx),
            .set_tab_placement => self.setPlacement(intent.argument),
            .set_theme => self.setTheme(intent.argument),
            .reveal_config => self.revealConfig(fx),
            .probe_config => self.probeConfig(),
            .new_window, .close_window, .focus_window => self.applyWindowIntent(intent, fx),
            .native_command => self.nativeCommand(intent.argument, fx),
        };
    }

    fn applyWindowIntent(self: *Engine, intent: protocol.Intent, fx: anytype) bool {
        return switch (intent.kind) {
            .new_window => self.newWindow(),
            .close_window => self.closeWindow(intent.window, fx),
            .focus_window => self.focusWindow(intent.window),
            else => unreachable,
        };
    }

    fn applyNavigationIntent(self: *Engine, intent: protocol.NavigationIntent, fx: anytype) bool {
        if (intent.expected_revision != self.revision) return self.refuse();
        const changed = switch (intent.kind) {
            .reconnect => self.reconnectNavigation(fx),
            .select => self.selectNavigation(intent, fx),
        };
        if (!changed) return self.refuse();
        self.intent_refused = false;
        self.revision +%= 1;
        return true;
    }

    pub fn navigationSnapshot(self: *Engine, request: []const u8, out: []u8) navigation.Error![]const u8 {
        self.refreshWorkspace();
        return navigation.encode(self.model, self.revision, request, out);
    }

    pub fn navigationSnapshotForAttachments(self: *Engine, request: []const u8, out: []u8, attachments: []const u64) navigation.Error![]const u8 {
        self.refreshWorkspace();
        return navigation.encodeForAttachments(self.model, self.revision, request, out, attachments);
    }

    fn selectNavigation(self: *Engine, intent: protocol.NavigationIntent, fx: anytype) bool {
        const destination = navigation.resolve(self.model, self.revision, intent.expected_revision, intent.index) orelse return false;
        return switch (destination) {
            .placed_terminal => |placed| self.selectPlacedNavigation(placed, fx),
            .available_terminal => |ref| self.selectAvailableNavigation(ref, fx),
            .session => |id| self.selectSessionNavigation(id, fx),
            .peer_session => |target| self.showPeerSession(target.coordinator, target.id, fx),
            .peer_unavailable => |coordinator| self.retryPeer(coordinator, fx),
        };
    }

    pub fn applyTabCommand(self: *Engine, bytes: []const u8) tab_commands.Receipt {
        return self.applySelectionCommand(bytes, &NoShells{});
    }

    pub fn applySelectionCommand(self: *Engine, bytes: []const u8, fx: anytype) tab_commands.Receipt {
        defer self.syncRemoteFocus();
        self.sequence +%= 1;
        const request = tab_commands.decode(bytes) orelse return self.tabReceipt(0, .invalid_command);
        return switch (request.target) {
            .tab => |target| self.applyTabTarget(request.id, target),
            .catalog => |target| self.applyCatalogTarget(request.id, target, fx),
            .operation => |operation| self.applyOperationTarget(request.id, operation, fx),
        };
    }

    fn applyTabTarget(self: *Engine, id: u64, target: tab_commands.Target) tab_commands.Receipt {
        const index = target.resolve(self.model) orelse return self.tabReceipt(id, .stale_target);
        // Resolve every identity component BEFORE adopting the native window.
        self.model.active_window = target.window;
        _ = self.selectTab(index);
        self.revision +%= 1;
        return self.tabReceipt(id, .none);
    }

    fn applyCatalogTarget(self: *Engine, id: u64, target: tab_commands.catalog.Target, fx: anytype) tab_commands.Receipt {
        const destination = target.resolve(self.model) orelse return self.tabReceipt(id, .stale_target);
        const status = self.admitCatalogSelection(id, destination, fx) orelse return self.tabReceipt(id, .unavailable);
        self.revision +%= 1;
        var receipt = self.tabReceipt(id, .none);
        receipt.status = status;
        return receipt;
    }

    fn admitCatalogSelection(self: *Engine, command_id: u64, destination: model_module.PaletteDestination, fx: anytype) ?tab_commands.Status {
        // Destination is resolved from full identity and its current placement.
        // A rejected stale identity cannot cancel an earlier pending selection.
        switch (destination) {
            .placed_terminal => |placed| {
                if (!self.selectPlacedNavigation(placed, fx)) return null;
                return .applied;
            },
            .available_terminal => |ref| return self.admitAvailableTerminal(command_id, ref, fx),
            .session => |session| return self.admitSession(command_id, session, fx),
            // That coordinator shows the session beside the others; its
            // attach is reported through its own connection, not through a
            // correlated session receipt.
            .peer_session => |target| return pendingIf(self.showPeerSession(target.coordinator, target.id, fx)),
            .peer_unavailable => |coordinator| return pendingIf(self.retryPeer(coordinator, fx)),
        }
    }

    fn pendingIf(accepted: bool) ?tab_commands.Status {
        return if (accepted) .accepted_pending else null;
    }

    /// The available inventory follows the focused pane's coordinator. The
    /// active coordinator's terminal of its current session attaches here;
    /// another session's switches first. A showing peer's terminal is placed
    /// as a new tab on that peer (`peer_edits.adopt`), never elsewhere.
    fn admitAvailableTerminal(self: *Engine, command_id: u64, ref: TerminalRef, fx: anytype) ?tab_commands.Status {
        const remote = self.model.phux() orelse return null;
        if (ref.provider_id != remote.providerId()) return self.adoptOnPeer(ref);
        if (remote.terminalSession(ref) == remote.selectedSessionId()) {
            self.creation.requestAttachCorrelated(self.model, ref, command_id) catch return null;
            self.model.shared_workspace.desired_terminal = null;
            self.creation.supersedeFocusExceptCommand(command_id);
            return .accepted_pending;
        }
        const session = remote.terminalSession(ref) orelse return null;
        return self.admitSessionCommand(command_id, session, ref, fx);
    }

    /// A refused adoption changes nothing; a queued one supersedes every
    /// other pending focus, as a peer's New Tab does.
    fn adoptOnPeer(self: *Engine, ref: TerminalRef) ?tab_commands.Status {
        const created = self.peer_edits.adopt(self.model, ref) catch return null;
        self.supersedeSelection();
        created.may_focus = true;
        return .accepted_pending;
    }

    fn admitSession(self: *Engine, command_id: u64, session: u32, fx: anytype) ?tab_commands.Status {
        if (self.sessionSelectionStatus(session) == .applied) {
            self.supersedeSelection();
            return .applied;
        }
        return self.admitSessionCommand(command_id, session, null, fx);
    }

    fn sessionSelectionStatus(self: *const Engine, id: u32) tab_commands.Status {
        const remote = self.model.phuxConst() orelse return .accepted_pending;
        if (remote.selectedSessionId() != id) return .accepted_pending;
        if (remote.session_id != null and remote.session_id != id) return .accepted_pending;
        if (remote.state() != .attached) return .accepted_pending;
        if (self.model.phux_reconnect_after_close) return .accepted_pending;
        if (self.model.shared_workspace.session != id) return .accepted_pending;
        if (self.model.shared_workspace.epoch != remote.connectionEpoch()) return .accepted_pending;
        if (self.model.shared_workspace.refused) return .accepted_pending;
        return .applied;
    }

    fn admitSessionCommand(self: *Engine, command_id: u64, session: u32, terminal: ?TerminalRef, fx: anytype) ?tab_commands.Status {
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime !@hasDecl(Fx, "restartPhux")) return null;
        const remote = self.model.phux() orelse return null;
        self.creation.reserveSessionCorrelated(self.model, session, terminal, command_id) catch return null;
        const previous = remote.session_id;
        _ = remote.selectSession(session) catch {
            _ = self.creation.cancelSessionReservation(command_id);
            return null;
        };
        self.model.shared_workspace.leaveSession(self.model) catch {
            remote.session_id = previous;
            _ = self.creation.cancelSessionReservation(command_id);
            return null;
        };
        self.session_handoff = command_id;
        self.creation.supersedeFocusExceptCommand(command_id);
        if (!fx.restartPhux(self)) self.failSessionHandoff();
        return .accepted_pending;
    }

    fn applyOperationTarget(self: *Engine, id: u64, operation: protocol.Intent, fx: anytype) tab_commands.Receipt {
        if (operation.expected_revision != self.revision) return self.tabReceipt(id, .stale_target);
        const previous = self.model.active_window;
        if (operation.window != 255) {
            if (!self.model.windowOpen(operation.window)) return self.tabReceipt(id, .stale_target);
            self.model.active_window = operation.window;
        }
        const status = self.peerOperation(operation) orelse self.executeOperation(id, operation, fx) orelse {
            self.model.active_window = previous;
            return self.tabReceipt(id, .unavailable);
        };
        self.revision +%= 1;
        var receipt = self.tabReceipt(id, .none);
        receipt.status = status;
        return receipt;
    }

    /// Edits of a peer's tab (close, close pane, split, reorder) and New Tab
    /// with a peer's pane focused go to that coordinator (peer_edits). The
    /// active coordinator's correlated queue cannot track another server's
    /// mutation, so the receipt is applied once the edit is queued there;
    /// the peer's next publication shows it. A peer that cannot take it
    /// refuses: the edit never falls through to the active coordinator.
    /// Null: not a peer operation.
    fn peerOperation(self: *Engine, operation: protocol.Intent) ?tab_commands.Status {
        if (self.model.phux() == null) return null;
        const sent = switch (operation.kind) {
            .close_tab => self.peerCloseTab(operation.argument),
            .new_terminal => self.peerCreate(.tab),
            .new_window => self.peerCreate(.window),
            .native_command => self.peerNativeCommand(operation.argument),
            else => null,
        } orelse return null;
        return if (sent) .applied else .rejected;
    }

    /// Null when the command does not address a peer's tab or pane.
    fn peerNativeCommand(self: *Engine, command: u8) ?bool {
        return switch (command) {
            3 => self.peerClosePane(),
            4 => self.peerCreate(.split_right),
            5 => self.peerCreate(.split_down),
            8, 9 => self.peerReorderSelected(command == 9),
            else => null,
        };
    }

    /// Null when the tab is not a peer's.
    fn peerCloseTab(self: *Engine, index: u8) ?bool {
        const workspace = self.model.ws();
        if (index >= workspace.tab_count) return null;
        if (!self.model.foreignTab(workspace, index)) return null;
        return self.closePeerWindow(workspace, index);
    }

    /// Null when the focused pane is not a peer's.
    fn peerClosePane(self: *Engine) ?bool {
        _ = self.focusedPeer() orelse return null;
        return self.closePeerPane(self.model.focusedTerminalRef().?);
    }

    /// The coordinator of the focused pane, when it is a peer's.
    fn focusedPeer(self: *const Engine) ?support.ProviderId {
        const ref = self.model.focusedTerminalRef() orelse return null;
        if (support.providerKind(ref) != .phux or self.model.activeOwnsRef(ref)) return null;
        return ref.provider_id;
    }

    /// New Tab or a split with a peer's pane focused: on that peer. Null
    /// when the focused pane is not a peer's.
    fn peerCreate(self: *Engine, kind: peer_edits.Kind) ?bool {
        const coordinator = self.focusedPeer() orelse return null;
        return self.createOnPeer(coordinator, kind, "", .focused);
    }

    /// Move a peer's selected tab in that peer's own order. Null when the
    /// selected tab is not a peer's.
    fn peerReorderSelected(self: *Engine, right: bool) ?bool {
        const workspace = self.model.ws();
        const index = workspace.selected_tab;
        if (!self.model.foreignTab(workspace, index)) return null;
        const owner = shared_workspace.tabAuthority(workspace.treeConst(index).?).?;
        const id = workspace.shared_ids[index] orelse return false;
        self.peer_edits.reorder(self.model, owner, id, right) catch return false;
        return true;
    }

    /// A refused edit changes nothing, the active coordinator's pending
    /// focus included; a queued one supersedes every other pending focus.
    fn createOnPeer(self: *Engine, coordinator: support.ProviderId, kind: peer_edits.Kind, cwd: []const u8, owner: peer_edits.Owner) bool {
        const created = self.peer_edits.create(self.model, coordinator, kind, cwd, owner) catch return false;
        self.supersedeSelection();
        created.may_focus = true;
        return true;
    }

    /// Go to Directory's Open Here on a peer's listing: a new tab on that
    /// coordinator, owned as `openTabAt` owns the active coordinator's.
    pub fn openPeerTabAt(self: *Engine, coordinator: support.ProviderId, cwd: []const u8, owner: ?TerminalRef) bool {
        return self.createOnPeer(coordinator, .tab, cwd, if (owner) |ref| .{ .terminal = ref } else .none);
    }

    /// Whether a new tab can open on coordinator `id` now: the active one,
    /// or a peer that is showing a projected session.
    pub fn opensTabsOn(self: *Engine, id: support.ProviderId) bool {
        if (self.model.phux()) |active| if (active.providerId() == id) return true;
        return peer_edits.editable(self.model, id);
    }

    fn executeOperation(self: *Engine, id: u64, operation: protocol.Intent, fx: anytype) ?tab_commands.Status {
        if (self.model.phux() == null) {
            if (!self.applyModelIntent(operation, fx)) return null;
            return .applied;
        }
        self.admitDurableOperation(id, operation) catch return null;
        if (operationSupersedesFocus(operation)) {
            self.model.shared_workspace.desired_terminal = null;
            self.creation.supersedeFocusExceptCommand(id);
        }
        return .accepted_pending;
    }

    fn admitDurableOperation(self: *Engine, id: u64, operation: protocol.Intent) !void {
        switch (operation.kind) {
            .new_terminal => try self.creation.requestCorrelated(self.model, .tab, id),
            .new_window => try self.creation.requestCorrelated(self.model, .window, id),
            .close_tab => {
                const workspace = self.model.ws();
                if (operation.argument >= workspace.tab_count) return error.StaleTarget;
                if (self.model.foreignTab(workspace, operation.argument)) return error.ForeignCoordinator;
                const shared_id = workspace.shared_ids[operation.argument] orelse return error.StaleTarget;
                try self.model.shared_mutations.requestRemoveWindowCorrelated(self.model, shared_id, id);
            },
            .native_command => try self.admitDurableNative(id, operation.argument),
            else => return error.InvalidCommand,
        }
    }

    fn admitDurableNative(self: *Engine, id: u64, command: u8) !void {
        switch (command) {
            3 => {
                const ref = self.model.focusedTerminalRef() orelse return error.StaleTarget;
                if (!self.model.activeOwnsRef(ref)) return error.ForeignCoordinator;
                try self.model.shared_mutations.requestRemoveCorrelated(self.model, ref, id);
            },
            4 => try self.creation.requestCorrelated(self.model, .split_right, id),
            5 => try self.creation.requestCorrelated(self.model, .split_down, id),
            8, 9 => try self.reorderCorrelated(id, command == 9),
            else => return error.InvalidCommand,
        }
    }

    fn reorderCorrelated(self: *Engine, command_id: u64, right: bool) !void {
        const model = self.model;
        if (model.foreignTab(model.ws(), model.ws().selected_tab)) return error.ForeignCoordinator;
        const id = model.ws().shared_ids[model.ws().selected_tab] orelse return error.StaleTarget;
        const target = peer_edits.neighborIndex(model.phux().?.workspaceSnapshot().windows, id, right) orelse return error.StaleTarget;
        try model.shared_mutations.requestReorderCorrelated(model, id, @intCast(target), command_id);
    }

    fn operationSupersedesFocus(operation: protocol.Intent) bool {
        if (operation.kind != .native_command) return true;
        return operation.argument >= 3 and operation.argument <= 5;
    }

    fn tabReceipt(self: *Engine, id: u64, reason: tab_commands.Reason) tab_commands.Receipt {
        self.intent_refused = reason != .none;
        return .{ .id = id, .reason = reason, .status = if (reason == .none) .applied else .rejected, .sequence = self.sequence, .revision = self.revision };
    }

    fn selectTab(self: *Engine, index: u8) bool {
        if (!self.model.selectTab(index)) return false;
        self.supersedeSelection();
        return true;
    }

    pub fn supersedeSelection(self: *Engine) void {
        self.selection_epoch +|= 1;
        self.cancelPendingSelection();
        peer_restore.cancelFront(self.model);
    }

    fn cancelPendingSelection(self: *Engine) void {
        self.model.shared_workspace.desired_terminal = null;
        self.creation.supersedeFocus();
        // A peer's pending placement must not take focus back either.
        for (self.model.peers.items) |entry| entry.workspace.desired_terminal = null;
        self.peer_edits.supersedeFocus();
        // A picked empty session's state gives way too, unless its first tab
        // is already opening.
        empty_session.dismiss(self.model);
    }

    fn selectPlacedNavigation(self: *Engine, placed: model_module.PlacedTerminalDestination, fx: anytype) bool {
        const model = self.model;
        if (!model.containsTerminal(placed.terminal_ref)) return false;
        const current = model.locateTerminal(placed.terminal_ref) orelse return false;
        if (current.window != placed.window) return false;
        const workspace = model.wsAt(current.window) orelse return false;
        if (!workspace.selectTerminal(placed.terminal_ref)) return false;
        const previous = model.active_window;
        model.active_window = current.window;
        self.supersedeSelection();
        pointer_input.endHiddenCaptures(model, fx);
        if (previous != current.window) self.showNavigationWindow(fx, current.window);
        return true;
    }

    fn showNavigationWindow(_: *Engine, fx: anytype, window: usize) void {
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime @hasDecl(Fx, "showWindow")) fx.showWindow(scene.windowLabelFor(window));
    }

    pub fn didSelectNavigationWindow(self: *Engine, window: usize, fx: anytype) void {
        self.supersedeSelection();
        pointer_input.endHiddenCaptures(self.model, fx);
        self.showNavigationWindow(fx, window);
        self.syncRemoteFocus();
    }

    fn selectAvailableNavigation(self: *Engine, ref: TerminalRef, fx: anytype) bool {
        const model = self.model;
        if (support.providerKind(ref) != .phux) return false;
        if (self.selectCatalogSession(ref, fx)) |accepted| return accepted;
        if (self.creation.hasPendingTerminal(ref)) return false;
        self.creation.requestAttach(model, ref) catch return false;
        self.creation.supersedeFocusExcept(ref);
        model.shared_workspace.desired_terminal = ref;
        return true;
    }

    fn selectCatalogSession(self: *Engine, ref: TerminalRef, fx: anytype) ?bool {
        const remote = self.model.phux() orelse return false;
        const session = remote.terminalSession(ref) orelse return false;
        if (session == remote.selectedSessionId()) return null;
        if (!self.selectSessionNavigation(session, fx)) return false;
        self.model.shared_workspace.desired_terminal = ref;
        return true;
    }

    fn selectSessionNavigation(self: *Engine, id: u32, fx: anytype) bool {
        const remote = self.model.phux() orelse return false;
        const window = self.model.active_window;
        self.showSessionFromInWindow(remote, id, window, self.model.window_epochs[window], fx) catch return false;
        return true;
    }

    // ------------------------------------ coordinators beside the active one
    //
    // The model holds the active Phux provider and dynamically owned peers:
    // this Mac's coordinator while a registered remote host is active, and
    // registered hosts. Each runs its own worker on its own channel
    // (support.phuxPeerChannelKey), so any one restarts or fails alone. A peer
    // LISTS its sessions (GET_STATE, never attached, so it sizes nobody's
    // panes and streams nothing) until one of them is picked. It then SHOWS
    // that session: it attaches it on its own connection and its shared
    // workspace projects into the same windows as the active coordinator's.
    // No other coordinator redials. Terminal identity carries the coordinator
    // (TerminalRef.provider_id), so every key, resize and selection routes to
    // the one that minted it.

    /// Open every peer's channel at launch.
    pub fn openPeerChannels(self: *Engine, fx: anytype, on_event: anytype) void {
        self.openPeerWakeForReplay(fx, on_event);
        for (0..self.model.peers.items.len) |slot| self.openPeerChannel(fx, slot, on_event);
    }

    /// Register the shared effect occupancy without starting any socket workers.
    /// Replay's posting handle is intentionally inert; recorded events feed it.
    pub fn openPeerWakeForReplay(self: *Engine, fx: anytype, on_event: anytype) void {
        if (self.peer_wake_key == 0) self.peer_wake_key = support.allocatePeerHandle() catch return;
        self.peer_wake_handle = fx.openChannel(.{ .key = self.peer_wake_key, .on_event = on_event, .max_pending = 1 });
    }

    fn livePeerWake(self: *Engine, fx: anytype, on_event: anytype) native_sdk.ChannelHandle {
        if (self.peer_wake_handle.live()) return self.peer_wake_handle;
        self.peer_wake_key = support.allocatePeerHandle() catch return .{};
        self.openPeerWakeForReplay(fx, on_event);
        return self.peer_wake_handle;
    }

    /// Open one peer's channel and start (or restart) its worker. A peer that
    /// cannot open is shown as unavailable until its next restart.
    pub fn openPeerChannel(self: *Engine, fx: anytype, slot: usize, on_event: anytype) void {
        if (comptime !support.phux_enabled) return;
        const peer = self.model.phuxPeerAt(slot) orelse return;
        const key = self.peerChannelKey(slot);
        if (key == 0) {
            self.model.peers.items[slot].failed = true;
            return;
        }
        const handle = if (self.peer_wake_key != 0)
            self.livePeerWake(fx, on_event)
        else
            fx.openChannel(.{ .key = key, .on_event = on_event, .max_pending = 1 });
        if (!handle.live()) {
            self.model.peers.items[slot].failed = true;
            return;
        }
        if (peer.state() == .new) {
            peer.open(handle) catch return self.peerOpenFailed(fx, slot);
        } else {
            peer.reconnect(handle) catch return self.peerOpenFailed(fx, slot);
        }
    }

    fn peerOpenFailed(self: *Engine, fx: anytype, slot: usize) void {
        self.model.peers.items[slot].failed = true;
        self.retirePeerChannel(fx, slot);
        self.schedulePeerRetry(fx, slot);
    }

    /// Arm the slot's automatic redial: a listing peer that failed is dialed
    /// again after 1 s, then after twice the last wait, at most 60 s. Only a
    /// peer that stayed listed `peer_retry_stable_ms` before this failure
    /// starts over at 1 s; one that lists and fails again keeps backing off.
    /// A showing peer is not redialed: its tabs keep their frozen frames,
    /// picking its row retries it, and once its tabs leave the screen it goes
    /// back to listing (`settlePeers`).
    fn schedulePeerRetry(self: *Engine, fx: anytype, slot: usize) void {
        self.cancelPeerRetry(fx, slot);
        // This connection's listing, if any, ends with this failure.
        const entry = self.model.peers.items[slot];
        if (self.peerListedStably(slot)) entry.retry_delay_ms = 0;
        entry.listed_since = null;
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime !@hasDecl(Fx, "schedulePeerRetry")) return;
        const peer = self.model.phuxPeerAt(slot) orelse return;
        if (peer.showing()) return;
        const delay = if (entry.retry_delay_ms == 0) peer_retry_initial_ms else entry.retry_delay_ms;
        entry.retry_delay_ms = @min(delay * 2, peer_retry_max_ms);
        entry.retry_key = support.allocatePeerHandle() catch return;
        fx.schedulePeerRetry(entry.retry_key.?, delay);
    }

    /// The slot now holds another coordinator (or none): the next failure
    /// waits 1 s again, and a timer already armed is stale.
    fn resetPeerRetry(self: *Engine, fx: anytype, slot: usize) void {
        self.cancelPeerRetry(fx, slot);
        const entry = self.model.peers.items[slot];
        entry.retry_delay_ms = 0;
        entry.listed_since = null;
    }

    fn cancelPeerRetry(self: *Engine, fx: anytype, slot: usize) void {
        const entry = self.model.peers.items[slot];
        const key = entry.retry_key orelse return;
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime @hasDecl(Fx, "cancelTimer")) fx.cancelTimer(key);
        entry.retry_key = null;
    }

    /// The slot listed on this connection: a timer already armed is stale.
    /// The backoff itself is not reset here; the next failure decides, by how
    /// long the peer stayed listed (`peerListedStably`).
    fn notePeerListed(self: *Engine, fx: anytype, slot: usize) void {
        self.cancelPeerRetry(fx, slot);
        const entry = self.model.peers.items[slot];
        if (entry.listed_since == null) entry.listed_since = std.Io.Clock.awake.now(self.model.provider.io);
    }

    fn peerListedStably(self: *const Engine, slot: usize) bool {
        const since = self.model.peers.items[slot].listed_since orelse return false;
        const now = std.Io.Clock.awake.now(self.model.provider.io);
        return since.durationTo(now).toMilliseconds() >= peer_retry_stable_ms;
    }

    /// A peer's retry timer fired. Only a peer still failed, still listing,
    /// on the channel generation the timer was armed for, is dialed again,
    /// and as a lister: its connection asks GET_STATE and never attaches, so
    /// it holds no viewport. If it listed, was picked, went away or began
    /// showing meanwhile, the timer is stale and does nothing.
    pub fn onPeerRetryTimer(self: *Engine, fx: anytype, key: u64) bool {
        if (comptime !support.phux_enabled) return false;
        const slot = self.peerRetrySlot(key) orelse return false;
        if (!self.peerRetryDue(slot)) return false;
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime !@hasDecl(Fx, "restartPeer")) return false;
        self.model.peers.items[slot].failed = false;
        _ = fx.restartPeer(self, slot);
        return self.commitProviderChange(true);
    }

    /// SDK admission failure is terminal for this timer. Keep manual retry
    /// available rather than recursively scheduling another rejected timer.
    pub fn onPeerRetryRejected(self: *Engine, key: u64) bool {
        const slot = self.peerRetrySlot(key) orelse return false;
        self.model.peers.items[slot].retry_key = null;
        return self.commitProviderChange(true);
    }

    fn peerRetrySlot(self: *const Engine, key: u64) ?usize {
        for (self.model.peers.items, 0..) |entry, slot| {
            if (entry.retry_key == key) return slot;
        }
        return null;
    }

    /// Consumes the slot's armed timer; true when it may redial now.
    fn peerRetryDue(self: *Engine, slot: usize) bool {
        const entry = self.model.peers.items[slot];
        if (entry.retry_key == null) return false;
        entry.retry_key = null;
        const peer = self.model.phuxPeerAt(slot) orelse return false;
        return entry.failed and !peer.showing();
    }

    /// Peer `slot`'s current channel key.
    pub fn peerChannelKey(self: *const Engine, slot: usize) u64 {
        return self.model.peers.items[slot].channel_key;
    }

    /// Close the slot's current channel and move its key to the next
    /// generation, so anything that occupancy still delivers is stale.
    fn retirePeerChannel(self: *Engine, fx: anytype, slot: usize) void {
        self.cancelPeerRetry(fx, slot);
        const Fx = navigationFxType(@TypeOf(fx));
        if (self.peer_wake_key == 0) {
            if (comptime @hasDecl(Fx, "closeChannel")) fx.closeChannel(self.peerChannelKey(slot));
        }
        const entry = self.model.peers.items[slot];
        entry.channel_key = support.allocatePeerHandle() catch 0;
    }

    /// Drain one peer's wake. A listing peer can only move the switcher's
    /// host groups; a showing peer also projects its workspace and rings
    /// its bells. Either way it is one ordered invalidation. An event of a
    /// channel the slot has since closed is not this peer's (`peerStaleEvent`).
    pub fn onPeerChannel(self: *Engine, fx: anytype, event: native_sdk.EffectChannelEvent, on_event: anytype) bool {
        if (comptime !support.phux_enabled) return false;
        if (self.peer_wake_key != 0 and event.key == self.peer_wake_key) return self.onPeerWake(fx, event.kind);
        // A showing peer's focused pane may have just become live.
        defer self.syncRemoteFocus();
        const slot = self.peerSlotForHandle(event.key) orelse return false;
        const entry = self.model.peers.items[slot];
        if (entry.provider == null) return false;
        if (event.key != entry.channel_key) return self.peerStaleEvent(fx, slot, event.kind, on_event);
        return switch (event.kind) {
            .data => self.drainPeer(fx, slot),
            .closed, .rejected => self.peerClosed(fx, slot),
        };
    }

    fn onPeerWake(self: *Engine, fx: anytype, kind: native_sdk.EffectChannelEventKind) bool {
        if (kind != .data) return self.peerWakeClosed(fx);
        defer self.syncRemoteFocus();
        var changed = false;
        // Visible work always gets the first turn. Each provider already
        // bounds its own FFI drain; a catalog flood never precedes input work.
        for (self.model.peers.items, 0..) |entry, slot| {
            if (entry.failed) continue;
            const peer = entry.provider orelse continue;
            if (!peer.showing() or !peerWakePending(peer)) continue;
            changed = self.drainPeer(fx, slot) or changed;
        }
        // One background source per turn is the minimum useful progress;
        // round-robin continuation prevents N catalogs multiplying a wake's
        // drain budget. A coalesced post is only a hint, never identity.
        if (self.nextBackgroundPeer()) |slot| changed = self.drainPeer(fx, slot) or changed;
        self.continuePeerWake();
        return changed;
    }

    fn peerWakePending(peer: *support.PhuxProvider) bool {
        return peer.bridge.incoming.hasReadiness();
    }

    fn nextBackgroundPeer(self: *Engine) ?usize {
        const peers = self.model.peers.items;
        for (0..peers.len) |_| {
            self.peer_background_cursor %= peers.len;
            const slot = self.peer_background_cursor;
            self.peer_background_cursor += 1;
            if (peers[slot].failed) continue;
            const peer = peers[slot].provider orelse continue;
            if (!peer.showing() and peerWakePending(peer)) return slot;
        }
        return null;
    }

    fn continuePeerWake(self: *Engine) void {
        for (self.model.peers.items) |entry| {
            if (entry.failed) continue;
            const peer = entry.provider orelse continue;
            if (!peerWakePending(peer)) continue;
            _ = self.peer_wake_handle.post("");
            return;
        }
    }

    fn peerWakeClosed(self: *Engine, fx: anytype) bool {
        var changed = false;
        for (self.model.peers.items, 0..) |entry, slot| {
            if (entry.provider == null) continue;
            changed = self.peerClosed(fx, slot) or changed;
        }
        return changed;
    }

    /// An event of a channel occupancy the slot has since closed, including
    /// one from before a Disconnect and a new Connect reused the slot. Only
    /// the close a restart waits for does anything: it opens the slot's next
    /// channel. Anything else is ignored, so it can stop no other connection.
    fn peerSlotForHandle(self: *const Engine, key: u64) ?usize {
        if (key == 0) return null;
        for (self.model.peers.items, 0..) |entry, slot| {
            if (entry.channel_key == key or entry.closing_key == key) return slot;
        }
        return null;
    }

    fn peerStaleEvent(self: *Engine, fx: anytype, slot: usize, kind: native_sdk.EffectChannelEventKind, on_event: anytype) bool {
        const entry = self.model.peers.items[slot];
        if (kind != .closed or !entry.reopen) return false;
        entry.reopen = false;
        entry.closing_key = null;
        self.openPeerChannel(fx, slot, on_event);
        return self.commitProviderChange(true);
    }

    fn drainPeer(self: *Engine, fx: anytype, slot: usize) bool {
        const peer = self.model.phuxPeerAt(slot).?;
        const limit = if (self.peer_wake_key != 0 and !peer.showing()) 1 else peer.bridge.incoming.pendingCount();
        const delta = peer.drainReadinessBudget(limit) catch return self.failPeer(fx, slot);
        if (delta.sessions_listed or delta.ready_published) {
            self.model.peers.items[slot].failed = false;
            self.notePeerListed(fx, slot);
        }
        if (peer.showing()) return self.drainShowingPeerWake(fx, slot, delta);
        return self.drainListingPeerWake(fx, slot, delta);
    }

    fn drainListingPeerWake(self: *Engine, fx: anytype, slot: usize, delta: support.SyncDelta) bool {
        const peer = self.model.phuxPeerAt(slot).?;
        // Nothing presents a listing peer's terminals, so nothing rings for
        // them; its list settling is what moves.
        while (peer.takeNotice()) |notice| peer.releaseNotice(notice);
        // A listing peer's only operations are conditional kills of its
        // strays: best effort, their outcomes are not waited for.
        while (peer.takeOperationResult()) |result| _ = self.completeLocalTool(peer, result);
        // Listing again: its strays (spawns whose placement was never sent)
        // are killed there, each only if still unattached since its spawn.
        if (delta.sessions_listed) _ = self.peer_edits.sendStrays(self.model, slot);
        // A remembered host's list judges what it showed at the last quit
        // (ADR-0110): only a front record is shown, and only now.
        if (delta.sessions_listed and peer_restore.onListed(self, fx, slot)) return self.commitProviderChange(true);
        return self.commitProviderChange(delta.sessions_listed or delta.detached or delta.sessions_renamed);
    }

    fn drainShowingPeerWake(self: *Engine, fx: anytype, slot: usize, delta: support.SyncDelta) bool {
        if (delta.detached) return self.peerDetached(fx, slot);
        const projected = self.drainShowingPeer(fx, slot, delta.sessions_listed);
        return self.commitProviderChange(projected or peerPublicationMoved(delta));
    }

    /// The server ended the shown session: its tabs leave, and the peer goes
    /// back to listing on a fresh connection instead of freezing for good.
    fn peerDetached(self: *Engine, fx: anytype, slot: usize) bool {
        self.stopShowingPeer(slot);
        self.model.phuxPeerAt(slot).?.standBy();
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime !@hasDecl(Fx, "restartPeer")) return self.failPeer(fx, slot);
        _ = fx.restartPeer(self, slot);
        return self.commitProviderChange(true);
    }

    /// A failed peer's group row, picked: dial it again. The group reads
    /// Connecting… until it lists.
    pub fn retryPeer(self: *Engine, coordinator: support.ProviderId, fx: anytype) bool {
        if (comptime !support.phux_enabled) return false;
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime !@hasDecl(Fx, "restartPeer")) return false;
        const slot = self.model.peerSlot(coordinator) orelse return false;
        if (!self.model.peers.items[slot].failed) return false;
        self.cancelPeerRetry(fx, slot);
        self.model.peers.items[slot].failed = false;
        return fx.restartPeer(self, slot);
    }

    /// A settled Go to Directory listing moves too: the picker may be
    /// listing through this peer.
    fn peerPublicationMoved(delta: support.SyncDelta) bool {
        return delta.sessions_listed or delta.sessions_renamed or delta.ready_published or delta.metadata_changed or delta.directory_changed;
    }

    /// The slot's current channel closed, or its open was refused, without
    /// Cockpit asking: the peer failed, and its group says why. A close
    /// Cockpit asked for names an older generation and never reaches here.
    fn peerClosed(self: *Engine, fx: anytype, slot: usize) bool {
        const model = self.model;
        const peer = model.phuxPeerAt(slot).?;
        empty_session.forgetAttachment(model, peer.context_id, false);
        peer.stop();
        self.peer_edits.forget(slot);
        empty_session.forgetPeer(model, peer.providerId(), false);
        peer_restore.failed(model, slot);
        // That occupancy is gone; the next one opens under a fresh key.
        model.peers.items[slot].channel_key = support.allocatePeerHandle() catch 0;
        model.peers.items[slot].failed = true;
        self.schedulePeerRetry(fx, slot);
        return self.commitProviderChange(true);
    }

    /// A peer's connection failed: stop it and say so in its host group.
    /// Its placed tabs keep their last frames, frozen, until it reconnects.
    fn failPeer(self: *Engine, fx: anytype, slot: usize) bool {
        const peer = self.model.phuxPeerAt(slot) orelse return false;
        empty_session.forgetAttachment(self.model, peer.context_id, false);
        peer.stop();
        self.peer_edits.forget(slot);
        empty_session.forgetPeer(self.model, peer.providerId(), false);
        peer_restore.failed(self.model, slot);
        self.model.peers.items[slot].failed = true;
        self.retirePeerChannel(fx, slot);
        self.schedulePeerRetry(fx, slot);
        return self.commitProviderChange(true);
    }

    /// A showing peer's wake, as `drainPhux` is the active one's: attach and
    /// detach results settle its subscriptions, its bells ring, and its
    /// shared workspace projects beside the other coordinators' tabs.
    fn drainShowingPeer(self: *Engine, fx: anytype, slot: usize, catalog_listed: bool) bool {
        const model = self.model;
        const peer = model.phuxPeerAt(slot).?;
        const state = &model.peers.items[slot].workspace;
        model.bindSharedAttachment(peer);
        var changed = self.drainShowingPeerOperations(peer, state, slot);
        changed = self.drainNotices(fx, peer) or changed;
        const published = peer.workspaceSnapshot();
        if (published.state == .unavailable) return changed;
        peerProjectionContext(peer, state) catch return changed;
        // Before projecting, so a confirmed new tab is selected as it lands.
        changed = self.peer_edits.pump(model, slot) or changed;
        const projected = self.projectPeer(peer, state, published);
        // A picked empty session New Tab showed: its first tab, on this peer.
        changed = empty_session.pump(self, slot) or changed;
        changed = self.settleEmptyAttachment(peer, slot, catalog_listed) or changed;
        // A terminal spawned for a new tab or split is not placed yet; it
        // must not be detached as unused before its placement lands.
        if (published.status != .pending and self.peer_edits.pendingCreations(slot) == 0) state.releaseUnused(model);
        state.subscribe(model);
        return projected or changed;
    }

    fn drainShowingPeerOperations(self: *Engine, peer: *support.PhuxProvider, state: *shared_workspace.State, slot: usize) bool {
        const model = self.model;
        var changed = false;
        while (peer.takeOperationResult()) |result| {
            if (self.completeLocalTool(peer, result)) {
                changed = true;
                continue;
            }
            _ = completeSubscriptions(state, peer, result);
            _ = self.peer_edits.complete(model, slot, result);
            changed = true;
        }
        return changed;
    }

    /// After pumping an exact attachment, settle its empty picks from that
    /// source's first-tab outcomes and whether this drain adopted a list.
    fn settleEmptyAttachment(self: *Engine, remote: *support.PhuxProvider, peer_slot: ?usize, catalog_listed: bool) bool {
        var reported: [16]empty_session.FirstTab = undefined;
        const count = self.emptyFirstTabs(remote, peer_slot, &reported);
        return empty_session.settleAttachment(self.model, remote, .{ .first_tabs = reported[0..count], .catalog_listed = catalog_listed });
    }

    fn emptyFirstTabs(self: *Engine, remote: *support.PhuxProvider, peer_slot: ?usize, out: []empty_session.FirstTab) usize {
        if (peer_slot) |slot| {
            var raw: [16]peer_edits.EmptyFirstTab = undefined;
            const count = self.peer_edits.collectEmptyFirstTabs(slot, &raw);
            const n = @min(count, out.len);
            for (raw[0..n], 0..) |entry, i| {
                out[i] = .{
                    .window = entry.window,
                    .window_epoch = entry.window_epoch,
                    .connection_epoch = entry.connection_epoch,
                    .outcome = switch (entry.outcome) {
                        .pending => .pending,
                        .refused => .refused,
                        .placed => .placed,
                    },
                };
            }
            return n;
        }
        _ = remote;
        var raw: [16]durable_creation.Creation.EmptyFirstTab = undefined;
        const count = self.creation.collectEmptyFirstTabs(&raw);
        const n = @min(count, out.len);
        for (raw[0..n], 0..) |entry, i| {
            out[i] = .{
                .window = entry.window,
                .window_epoch = entry.window_epoch,
                .connection_epoch = entry.connection_epoch,
                .outcome = switch (entry.outcome) {
                    .pending => .pending,
                    .refused => .refused,
                    .placed => .placed,
                },
            };
        }
        return n;
    }

    /// Apply a peer's publication beside the other coordinators' tabs. A
    /// refused one keeps the last good tabs and is named on its rows.
    fn projectPeer(self: *Engine, peer: *support.PhuxProvider, state: anytype, published: anytype) bool {
        const model = self.model;
        const generation = state.projection_generation;
        const first = state.session == 0;
        const projected = state.apply(model, published, peer.connectionEpoch()) catch blk: {
            const newly = !state.refused;
            state.refused = true;
            break :blk newly;
        };
        // Showing a session means seeing it: its first projection takes the
        // selection, so it is not taken back to listing as hidden.
        // A remembered tab (ADR-0110) is selected when it still exists.
        if (first and state.session != 0) self.revealPeerProjection(peer, state.session);
        if (generation != state.projection_generation) self.split_drag = null;
        return projected;
    }

    fn revealPeerProjection(self: *Engine, peer: *support.PhuxProvider, session: u32) void {
        const model = self.model;
        const slot = model.peerSlotForAttachment(peer.context_id) orelse return;
        const entry = model.peers.items[slot];
        if (entry.selection_epoch) |epoch| if (epoch != self.selection_epoch) return;
        const hint = peer_restore.takeHint(model, peer.providerId(), session);
        session_attachments.revealProjected(model, peer.context_id, hint);
    }

    /// Select the tab coordinator `id` projected for shared window
    /// `preferred` when there is one, else its first tab in the first window
    /// holding one, and make that window active.
    fn revealAuthority(model: *Model, id: support.ProviderId, preferred: ?[16]u8) void {
        if (preferred) |wanted| if (revealSharedWindow(model, id, wanted)) return;
        for (0..model_module.max_windows) |index| {
            if (!model.windowOpen(index)) continue;
            const workspace = model.wsAt(index) orelse continue;
            for (0..workspace.tab_count) |tab| {
                const tree = workspace.treeConst(tab) orelse continue;
                if (shared_workspace.tabAuthority(tree) != id) continue;
                workspace.selected_tab = tab;
                workspace.web_selected = false;
                model.active_window = index;
                return;
            }
        }
    }

    /// Select coordinator `id`'s tab of shared window `wanted`; false when
    /// no open window holds it (the window was closed or moved meanwhile).
    fn revealSharedWindow(model: *Model, id: support.ProviderId, wanted: [16]u8) bool {
        for (0..model_module.max_windows) |index| {
            if (!model.windowOpen(index)) continue;
            const workspace = model.wsAt(index) orelse continue;
            for (0..workspace.tab_count) |tab| {
                const tree = workspace.treeConst(tab) orelse continue;
                if (shared_workspace.tabAuthority(tree) != id) continue;
                const shared_id = workspace.shared_ids[tab] orelse continue;
                if (!std.mem.eql(u8, &shared_id, &wanted)) continue;
                workspace.selected_tab = tab;
                workspace.web_selected = false;
                model.active_window = index;
                return true;
            }
        }
        return false;
    }

    /// Keep each showing peer on screen or let it go. A peer is shown only
    /// while one of its tabs is the selected, painted tab of an open window.
    /// Once none is (another session's tab was chosen, its tabs were closed,
    /// its session has no windows) it returns to listing, so it holds no
    /// viewport that could size its session for anyone else. Before its
    /// first projection lands there is nothing to judge. Called after every
    /// native mutation; true when a peer went back to listing.
    pub fn settlePeers(self: *Engine, fx: anytype) bool {
        if (comptime !support.phux_enabled) return false;
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime !@hasDecl(Fx, "restartPeer")) return false;
        // A peer's New Window whose tab never landed leaves no empty window.
        var changed = self.peer_edits.retireOrphans(self.model);
        for (0..self.model.peers.items.len) |slot| {
            if (self.emptyTabHolds(slot)) continue;
            if (!self.peerHidden(slot)) continue;
            self.unshowPeer(fx, slot);
            changed = true;
        }
        return self.commitProviderChange(changed);
    }

    /// A peer shown by New Tab in a picked empty session stays shown until
    /// that tab lands on screen, or its spawn is gone without one.
    fn emptyTabHolds(self: *Engine, slot: usize) bool {
        const peer = self.model.phuxPeerAtConst(slot) orelse return false;
        const visible = authorityVisible(self.model, peer.providerId());
        return empty_session.holds(self.model, slot, self.peer_edits.pendingCreations(slot), visible);
    }

    fn peerHidden(self: *const Engine, slot: usize) bool {
        const model = self.model;
        const peer = model.phuxPeerAtConst(slot) orelse return false;
        if (!peer.showing()) return false;
        if (model.peers.items[slot].workspace.session == 0) return false;
        if (model.peers.items[slot].workspace.attachment_id != null) return !session_attachments.visibleElsewhere(model, peer.context_id, model_module.max_windows);
        return !authorityVisible(model, peer.providerId());
    }

    fn authorityVisible(model: *const Model, id: support.ProviderId) bool {
        for (0..model_module.max_windows) |index| {
            if (!model.windowOpen(index)) continue;
            const workspace = model.wsAtConst(index) orelse continue;
            if (workspace.web_selected) continue;
            const tree = workspace.treeConst(workspace.selected_tab) orelse continue;
            if (shared_workspace.tabAuthority(tree) == id) return true;
        }
        return false;
    }

    /// Back to listing: its tabs leave, and it reconnects as a standby, so
    /// its attach (and every viewport it held) ends and the new connection
    /// only asks GET_STATE.
    fn unshowPeer(self: *Engine, fx: anytype, slot: usize) void {
        self.stopShowingPeer(slot);
        self.model.phuxPeerAt(slot).?.standBy();
        _ = fx.restartPeer(self, slot);
    }

    /// Close a peer's tab on that coordinator: the same layout-only window
    /// removal a tab of the active coordinator sends to its own. Its
    /// terminals keep running there; its next publication drops the tab.
    fn closePeerWindow(self: *Engine, workspace: *const model_module.Workspace, index: usize) bool {
        const tree = workspace.treeConst(index) orelse return false;
        const id = workspace.shared_ids[index] orelse return false;
        const owner = shared_workspace.tabAuthority(tree) orelse return false;
        self.peer_edits.removeWindow(self.model, owner, id) catch return false;
        self.supersedeSelection();
        return true;
    }

    /// Close a peer's pane on that coordinator.
    fn closePeerPane(self: *Engine, ref: TerminalRef) bool {
        self.peer_edits.removePane(self.model, ref) catch return false;
        self.supersedeSelection();
        return true;
    }

    fn peerProjectionContext(peer: anytype, state: anytype) !void {
        const server = peer.serverId() orelse return error.MissingServerIdentity;
        var buffer: [@import("../attachment_state.zig").max_endpoint_bytes]u8 = undefined;
        const endpoint = try coordinatorEndpoint(peer, &buffer);
        state.authority = peer.providerId();
        state.setContext(contextHash(endpoint, server));
    }

    /// Restart one peer as Reconnect restarts the active provider: a live
    /// channel publishes its close first, and the slot's next channel opens
    /// on that event (`peerStaleEvent`), under the next generation's key.
    /// Waiting keeps each slot to one occupancy of the runtime's small
    /// channel table.
    pub fn restartPeerConnection(self: *Engine, fx: anytype, slot: usize, on_event: anytype) bool {
        if (comptime !support.phux_enabled) return false;
        const peer = self.model.phuxPeerAt(slot) orelse return false;
        if (peer.state() != .new) peer.stop();
        // The next connection is a new epoch: nothing queued for this one
        // may reach it.
        self.peer_edits.forget(slot);
        if (self.peer_wake_key != 0) {
            self.retirePeerChannel(fx, slot);
            self.openPeerChannel(fx, slot, on_event);
            return true;
        }
        // A reopen already waits for its close; that close opens it.
        const entry = self.model.peers.items[slot];
        if (entry.reopen) return true;
        if (fx.peerChannelLive(self.peerChannelKey(slot))) {
            entry.reopen = true;
            entry.closing_key = entry.channel_key;
            self.retirePeerChannel(fx, slot);
        } else {
            self.openPeerChannel(fx, slot, on_event);
        }
        return true;
    }

    /// Remove one peer entirely: its host group leaves the switcher and its
    /// tabs leave the windows. Every other coordinator stays as it was.
    pub fn dropPeer(self: *Engine, fx: anytype, slot: usize) void {
        if (comptime !support.phux_enabled) return;
        const model = self.model;
        const peer = model.phuxPeerAt(slot) orelse return;
        empty_session.forgetAttachment(model, peer.context_id, true);
        self.stopShowingPeer(slot);
        empty_session.forgetPeer(model, peer.providerId(), true);
        self.peer_edits.dropStrays(slot);
        const entry = model.peers.items[slot];
        entry.restore = null;
        entry.provider = null;
        entry.reopen = false;
        entry.closing_key = null;
        entry.failed = false;
        entry.coordinator_context = null;
        entry.session_created_at = null;
        entry.selection_epoch = null;
        self.resetPeerRetry(fx, slot);
        peer.stop();
        // Its close event may arrive after another peer takes the slot; the
        // new generation's key tells the two apart.
        self.retirePeerChannel(fx, slot);
        peer.destroy();
        self.revision +%= 1;
    }

    /// Start or restart the configured local coordinator without retargeting an
    /// ambient remote provider or following the focused terminal's host.
    pub fn connectConfiguredLocal(self: *Engine, origin: @import("ts_window_navigation.zig").Target, fx: anytype, phux_event: anytype) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        if (!origin.validWindow(self.model)) return error.StaleTarget;
        const path = self.model.config.phux_socket.slice();
        if (path.len == 0) return error.NoProvider;
        if (self.localToolProvider()) |remote| {
            if (self.model.phux() == remote) {
                self.openPhuxChannel(fx, phux_event, remote.state() != .new);
                return;
            }
            const slot = self.model.peerSlotForAttachment(remote.context_id) orelse return error.StaleTarget;
            self.model.peers.items[slot].failed = false;
            if (!fx.restartPeer(self, slot)) return error.ConnectionUnavailable;
            return;
        }
        const remote = try support.PhuxProvider.create(self.model.provider.gpa, self.model.provider.io, .{ .unix = path }, null, "phux-cockpit");
        errdefer remote.destroy();
        if (self.model.phux() == null) {
            model_module.attachPhuxProvider(self.model, remote);
            initializeSharedPresentation(self.model);
            self.openPhuxChannel(fx, phux_event, false);
            return;
        }
        try self.adoptCapturedPeer(remote, fx);
    }

    /// Transfer the captured provider into stable heap-owned peer storage.
    pub fn adoptCapturedPeer(self: *Engine, remote: *support.PhuxProvider, fx: anytype) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const slot = self.model.freePeerSlot() catch |err| {
            remote.destroy();
            return err;
        };
        self.model.peers.items[slot].provider = remote;
        remote.standBy();
        self.model.bindSharedAttachment(remote);
        if (!fx.restartPeer(self, slot)) {
            self.dropPeer(fx, slot);
            return error.ConnectionUnavailable;
        }
    }

    pub fn retryCapturedPeer(self: *Engine, target: machine_runtime.Target, tunnel: @import("machines.zig").Tunnel, identity: machine_runtime.RegistryIdentity, fx: anytype) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        if (!target.matches(self.model)) {
            tunnel.close();
            return error.StaleTarget;
        }
        const remote = self.model.phuxForAttachment(target.attachment_id).?;
        try remote.replaceCapturedTunnel(tunnel, identity);
        if (self.model.peerSlotForAttachment(target.attachment_id)) |slot| {
            self.cancelPeerRetry(fx, slot);
            self.model.peers.items[slot].failed = false;
            if (!fx.restartPeer(self, slot)) return error.ConnectionUnavailable;
        } else {
            if (!fx.restartPhux(self)) return error.ConnectionUnavailable;
        }
    }

    pub fn disconnectCapturedPeers(self: *Engine, targets: []const machine_runtime.Target, fx: anytype) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        // Every exact target is checked before the first mutation.
        for (targets) |target| {
            if (!target.matches(self.model)) return error.StaleTarget;
        }
        for (targets) |target| {
            if (self.model.peerSlotForAttachment(target.attachment_id)) |slot| {
                self.dropPeer(fx, slot);
            } else if (self.model.phuxForAttachment(target.attachment_id)) |remote| {
                self.dropActiveAttachment(remote, fx);
            }
        }
    }

    fn dropActiveAttachment(self: *Engine, remote: *support.PhuxProvider, fx: anytype) void {
        const model = self.model;
        self.creation.disconnect(model);
        model.shared_workspace.leaveSession(model) catch {};
        remote.stop();
        model.phux_provider = null;
        model.phux_reconnect_after_close = false;
        model.phux_connection_unavailable = false;
        fx.closeChannel(support.phux_channel_key);
        remote.destroy();
        model.shared_workspace.deinit();
        model.shared_workspace = .{};
    }

    /// Take a showing peer's tabs out of the windows and forget its
    /// projection. Done before its identity changes or it goes.
    fn stopShowingPeer(self: *Engine, slot: usize) void {
        const model = self.model;
        const peer = model.phuxPeerAt(slot) orelse return;
        const state = &model.peers.items[slot].workspace;
        self.peer_edits.forget(slot);
        if (peer.showing()) {
            // Its own tabs, by the id they carry; never another's.
            state.authority = peer.providerId();
            state.leaveSession(model) catch {};
            self.split_drag = null;
        }
        state.deinit();
        state.* = .{};
    }

    /// Make `endpoint` the active coordinator and keep the one it leaves
    /// listed beside the others. A peer already reaching `endpoint` trades
    /// places with the active provider; otherwise the leaving coordinator
    /// takes a free slot. Only those two restart, through the path Reconnect
    /// uses. `endpoint`, `session` and `label` must not borrow either
    /// provider's pending target.
    pub fn exchangeCoordinators(self: *Engine, fx: anytype, endpoint: support.PhuxEndpoint, session: ?[]const u8, label: ?[]const u8) !void {
        if (comptime !support.phux_enabled) return error.NoProvider;
        const model = self.model;
        const active = model.phux() orelse return error.NoProvider;
        // Already the active coordinator: a plain retarget, no second copy.
        if (support.PhuxProvider.coordinatorId(endpoint) == active.effectiveProviderId()) {
            try active.requestRetarget(endpoint, session, label);
            // The user chose which coordinator to show (ADR-0110).
            peer_restore.cancelFront(model);
            const Fx = navigationFxType(@TypeOf(fx));
            if (comptime @hasDecl(Fx, "restartPhux")) _ = fx.restartPhux(self);
            return;
        }
        const slot = self.peerSlotForEndpoint(endpoint) orelse try model.freePeerSlot();
        const gpa = std.heap.page_allocator;
        // Copied: the active retarget below frees a pending target these
        // slices would otherwise point into.
        var leaving = try active.copyTarget(gpa);
        defer leaving.deinit(gpa);
        // All-or-nothing: every allocation happens before either side is
        // committed, so a failure leaves both providers as they were.
        const next_active = try active.prepareRetarget(endpoint, session, label);
        errdefer active.discardRetarget(next_active);
        if (model.peers.items[slot].provider) |peer| {
            const next_peer = try peer.prepareRetarget(leaving.descriptor(), leaving.session, leaving.label);
            // Its tabs carry the identity it is about to give up.
            self.stopShowingPeer(slot);
            peer.standBy();
            active.commitRetarget(next_active);
            peer.commitRetarget(next_peer);
        } else {
            const peer = try support.PhuxProvider.create(gpa, model.provider.io, leaving.descriptor(), leaving.session, "phux-cockpit");
            errdefer peer.destroy();
            if (leaving.label) |text| try peer.setRemoteLabel(text);
            // Lists sessions only; never attaches, so it sizes nobody's panes.
            peer.standBy();
            active.commitRetarget(next_active);
            model.peers.items[slot].provider = peer;
        }
        model.peers.items[slot].failed = false;
        self.resetPeerRetry(fx, slot);
        // The slot now names another coordinator: strays of the one it
        // held are not this one's to kill.
        self.peer_edits.dropStrays(slot);
        // Nor is its restore state; and the user chose which coordinator to
        // show, so no front record waiting for its list is shown (ADR-0110).
        model.peers.items[slot].restore = null;
        peer_restore.cancelFront(model);
        self.restartCoordinators(fx, slot);
    }

    fn peerSlotForEndpoint(self: *const Engine, endpoint: support.PhuxEndpoint) ?usize {
        const wanted = support.PhuxProvider.coordinatorId(endpoint);
        for (self.model.peers.items, 0..) |entry, slot| {
            const peer = entry.provider orelse continue;
            if (peer.effectiveProviderId() == wanted) return slot;
        }
        return null;
    }

    fn restartCoordinators(self: *Engine, fx: anytype, slot: usize) void {
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime @hasDecl(Fx, "restartPhux")) _ = fx.restartPhux(self);
        if (comptime @hasDecl(Fx, "restartPeer")) _ = fx.restartPeer(self, slot);
    }

    /// A session of a peer coordinator: show it beside the others. Only that
    /// peer restarts its connection, and after HELLO_OK and its read-only
    /// rename subscription it sends ATTACH for that session; the active coordinator and every other
    /// peer keep their connections. A peer already showing a session leaves
    /// it for this one.
    pub fn showPeerSession(self: *Engine, coordinator: support.ProviderId, session: u32, fx: anytype) bool {
        if (comptime !support.phux_enabled) return false;
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime !@hasDecl(Fx, "restartPeer")) return false;
        const model = self.model;
        const slot = self.showableSlot(coordinator, session) orelse return false;
        // Compatibility for coordinator-only callers. Captured navigation uses
        // showSessionFromInWindow and retains the exact per-window attachment.
        if (empty_session.peerSessionEmpty(model, coordinator, session)) {
            self.supersedeSelection();
            return empty_session.pickPeer(model, coordinator, session);
        }
        self.showSessionFromInWindow(model.phuxPeerAt(slot).?, session, model.active_window, model.window_epochs[model.active_window], fx) catch return false;
        return true;
    }

    pub fn showSessionFromInWindow(self: *Engine, remote: *support.PhuxProvider, session: u32, window: usize, epoch: u64, fx: anytype) !void {
        try session_attachments.show(self, remote, session, window, epoch, fx);
    }

    pub fn openPeerTabFromInWindow(self: *Engine, remote: *support.PhuxProvider, window: usize, epoch: u64, cwd: []const u8, may_focus: bool) !void {
        _ = try self.peer_edits.createTabIn(self.model, remote, window, epoch, cwd, may_focus);
    }

    pub fn localToolProvider(self: *Engine) ?*support.PhuxProvider {
        return @constCast(self.model.localPhuxProviderConst());
    }

    pub fn localToolSelectionEpoch(self: *const Engine) u64 {
        return self.selection_epoch;
    }

    /// A local tool never follows the focused remote. A listing local peer is
    /// first attached to one of its own sessions in the invoking window; the
    /// retained adapter retries only after that exact provider is ready.
    pub fn ensureLocalSessionInWindow(self: *Engine, window: usize, epoch: u64, fx: anytype) !*support.PhuxProvider {
        if (!self.model.windowOpen(window) or self.model.window_epochs[window] != epoch) return error.InvalidWindow;
        const remote = self.localToolProvider() orelse return error.NotReady;
        if (remote.state() == .attached and remote.selectedSessionId() != null) return remote;
        if (self.model.peerSlotForAttachment(remote.context_id) != null) {
            const catalog = remote.standbyCatalog();
            if (catalog.len != 0) try self.showSessionFromInWindow(remote, catalog[0].id, window, epoch, fx);
        }
        return error.NotReady;
    }

    pub fn placeLocalToolSpawn(self: *Engine, remote: *support.PhuxProvider, window: usize, epoch: u64, result: support.OperationResult, may_focus: bool) !u64 {
        const ref = result.terminal_ref orelse return error.MissingIdentity;
        const slot = self.vacantLocalToolPlacement() orelse return error.OperationCapacity;
        const ticket = self.next_local_tool_ticket;
        const next = std.math.add(u64, ticket, 1) catch return error.OperationCapacity;
        if (self.model.phux() == remote) {
            try self.creation.adoptSpawnIn(self.model, remote, result, window, epoch, ticket, may_focus);
        } else {
            _ = try self.peer_edits.adoptSpawnIn(self.model, remote, result, window, epoch, may_focus);
        }
        slot.* = .{
            .ticket = ticket,
            .provider_context = remote.context_id,
            .host_context = remote.host.context_id,
            .connection_epoch = remote.connectionEpoch(),
            .window = window,
            .window_epoch = epoch,
            .terminal_ref = ref,
        };
        self.next_local_tool_ticket = next;
        return ticket;
    }

    pub fn localToolPlacementStatus(self: *Engine, remote: *support.PhuxProvider, ticket: u64) local_tool_launch.PlacementStatus {
        const slot = self.localToolPlacement(ticket) orelse return .unknown;
        const tracked = slot.*.?;
        if (!localToolSourceCurrent(tracked, remote)) return finishLocalToolPlacement(slot, .unknown);
        if (self.model.phux() == remote) return self.activeLocalToolPlacementStatus(slot, ticket);
        return self.peerLocalToolPlacementStatus(slot, remote, tracked);
    }

    fn activeLocalToolPlacementStatus(self: *Engine, slot: *?LocalToolPlacement, ticket: u64) local_tool_launch.PlacementStatus {
        const completion = self.creation.completionFor(ticket) orelse return .pending;
        const status: local_tool_launch.PlacementStatus = switch (completion.placement) {
            .placed => .placed,
            .refused, .destination_lost => .refused,
            .unknown, .not_requested => .unknown,
        };
        _ = self.creation.ackCompletion(ticket);
        return finishLocalToolPlacement(slot, status);
    }

    fn peerLocalToolPlacementStatus(self: *Engine, slot: *?LocalToolPlacement, remote: *support.PhuxProvider, tracked: LocalToolPlacement) local_tool_launch.PlacementStatus {
        if (localToolPlaced(self.model, tracked)) return finishLocalToolPlacement(slot, .placed);
        const peer_slot = self.model.peerSlotForAttachment(remote.context_id) orelse return finishLocalToolPlacement(slot, .unknown);
        if (self.peer_edits.pendingTerminal(peer_slot, tracked.terminal_ref)) return .pending;
        return finishLocalToolPlacement(slot, if (remote.state() == .attached) .refused else .unknown);
    }

    fn vacantLocalToolPlacement(self: *Engine) ?*?LocalToolPlacement {
        for (&self.local_tool_placements) |*slot| if (slot.* == null) return slot;
        return null;
    }

    fn localToolPlacement(self: *Engine, ticket: u64) ?*?LocalToolPlacement {
        for (&self.local_tool_placements) |*slot| {
            const tracked = slot.* orelse continue;
            if (tracked.ticket == ticket) return slot;
        }
        return null;
    }

    fn localToolSourceCurrent(tracked: LocalToolPlacement, remote: *const support.PhuxProvider) bool {
        return tracked.provider_context == remote.context_id and tracked.host_context == remote.host.context_id and tracked.connection_epoch == remote.connectionEpoch();
    }

    fn localToolPlaced(model: *const Model, tracked: LocalToolPlacement) bool {
        if (!model.windowOpen(tracked.window) or model.window_epochs[tracked.window] != tracked.window_epoch) return false;
        const workspace = model.wsAtConst(tracked.window) orelse return false;
        return workspace.tabOfTerminal(tracked.terminal_ref) != null;
    }

    fn finishLocalToolPlacement(slot: *?LocalToolPlacement, status: local_tool_launch.PlacementStatus) local_tool_launch.PlacementStatus {
        slot.* = null;
        return status;
    }

    pub fn captureNewSessionDestination(self: *Engine) ?new_session.Destination {
        return new_session_runtime.capture(self, self.model.phuxForWindow(self.model.active_window), self.selection_epoch);
    }

    pub fn newSessionDestinationCurrent(self: *Engine, destination: new_session.Destination) bool {
        return new_session_runtime.current(self, destination);
    }

    pub fn sendNewSession(self: *Engine, destination: new_session.Destination, name: []const u8, keep_empty: bool) !u32 {
        return new_session_runtime.send(self, destination, name, keep_empty);
    }

    pub fn pollNewSession(self: *Engine, destination: new_session.Destination, request: u32) new_session.Outcome {
        return new_session_runtime.poll(self, destination, request);
    }

    pub fn releaseNewSession(self: *Engine, destination: new_session.Destination, request: u32) void {
        new_session_runtime.release(self, destination, request);
    }

    pub fn didCreateSession(self: *Engine, destination: new_session.Destination, session: u32, fx: anytype) void {
        new_session_runtime.didCreate(self, destination, session, self.selection_epoch, fx, Engine.showSessionFromInWindow);
    }

    /// The slot of a peer that lists `session` and may show it now.
    fn showableSlot(self: *const Engine, coordinator: support.ProviderId, session: u32) ?usize {
        const model = self.model;
        // Mid-exchange two providers can briefly hold one coordinator id.
        if (model.phuxConst()) |active| if (active.pending_retarget != null) return null;
        const slot = model.peerSlot(coordinator) orelse return null;
        if (!peerListsSession(model.peers.items[slot].provider.?, session)) return null;
        return slot;
    }

    fn peerListsSession(peer: anytype, id: u32) bool {
        for (peer.standbyCatalog()) |entry| {
            if (entry.id == id) return true;
        }
        return false;
    }

    fn reconnectNavigation(self: *Engine, fx: anytype) bool {
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime !@hasDecl(Fx, "restartPhux")) return false;
        if (navigation.connection(self.model) != .offline) return false;
        return fx.restartPhux(self);
    }

    fn navigationFxType(comptime T: type) type {
        return switch (@typeInfo(T)) {
            .pointer => |pointer| pointer.child,
            else => T,
        };
    }

    /// A live source must publish its close before its replacement opens. An
    /// already-closed source has no future close event, so reopen it directly.
    /// onPhuxChannel owns the existing reconnect-after-close continuation.
    pub fn restartNavigationConnection(self: *Engine, fx: anytype, on_event: anytype) bool {
        const model = self.model;
        const remote = model.phux() orelse return false;
        self.providerDisconnectedExcept(self.session_handoff);
        remote.stop();
        model.phux_connection_unavailable = false;
        if (fx.phuxChannelLive()) {
            model.phux_reconnect_after_close = true;
            fx.closeChannel(support.phux_channel_key);
        } else {
            model.phux_reconnect_after_close = false;
            self.openPhuxChannel(fx, on_event, remote.state() != .new);
        }
        return true;
    }

    fn refuse(self: *Engine) bool {
        self.intent_refused = true;
        return false;
    }

    /// Mirrors `update.zig`'s `.new_terminal` transaction. The shell itself
    /// is spawned by the next `spawnShells`, which the extension runs after
    /// every intent and every frame; the pane exists and is selected first,
    /// exactly as in the shipping app. Refusals stay visible through the
    /// same model flags the shipping app uses.
    fn newTerminal(self: *Engine) bool {
        const model = self.model;
        if (model.phux() != null) return self.peerCreate(.tab) orelse self.createDurable(.tab);
        if (!model.canAddPane()) return false;
        const pane = model.provider.createTerminal() catch {
            model.terminal_limit_refused = true;
            return false;
        };
        if (!model.admitTab(pane.id)) {
            _ = model.provider.destroyTerminal(pane.id);
            model.ws().tab_limit_refused = true;
            return false;
        }
        _ = model.selectTerminal(pane.id);
        model.terminal_limit_refused = false;
        model.ws().tab_limit_refused = false;
        return true;
    }

    /// Closing the last leaf retires its window; remote execution stays live.
    pub fn closeTab(self: *Engine, index: u8, fx: anytype) bool {
        const model = self.model;
        if (model.phux() != null) {
            if (index >= model.ws().tab_count) return false;
            if (model.foreignTab(model.ws(), index)) return self.closePeerWindow(model.ws(), index);
            const id = model.ws().shared_ids[index] orelse return false;
            model.shared_mutations.requestRemoveWindowNative(model, id) catch return false;
            self.supersedeSelection();
            return true;
        }
        return lifecycle.closeTab(self.model, fx, self.model.active_window, index);
    }

    /// The settings surface's Save: mirrors update.zig's .settings_commit,
    /// including the write and its refusal flag, minus the preview/restore
    /// dance the core keeps to itself.
    fn setTheme(self: *Engine, index: u8) bool {
        if (index >= theme_module.builtins.len) return false;
        const model = self.model;
        if (!model.config.setTheme(theme_module.builtins[index].name)) return false;
        switch (model.writeConfigTheme(model.provider.io)) {
            .written => model.config_write_refused = false,
            .refused => model.config_write_refused = true,
            .no_destination => {},
        }
        return true;
    }

    fn revealConfig(self: *Engine, fx: anytype) bool {
        const model = self.model;
        if (!model.config_file.enabled() or !self.config_probe.exists) return false;
        fx.hostSend("native-sdk.os.revealPath", model.config_file.path());
        return true;
    }

    /// Asked once, when the surface opens, exactly as the shipping app asks:
    /// a view is pure and must not touch a disk, and the answer only has to
    /// be true at the moment the person reads the line.
    pub fn probeConfig(self: *Engine) bool {
        const model = self.model;
        self.config_probe = .{
            .exists = model.configFileExists(model.provider.io),
            .writable = model.configFileWritable(model.provider.io),
            .probed = true,
        };
        return true;
    }

    // ----------------------------------------------------------- windows

    /// update.zig's .new_window: a window slot, a workspace, one shell in
    /// it, selected. Refusals put everything back and stay visible.
    fn newWindow(self: *Engine) bool {
        const model = self.model;
        // With a peer's pane focused, New Window opens its tab on that peer.
        if (model.phux() != null) return self.peerCreate(.window) orelse self.createDurable(.window);
        if (!model.canAddPane()) return false;
        const index = model.freeWindowIndex() orelse {
            model.window_limit_refused = true;
            return false;
        };
        const workspace = model.openWindow(index) orelse {
            model.window_limit_refused = true;
            return false;
        };
        const previous = model.active_window;
        model.active_window = index;
        const pane = model.provider.createTerminal() catch {
            model.terminal_limit_refused = true;
            model.closeWindow(index);
            model.active_window = previous;
            return false;
        };
        if (!workspace.admitTab(pane.id)) {
            _ = model.provider.destroyTerminal(pane.id);
            model.closeWindow(index);
            model.active_window = previous;
            return false;
        }
        _ = workspace.selectTerminal(pane.id);
        model.window_limit_refused = false;
        return true;
    }

    /// Local processes end; coordinator-owned terminals detach presentation.
    fn closeWindow(self: *Engine, index: u8, fx: anytype) bool {
        if (self.model.phux() != null) return self.closeSharedWindow(index, fx);
        return lifecycle.closeWindow(self.model, fx, index);
    }

    fn closeSharedWindow(self: *Engine, index: u8, fx: anytype) bool {
        session_attachments.closeWindow(self.model, fx, index) catch return false;
        self.supersedeSelection();
        pointer_input.endHiddenCaptures(self.model, fx);
        return true;
    }

    fn focusWindow(self: *Engine, index: u8) bool {
        const model = self.model;
        if (!model.windowOpen(index)) return false;
        model.active_window = index;
        self.supersedeSelection();
        return true;
    }

    /// Commands registered by the manifest remain native operations over the
    /// same engine model. TypeScript maps names to this compact enum, but no
    /// terminal bytes, pane nodes, or platform ids cross the seam.
    fn nativeCommand(self: *Engine, raw: u8, fx: anytype) bool {
        const command = protocol.decodeNativeCommand(raw) orelse return false;
        const model = self.model;
        const changed = switch (command) {
            .previous_tab, .next_tab, .move_tab_left, .move_tab_right => self.tabCommand(command),
            .close_focused_pane => self.closeFocusedPane(fx),
            .split_right => self.splitFocusedPane(.horizontal),
            .split_down => self.splitFocusedPane(.vertical),
            .previous_pane, .next_pane, .focus_left, .focus_right, .focus_up, .focus_down => self.paneFocusCommand(command),
            .copy, .paste => self.clipboardCommand(command, fx),
            .select_all, .clear, .find, .find_next, .find_previous => self.localPresentationCommand(command, fx),
            .font_larger, .font_smaller, .font_reset, .fullscreen, .minimize => self.displayCommand(command, fx),
        };
        if (changed) pointer_input.endAllCaptures(model, fx);
        return changed;
    }

    fn tabCommand(self: *Engine, command: protocol.NativeCommand) bool {
        const model = self.model;
        const workspace = model.ws();
        switch (command) {
            .previous_tab, .next_tab => {
                if (workspace.tab_count < 2) return false;
                const delta: i32 = if (command == .next_tab) 1 else -1;
                const count: i32 = @intCast(workspace.tab_count);
                const selected: i32 = @intCast(workspace.selected_tab);
                workspace.selected_tab = @intCast(@mod(selected + delta, count));
                self.supersedeSelection();
            },
            .move_tab_left, .move_tab_right => {
                if (model.phux() != null) return self.moveSharedTab(command == .move_tab_right);
                const terminal = workspace.tabTerminal(workspace.selected_tab) orelse return false;
                if (!model.moveTerminal(terminal, if (command == .move_tab_right) 1 else -1)) return false;
            },
            else => unreachable,
        }
        return true;
    }

    fn moveSharedTab(self: *Engine, right: bool) bool {
        const model = self.model;
        if (self.peerReorderSelected(right)) |sent| return sent;
        const id = model.ws().shared_ids[model.ws().selected_tab] orelse return false;
        const remote = model.phux() orelse return false;
        const target = peer_edits.neighborIndex(remote.workspaceSnapshot().windows, id, right) orelse return false;
        model.shared_mutations.requestReorderNative(model, id, @intCast(target)) catch return false;
        return true;
    }

    fn clipboardCommand(self: *Engine, command: protocol.NativeCommand, fx: anytype) bool {
        const model = self.model;
        const ref = model.focusedTerminalRef() orelse return false;
        switch (command) {
            .copy => interaction.copy(model, fx, ref),
            .paste => interaction.requestPaste(model, fx, ref),
            else => unreachable,
        }
        return true;
    }

    fn localPresentationCommand(self: *Engine, command: protocol.NativeCommand, fx: anytype) bool {
        const ref = self.model.focusedTerminalRef() orelse return false;
        if (support.providerKind(ref) == .phux) return remote_commands.command(self.model, ref, command);
        const pane = self.focusedPane() orelse return false;
        return localPaneCommand(pane, command, fx);
    }

    fn localPaneCommand(pane: *model_module.Pane, command: protocol.NativeCommand, fx: anytype) bool {
        switch (command) {
            .select_all => {
                if (!pane.session.selectAllHistory()) return false;
                pane.selecting = false;
            },
            .clear => {
                pane.selecting = false;
                pane.session.clearSelection();
                terminal_runtime.feedOutput(pane, fx, "\x1b[H\x1b[2J\x1b[3J");
                pane.session.scrollToBottom();
                pane.session.refreshScreenText();
                terminal_runtime.moveResponsesToOutbound(pane, fx);
            },
            .find => {
                if (pane.selecting) {
                    pane.selecting = false;
                    pane.session.clearSelection();
                }
                pane.session.searchOpen();
            },
            .find_next, .find_previous => {
                _ = pane.session.searchStep(command == .find_next);
            },
            else => unreachable,
        }
        return true;
    }

    fn displayCommand(self: *Engine, command: protocol.NativeCommand, fx: anytype) bool {
        const model = self.model;
        switch (command) {
            .font_larger => return model.stepFontSize(1),
            .font_smaller => return model.stepFontSize(-1),
            .font_reset => return model.resetFontSize(),
            .fullscreen => fx.toggleFullscreenWindow(scene.windowLabelFor(model.active_window)),
            .minimize => fx.minimizeWindow(scene.windowLabelFor(model.active_window)),
            else => unreachable,
        }
        return true;
    }

    fn paneFocusCommand(self: *Engine, command: protocol.NativeCommand) bool {
        const model = self.model;
        const tree = model.selectedTree() orelse return false;
        const next = switch (command) {
            .previous_pane, .next_pane => tree.cycleFocus(if (command == .next_pane) 1 else -1),
            .focus_left, .focus_right, .focus_up, .focus_down => {
                return self.focusDirection(command);
            },
            else => unreachable,
        } orelse return false;
        if (next == tree.focus) return false;
        tree.focus = next;
        self.supersedeSelection();
        return true;
    }

    fn focusDirection(self: *Engine, command: protocol.NativeCommand) bool {
        const model = self.model;
        const tree = model.selectedTree() orelse return false;
        const workspace = model.wsConst();
        const chrome = projection.workspaceChromeIn(model, workspace, workspace.surface_size);
        const direction: layout.Direction = switch (command) {
            .focus_left => .left,
            .focus_right => .right,
            .focus_up => .up,
            .focus_down => .down,
            else => unreachable,
        };
        const next = tree.focusDirection(chrome.content, projection.split_divider_width, projection.split_pane_min_width, projection.split_pane_min_height, direction) orelse return false;
        if (next == tree.focus) return false;
        tree.focus = next;
        self.supersedeSelection();
        return true;
    }

    fn splitFocusedPane(self: *Engine, orientation: layout.Orientation) bool {
        const model = self.model;
        if (model.phux() != null) {
            // A peer's pane splits on that peer.
            if (self.peerCreate(if (orientation == .horizontal) .split_right else .split_down)) |sent| return sent;
            return self.createDurable(if (orientation == .horizontal) .split_right else .split_down);
        }
        if (!model.canAddPane()) return false;
        const tree = model.selectedTree() orelse return false;
        const target = tree.focus;
        if (target == layout.none or tree.node(target).kind != .leaf) return false;
        const origin = tree.focusedTerminal();
        const pane = model.provider.createTerminal() catch {
            model.terminal_limit_refused = true;
            return false;
        };
        self.inheritWorkingDirectory(pane, origin);
        _ = tree.split(target, orientation, pane.id) catch {
            _ = model.provider.destroyTerminal(pane.id);
            return false;
        };
        model.terminal_limit_refused = false;
        return true;
    }

    fn inheritWorkingDirectory(self: *Engine, pane: *model_module.Pane, origin: ?TerminalRef) void {
        const model = self.model;
        if (!model.config.inherit_working_directory) return;
        const source_ref = origin orelse return;
        const source = model.provider.terminalConst(source_ref) orelse return;
        const cwd = source.pwd();
        if (cwd.len == 0) return;
        const slot = model.provider.slotIndex(pane.id) orelse return;
        pane.argv = local.paneArgvIn(cwd, &model.cwd_argv[slot]);
    }

    fn closeFocusedPane(self: *Engine, fx: anytype) bool {
        const ref = self.model.focusedTerminalRef() orelse return false;
        if (self.model.phux() != null) {
            // Another coordinator's pane closes on that coordinator.
            if (!self.model.activeOwnsRef(ref)) return self.closePeerPane(ref);
            self.model.shared_mutations.requestRemoveNative(self.model, ref) catch return false;
            self.supersedeSelection();
            return true;
        }
        return lifecycle.closePane(self.model, fx, ref, true);
    }

    /// Go to Directory: a new durable tab whose shell starts in `cwd` on the
    /// connected coordinator's host, placed like New Tab.
    /// Go to Directory's Open Here. `owner` is the satellite pane a
    /// satellite listing was made for, so the tab opens on that satellite;
    /// null opens on the coordinator's own host.
    pub fn openTabAt(self: *Engine, cwd: []const u8, owner: ?support.TerminalRef) bool {
        if (self.model.phux() == null) return false;
        self.supersedeSelection();
        self.creation.requestIn(self.model, .tab, cwd, owner) catch |err| {
            // Only capacity is the terminal limit; any other refusal is the
            // picker's own to state.
            self.model.terminal_limit_refused = durable_creation.isCapacityError(err);
            return false;
        };
        self.model.terminal_limit_refused = false;
        return true;
    }

    fn createDurable(self: *Engine, kind: durable_creation.Kind) bool {
        self.supersedeSelection();
        self.creation.request(self.model, kind) catch |err| {
            // A split of another coordinator's pane is refused, not a limit.
            if (err != error.ForeignCoordinator) self.model.terminal_limit_refused = true;
            return false;
        };
        self.model.terminal_limit_refused = false;
        return true;
    }

    /// The window index a canvas label names, by the shipping scene's own
    /// table: the spike declares the same labels, so per-window painting,
    /// frames and input resolve through one function.
    pub fn windowIndexForCanvas(label: []const u8) ?usize {
        return scene.windowIndexForCanvas(label);
    }

    /// Bind the native incarnation as soon as the SDK materializes a window,
    /// including a close before its first GPU frame. Never replace a known id
    /// from a delayed native notification for a previously retired slot.
    pub fn noteNativeWindow(self: *Engine, info: platform.WindowInfo) void {
        if (!info.open) return;
        for (0..model_module.max_windows) |index| {
            if (!std.mem.eql(u8, info.label, scene.windowLabelFor(index))) continue;
            const workspace = self.model.wsAt(index) orelse return;
            if (workspace.window_id == 0) workspace.window_id = info.id;
            return;
        }
    }

    /// An OS close is an observed fact, not an index-based command computed
    /// from a snapshot. Match its exact native incarnation, then publish the
    /// retirement even when unrelated title evidence advanced the revision.
    pub fn closeNativeWindow(self: *Engine, fx: anytype, window_id: platform.WindowId) bool {
        if (window_id == 0) return false;
        for (1..model_module.max_windows) |index| {
            const workspace = self.model.wsAtConst(index) orelse continue;
            if (workspace.window_id != window_id) continue;
            self.retireWindowInput(fx, window_id);
            if (self.closeWindow(@intCast(index), fx)) return self.commitProviderChange(true);
            // The OS incarnation is gone even when there is no room to
            // rehome shared tabs. Keep that workspace and recover its native
            // presentation through the next snapshot, with a fresh id.
            self.model.wsAt(index).?.window_id = 0;
            _ = self.commitProviderChange(true);
            self.intent_refused = true;
            return true;
        }
        return false;
    }

    fn retireWindowInput(self: *Engine, fx: anytype, window_id: platform.WindowId) void {
        if (self.split_drag) |drag| {
            if (drag.window_id == window_id) self.cancelSplitDrag();
        }
        shipping_pointer.cancelLocalWindow(self.model, fx, window_id);
        self.remote_pointer.cancelWindow(self.model, window_id);
    }

    pub fn matchesNativeWindow(self: *const Engine, index: usize, window_id: platform.WindowId) bool {
        if (!self.model.windowOpen(index)) return false;
        const workspace = self.model.wsAtConst(index) orelse return false;
        return workspace.window_id == window_id;
    }

    /// Adopt the platform's focused window as the active one, the way
    /// CockpitHost adopts a routed event's window; a window this engine has
    /// not painted yet has no id to match.
    pub fn adoptFocusedWindow(self: *Engine, window_id: platform.WindowId) void {
        const model = self.model;
        for (0..model_module.max_windows) |index| {
            const workspace = model.wsAtConst(index) orelse continue;
            if (workspace.window_id != window_id) continue;
            if (model.windowOpen(index)) model.active_window = index;
            return;
        }
    }

    pub fn beginPublication(self: *const Engine) publication.Checkpoint {
        return publication.Checkpoint.capture(self.model, self.sequence, self.revision);
    }

    /// Finish one native transition. Focus changes invalidate ambient/positional
    /// targets; persistence feedback only publishes status. A refusal announces
    /// a sequence but supplies no target fence, so those are checked separately.
    pub fn finishPublication(self: *Engine, before: publication.Checkpoint) bool {
        defer self.syncRemoteFocus();
        const changes = before.changes(self.model);
        if (!changes.needsSnapshot()) return false;
        const needs_focus_fence = changes.focus and self.revision == before.revision;
        if (self.sequence != before.sequence and !needs_focus_fence) return false;
        if (needs_focus_fence) self.revision +%= 1;
        self.sequence +%= 1;
        return true;
    }

    /// Compatibility entry for hosts that only observe window adoption.
    pub fn commitWindowAdoption(self: *Engine, before_window: usize, before_sequence: u64) bool {
        var before = self.beginPublication();
        before.window = before_window;
        before.sequence = before_sequence;
        return self.finishPublication(before);
    }

    // ------------------------------------------------------------ shells

    /// Spawn a shell for every registered pane that does not have one, the
    /// way `update.initFx` does at boot. Idempotent: a slot keeps its shell
    /// until the pane is destroyed, and a reused slot carries a new pty key.
    pub fn spawnShells(self: *Engine, fx: anytype, on_event: anytype) void {
        const model = self.model;
        for (0..max_terminals) |index| {
            if (model.provider.states[index] != .active) {
                self.spawned_keys[index] = 0;
                continue;
            }
            const pane = model.provider.slot(index);
            if (self.spawned_keys[index] == pane.pty_key) continue;
            terminal_runtime.spawnPane(pane, fx, on_event);
            model_module.applySessionConfig(&model.config, pane.session);
            self.spawned_keys[index] = pane.pty_key;
        }
    }

    fn paneChromeFingerprint(self: *const Engine, pane: *const model_module.Pane) u64 {
        var hasher = std.hash.Wyhash.init(0);
        std.hash.autoHash(&hasher, pane.phase);
        std.hash.autoHash(&hasher, pane.exit_code);
        std.hash.autoHash(&hasher, pane.exit_signal);
        std.hash.autoHash(&hasher, pane.exit_reason);
        var title_room: [ts_snapshot.max_title_bytes]u8 = undefined;
        const title = projection.terminalTitleInto(self.model, pane.id, &title_room);
        hasher.update(title);
        hasher.update(pane.pwd());
        std.hash.autoHash(&hasher, projection.terminalNeedsAttention(self.model, pane.id));
        return hasher.final();
    }

    /// One pty event, applied the way `update.zig`'s `.shell` arm applies it.
    /// The return is edge-triggered only for state represented in the core's
    /// snapshot (phase/title/cwd/attention); ordinary terminal output still
    /// wakes native painting without forcing a snapshot on every byte batch.
    pub fn onShellEvent(self: *Engine, fx: anytype, event: native_sdk.EffectPtyEvent) bool {
        defer self.syncRemoteFocus();
        const pane = terminal_runtime.paneForKey(self.model, event.key) orelse return false;
        const chrome_before = self.paneChromeFingerprint(pane);
        switch (event.kind) {
            .output => self.feedShellOutput(fx, pane, event.bytes),
            .exit => {
                terminal_runtime.finishSession(pane, event);
                pointer_input.endCapturesForTerminal(self.model, fx, pane.id);
                if (pane.phase == .ended) {
                    _ = lifecycle.closePane(self.model, fx, pane.id, false);
                    return self.commitProviderChange(true);
                }
            },
            // Write acknowledgements never reach a pty event constructor;
            // the shipping app marks the arm unreachable for the same reason.
            .write => {},
        }
        if (chrome_before == self.paneChromeFingerprint(pane)) return false;
        self.sequence +%= 1;
        self.revision +%= 1;
        self.intent_refused = false;
        return true;
    }

    fn feedShellOutput(self: *Engine, fx: anytype, pane: *model_module.Pane, bytes: []const u8) void {
        pane.phase = .live;
        pane.output_batches += 1;
        pane.output_bytes += bytes.len;
        const protocol_before = pane.mouse_protocol_fingerprint;
        // Preserve the bell's rising edge before mutating the terminal.
        const bell_before = pane.bellRung();
        terminal_runtime.feedOutput(pane, fx, bytes);
        pointer_input.syncMouseProtocol(pane);
        if (protocol_before != 0 and protocol_before != pane.mouse_protocol_fingerprint) {
            pointer_input.endMismatchedMouseCaptures(self.model, fx, pane);
        }
        pane.session.refreshScreenText();
        self.notifyBackgroundBell(fx, pane, bell_before);
        if (pane.selecting and !pane.session.rebaseSelection()) pane.selecting = false;
        terminal_runtime.flushOutbound(pane, fx);
        terminal_runtime.moveResponsesToOutbound(pane, fx);
    }

    pub fn maintenancePending(self: *const Engine) bool {
        return terminal_runtime.maintenancePending(self.model);
    }

    pub fn maintain(self: *Engine, fx: anytype) bool {
        var chrome_changed = false;
        for (0..max_terminals) |index| {
            if (self.model.provider.states[index] != .active) continue;
            const pane = self.model.provider.slot(index);
            const before = self.paneChromeFingerprint(pane);
            terminal_runtime.maintainPane(pane, fx);
            chrome_changed = chrome_changed or before != self.paneChromeFingerprint(pane);
        }
        return self.commitProviderChange(chrome_changed);
    }

    // ------------------------------------------------------------- input

    fn onRemoteKey(self: *Engine, fx: anytype, ref: TerminalRef, event: canvas.WidgetKeyboardEvent) void {
        const state = self.model.remoteUi(ref) orelse return;
        if (state.search.open) return remote_commands.key(self.model, fx, ref, event);
        if (self.remoteShortcut(fx, ref, event)) return;
        if (state.selecting) {
            self.remoteSelectionKey(fx, ref, event);
            return;
        }
        if (self.remoteNaturalKey(ref, event)) return;
        interaction.rememberKey(self.model, ref, event);
        interaction.remoteKey(self.model, ref, event);
    }

    fn remoteShortcut(self: *Engine, fx: anytype, ref: TerminalRef, event: canvas.WidgetKeyboardEvent) bool {
        if (!event.modifiers.super or event.modifiers.control) return false;
        if (self.external_keybindings) return self.remoteModeShortcut(ref, event);
        if (keyIs(event.key, "c")) {
            interaction.copy(self.model, fx, ref);
            return true;
        }
        if (keyIs(event.key, "v")) {
            interaction.requestPaste(self.model, fx, ref);
            return true;
        }
        if (terminalShortcut(event)) |command| return remote_commands.command(self.model, ref, command);
        return self.remoteModeShortcut(ref, event);
    }

    fn remoteModeShortcut(self: *Engine, ref: TerminalRef, event: canvas.WidgetKeyboardEvent) bool {
        if (event.modifiers.shift and keyIs(event.key, "space")) {
            const state = self.model.remoteUi(ref) orelse return true;
            if (state.selecting) update_module.remote_selection.clear(self.model, state) else update_module.remote_selection.begin(self.model, ref, state);
            return true;
        }
        return self.remoteScrollKey(ref, event);
    }

    fn remoteNaturalKey(self: *Engine, ref: TerminalRef, event: canvas.WidgetKeyboardEvent) bool {
        const input = terminal_runtime.providerNaturalKey(event) orelse return false;
        const remote = self.model.phuxForRef(ref) orelse return false;
        const owner = self.model.terminalOwner(ref) orelse return false;
        self.remote_natural_keys_held |= terminal_runtime.macosNaturalTextKeyMask(event.key);
        remote.sendKey(owner, &input) catch {};
        return true;
    }

    fn remoteScrollKey(self: *Engine, ref: TerminalRef, event: canvas.WidgetKeyboardEvent) bool {
        const state = self.model.remoteUi(ref) orelse return false;
        const remote = self.model.phuxForRef(ref) orelse return false;
        const presentation = self.model.remotePresentation(ref) orelse return false;
        const scroll = scrollKey(event, presentation.rows) orelse return false;
        remote.scrollViewport(state.owner, scroll) catch {};
        return true;
    }

    fn scrollKey(event: canvas.WidgetKeyboardEvent, page_rows: u16) ?support.Scroll {
        const direction: i64 = if (keyIs(event.key, "arrowup")) -1 else if (keyIs(event.key, "arrowdown")) 1 else 0;
        if (direction != 0) {
            const rows: i64 = if (event.modifiers.shift) page_rows else 1;
            return .{ .kind = .delta, .value = direction * rows };
        }
        if (keyIs(event.key, "home")) return .{ .kind = .top };
        if (keyIs(event.key, "end")) return .{ .kind = .bottom };
        return null;
    }

    fn remoteSelectionKey(self: *Engine, fx: anytype, ref: TerminalRef, event: canvas.WidgetKeyboardEvent) void {
        const state = self.model.remoteUi(ref) orelse return;
        if (keyIs(event.key, "escape")) return update_module.remote_selection.clear(self.model, state);
        if (keyIs(event.key, "enter")) return interaction.copy(self.model, fx, ref);
        if (keyIs(event.key, "b")) {
            state.rectangle = !state.rectangle;
            update_module.remote_selection.apply(self.model, state);
            return;
        }
        const movements = .{
            .{ "arrowleft", -1, 0 }, .{ "arrowright", 1, 0 },
            .{ "arrowup", 0, -1 },   .{ "arrowdown", 0, 1 },
        };
        inline for (movements) |move| {
            if (keyIs(event.key, move[0])) return update_module.remote_selection.move(self.model, ref, state, move[1], move[2]);
        }
    }

    /// A key that no chrome widget claimed, the way update.zig's handleKey
    /// treats the terminal block: the search field first, then the app's own
    /// chords (find, select, copy, paste, select all), and everything else to
    /// the focused pane's emulator encoder, which alone knows the live modes
    /// the bytes depend on. Releases only ever reach the encoder.
    pub fn onKey(self: *Engine, fx: anytype, event: canvas.WidgetKeyboardEvent) void {
        if (!self.model.focused or self.input_suspended) return;
        if (event.phase == .key_up) return self.releaseKey(fx, event);
        self.remote_natural_keys_held &= ~terminal_runtime.macosNaturalTextKeyMask(event.key);
        const ref = self.model.focusedTerminalRef() orelse return;
        if (support.providerKind(ref) == .phux) {
            self.onRemoteKey(fx, ref, event);
            return;
        }
        self.onLocalKey(fx, ref, event);
    }

    fn onLocalKey(self: *Engine, fx: anytype, ref: TerminalRef, event: canvas.WidgetKeyboardEvent) void {
        const pane = self.model.provider.terminal(ref) orelse return;
        if (pane.session.search.open) {
            self.searchKey(fx, pane, event);
            return;
        }
        if (self.localShortcut(fx, pane, event)) return;
        if (pane.selecting) return self.localSelectionKey(fx, pane, event);
        if (!pane.acceptsInput()) return;
        interaction.rememberKey(self.model, ref, event);
        terminal_runtime.encodeKeyEvent(pane, fx, event, .press);
    }

    pub fn textInputOwnsKeyboard(self: *const Engine) bool {
        const ref = self.model.focusedTerminalRef() orelse return false;
        if (support.providerKind(ref) == .phux) {
            const state = self.model.remoteUiConst(ref) orelse return false;
            return state.search.open;
        }
        const pane = self.model.provider.terminalConst(ref) orelse return false;
        return pane.session.search.open;
    }

    fn releaseKey(self: *Engine, fx: anytype, event: canvas.WidgetKeyboardEvent) void {
        const mask = terminal_runtime.macosNaturalTextKeyMask(event.key);
        if (self.remote_natural_keys_held & mask != 0) {
            self.remote_natural_keys_held &= ~mask;
            return;
        }
        interaction.releaseKey(self.model, fx, event);
    }

    fn localShortcut(self: *Engine, fx: anytype, pane: *model_module.Pane, event: canvas.WidgetKeyboardEvent) bool {
        const mods = event.modifiers;
        if (!mods.super or mods.control) return false;
        if (mods.shift and keyIs(event.key, "space")) {
            if (pane.selecting) {
                pane.selecting = false;
                pane.session.clearSelection();
            } else {
                pane.selecting = true;
                pane.session.beginSelection(false);
            }
            return true;
        }
        if (self.external_keybindings) return false;
        const command = terminalShortcut(event) orelse return false;
        if (command == .copy and !pane.session.selectionActive()) return false;
        _ = self.nativeCommand(@intFromEnum(command), fx);
        return true;
    }

    fn terminalShortcut(event: canvas.WidgetKeyboardEvent) ?protocol.NativeCommand {
        if (keyIs(event.key, "c")) return .copy;
        if (keyIs(event.key, "v")) return .paste;
        if (event.modifiers.alt or event.modifiers.control) return null;
        if (keyIs(event.key, "g")) return if (event.modifiers.shift) .find_previous else .find_next;
        if (event.modifiers.shift) return null;
        if (keyIs(event.key, "f")) return .find;
        if (keyIs(event.key, "a")) return .select_all;
        return null;
    }

    fn localSelectionKey(self: *Engine, fx: anytype, pane: *model_module.Pane, event: canvas.WidgetKeyboardEvent) void {
        if (keyIs(event.key, "escape")) {
            pane.selecting = false;
            pane.session.clearSelection();
            return;
        }
        if (keyIs(event.key, "enter")) return self.copySelection(fx, pane);
        if (keyIs(event.key, "b")) return pane.session.toggleSelectionBlock();
        const movements = .{
            .{ "arrowleft", -1, 0 }, .{ "arrowright", 1, 0 },
            .{ "arrowup", 0, -1 },   .{ "arrowdown", 0, 1 },
        };
        inline for (movements) |move| {
            if (keyIs(event.key, move[0])) return pane.session.moveSelection(move[1], move[2], event.modifiers.shift);
        }
    }

    /// update.zig's handleSearchKey: the field owns Escape, Enter and
    /// Backspace; paste goes into the needle; copy still copies.
    fn searchKey(self: *Engine, fx: anytype, pane: *model_module.Pane, event: canvas.WidgetKeyboardEvent) void {
        const primary = event.modifiers.hasCommandModifier();
        if (primary and keyIs(event.key, "v")) {
            self.requestPaste(fx, pane);
            return;
        }
        if (primary and keyIs(event.key, "c") and (pane.selecting or pane.session.selectionActive())) {
            self.copySelection(fx, pane);
            return;
        }
        if (keyIs(event.key, "escape")) {
            pane.session.searchClose();
            return;
        }
        if (keyIs(event.key, "enter") or keyIs(event.key, "return")) {
            _ = pane.session.searchStep(!event.modifiers.shift);
            return;
        }
        if (keyIs(event.key, "backspace") or keyIs(event.key, "delete")) {
            _ = pane.session.searchBackspace();
            return;
        }
    }

    /// Committed text: into an open search needle, else into the shell the
    /// way update.zig's .text arm sends it (never over a keyboard selection,
    /// never into an ended shell, always after scrolling to the bottom).
    pub fn onText(self: *Engine, fx: anytype, event: canvas.WidgetKeyboardEvent) void {
        if (!self.model.focused or self.input_suspended) return;
        const ref = self.model.focusedTerminalRef() orelse return;
        interaction.rememberKey(self.model, ref, event);
        if (support.providerKind(ref) == .phux) return interaction.remoteText(self.model, ref, event);
        const pane = self.focusedPane() orelse return;
        localText(pane, fx, event.text);
    }

    fn localText(pane: *model_module.Pane, fx: anytype, text: []const u8) void {
        if (text.len == 0) return;
        if (pane.session.search.open) {
            _ = pane.session.searchInput(text);
            return;
        }
        if (pane.selecting or !pane.acceptsInput()) return;
        if (pane.session.selectionActive()) pane.session.clearSelection();
        pane.session.scrollToBottom();
        terminal_runtime.sendCommittedText(pane, fx, text);
    }

    // -------------------------------------------------------- clipboard

    /// update.zig's copySelection for a local pane. The effects wrapper the
    /// graph hands in supplies the result constructor; the answer lands in
    /// `onClipboardWritten`.
    fn copySelection(self: *Engine, fx: anytype, pane: *model_module.Pane) void {
        interaction.copy(self.model, fx, pane.id);
    }

    /// The clipboard write's answer (update.zig's .clipboard arm): a
    /// successful copy keeps the range highlighted and ends keyboard
    /// selection; a failed one says so on the pane.
    pub fn onClipboardWritten(self: *Engine, ok: bool) void {
        interaction.copied(self.model, ok);
    }

    fn requestPaste(self: *Engine, fx: anytype, pane: *model_module.Pane) void {
        interaction.requestPaste(self.model, fx, pane.id);
    }

    /// The clipboard read's answer (update.zig's .paste_clipboard arm): into
    /// the needle if that is where it was aimed, else a bracketed paste into
    /// the pane it was requested for, never a different one.
    pub fn onClipboardRead(self: *Engine, fx: anytype, ok: bool, text: []const u8) void {
        interaction.pasted(self.model, fx, ok, text);
    }

    // ---------------------------------------------------------- pointer

    const shipping_pointer = @import("shipping_pointer.zig");

    /// Route one raw surface pointer event into the pane under it, the way
    /// CockpitHost routes the widget-routed one: a new down supersedes this
    /// pointer's old capture, a move/up/cancel follows its capture wherever
    /// the pointer went, a hover or wheel goes to the pane under the point.
    /// Returns whether a terminal took it; chrome is never under a pane's
    /// frame, and the caller keeps overlays out.
    pub fn onPointer(self: *Engine, fx: anytype, raw: platform.GpuSurfaceInputEvent) PointerOutcome {
        if (!self.pointerInputEnabled()) return .ignored;
        const window_index = windowIndexForCanvas(raw.label) orelse return .ignored;
        if (!self.matchesNativeWindow(window_index, raw.window_id)) return .ignored;
        const model = self.model;
        const phase = shipping_pointer.phase(raw) orelse return .ignored;
        if (phase == .down) {
            self.supersedeSelection();
            self.cancelSplitPointer(raw);
            shipping_pointer.cancelLocal(model, fx, raw);
            self.remote_pointer.cancelPointer(model, raw);
        }
        if (!self.pointerInWorkspace(raw, window_index)) return .ignored;
        model.active_window = window_index;
        defer self.syncRemoteFocus();
        const point = geometry.PointF.init(raw.x, raw.y);
        if (self.routeSplitDrag(raw, point)) |changed| {
            return if (changed) .geometry_changed else .consumed;
        }
        return self.routeTerminalPointer(fx, raw, phase, point);
    }

    fn pointerInWorkspace(self: *const Engine, raw: platform.GpuSurfaceInputEvent, window_index: usize) bool {
        const workspace = self.model.wsAtConst(window_index).?;
        const content = projection.workspaceChromeIn(self.model, workspace, workspace.surface_size).content;
        if (content.containsPoint(.init(raw.x, raw.y))) return true;
        if (self.split_drag) |drag| {
            if (drag.window_id == raw.window_id and drag.pointer_id == raw.pointer_id) return true;
        }
        return shipping_pointer.localCaptured(self.model, raw) or self.remote_pointer.hasCapture(raw);
    }

    fn cancelSplitPointer(self: *Engine, raw: platform.GpuSurfaceInputEvent) void {
        const drag = self.split_drag orelse return;
        if (drag.window_id == raw.window_id and drag.pointer_id == raw.pointer_id) self.cancelSplitDrag();
    }

    fn routeTerminalPointer(self: *Engine, fx: anytype, raw: platform.GpuSurfaceInputEvent, phase: canvas.WidgetPointerPhase, point: geometry.PointF) PointerOutcome {
        const model = self.model;
        if (phase == .down and !self.remote_pointer.continuesClick(model, raw)) self.last_click_count = 0;
        const clicks = self.clickCount(phase, point, raw.timestamp_ns);
        if (shipping_pointer.localCaptured(model, raw)) {
            return if (shipping_pointer.dispatchLocal(model, fx, raw, clicks)) .consumed else .ignored;
        }
        if (self.remote_pointer.route(model, raw, clicks)) |consumed| {
            return if (consumed) .consumed else .ignored;
        }
        return if (shipping_pointer.dispatchLocal(model, fx, raw, clicks)) .consumed else .ignored;
    }

    /// Divider identity and pointer capture stay native. The interaction tree
    /// and painter both consume the same branch fractions, while a drag never
    /// exports a layout.NodeId or platform window id through the TS seam.
    fn routeSplitDrag(self: *Engine, raw: platform.GpuSurfaceInputEvent, point: geometry.PointF) ?bool {
        if (self.split_drag) |drag| {
            if (drag.window_id != raw.window_id or drag.pointer_id != raw.pointer_id) return null;
            return switch (raw.kind) {
                .pointer_drag, .pointer_move => self.moveSplitDrag(drag, point),
                .pointer_up, .pointer_cancel => self.finishSplitDrag(drag, raw.kind == .pointer_up),
                else => return false,
            };
        }
        if (raw.kind != .pointer_down) return null;
        return self.beginSplitDrag(raw, point);
    }

    fn splitDragTree(self: *Engine, drag: SplitDrag) ?*layout.Tree {
        const workspace = self.model.wsAt(drag.window_index) orelse return null;
        if (self.model.window_epochs[drag.window_index] != drag.window_epoch) return null;
        const id = drag.shared_id orelse return localSplitDragTree(workspace, drag);
        if (self.projectionRevision(drag.authority) != drag.shared_revision) return null;
        for (workspace.shared_ids[0..workspace.tab_count], 0..) |candidate, index| {
            const known = candidate orelse continue;
            if (!std.mem.eql(u8, &known, &id)) continue;
            // Two coordinators may name a window alike: the drag's tab is
            // the one its own coordinator projected. beginSplitDrag always
            // records it; a divider implies leaves.
            if (drag.authority) |authority| if (shared_workspace.tabAuthority(&workspace.tabs[index]) != authority) continue;
            return &workspace.tabs[index];
        }
        return null;
    }

    /// The revision of the projection a tab of `authority` came from: a
    /// peer's own, else the active coordinator's.
    fn projectionRevision(self: *Engine, authority: ?support.ProviderId) u64 {
        const id = authority orelse return self.model.shared_workspace.revision;
        const state = self.model.sharedWorkspaceFor(id) orelse return self.model.shared_workspace.revision;
        return state.revision;
    }

    fn localSplitDragTree(workspace: *model_module.Workspace, drag: SplitDrag) ?*layout.Tree {
        const tree = workspace.selectedTree() orelse return null;
        if (localSplitFingerprint(tree) != drag.local_fingerprint) return null;
        return tree;
    }

    /// Ignore presentation fractions/focus, but bind the capture to all exact
    /// terminal identities and branch relationships. A recycled node or tab
    /// position is not the divider the pointer originally pressed.
    fn localSplitFingerprint(tree: *const layout.Tree) u64 {
        var hasher = std.hash.Wyhash.init(0);
        std.hash.autoHash(&hasher, tree.root);
        for (tree.nodes) |node| {
            std.hash.autoHash(&hasher, node.kind);
            std.hash.autoHash(&hasher, node.parent);
            std.hash.autoHash(&hasher, node.first);
            std.hash.autoHash(&hasher, node.second);
            std.hash.autoHash(&hasher, node.orientation);
            std.hash.autoHash(&hasher, node.terminal);
        }
        return hasher.final();
    }

    fn moveSplitDrag(self: *Engine, drag: SplitDrag, point: geometry.PointF) bool {
        const tree = self.splitDragTree(drag) orelse {
            self.split_drag = null;
            return false;
        };
        const available = switch (drag.orientation) {
            .horizontal => @max(1, drag.bounds.width - projection.split_divider_width),
            .vertical => @max(1, drag.bounds.height - projection.split_divider_width),
        };
        const offset = switch (drag.orientation) {
            .horizontal => point.x - drag.bounds.x,
            .vertical => point.y - drag.bounds.y,
        };
        const before = tree.node(drag.node).fraction;
        tree.setFraction(drag.node, offset / available);
        if (tree.node(drag.node).fraction == before) return false;
        self.sequence +%= 1;
        self.revision +%= 1;
        self.intent_refused = false;
        return true;
    }

    fn finishSplitDrag(self: *Engine, drag: SplitDrag, commit: bool) bool {
        self.split_drag = null;
        const id = drag.shared_id orelse return false;
        const tree = self.splitDragTree(drag) orelse return false;
        const ratio = tree.node(drag.node).fraction;
        tree.setFraction(drag.node, drag.original_fraction);
        if (!commit or ratio == drag.original_fraction) return ratio != drag.original_fraction;
        const path = @import("../shared_workspace.zig").splitPath(tree, drag.node) catch return false;
        // Another coordinator's divider resizes on that coordinator; its
        // projection moves the divider when it confirms. A refusal leaves it
        // snapped back and names the peer's workspace, never the active one.
        if (self.model.foreignTree(tree)) {
            const owner = shared_workspace.tabAuthority(tree) orelse return true;
            self.peer_edits.resize(self.model, owner, id, path.bits, path.len, ratio) catch {};
            return true;
        }
        self.model.shared_mutations.requestResizeNative(self.model, id, path.bits, path.len, ratio) catch {
            self.model.shared_workspace.refused = true;
        };
        return true;
    }

    fn cancelSplitDrag(self: *Engine) void {
        const drag = self.split_drag orelse return;
        _ = self.finishSplitDrag(drag, false);
    }

    fn beginSplitDrag(self: *Engine, raw: platform.GpuSurfaceInputEvent, point: geometry.PointF) ?bool {
        const window_index = windowIndexForCanvas(raw.label) orelse return null;
        const workspace = self.model.wsAt(window_index) orelse return null;
        const tree = workspace.selectedTree() orelse return null;
        const chrome = projection.workspaceChromeIn(self.model, workspace, workspace.surface_size);
        var dividers: [layout.max_panes - 1]layout.Divider = undefined;
        const count = tree.dividers(
            chrome.content,
            projection.split_divider_width,
            projection.split_pane_min_width,
            projection.split_pane_min_height,
            &dividers,
        );
        for (dividers[0..count]) |divider| {
            if (point.x < divider.rect.x or point.x > divider.rect.x + divider.rect.width or
                point.y < divider.rect.y or point.y > divider.rect.y + divider.rect.height) continue;
            self.model.active_window = window_index;
            const authority = shared_workspace.tabAuthority(tree);
            self.split_drag = .{
                .window_id = raw.window_id,
                .pointer_id = raw.pointer_id,
                .window_index = window_index,
                .node = divider.node,
                .orientation = divider.orientation,
                .bounds = divider.bounds,
                .shared_id = workspace.shared_ids[workspace.selected_tab],
                .shared_revision = self.projectionRevision(authority),
                .window_epoch = self.model.window_epochs[window_index],
                .local_fingerprint = localSplitFingerprint(tree),
                .original_fraction = tree.node(divider.node).fraction,
                .authority = authority,
            };
            return false;
        }
        return null;
    }

    /// Finder drops stay wholly native: quote the selected paths, resolve the
    /// pane under the drop point, and use the same bracketed-paste path as
    /// cmd+V. Paths and pane identities never enter the compiled core.
    pub fn onDrop(self: *Engine, fx: anytype, drop: platform.FileDropEvent) bool {
        if (!self.pointerInputEnabled()) return false;
        defer self.syncRemoteFocus();
        if (drop.paths.len == 0) return false;
        const model = self.model;
        const terminal = self.dropTarget(drop) orelse return false;
        var quoted: [shell_words.max_quoted_bytes]u8 = undefined;
        const text = shell_words.quotePaths(drop.paths, &quoted) orelse return false;
        if (!provider_contract.isLocal(terminal)) return shipping_pointer.pasteDrop(model, terminal, text);
        const pane = model.provider.terminal(terminal) orelse return false;
        if (!pane.acceptsInput()) return false;
        if (model.selectedTree()) |tree| _ = tree.focusTerminal(terminal);
        update_module.pasteClipboardText(model, pane, fx, text);
        return true;
    }

    fn dropTarget(self: *Engine, drop: platform.FileDropEvent) ?TerminalRef {
        const model = self.model;
        const window_index = windowIndexForCanvas(drop.view_label) orelse return null;
        if (!model.windowOpen(window_index)) return null;
        model.active_window = window_index;
        return if (drop.point) |point|
            pointer_input.terminalRefAtPoint(model, point.x, point.y)
        else
            model.focusedTerminalRef();
    }

    fn pointerInputEnabled(self: *const Engine) bool {
        return !self.input_suspended;
    }

    pub fn selectionAutoscrollActive(self: *const Engine) bool {
        if (self.input_suspended) return false;
        return pointer_input.modelHasSelectionAutoscroll(self.model) or self.remote_pointer.autoscrollActive(self.model);
    }

    pub fn selectionAutoscroll(self: *Engine, fx: anytype) void {
        if (self.input_suspended) return;
        pointer_input.handleSelectionAutoscroll(self.model, fx);
        self.remote_pointer.autoscroll(self.model);
    }

    const double_click_window_ns: u64 = 400 * std.time.ns_per_ms;
    const double_click_radius: f32 = 4;

    fn clickCount(self: *Engine, phase: canvas.WidgetPointerPhase, point: geometry.PointF, now_ns: u64) u8 {
        if (phase != .down) return @max(1, self.last_click_count);
        const near = @abs(point.x - self.last_down_point.x) <= double_click_radius and
            @abs(point.y - self.last_down_point.y) <= double_click_radius;
        const soon = now_ns >= self.last_down_ns and now_ns - self.last_down_ns <= double_click_window_ns;
        self.last_click_count = if (near and soon and self.last_click_count < 3) self.last_click_count + 1 else 1;
        self.last_down_ns = now_ns;
        self.last_down_point = point;
        return self.last_click_count;
    }

    // ------------------------------------------------------------ focus

    /// update.zig's .focus_changed arm: blur strands every held key and
    /// pointer capture, and a bell that rings while unfocused notifies.
    pub fn setFocused(self: *Engine, fx: anytype, focused: bool) void {
        const model = self.model;
        if (model.focused == focused) return;
        model.focused = focused;
        if (!focused) {
            self.cancelSplitDrag();
            self.last_click_count = 0;
            self.remote_pointer.cancelAll(model);
            self.remote_natural_keys_held = 0;
            pointer_input.endAllCaptures(model, fx);
            for (&model.held_terminal_keys) |*held| held.* = .{};
        }
        self.syncRemoteFocus();
    }

    fn syncRemoteFocus(self: *Engine) void {
        self.creation.observeFocus(self.model);
        const next = self.currentRemoteFocusOwner();
        if (support.optOwnerEql(self.remote_focus_owner, next)) return;
        // Focus can move between coordinators: each side hears it on its own
        // connection.
        if (self.remote_focus_owner) |previous| {
            if (self.model.phuxForOwner(previous)) |remote| remote.sendFocus(previous, false) catch {};
        }
        self.remote_focus_owner = next;
        if (next) |owner| {
            if (self.model.phuxForOwner(owner)) |remote| remote.sendFocus(owner, true) catch {};
        }
    }

    fn currentRemoteFocusOwner(self: *Engine) ?support.ReplicaOwner {
        const ref = if (self.input_suspended) null else update_module.remoteFocusTarget(self.model);
        if (comptime support.phux_enabled) {
            if (ref) |value| if (self.model.phuxForRef(value)) |remote| remote.acknowledgeBell(value);
        }
        return if (ref) |value| self.model.terminalOwner(value) else null;
    }

    pub fn setInputSuspended(self: *Engine, fx: anytype, suspended: bool) void {
        if (self.input_suspended == suspended) return;
        self.input_suspended = suspended;
        if (suspended) {
            // Suspending input chooses nothing to show: a front record still
            // waiting for its host's list survives it (ADR-0110).
            self.cancelPendingSelection();
            self.remote_pointer.cancelAll(self.model);
            self.cancelSplitDrag();
            self.last_click_count = 0;
            self.remote_natural_keys_held = 0;
            for (&self.model.held_terminal_keys) |*held| held.* = .{};
            pointer_input.endAllCaptures(self.model, fx);
        }
        self.syncRemoteFocus();
    }

    /// update.zig's notifyBackgroundBell: the rising edge of a bell while
    /// the app is in the background reaches the person who is not looking.
    fn notifyBackgroundBell(self: *Engine, fx: anytype, pane: *const model_module.Pane, rang_before: bool) void {
        const model = self.model;
        if (model.focused) return;
        if (rang_before or !pane.bellRung()) return;
        var title_storage: [projection.max_terminal_title_bytes]u8 = undefined;
        const title = projection.terminalTitleInto(model, pane.id, &title_storage);
        if (!model.recordNotification(title)) return;
        fx.showNotification(.{ .title = title, .subtitle = "Phux Cockpit", .body = "Terminal bell" });
    }

    fn focusedPane(self: *Engine) ?*model_module.Pane {
        const terminal_ref = self.model.focusedTerminalRef() orelse return null;
        return self.model.provider.terminal(terminal_ref);
    }

    // ------------------------------------------------------------- frames

    /// The resize pump: converge every pane of the main window on the grid
    /// the painter measured, and tell each child. The same derivation the
    /// shipping app's frame pump uses (`proposedViewportsIn`), so the painter,
    /// the hit tests and the pty never disagree about a pane's cells.
    pub fn pumpViewports(self: *Engine, fx: anytype, frame: native_sdk.platform.GpuFrame) void {
        if (frame.size.width <= 0 or frame.size.height <= 0) return;
        const model = self.model;
        const index = windowIndexForCanvas(frame.label) orelse return;
        const workspace = model.wsAt(index) orelse return;
        if (workspace.window_id != 0 and workspace.window_id != frame.window_id) return;
        workspace.surface_size = frame.size;
        workspace.surface_measured = true;
        workspace.window_id = frame.window_id;
        if (frame.scale_factor > 0) workspace.surface_scale_factor = frame.scale_factor;
        const proposals = projection.proposedViewportsIn(model, workspace, frame.size);
        for (proposals.slice()) |proposal| {
            self.resizeProjection(fx, proposal);
        }
        // A front record that waited for a measured window shows now, at
        // that window's real size (ADR-0110).
        _ = self.commitProviderChange(peer_restore.onFrame(self, fx));
    }

    fn resizeProjection(self: *Engine, fx: anytype, proposal: projection.PaneViewport) void {
        const viewport: @import("provider_contract").Viewport = .{ .cols = proposal.cols, .rows = proposal.rows };
        const owner = proposal.owner orelse {
            interaction.resize(self.model, fx, proposal.terminal, viewport);
            return;
        };
        const remote = self.model.phuxForOwner(owner) orelse return;
        if (!self.model.ownerIsCurrent(owner)) return;
        if (remote.lastViewport(proposal.terminal)) |last| if (last.eql(viewport)) return;
        remote.viewportResize(proposal.terminal, viewport) catch {};
    }

    /// Paint the main window's grids beneath the markup chrome: the shipping
    /// painter, unchanged, on this engine's model. Markup owns the strip or
    /// rail above; the grids take the rest, sized by the same geometry.
    pub fn paint(self: *const Engine, builder: *canvas.Builder, size: geometry.SizeF, tokens: canvas.DesignTokens) anyerror!void {
        return terminal_painter.paintWindowIndex(self.model, builder, 0, size, tokens, 0);
    }

    /// One window's grids, by its canvas label: the shipping painter's own
    /// per-window entry, unchanged.
    pub fn paintWindow(self: *const Engine, builder: *canvas.Builder, canvas_label: []const u8, window_id: platform.WindowId, size: geometry.SizeF, tokens: canvas.DesignTokens) anyerror!void {
        const index = windowIndexForCanvas(canvas_label) orelse return;
        return terminal_painter.paintWindowIndex(self.model, builder, index, size, tokens, window_id);
    }

    fn setPlacement(self: *Engine, argument: u8) bool {
        const placement: model_module.TabPlacement = if (argument == 1) .side else .top;
        if (self.model.tab_placement == placement) return false;
        self.model.tab_placement = placement;
        return true;
    }

    pub fn snapshot(self: *Engine, out: []u8) ts_snapshot.Error![]const u8 {
        self.last_runs = self.currentRuns();
        const bytes = ts_snapshot.encode(self.model, self.sequence, self.revision, self.last_runs, self.config_probe, out);
        if (self.intent_refused) out[22] |= intent_refused_flag;
        return bytes;
    }

    /// The tabs the band has room for, by the shipping projection's rule. The
    /// strip uses the measured compiled markup slot, so toolbar changes cannot
    /// make the run overlap controls. The rail is rows of 32pt in the height its own furniture
    /// leaves (an 8pt padding, a 40pt header, four 8pt gaps, three 28pt
    /// rows), with a 40pt cue reserved once anything is hidden. Before the
    /// first frame the surface is unknown and every tab is in the run.
    pub fn currentRun(self: *const Engine) ts_snapshot.TabRun {
        return self.runFor(self.model.active_window);
    }

    pub fn currentRuns(self: *const Engine) ts_snapshot.WindowRuns {
        var runs: ts_snapshot.WindowRuns = [_]ts_snapshot.TabRun{.{}} ** (1 + model_module.max_secondary_windows);
        for (0..runs.len) |index| {
            if (self.model.windowOpen(index)) runs[index] = self.runFor(index);
        }
        return runs;
    }

    fn runFor(self: *const Engine, index: usize) ts_snapshot.TabRun {
        const workspace = self.model.wsAtConst(index) orelse return .{};
        const total = workspace.tab_count;
        if (total == 0) return .{};
        const size = workspace.surface_size;
        if (size.width <= 0 or size.height <= 0) return .{ .first = 0, .count = @intCast(total), .extent = 168 };
        if (self.model.tab_placement == .top) {
            return stripRun(workspace);
        }
        // Every window uses the same scrollable rail; it owns vertical overflow.
        return .{ .first = 0, .count = @intCast(total), .extent = 168 };
    }

    fn stripRun(workspace: *const model_module.Workspace) ts_snapshot.TabRun {
        const total = workspace.tab_count;
        if (total == 0) return .{};
        // app.native and cockpit-window.native use 4pt gaps and one 32pt
        // overflow cue. The old Zig chrome run also reserved an inline plus
        // button and two cues; its surrounding toolbar was different too.
        const gap = projection.chrome_band_inset;
        const step = projection.tab_min_extent + gap;
        var usable = @max(0, workspace.shipping_tab_strip_width) + gap;
        var count = @max(1, @as(usize, @intFromFloat(@floor(usable / step))));
        if (count < total) {
            usable = @max(0, usable - projection.chrome_control_extent - gap);
            count = @max(1, @as(usize, @intFromFloat(@floor(usable / step))));
        }
        count = @min(total, count);
        const selected = @min(workspace.selected_tab, total - 1);
        const first = if (selected >= count) selected - count + 1 else 0;
        const extent = @max(0, @min(projection.tab_extent, usable / @as(f32, @floatFromInt(count)) - gap));
        return .{ .first = @intCast(first), .count = @intCast(count), .extent = @intFromFloat(extent) };
    }

    /// Re-derive the run after a frame; true when it moved, in which case
    /// the caller announces so the core resyncs. Sequence advances then too:
    /// it counts announcements, and this is one.
    pub fn refreshRun(self: *Engine) bool {
        const runs = self.currentRuns();
        var moved = false;
        for (runs, self.last_runs) |run, last| {
            if (run.first != last.first or run.count != last.count or run.extent != last.extent) moved = true;
        }
        if (!moved) return false;
        self.last_runs = runs;
        self.sequence +%= 1;
        return true;
    }

    pub fn invalidation(self: *const Engine) [protocol.invalidation_len]u8 {
        return protocol.encodeInvalidation(self.sequence, self.revision);
    }
};

/// The focused pane's frame in surface points, for tests that aim raw input
/// at the grid; null before a frame has sized the surface.
pub fn pointerFrame(engine: *const Engine) ?geometry.RectF {
    const ref = engine.model.focusedTerminalRef() orelse return null;
    return pointer_input.paneFrameForTerminal(engine.model, ref);
}
