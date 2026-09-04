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
const pointer_input = @import("../pointer_input.zig");
const update_module = @import("../update.zig");
const provider_contract = @import("provider_contract");
const local = @import("../../providers/local/provider.zig");
const scene = @import("scene.zig");
const projection = @import("workspace_projection.zig");
const view = @import("view.zig");
const protocol = @import("ts_protocol.zig");
const ts_snapshot = @import("ts_snapshot.zig");
const theme_module = @import("../../config/theme.zig");
const startup = @import("../startup.zig");
const shell_words = @import("../shell_words.zig");
const session_state = @import("../session_state.zig");

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
    pub fn showNotification(_: *const NoShells, _: anytype) void {}
    pub fn writeClipboard(_: *const NoShells, _: anytype) void {}
    pub fn readClipboard(_: *const NoShells, _: anytype) void {}
    pub fn openUrl(_: *const NoShells, _: []const u8) void {}
    pub fn toggleFullscreenWindow(_: *const NoShells, _: []const u8) void {}
    pub fn minimizeWindow(_: *const NoShells, _: []const u8) void {}
};

/// Where a clipboard read is going once it lands, mirroring update.zig's
/// `paste_target`: into the focused pane as a bracketed paste, or into the
/// open search needle.
const PasteTarget = enum { pane, search_needle };

pub const PointerOutcome = enum { ignored, consumed, geometry_changed };

