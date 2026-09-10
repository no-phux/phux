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
    original_fraction: f32 = 0.5,
};

/// Snapshot flag bit reserved for the engine: the last intent was refused
/// because it named a revision the engine had already moved past (or could
/// not be decoded at all). Bits 0..6 belong to `ts_snapshot.snapshotFlags`.
pub const intent_refused_flag: u8 = 1 << 7;

pub const Engine = struct {
    model: *Model,
    sequence: u64 = 0,
    revision: u64 = 1,
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
        engine.* = .{ .model = model };
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
        engine.* = .{ .model = model };
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
    }

    pub fn destroy(self: *Engine) void {
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
        return self.drainRemoteNotices(fx) or delta.generation_changed or changed;
    }

    fn drainRemoteNotices(self: *Engine, fx: anytype) bool {
        if (comptime !support.phux_enabled) return false;
        const remote = self.model.phux() orelse return false;
        var changed = false;
        // Host admission is bounded to max_notices; every owned payload is freed.
        while (remote.takeNotice()) |notice| {
            defer remote.releaseNotice(notice);
            if (!notice.isBell()) continue;
            const owner: support.ReplicaOwner = .{ .terminal_ref = notice.terminal_ref, .generation = notice.generation };
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
            if (workspace.state != .unavailable) return self.synchronizeSharedWorkspace() or changed;
        }
        return changed;
    }

    fn pumpOperations(self: *Engine) bool {
        const model = self.model;
        const remote = model.phux() orelse return false;
        var changed = false;
        while (remote.takeOperationResult()) |result| {
            _ = self.creation.complete(model, result);
            // Subscription restoration may share the provider's deduplicated
            // attach request. Both exact owners must observe its completion.
            _ = model.shared_workspace.completeSubscription(result, remote.workspaceSnapshot().request_id);
            changed = true;
        }
        return changed;
    }

    fn synchronizeSharedWorkspace(self: *Engine) bool {
        const model = self.model;
        const remote = model.phux() orelse return false;
        self.sharedContext() catch return self.projectionRefused();
        var changed = model.shared_mutations.pump(model);
        changed = self.creation.pump(model) or changed;
        changed = (self.projectSharedWorkspace() catch return self.projectionRefused()) or changed;
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
        const endpoint = switch (remote.endpointDescriptor()) {
            .unix => |path| path,
            else => return error.UnsupportedEndpoint,
        };
        var hash = std.hash.Wyhash.init(0);
        hash.update(endpoint);
        hash.update(server);
        model.shared_workspace.setContext(hash.final());
        try model.setAttachmentContext(endpoint, server, session);
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
        const remote = self.model.phux() orelse return;
        if (remote.state() != .attached) return;
        const now = std.Io.Clock.awake.now(self.model.provider.io);
        if (self.last_workspace_refresh) |last| {
            if (last.durationTo(now).toMilliseconds() < 1000) return;
        }
        const request = remote.requestWorkspaceRefresh() catch return;
        if (request != null) self.last_workspace_refresh = now;
    }

    fn providerDisconnected(self: *Engine) void {
        self.providerDisconnectedExcept(null);
        self.session_handoff = null;
    }

    fn providerDisconnectedExcept(self: *Engine, handoff: ?u64) void {
        self.cancelSplitDrag();
        self.model.captureRemotePaint();
        if (self.model.phux()) |remote| {
            remote.stop();
            while (remote.takeOperationResult()) |result| {
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
        const state = &self.model.state;
        const fingerprint = self.model.topologyFingerprint();
        if (fingerprint == state.fingerprint) return;
        state.fingerprint = fingerprint;
        if (!state.enabled()) return;
        state.pending = true;
        state.retry_count = 0;
        self.armTopologyPersist(fx, on_fire);
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
        if (intent.expected_revision != self.revision) return self.refuse();
        // A tab intent means the window whose chrome sent it. Adopting it as
        // active first is what CockpitHost does with a routed event's window.
        // 255 means "the platform event's already-adopted focused window".
        // Markup intents carry an explicit 0..4 slot; native command mapping
        // has no window field, so the extension adopts CommandEvent.window_id
        // before the compiled core dispatches this intent.
        if (intent.window != 255) {
            if (intent.window != 0 and !self.model.windowOpen(intent.window)) return self.refuse();
            self.model.active_window = intent.window;
        }
        if (!self.applyModelIntent(intent, fx)) return self.refuse();
        self.intent_refused = false;
        self.revision +%= 1;
        return true;
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

    fn selectNavigation(self: *Engine, intent: protocol.NavigationIntent, fx: anytype) bool {
        const destination = navigation.resolve(self.model, self.revision, intent.expected_revision, intent.index) orelse return false;
        return switch (destination) {
            .placed_terminal => |placed| self.selectPlacedNavigation(placed, fx),
            .available_terminal => |ref| self.selectAvailableNavigation(ref, fx),
            .session => |id| self.selectSessionNavigation(id, fx),
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
            .available_terminal => |ref| {
                const remote = self.model.phux() orelse return null;
                if (remote.terminalSession(ref) == remote.selectedSessionId()) {
                    self.creation.requestAttachCorrelated(self.model, ref, command_id) catch return null;
                    self.model.shared_workspace.desired_terminal = null;
                    self.creation.supersedeFocusExceptCommand(command_id);
                    return .accepted_pending;
                }
                const session = remote.terminalSession(ref) orelse return null;
                return self.admitSessionCommand(command_id, session, ref, fx);
            },
            .session => |session| {
                if (self.sessionSelectionStatus(session) == .applied) {
                    self.supersedeSelection();
                    return .applied;
                }
                return self.admitSessionCommand(command_id, session, null, fx);
            },
        }
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
        const status = self.executeOperation(id, operation, fx) orelse {
            self.model.active_window = previous;
            return self.tabReceipt(id, .unavailable);
        };
        self.revision +%= 1;
        var receipt = self.tabReceipt(id, .none);
        receipt.status = status;
        return receipt;
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
        const id = model.ws().shared_ids[model.ws().selected_tab] orelse return error.StaleTarget;
        const windows = model.phux().?.workspaceSnapshot().windows;
        for (windows, 0..) |window, index| {
            if (!std.mem.eql(u8, &window.id, &id)) continue;
            if ((!right and index == 0) or (right and index + 1 == windows.len)) return error.StaleTarget;
            const target = if (right) index + 1 else index - 1;
            try model.shared_mutations.requestReorderCorrelated(model, id, @intCast(target), command_id);
            return;
        }
        return error.StaleTarget;
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

    fn supersedeSelection(self: *Engine) void {
        self.model.shared_workspace.desired_terminal = null;
        self.creation.supersedeFocus();
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
        const Fx = navigationFxType(@TypeOf(fx));
        if (comptime !@hasDecl(Fx, "restartPhux")) return false;
        const remote = self.model.phux() orelse return false;
        const previous = remote.selectedSessionId() orelse return false;
        const changed = remote.selectSession(id) catch return false;
        // Selecting the current session is an accepted idempotent action;
        // acknowledge it without restarting the connection or flashing refusal.
        if (!changed) {
            self.refreshWorkspace();
            self.supersedeSelection();
            return true;
        }
        self.model.shared_workspace.leaveSession(self.model) catch {
            _ = remote.selectSession(previous) catch {};
            return false;
        };
        const accepted = fx.restartPhux(self);
        if (accepted) self.supersedeSelection();
        return accepted;
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
        if (model.phux() != null) return self.createDurable(.tab);
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
    fn closeTab(self: *Engine, index: u8, fx: anytype) bool {
        const model = self.model;
        if (model.phux() != null) {
            if (index >= model.ws().tab_count) return false;
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
        if (model.phux() != null) return self.createDurable(.window);
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
        @import("../shared_workspace.zig").closeNativeWindow(self.model, index) catch return false;
        self.supersedeSelection();
        pointer_input.endHiddenCaptures(self.model, fx);
        if (index == 0) fx.closeWindow(scene.main_window_label);
        if (self.model.openWindowCount() == 0) fx.quitApp();
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
        const id = model.ws().shared_ids[model.ws().selected_tab] orelse return false;
        const remote = model.phux() orelse return false;
        const windows = remote.workspaceSnapshot().windows;
        for (windows, 0..) |window, index| {
            if (!std.mem.eql(u8, &window.id, &id)) continue;
            if ((!right and index == 0) or (right and index + 1 == windows.len)) return false;
            const target = if (right) index + 1 else index - 1;
            model.shared_mutations.requestReorderNative(model, id, @intCast(target)) catch return false;
            return true;
        }
        return false;
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
        if (model.phux() != null) return self.createDurable(if (orientation == .horizontal) .split_right else .split_down);
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
            self.model.shared_mutations.requestRemoveNative(self.model, ref) catch return false;
            self.supersedeSelection();
            return true;
        }
        return lifecycle.closePane(self.model, fx, ref, true);
    }

    fn createDurable(self: *Engine, kind: durable_creation.Kind) bool {
        self.supersedeSelection();
        self.creation.request(self.model, kind) catch {
            self.model.terminal_limit_refused = true;
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
                pane.phase = if (event.reason == .rejected or event.reason == .spawn_failed) .failed else .ended;
                pane.exit_code = event.code;
                pane.exit_signal = event.signal;
                pane.exit_reason = event.reason;
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
        const remote = self.model.phux() orelse return false;
        const owner = self.model.terminalOwner(ref) orelse return false;
        self.remote_natural_keys_held |= terminal_runtime.macosNaturalTextKeyMask(event.key);
        remote.sendKey(owner, &input) catch {};
        return true;
    }

    fn remoteScrollKey(self: *Engine, ref: TerminalRef, event: canvas.WidgetKeyboardEvent) bool {
        const state = self.model.remoteUi(ref) orelse return false;
        const remote = self.model.phux() orelse return false;
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
        const pane = self.focusedPane() orelse return;
        if (pane.session.search.open) {
            self.searchKey(fx, pane, event);
            return;
        }
        if (self.localShortcut(fx, pane, event)) return;
        if (pane.selecting) return self.localSelectionKey(fx, pane, event);
        interaction.rememberKey(self.model, ref, event);
        terminal_runtime.encodeKeyEvent(pane, fx, event, .press);
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
        if (!self.model.windowOpen(window_index)) return .ignored;
        self.model.active_window = window_index;
        defer self.syncRemoteFocus();
        const model = self.model;
        const phase = shipping_pointer.phase(raw) orelse return .ignored;
        if (phase == .down) {
            self.supersedeSelection();
            shipping_pointer.cancelLocal(model, fx, raw);
            self.remote_pointer.cancelPointer(model, raw);
        }
        const point = geometry.PointF.init(raw.x, raw.y);
        if (self.routeSplitDrag(raw, point)) |changed| {
            return if (changed) .geometry_changed else .consumed;
        }
        return self.routeTerminalPointer(fx, raw, phase, point);
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
        const id = drag.shared_id orelse return workspace.selectedTree();
        if (self.model.window_epochs[drag.window_index] != drag.window_epoch) return null;
        if (self.model.shared_workspace.revision != drag.shared_revision) return null;
        for (workspace.shared_ids[0..workspace.tab_count], 0..) |candidate, index| {
            const known = candidate orelse continue;
            if (std.mem.eql(u8, &known, &id)) return &workspace.tabs[index];
        }
        return null;
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
            self.split_drag = .{
                .window_id = raw.window_id,
                .pointer_id = raw.pointer_id,
                .window_index = window_index,
                .node = divider.node,
                .orientation = divider.orientation,
                .bounds = divider.bounds,
                .shared_id = workspace.shared_ids[workspace.selected_tab],
                .shared_revision = self.model.shared_workspace.revision,
                .window_epoch = self.model.window_epochs[window_index],
                .original_fraction = tree.node(divider.node).fraction,
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
        const remote = self.model.phux() orelse return;
        if (self.remote_focus_owner) |previous| remote.sendFocus(previous, false) catch {};
        self.remote_focus_owner = next;
        if (next) |owner| remote.sendFocus(owner, true) catch {};
    }

    fn currentRemoteFocusOwner(self: *Engine) ?support.ReplicaOwner {
        const ref = if (self.input_suspended) null else update_module.remoteFocusTarget(self.model);
        if (comptime support.phux_enabled) {
            if (ref) |value| if (self.model.phux()) |remote| remote.acknowledgeBell(value);
        }
        return if (ref) |value| self.model.terminalOwner(value) else null;
    }

    pub fn setInputSuspended(self: *Engine, fx: anytype, suspended: bool) void {
        if (self.input_suspended == suspended) return;
        self.input_suspended = suspended;
        if (suspended) {
            self.supersedeSelection();
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
        workspace.surface_size = frame.size;
        workspace.window_id = frame.window_id;
        if (frame.scale_factor > 0) workspace.surface_scale_factor = frame.scale_factor;
        const proposals = projection.proposedViewportsIn(model, workspace, frame.size);
        for (proposals.slice()) |proposal| {
            interaction.resize(model, fx, proposal.terminal, .{ .cols = proposal.cols, .rows = proposal.rows });
        }
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