const SplitDrag = struct {
    window_id: platform.WindowId,
    pointer_id: u64,
    window_index: usize,
    node: layout.NodeId,
    orientation: layout.Orientation,
    bounds: geometry.RectF,
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
    /// The pane a clipboard read was requested for, resolved when the result
    /// lands; the model's own paste flags carry the rest.
    paste_ref: ?TerminalRef = null,
    paste_target: PasteTarget = .pane,
    /// Click coalescing for raw surface input, which carries no click count
    /// of its own: a down within the double-click window and radius of the
    /// last one counts up, the way the routed widget path counts for the
    /// Zig chrome.
    last_down_ns: u64 = 0,
    last_down_point: geometry.PointF = .{},
    last_click_count: u8 = 0,
    split_drag: ?SplitDrag = null,

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
        var initialized = try startup.initializeModel(gpa, init);
        errdefer model_module.deinitModel(&initialized.model);
        const model = try std.heap.page_allocator.create(Model);
        errdefer std.heap.page_allocator.destroy(model);
        model.* = initialized.model;
        if (initialized.provenance == .restored) {
            model_module.applyRestoredWorkingDirectories(model, &initialized.restored_snapshot);
        }
        const engine = try std.heap.page_allocator.create(Engine);
        engine.* = .{ .model = model };
        return engine;
    }

    pub fn destroy(self: *Engine) void {
        model_module.deinitModel(self.model);
        std.heap.page_allocator.destroy(self.model);
        std.heap.page_allocator.destroy(self);
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
        self.sequence +%= 1;
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
        const changed = switch (intent.kind) {
            .select_tab => self.model.selectTab(intent.argument),
            .new_terminal => self.newTerminal(),
            .close_tab => self.closeTab(intent.argument, fx),
            .set_tab_placement => self.setPlacement(intent.argument),
            .set_theme => self.setTheme(intent.argument),
            .reveal_config => self.revealConfig(fx),
            .probe_config => self.probeConfig(),
            .new_window => self.newWindow(),
            .close_window => self.closeWindow(intent.window, fx),
            .focus_window => self.focusWindow(intent.window),
            .native_command => self.nativeCommand(intent.argument, fx),
        };
        if (!changed) return self.refuse();
        self.intent_refused = false;
        self.revision +%= 1;
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

    /// The last tab is never closed here: in the shipping app that closes
    /// the window, and window lifecycle is not this seam's to decide.
    fn closeTab(self: *Engine, index: u8, fx: anytype) bool {
        const model = self.model;
        const workspace = model.wsConst();
        if (workspace.tab_count <= 1) return false;
        const tree = workspace.treeConst(index) orelse return false;
        var refs: [layout.max_panes]TerminalRef = undefined;
        const count = tree.terminals(&refs);
        model.dropTab(index);
        for (refs[0..count]) |id| {
            if (model.provider.terminal(id)) |pane| fx.ptyKill(pane.pty_key);
            _ = model.provider.destroyTerminal(id);
        }
        return true;
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
    fn probeConfig(self: *Engine) bool {
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

    /// update.zig's closeWholeWindow for a secondary window: every tab's
    /// shells are killed and its panes destroyed, then the slot retires. The
    /// main window is not this seam's to close; that is the app's quit.
    fn closeWindow(self: *Engine, index: u8, fx: anytype) bool {
        const model = self.model;
        if (index == 0 or !model.windowOpen(index)) return false;
        while (model.wsAt(index)) |workspace| {
            if (workspace.tab_count == 0) break;
            const tree = workspace.treeConst(0) orelse break;
            var refs: [layout.max_panes]TerminalRef = undefined;
            const count = tree.terminals(&refs);
            workspace.dropTab(0);
            for (refs[0..count]) |id| {
                if (model.provider.terminal(id)) |pane| fx.ptyKill(pane.pty_key);
                _ = model.provider.destroyTerminal(id);
            }
        }
        model.closeWindow(index);
        if (model.active_window == index) model.active_window = 0;
        return true;
    }

    fn focusWindow(self: *Engine, index: u8) bool {
        const model = self.model;
        if (!model.windowOpen(index)) return false;
        if (model.active_window == index) return false;
        model.active_window = index;
        return true;
    }

    /// Commands registered by the manifest remain native operations over the
    /// same engine model. TypeScript maps names to this compact enum, but no
    /// terminal bytes, pane nodes, or platform ids cross the seam.
    fn nativeCommand(self: *Engine, raw: u8, fx: anytype) bool {
        const command = protocol.decodeNativeCommand(raw) orelse return false;
        const model = self.model;
        switch (command) {
            .previous_tab, .next_tab => {
                const workspace = model.ws();
                if (workspace.tab_count < 2) return false;
                const delta: i32 = if (command == .next_tab) 1 else -1;
                const count: i32 = @intCast(workspace.tab_count);
                const selected: i32 = @intCast(workspace.selected_tab);
                workspace.selected_tab = @intCast(@mod(selected + delta, count));
            },
            .close_focused_pane => return self.closeFocusedPane(fx),
            .split_right => return self.splitFocusedPane(.horizontal),
            .split_down => return self.splitFocusedPane(.vertical),
            .previous_pane, .next_pane => {
                const tree = model.selectedTree() orelse return false;
                const next = tree.cycleFocus(if (command == .next_pane) 1 else -1) orelse return false;
                if (next == tree.focus) return false;
                tree.focus = next;
            },
            .move_tab_left, .move_tab_right => {
                const workspace = model.wsConst();
                const terminal = workspace.tabTerminal(workspace.selected_tab) orelse return false;
                if (!model.moveTerminal(terminal, if (command == .move_tab_right) 1 else -1)) return false;
            },
            .select_all => {
                const pane = self.focusedPane() orelse return false;
                if (!pane.session.selectAllHistory()) return false;
                pane.selecting = false;
            },
            .copy => {
                const pane = self.focusedPane() orelse return false;
                if (!pane.selecting and !pane.session.selectionActive()) return false;
                self.copySelection(fx, pane);
            },
            .paste => {
                const pane = self.focusedPane() orelse return false;
                self.requestPaste(fx, pane, .pane);
            },
            .clear => {
                const pane = self.focusedPane() orelse return false;
                pane.selecting = false;
                pane.session.clearSelection();
                terminal_runtime.feedOutput(pane, fx, "\x1b[H\x1b[2J\x1b[3J");
                pane.session.scrollToBottom();
                pane.session.refreshScreenText();
                terminal_runtime.moveResponsesToOutbound(pane, fx);
            },
            .find => {
                const pane = self.focusedPane() orelse return false;
                if (pane.selecting) {
                    pane.selecting = false;
                    pane.session.clearSelection();
                }
                pane.session.searchOpen();
            },
            .find_next, .find_previous => {
                const pane = self.focusedPane() orelse return false;
                _ = pane.session.searchStep(command == .find_next);
            },
            .font_larger => return model.stepFontSize(1),
            .font_smaller => return model.stepFontSize(-1),
            .font_reset => return model.resetFontSize(),
            .focus_left, .focus_right, .focus_up, .focus_down => {
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
                const next = tree.focusDirection(
                    chrome.content,
                    projection.split_divider_width,
                    projection.split_pane_min_width,
                    projection.split_pane_min_height,
                    direction,
                ) orelse return false;
                if (next == tree.focus) return false;
                tree.focus = next;
            },
            .fullscreen => fx.toggleFullscreenWindow(scene.windowLabelFor(model.active_window)),
            .minimize => fx.minimizeWindow(scene.windowLabelFor(model.active_window)),
        }
        pointer_input.endAllCaptures(model, fx);
        return true;
    }

    fn splitFocusedPane(self: *Engine, orientation: layout.Orientation) bool {
        const model = self.model;
        const tree = model.selectedTree() orelse return false;
        const target = tree.focus;
        if (target == layout.none or tree.node(target).kind != .leaf) return false;
        const origin = tree.focusedTerminal();
        const pane = model.provider.createTerminal() catch {
            model.terminal_limit_refused = true;
            return false;
        };
        if (model.config.inherit_working_directory) {
            if (origin) |source_ref| if (model.provider.terminalConst(source_ref)) |source| {
                const cwd = source.pwd();
                if (cwd.len > 0) if (model.provider.slotIndex(pane.id)) |slot| {
                    pane.argv = local.paneArgvIn(cwd, &model.cwd_argv[slot]);
                };
            };
        }
        _ = tree.split(target, orientation, pane.id) catch {
            _ = model.provider.destroyTerminal(pane.id);
            return false;
        };
        model.terminal_limit_refused = false;
        return true;
    }

    fn closeFocusedPane(self: *Engine, fx: anytype) bool {
        const model = self.model;
        const workspace = model.ws();
        const tree = workspace.selectedTree() orelse return false;
        const terminal = tree.focusedTerminal() orelse return false;
        const pane = model.provider.terminal(terminal) orelse return false;
        const pty_key = pane.pty_key;
        const had_live_pty = pane.phase == .starting or pane.phase == .live;
        _ = tree.closeTerminal(terminal) orelse return false;
        _ = model.provider.destroyTerminal(terminal);
        if (had_live_pty) fx.ptyKill(pty_key);
        if (tree.isEmpty()) workspace.dropTab(workspace.selected_tab);
        model.terminal_limit_refused = false;
        workspace.tab_limit_refused = false;
        pointer_input.endAllCaptures(model, fx);
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

    /// A platform event may adopt a window before the core handles it. Native
    /// commands already advance the ordered seam inside that same dispatch;
    /// core-only commands (the switcher) do not, so finish the adoption here
    /// exactly once after the inner app has run.
    pub fn commitWindowAdoption(self: *Engine, before_window: usize, before_sequence: u64) bool {
        if (self.model.active_window == before_window or self.sequence != before_sequence) return false;
        self.sequence +%= 1;
        self.revision +%= 1;
        self.intent_refused = false;
        return true;
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

    /// One pty event, applied the way `update.zig`'s `.shell` arm applies it
    /// minus the bell, pointer-protocol and selection bookkeeping that belong
    /// to chrome this graph does not paint natively yet. `event.bytes` is
    /// drain scratch: the emulator is fed inside this call.
    pub fn onShellEvent(self: *Engine, fx: anytype, event: native_sdk.EffectPtyEvent) void {
        const pane = terminal_runtime.paneForKey(self.model, event.key) orelse return;
        switch (event.kind) {
            .output => {
                pane.phase = .live;
                pane.output_batches += 1;
                pane.output_bytes += event.bytes.len;
                const protocol_before = pane.mouse_protocol_fingerprint;
                // Read BEFORE the feed: the notification fires on the latch's
                // rising edge, and after the feed a fresh bell and a standing
                // one look the same.
                const bell_before = pane.bellRung();
                terminal_runtime.feedOutput(pane, fx, event.bytes);
                pointer_input.syncMouseProtocol(pane);
                if (protocol_before != 0 and protocol_before != pane.mouse_protocol_fingerprint) {
                    pointer_input.endMismatchedMouseCaptures(self.model, fx, pane);
                }
                pane.session.refreshScreenText();
                self.notifyBackgroundBell(fx, pane, bell_before);
                if (pane.selecting and !pane.session.rebaseSelection()) pane.selecting = false;
                terminal_runtime.flushOutbound(pane, fx);
                terminal_runtime.moveResponsesToOutbound(pane, fx);
            },
            .exit => {
                pane.phase = if (event.reason == .rejected or event.reason == .spawn_failed) .failed else .ended;
                pane.exit_code = event.code;
                pane.exit_signal = event.signal;
                pane.exit_reason = event.reason;
            },
            // Write acknowledgements never reach a pty event constructor;
            // the shipping app marks the arm unreachable for the same reason.
            .write => {},
        }
    }

    // ------------------------------------------------------------- input

    /// A key that no chrome widget claimed, the way update.zig's handleKey
    /// treats the terminal block: the search field first, then the app's own
    /// chords (find, select, copy, paste, select all), and everything else to
    /// the focused pane's emulator encoder, which alone knows the live modes
    /// the bytes depend on. Releases only ever reach the encoder.
    pub fn onKey(self: *Engine, fx: anytype, event: canvas.WidgetKeyboardEvent) void {
        const pane = self.focusedPane() orelse return;
        if (event.phase == .key_up) {
            terminal_runtime.encodeKeyEvent(pane, fx, event, .release);
            return;
        }
        const mods = event.modifiers;
        const primary = mods.hasCommandModifier();
        if (pane.session.search.open) {
            self.searchKey(fx, pane, event);
            return;
        }
        if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "f")) {
            if (pane.selecting) {
                pane.selecting = false;
                pane.session.clearSelection();
            }
            pane.session.searchOpen();
            return;
        }
        if (primary and !mods.alt and !mods.control and keyIs(event.key, "g")) {
            _ = pane.session.searchStep(!mods.shift);
            return;
        }
        if (primary and mods.shift and keyIs(event.key, "space")) {
            if (pane.selecting) {
                pane.selecting = false;
                pane.session.clearSelection();
            } else {
                pane.selecting = true;
                pane.session.beginSelection(false);
            }
            return;
        }
        if (primary and keyIs(event.key, "c") and (pane.selecting or pane.session.selectionActive())) {
            self.copySelection(fx, pane);
            return;
        }
        if (primary and keyIs(event.key, "v")) {
            self.requestPaste(fx, pane, .pane);
            return;
        }
        if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "a")) {
            if (pane.session.selectAllHistory()) pane.selecting = false;
            return;
        }
        terminal_runtime.encodeKeyEvent(pane, fx, event, .press);
    }

    /// update.zig's handleSearchKey: the field owns Escape, Enter and
    /// Backspace; paste goes into the needle; copy still copies.
    fn searchKey(self: *Engine, fx: anytype, pane: *model_module.Pane, event: canvas.WidgetKeyboardEvent) void {
        const primary = event.modifiers.hasCommandModifier();
        if (primary and keyIs(event.key, "v")) {
            self.requestPaste(fx, pane, .search_needle);
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
        const pane = self.focusedPane() orelse return;
        if (event.text.len == 0) return;
        if (pane.session.search.open) {
            _ = pane.session.searchInput(event.text);
            return;
        }
        if (pane.selecting or !pane.acceptsInput()) return;
        if (pane.session.selectionActive()) pane.session.clearSelection();
        pane.session.scrollToBottom();
        terminal_runtime.sendCommittedText(pane, fx, event.text);
    }

    // -------------------------------------------------------- clipboard

    /// update.zig's copySelection for a local pane. The effects wrapper the
    /// graph hands in supplies the result constructor; the answer lands in
    /// `onClipboardWritten`.
    fn copySelection(self: *Engine, fx: anytype, pane: *model_module.Pane) void {
        const model = self.model;
        if (model.copy_inflight) return;
        pane.copy_failed = false;
        const text = (pane.session.selectionText(pane.session.gpa) catch {
            pane.copy_failed = true;
            pane.copied_bytes = 0;
            return;
        }) orelse {
            if (pane.session.selectionActive()) {
                pane.copy_failed = true;
                pane.copied_bytes = 0;
            }
            return;
        };
        defer pane.session.gpa.free(text);
        pane.copied_bytes = text.len;
        model.copy_inflight = true;
        fx.writeClipboard(.{ .key = local.clipboard_key, .text = text });
    }

    /// The clipboard write's answer (update.zig's .clipboard arm): a
    /// successful copy keeps the range highlighted and ends keyboard
    /// selection; a failed one says so on the pane.
    pub fn onClipboardWritten(self: *Engine, ok: bool) void {
        const model = self.model;
        if (!model.copy_inflight) return;
        model.copy_inflight = false;
        const pane = self.focusedPane() orelse return;
        if (ok) {
            pane.selecting = false;
        } else {
            pane.copied_bytes = 0;
            pane.copy_failed = true;
        }
    }

    fn requestPaste(self: *Engine, fx: anytype, pane: *model_module.Pane, target: PasteTarget) void {
        const model = self.model;
        if (model.paste_inflight) return;
        if (target == .pane and !pane.acceptsInput()) {
            model.paste_failed = true;
            return;
        }
        model.paste_inflight = true;
        self.paste_ref = pane.id;
        self.paste_target = target;
        fx.readClipboard(.{ .key = local.paste_clipboard_key });
    }

    /// The clipboard read's answer (update.zig's .paste_clipboard arm): into
    /// the needle if that is where it was aimed, else a bracketed paste into
    /// the pane it was requested for, never a different one.
    pub fn onClipboardRead(self: *Engine, fx: anytype, ok: bool, text: []const u8) void {
        const model = self.model;
        if (!model.paste_inflight) return;
        model.paste_inflight = false;
        const ref = self.paste_ref orelse return;
        self.paste_ref = null;
        if (!ok) {
            model.paste_failed = true;
            return;
        }
        const pane = model.provider.terminal(ref) orelse return;
        if (self.paste_target == .search_needle) {
            model.paste_failed = !pane.session.searchPaste(text);
            return;
        }
        if (!pane.acceptsInput()) {
            model.paste_failed = true;
            return;
        }
        model.paste_failed = false;
        update_module.pasteClipboardText(model, pane, fx, text);
    }

    // ---------------------------------------------------------- pointer

    /// Route one raw surface pointer event into the pane under it, the way
    /// CockpitHost routes the widget-routed one: a new down supersedes this
    /// pointer's old capture, a move/up/cancel follows its capture wherever
    /// the pointer went, a hover or wheel goes to the pane under the point.
    /// Returns whether a terminal took it; chrome is never under a pane's
    /// frame, and the caller keeps overlays out.
    pub fn onPointer(self: *Engine, fx: anytype, raw: platform.GpuSurfaceInputEvent) PointerOutcome {
        const model = self.model;
        const phase: canvas.WidgetPointerPhase = switch (raw.kind) {
            .pointer_down => .down,
            .pointer_up => .up,
            .pointer_cancel => .cancel,
            .pointer_move => .hover,
            .pointer_drag => .move,
            .scroll => .wheel,
            else => return .ignored,
        };
        const point = geometry.PointF.init(raw.x, raw.y);
        if (self.routeSplitDrag(raw, point)) |changed| {
            return if (changed) .geometry_changed else .consumed;
        }
        if (phase == .down) {
            if (pointer_input.pointerCaptureFor(model, raw.window_id, raw.pointer_id)) |previous| {
                pointer_input.handleTerminalPointer(model, fx, .{
                    .window_id = previous.window_id,
                    .terminal_id = previous.terminal_id,
                    .generation = previous.generation,
                    .phase = .cancel,
                    .pointer_id = previous.pointer_id,
                    .button = previous.button,
                    .point = previous.last_point,
                    .frame = previous.frame,
                    .modifiers = previous.modifiers,
                });
            }
        }
        const capture = switch (phase) {
            .move, .up, .cancel => pointer_input.pointerCaptureFor(model, raw.window_id, raw.pointer_id),
            .hover, .down, .wheel => null,
        };
        var terminal_id: support.LocalTerminalId = undefined;
        var generation: u64 = 0;
        var frame: geometry.RectF = .{};
        if (capture) |owned| {
            terminal_id = owned.terminal_id;
            generation = owned.generation;
            frame = pointer_input.paneFrameForTerminal(model, support.localRef(owned.terminal_id)) orelse owned.frame;
        } else {
            if (phase == .move or phase == .up or phase == .cancel) return .ignored;
            const ref = pointer_input.terminalRefAtPoint(model, raw.x, raw.y) orelse return .ignored;
            const pane = model.provider.terminal(ref) orelse return .ignored;
            terminal_id = provider_contract.localId(ref) orelse return .ignored;
            generation = pane.session_generation;
            frame = pointer_input.paneFrameForTerminal(model, ref) orelse return .ignored;
        }
        pointer_input.handleTerminalPointer(model, fx, .{
            .window_id = raw.window_id,
            .terminal_id = terminal_id,
            .generation = generation,
            .phase = phase,
            .pointer_id = raw.pointer_id,
            .button = raw.button,
            .click_count = self.clickCount(phase, point, raw.timestamp_ns),
            .point = point,
            .frame = frame,
            .delta = geometry.OffsetF.init(raw.delta_x, raw.delta_y),
            .modifiers = .{
                .shift = raw.modifiers.shift,
                .control = raw.modifiers.control,
                .alt = raw.modifiers.option,
                .super = raw.modifiers.command,
            },
        });
        return .consumed;
    }

    /// Divider identity and pointer capture stay native. The interaction tree
    /// and painter both consume the same branch fractions, while a drag never
    /// exports a layout.NodeId or platform window id through the TS seam.
    fn routeSplitDrag(self: *Engine, raw: platform.GpuSurfaceInputEvent, point: geometry.PointF) ?bool {
        if (self.split_drag) |drag| {
            if (drag.window_id != raw.window_id or drag.pointer_id != raw.pointer_id) return null;
            switch (raw.kind) {
                .pointer_drag, .pointer_move => {
                    const available = switch (drag.orientation) {
                        .horizontal => @max(1, drag.bounds.width - projection.split_divider_width),
                        .vertical => @max(1, drag.bounds.height - projection.split_divider_width),
                    };
                    const offset = switch (drag.orientation) {
                        .horizontal => point.x - drag.bounds.x,
                        .vertical => point.y - drag.bounds.y,
                    };
                    const workspace = self.model.wsAt(drag.window_index) orelse {
                        self.split_drag = null;
                        return false;
                    };
                    const tree = workspace.selectedTree() orelse {
                        self.split_drag = null;
                        return false;
                    };
                    const before = tree.node(drag.node).fraction;
                    tree.setFraction(drag.node, offset / available);
                    const changed = tree.node(drag.node).fraction != before;
                    if (changed) {
                        self.sequence +%= 1;
                        self.revision +%= 1;
                        self.intent_refused = false;
                    }
                    return changed;
                },
                .pointer_up, .pointer_cancel => {
                    self.split_drag = null;
                    return false;
                },
                else => return false,
            }
        }
        if (raw.kind != .pointer_down) return null;
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
            };
            return false;
        }
        return null;
    }

    /// Finder drops stay wholly native: quote the selected paths, resolve the
    /// pane under the drop point, and use the same bracketed-paste path as
    /// cmd+V. Paths and pane identities never enter the compiled core.
    pub fn onDrop(self: *Engine, fx: anytype, drop: platform.FileDropEvent) bool {
        if (drop.paths.len == 0) return false;
        const model = self.model;
        if (windowIndexForCanvas(drop.view_label)) |window_index| {
            if (model.windowOpen(window_index)) model.active_window = window_index;
        }
        const terminal = if (drop.point) |point|
            pointer_input.terminalRefAtPoint(model, point.x, point.y) orelse model.focusedTerminalRef() orelse return false
        else
            model.focusedTerminalRef() orelse return false;
        const pane = model.provider.terminal(terminal) orelse return false;
        if (!pane.acceptsInput()) return false;
        var quoted: [shell_words.max_quoted_bytes]u8 = undefined;
        const text = shell_words.quotePaths(drop.paths, &quoted) orelse return false;
        if (model.selectedTree()) |tree| _ = tree.focusTerminal(terminal);
        update_module.pasteClipboardText(model, pane, fx, text);
        return true;
    }

    pub fn selectionAutoscrollActive(self: *const Engine) bool {
        return pointer_input.modelHasSelectionAutoscroll(self.model);
    }

    pub fn selectionAutoscroll(self: *Engine, fx: anytype) void {
        pointer_input.handleSelectionAutoscroll(self.model, fx);
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
        if (!focused) pointer_input.endAllCaptures(model, fx);
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
            const pane = model.provider.terminal(proposal.terminal) orelse continue;
            if (pane.cols == proposal.cols and pane.rows == proposal.rows) continue;
            if (!pane.session.resize(proposal.cols, proposal.rows)) continue;
            pane.cols = proposal.cols;
            pane.rows = proposal.rows;
            pane.session.refreshScreenText();
            fx.ptyResize(pane.pty_key, proposal.cols, proposal.rows);
        }
    }

    /// Paint the main window's grids beneath the markup chrome: the shipping
    /// painter, unchanged, on this engine's model. Markup owns the strip or
    /// rail above; the grids take the rest, sized by the same geometry.
    pub fn paint(self: *const Engine, builder: *canvas.Builder, size: geometry.SizeF, tokens: canvas.DesignTokens) anyerror!void {
        return view.buildChrome(self.model, builder, size, tokens);
    }

    /// One window's grids, by its canvas label: the shipping painter's own
    /// per-window entry, unchanged.
    pub fn paintWindow(self: *const Engine, builder: *canvas.Builder, canvas_label: []const u8, window_id: platform.WindowId, size: geometry.SizeF, tokens: canvas.DesignTokens) anyerror!void {
        const index = windowIndexForCanvas(canvas_label) orelse return;
        return view.paintWindowIndex(self.model, builder, index, size, tokens, window_id);
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
    /// strip hands `visibleTabRun` the width the markup leaves it: the surface
    /// minus the traffic-light spacer and the row padding (app.native's own
    /// numbers). The rail is rows of 32pt in the height its own furniture
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
            const run_width = projection.tabRunWidthIn(self.model, workspace, size.width - 78 - 8);
            const window = projection.visibleTabRun(workspace, run_width);
            return .{
                .first = @intCast(window.first),
                .count = @intCast(window.count),
                .extent = @intFromFloat(@max(0, @min(65535, window.extent))),
            };
        }
        const row: f32 = 32;
        const furniture: f32 = 8 + 40 + 32 + 84;
        const usable = @max(row, size.height - furniture);
        var count: usize = @max(1, @as(usize, @intFromFloat(@floor(usable / row))));
        if (count < total) {
            const cued = @max(row, usable - 40);
            count = @max(1, @as(usize, @intFromFloat(@floor(cued / row))));
        }
        count = @min(count, total);
        const selected = @min(workspace.selected_tab, total - 1);
        const first: usize = if (selected >= count) selected - count + 1 else 0;
        return .{ .first = @intCast(first), .count = @intCast(count), .extent = 168 };
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
