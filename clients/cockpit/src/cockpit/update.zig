const std = @import("std");
const native_sdk = @import("native_sdk");
const vt = @import("ghostty-vt");
const provider_contract = @import("provider_contract");
const support = @import("phux_support.zig");
const local = @import("../providers/local/provider.zig");
const grid = @import("../terminal/grid.zig");
const topology = @import("topology.zig");
const layout = @import("layout.zig");
const model_module = @import("model.zig");
const session_state = @import("session_state.zig");
const app_types = @import("app_types.zig");
const runtime = @import("terminal_runtime.zig");
const pointer_input = @import("pointer_input.zig");
const projection = @import("native/workspace_projection.zig");
const scene = @import("native/scene.zig");
const theme_module = @import("../config/theme.zig");
const shell_words = @import("shell_words.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;
const Model = model_module.Model;
const RemoteUiState = model_module.RemoteUiState;
const Msg = app_types.Msg;
const Fx = app_types.Fx;
const Pane = local.Pane;
const TerminalRef = support.TerminalRef;
const ReplicaOwner = support.ReplicaOwner;
const PhysicalKey = support.PhysicalKey;
const ModifierMask = support.ModifierMask;
const KeyInput = support.KeyInput;
const Viewport = support.Viewport;
const phux_enabled = support.phux_enabled;
const phux_channel_key = support.phux_channel_key;
const pointer_channel_key = support.pointer_channel_key;
const pointer_module = support.pointer_module;
const providerKind = support.providerKind;
const optRefEql = support.optRefEql;
const optOwnerEql = support.optOwnerEql;
const max_terminals = local.max_terminals;
const max_tabs = topology.max_tabs;
const clipboard_key = local.clipboard_key;
const paste_clipboard_key = local.paste_clipboard_key;
const max_held_terminal_keys = model_module.max_held_terminal_keys;
const replicaOwnerForPane = local.replicaOwnerForPane;
const spawnPane = runtime.spawnPane;
const paneForKey = runtime.paneForKey;
const feedOutput = runtime.feedOutput;
const flushOutbound = runtime.flushOutbound;
const moveResponsesToOutbound = runtime.moveResponsesToOutbound;
const enqueueOutbound = runtime.enqueueOutbound;
const enqueueTransient = runtime.enqueueTransient;
const encodeKeyEvent = runtime.encodeKeyEvent;
const sendCommittedText = runtime.sendCommittedText;
const keyIs = runtime.keyIs;
const drainPointerEvents = pointer_input.drainPointerEvents;
const handleTerminalPointer = pointer_input.handleTerminalPointer;
const endCapturesForTerminal = pointer_input.endCapturesForTerminal;
const endMismatchedMouseCaptures = pointer_input.endMismatchedMouseCaptures;
const endAllCaptures = pointer_input.endAllCaptures;
const clearHoverLinks = pointer_input.clearHoverLinks;
const endHiddenCaptures = pointer_input.endHiddenCaptures;
const handleSelectionAutoscroll = pointer_input.handleSelectionAutoscroll;
const terminalRefAtPoint = pointer_input.terminalRefAtPoint;
const paneFrameForTerminal = pointer_input.paneFrameForTerminal;
const validScale = pointer_input.validScale;
const syncMouseProtocol = pointer_input.syncMouseProtocol;
const cockpit_shortcuts = scene.cockpit_shortcuts;
const terminalTokens = projection.terminalTokens;
const selectedTerminalCanClose = projection.selectedTerminalCanClose;

/// Engine-owned selection primitives shared with the shipping native adapter.
/// Chrome and effect dispatch stay in their respective coordinators.
pub const remote_selection = struct {
    pub const begin = beginRemoteSelection;
    pub const clear = clearRemoteSelection;
    pub const move = moveRemoteSelection;
    pub const apply = applyRemoteSelection;
};

pub const key_owners = struct {
    pub const remember = rememberHeldTerminalKey;
    pub const take = takeHeldTerminalKeyOwner;
};

/// Spawn a pane and then hand its fresh emulator the user's terminal-level
/// settings. `spawnPane` hard-resets the emulator, so this ORDER is the whole
/// point: every path that starts a shell (boot, New, split, Restart) goes
/// through here so no terminal can end up with the default palette because
/// its entry point forgot.
fn spawnConfiguredPane(model: *Model, pane: *Pane, fx: *Fx) void {
    spawnPane(pane, fx, Fx.ptyMsg(.shell));
    model_module.applySessionConfig(&model.config, pane.session);
}

/// A new terminal or split starts in the focused pane's directory, the way
/// Ghostty does — `inherit-working-directory` turns it off.
///
/// Set BEFORE the spawn, because the argv is what the spawn carries. The
/// generated argv is quoted against hostile paths by `paneArgvIn` and falls
/// back to `$HOME` (never to no shell at all) when the directory has moved.
/// A pane whose shell never reported OSC 7 has no cwd to inherit, so the
/// plain argv stands.
fn adoptWorkingDirectory(model: *Model, origin: ?TerminalRef, pane: *Pane) void {
    if (!model.config.inherit_working_directory) return;
    const source_ref = origin orelse return;
    const source = model.provider.terminalConst(source_ref) orelse return;
    const cwd = source.pwd();
    if (cwd.len == 0) return;
    const slot = model.provider.slotIndex(pane.id) orelse return;
    pane.argv = local.paneArgvIn(cwd, &model.cwd_argv[slot]);
}

pub fn initFx(model: *Model, fx: *Fx) void {
    for (0..max_terminals) |index| {
        if (model.provider.states[index] == .active) spawnConfiguredPane(model, model.provider.slot(index), fx);
    }
    openPhuxChannel(model, fx, false);
    openPointerMonitor(model, fx);
}

fn openPhuxChannel(model: *Model, fx: *Fx, reconnect: bool) void {
    const remote = model.phux() orelse return;
    const handle = fx.openChannel(.{
        .key = phux_channel_key,
        .on_event = Fx.channelMsg(.phux_channel),
        .max_pending = 1,
    });
    if (!handle.live()) {
        model.phux_connection_unavailable = true;
        return;
    }
    if (reconnect) {
        remote.reconnect(handle) catch {
            model.phux_connection_unavailable = true;
            fx.closeChannel(phux_channel_key);
            return;
        };
    } else {
        remote.open(handle) catch {
            model.phux_connection_unavailable = true;
            fx.closeChannel(phux_channel_key);
            return;
        };
    }
}
fn openPointerMonitor(model: *Model, fx: *Fx) void {
    if (comptime !phux_enabled) return;
    const pointer_state = model.pointer_state orelse return;
    if (pointer_state.monitor != null) return;
    const handle = fx.openChannel(.{
        .key = pointer_channel_key,
        .on_event = Fx.channelMsg(.pointer_channel),
        .max_pending = 1,
    });
    if (!handle.live()) return;
    pointer_state.monitor = pointer_module.Monitor.start(
        std.heap.page_allocator,
        &pointer_state.queue,
        handle,
    ) catch {
        fx.closeChannel(pointer_channel_key);
        return;
    };
}
/// The terminal that should currently believe it holds keyboard focus, or
/// null when nothing remote should.
///
/// Remote focus is DERIVED, never announced by individual message arms. Focus
/// moves through many paths — tab shortcuts, tab cycling, a pointer press, New,
/// Close, attach/detach, leaving for the Web surface, the window itself losing
/// key — and every arm that has to remember to publish it is an arm that can
/// forget. Several already had: keyboard tab selection left a Phux terminal
/// latched focused while typing went elsewhere, and returning from Web
/// re-selected a terminal that had been told it was blurred.
pub fn remoteFocusTarget(model: *const Model) ?TerminalRef {
    if (!model.focused) return null;
    if (model.wsConst().web_selected) return null;
    const terminal_ref = model.focusedTerminalRef() orelse return null;
    if (providerKind(terminal_ref) != .phux) return null;
    return terminal_ref;
}

fn remoteFocusOwner(model: *const Model) ?ReplicaOwner {
    const terminal_ref = remoteFocusTarget(model) orelse return null;
    return model.terminalOwner(terminal_ref);
}

/// Anything that happened on a terminal you are LOOKING at is not news.
///
/// Acknowledged for every pane of the selected tab — not just the focused one
/// — because every pane of the selected tab is on screen. Skipped entirely
/// when the window does not have key: a bell that rings while the app is in
/// the background is exactly the one worth keeping.
///
/// Covers I/O loss as well as the bell. The loss COUNTERS stay cumulative (the
/// accessibility label reports them, and a diagnostic that resets is a
/// diagnostic that lies); what is acknowledged is the operator having seen the
/// current totals. Without this, loss-driven attention was permanent, and
/// permanent attention pinned the tab band open — which resizes every live PTY
/// under it.
fn acknowledgeVisibleAttention(model: *Model) void {
    if (!model.focused) return;
    const current = model.selectedTreeConst() orelse return;
    var refs: [layout.max_panes]TerminalRef = undefined;
    const count = current.terminals(&refs);
    for (refs[0..count]) |id| {
        const pane = model.provider.terminal(id) orelse continue;
        pane.clearBell();
        pane.acknowledgeLoss();
    }
}

/// Effect key for the layout snapshot write. Shares the key space with pty
/// spawns (1..) and the clipboard pair (100, 101), so it sits well clear of
/// both; the SDK checks file keys against live spawns and fetches.
pub const topology_state_file_key: u64 = 200;

/// Timer key for the save debounce. Timer keys are their own namespace, so
/// this cannot collide with the file key above or the selection autoscroll
/// timer (which is a RUNTIME timer, not an fx one).
pub const topology_persist_timer_key: u64 = 200;

/// How long the layout has to hold still before it is written.
///
/// Long enough that a divider drag — which changes the tree on every frame —
/// costs exactly one write when the mouse comes to rest, and that opening
/// three tabs in a burst costs one write rather than three. Short enough that
/// a change is on disk long before anything a user would call "later". The
/// shutdown flush covers the window between the last change and this expiry.
pub const topology_persist_debounce_ms: u64 = 750;

/// A stable topology gets three delayed retries before persistence waits for
/// another topology change or the synchronous shutdown flush.
const topology_persist_retry_limit: u8 = 3;

/// Take a snapshot and hand it to the SDK's file writer.
///
/// A LEAF function on purpose: the serialized buffer is `max_state_bytes` of
/// stack, and the SDK copies it at the call, so it must exist here and nowhere
/// up the already-deep dispatch frame.
fn writeTopologySnapshot(model: *Model, fx: *Fx) void {
    if (!model.state.enabled()) return;
    var bytes: [session_state.max_state_bytes]u8 = undefined;
    const snapshot = model.topologySnapshot() catch return;
    const encoded = session_state.serialize(&snapshot, &bytes) catch return;
    model.state.inflight = true;
    model.state.inflight_fingerprint = model.state.fingerprint;
    model.state.pending = false;
    fx.writeFile(.{
        .key = topology_state_file_key,
        .path = model.state.path(),
        .bytes = encoded,
        .on_result = Fx.fileMsg(.topology_persisted),
    });
}

/// Write the chosen theme into the user's config file.
///
/// A one-line wrapper so the arm above reads as one verb, and so the 128 KB of
/// buffers `writeConfigTheme` needs stays in a LEAF frame rather than in the
/// already-deep `updateModel` one. Synchronous rather than an effect — see
/// `Model.writeConfigTheme` for that reason, and for why it is no longer
/// silent when the write does not land.
///
/// The latch is the point. `.refused` here is a save the user just made that
/// the file did not take: the theme is applied live regardless, so without a
/// band nothing on screen distinguishes it from the save that worked, and the
/// difference only shows up as a mystery revert at the next launch. `.written`
/// clears it so one read-only afternoon does not leave a permanent complaint,
/// and `.no_destination` leaves it alone because that state is already said in
/// full by the panel's own line.
fn persistThemeChoice(model: *Model) void {
    switch (model.writeConfigTheme(model.provider.io)) {
        .written => model.config_write_refused = false,
        .refused => model.config_write_refused = true,
        .no_destination => {},
    }
}

/// Re-arm the debounce. Starting a timer key that is already active REPLACES
/// it in place, so a burst of changes leaves exactly one pending expiry.
fn armTopologyPersist(model: *Model, fx: *Fx) void {
    if (!model.state.enabled()) return;
    fx.startTimer(.{
        .key = topology_persist_timer_key,
        .interval_ms = topology_persist_debounce_ms,
        .mode = .one_shot,
        .on_fire = Fx.timerMsg(.persist_topology),
    });
}

/// Retry every pane's refused writes and retained query replies.
///
/// A refused `ptyWrite` stays in its pane's ring and is retried "on the next
/// output, resize, or frame" — but `view.onFrame` returns at most ONE message,
/// and a pane whose grid no longer matches its rect wins that slot over
/// `.flush_outbound`. So for as long as ANY pane was mid-resize — the whole of
/// a live window drag — no pane got its retry, and a child blocked on a DSR
/// answer it had already been promised stayed blocked until the drag ended.
///
/// Draining every pane from the resize arm as well closes that window: the two
/// messages that can occupy the frame's one slot now both do the retry, so the
/// pump has no state in which it stops making progress.
fn drainEveryPane(model: *Model, fx: *Fx) void {
    for (0..max_terminals) |index| {
        if (model.provider.states[index] != .active) continue;
        const pane = model.provider.slot(index);
        flushOutbound(pane, fx);
        // The drain may have freed room for query replies a full ring left
        // retained in the emulator's buffer.
        moveResponsesToOutbound(pane, fx);
    }
}

/// Keep every window's tab strip scrolled so its selected tab is on screen.
///
/// EDGE triggered off the resulting state, for the same reason topology
/// persistence is: the arms that can move a tab out of view are select, cycle,
/// close, reorder, live drag, restore, resize and the placement toggle, and any
/// one of them is an arm that could forget. Recomputing after every message
/// cannot forget, and it is a handful of integer comparisons per open window.
///
/// EVERY window, not just the active one: a resize or a close in a background
/// window changes what its strip can show, and it must not be left showing a
/// window that excludes its own selection until the user happens to click it.
fn followTabSelection(model: *Model) void {
    const padding = projection.windowPadding(model);
    for (0..model_module.max_windows) |window_index| {
        if (!model.windowOpen(window_index)) continue;
        const workspace = model.wsAt(window_index) orelse continue;
        // The same band width every other tab-strip derivation uses (see
        // `applyTabDrag` and `view.tabStrip`), so the window this records is
        // the window that will be drawn rather than a second opinion about it.
        projection.syncTabWindowIn(model, workspace, workspace.surface_size.width - padding * 2);
    }
}

fn activatePlacedDestination(
    model: *Model,
    fx: *Fx,
    placed: model_module.PlacedTerminalDestination,
) void {
    if (!model.containsTerminal(placed.terminal_ref)) return;
    const current = model.locateTerminal(placed.terminal_ref) orelse return;
    // The terminal identity survives tab reorder, but not a move to another
    // window after the row was projected.
    if (current.window != placed.window) return;
    const workspace = model.wsAt(current.window) orelse return;
    if (!workspace.selectTerminal(placed.terminal_ref)) return;
    const previous_window = model.active_window;
    model.active_window = current.window;
    endHiddenCaptures(model, fx);
    if (previous_window != current.window) fx.showWindow(scene.windowLabelFor(current.window));
}

fn activateAvailableDestination(model: *Model, fx: *Fx, terminal_ref: TerminalRef) void {
    if (providerKind(terminal_ref) != .phux or !model.containsTerminal(terminal_ref)) return;
    if (model.locateTerminal(terminal_ref)) |current| {
        activatePlacedDestination(model, fx, .{
            .window = @intCast(current.window),
            .tab = @intCast(current.tab),
            .terminal_ref = terminal_ref,
        });
        return;
    }
    if (!model.admitTab(terminal_ref)) return;
    _ = model.selectTerminal(terminal_ref);
    endHiddenCaptures(model, fx);
}

fn activateSessionDestination(model: *Model, fx: *Fx, session_id: u32) void {
    const remote = model.phux() orelse return;
    const changed = remote.selectSession(session_id) catch return;
    if (!changed) return;
    model.phux_reconnect_after_close = true;
    remote.stop();
    fx.closeChannel(phux_channel_key);
}

fn activatePaletteDestination(model: *Model, fx: *Fx, destination: app_types.PaletteDestination) void {
    switch (destination) {
        .placed_terminal => |placed| activatePlacedDestination(model, fx, placed),
        .available_terminal => |terminal_ref| activateAvailableDestination(model, fx, terminal_ref),
        .session => |session_id| activateSessionDestination(model, fx, session_id),
        // The retained spike update path never holds a peer coordinator.
        .peer_session, .peer_unavailable => {},
    }
}
/// Convert the presentational cursor to a durable payload at the moment the
/// user moves or filters the highlight. Provider publications may rebuild
/// after this point; Enter must retain this identity rather than re-resolve the
/// same numeric row against new catalog contents.
fn syncPaletteHighlight(model: *const Model, workspace: *model_module.Workspace) void {
    workspace.palette.highlighted = projection.paletteEntryAtCursorIn(model, workspace);
}

pub fn update(model: *Model, msg: Msg, fx: *Fx) void {
    const owner_before = remoteFocusOwner(model);
    updateModel(model, msg, fx);
    acknowledgeVisibleAttention(model);
    followTabSelection(model);
    // Persistence is EDGE triggered off the shape hash, not wired into the
    // dozen arms that can reshape a workspace (new tab, close, reorder,
    // select, split, divider drag, pane focus, placement toggle, a shell
    // exiting on its own, a remote reconciliation). Every one of those arms
    // is an arm that could forget; a hash cannot.
    const fingerprint = model.topologyFingerprint();
    if (fingerprint != model.state.fingerprint) {
        model.state.fingerprint = fingerprint;
        if (model.state.enabled()) {
            model.state.pending = true;
            model.state.retry_count = 0;
            armTopologyPersist(model, fx);
        }
    }
    const owner_after = remoteFocusOwner(model);
    if (!optOwnerEql(owner_before, owner_after)) {
        // Blur first, then focus: a provider that sees two focused terminals
        // for even one message would have to guess which owns the keyboard.
        if (owner_before) |owner| sendRemoteFocus(model, owner, false);
        if (owner_after) |owner| sendRemoteFocus(model, owner, true);
    }
}

fn updateModel(model: *Model, msg: Msg, fx: *Fx) void {
    switch (msg) {
        .shell => |event| {
            // A retired terminal's slot is already vacant and its pty key is
            // never reissued, so a late event resolves to nothing.
            const pane = paneForKey(model, event.key) orelse return;
            switch (event.kind) {
                .output => {
                    pane.phase = .live;
                    pane.output_batches += 1;
                    pane.output_bytes += event.bytes.len;
                    const pointer_protocol_before = pane.mouse_protocol_fingerprint;
                    // Read BEFORE the feed: the notification below fires on
                    // the latch's rising edge, and after `feedOutput` there is
                    // no way left to tell a fresh bell from a standing one.
                    const bell_before = pane.bellRung();
                    feedOutput(pane, fx, event.bytes);
                    syncMouseProtocol(pane);
                    if (pointer_protocol_before != 0 and pointer_protocol_before != pane.mouse_protocol_fingerprint) {
                        endMismatchedMouseCaptures(model, fx, pane);
                    }
                    pane.session.refreshScreenText();
                    notifyBackgroundBell(model, fx, pane, bell_before);
                    if (pane.selecting and !pane.session.rebaseSelection()) {
                        pane.selecting = false;
                    }
                    flushOutbound(pane, fx);
                    moveResponsesToOutbound(pane, fx);
                },
                .exit => {
                    pane.phase = if (event.reason == .rejected or event.reason == .spawn_failed) .failed else .ended;
                    pane.exit_code = event.code;
                    pane.exit_signal = event.signal;
                    pane.exit_reason = event.reason;
                    endCapturesForTerminal(model, fx, pane.id);
                    pane.native_delivery_failures = event.dropped_writes -| pane.write_refusals_total;
                    pane.write_refusals = 0;
                    pane.outbound_dropped += pane.outbound_len;
                    pane.outbound_dropped += pane.session.pendingResponses().len;
                    pane.outbound_head = 0;
                    pane.outbound_len = 0;
                    pane.session.clearResponses();
                    // A shell that ENDED means the pane is done, whatever its
                    // status. Exit code is the child's answer about the last
                    // command it ran, not a claim about whether this pane is
                    // still wanted — and `exit` inherits that status, so
                    // gating the close on it left an ordinary session behind a
                    // permanent `EXIT 1` husk that held its rect and never gave
                    // the space back to its sibling.
                    //
                    // Only a pane that never got a process stays (see
                    // `paneLifecycleFailed`): there is nothing to close to, and
                    // a split that silently vanished would read as a broken
                    // `cmd+D`.
                    if (!projection.paneLifecycleFailed(pane)) closePaneForTerminal(model, fx, pane.id, false);
                },
                .write => unreachable,
            }
        },
        .phux_channel => |event| {
            if (event.key != phux_channel_key) return;
            const remote = model.phux() orelse return;
            switch (event.kind) {
                .data => {
                    const delta = remote.drainReadiness() catch |err| {
                        std.log.err("Phux provider frame failed: {s} (state {t})", .{ @errorName(err), remote.state() });
                        model.phux_connection_unavailable = true;
                        remote.stop();
                        fx.closeChannel(phux_channel_key);
                        return;
                    };
                    if (delta.detached) {
                        model.phux_connection_unavailable = true;
                        remote.stop();
                        fx.closeChannel(phux_channel_key);
                        return;
                    }
                    if (delta.ready_published) model.phux_connection_unavailable = false;
                    const terminal_set_changed =
                        delta.ready_published or delta.added_count != 0 or delta.removed_count != 0;
                    if (terminal_set_changed) model.reconcileRemoteTerminals();
                    _ = model.shared_workspace.apply(model, remote.workspaceSnapshot(), remote.connectionEpoch()) catch {
                        model.shared_workspace.refused = true;
                        return;
                    };
                },
                .closed, .rejected => {
                    std.log.warn("Phux provider channel {t} (state {t})", .{ event.kind, remote.state() });
                    remote.stop();
                    if (model.phux_reconnect_after_close) {
                        model.phux_reconnect_after_close = false;
                        openPhuxChannel(model, fx, remote.state() != .new);
                    } else {
                        model.phux_connection_unavailable = true;
                    }
                },
            }
        },
        .pointer_channel => |event| {
            if (comptime !phux_enabled) return;
            if (event.key != pointer_channel_key) return;
            const pointer_state = model.pointer_state orelse return;
            switch (event.kind) {
                .data => drainPointerEvents(model),
                .closed, .rejected => {
                    if (pointer_state.monitor) |*monitor| monitor.stop();
                    pointer_state.monitor = null;
                    pointer_state.queue.reset();
                    pointer_state.capture = null;
                    openPointerMonitor(model, fx);
                },
            }
        },
        .key => |event| handleKey(model, fx, event),
        .text => |event| {
            if (!model.focused or event.text.len == 0) return;
            // The palette is checked BEFORE the terminal gate below, and
            // before `selectedTerminalRef`: it is a workspace-level mode, so
            // it must still be typeable over the web surface and over a
            // workspace whose focused terminal has gone away.
            if (model.ws().palette.open) {
                const workspace = model.ws();
                workspace.palette.append(event.text);
                syncPaletteHighlight(model, workspace);
                return;
            }
            // The settings surface has no text field, and that is precisely
            // why committed text has to be SWALLOWED here rather than fall
            // through. A panel with nothing to type into is the easiest kind
            // to leak from: without this line, every character typed while it
            // is up would reach the shell underneath it.
            if (model.ws().settings.open) return;
            if (model.selectedTerminalRef() == null) return;
            const terminal_ref = model.focusedTerminalRef() orelse return;
            if (model.provider.terminal(terminal_ref)) |pane| {
                // An open search field is where committed text GOES. This is
                // the other half of the modal gate in `handleKey`: printable
                // presses never reach that function, so the field would be
                // untypeable and the shell would receive the needle instead.
                if (pane.session.search.open) {
                    _ = pane.session.searchInput(event.text);
                    return;
                }
                if (pane.selecting or !pane.acceptsInput()) return;
                if (pane.session.selectionActive()) pane.session.clearSelection();
                pane.session.scrollToBottom();
                sendCommittedText(pane, fx, event.text);
                return;
            }
            const state = model.remoteUi(terminal_ref) orelse return;
            if (state.selecting) return;
            const remote = model.phuxForOwner(state.owner) orelse return;
            remote.scrollViewport(state.owner, .{ .kind = .bottom }) catch return;
            var input: KeyInput = .{
                .action = .press,
                .physical = @enumFromInt(0),
                .modifiers = providerModifiers(event),
                .text = event.text,
            };
            remote.sendKey(state.owner, &input) catch {};
        },
        .viewport => |size| {
            // Remember the surface the frame pump measured against, so
            // the wheel hit test has rectangles to resolve into. Addressed to
            // the window the frame came from, NOT to the active one: a
            // background window's frames must not rewrite the geometry the
            // front window's hit tests resolve against.
            if (model.wsAt(size.window)) |workspace| {
                workspace.surface_size = size.size;
                workspace.window_id = size.window_id;
                if (validScale(size.scale_factor)) workspace.surface_scale_factor = size.scale_factor;
            }
            // Converge EVERY pane of this window, not just the one the frame
            // pump named.
            //
            // `onFrame` returns at most one Msg and stops at the first pane
            // whose grid is wrong, so with N panes convergence used to take N
            // frames. During a live window drag the size moves every frame, so
            // only one pane ever tracked the window and the other N-1 rendered
            // at a stale grid for the whole gesture.
            //
            // The fix deliberately does NOT batch the panes into the Msg. Msg
            // is passed BY VALUE through every dispatch and is already 320
            // bytes; a 16-entry array of (ref, cols, rows) triples would have
            // roughly doubled that on every message in the app to save a few
            // frames of latency, which is a plausible regression against the
            // very latency this is meant to fix (see the size pin in
            // app_contract_tests). The commit path re-derives instead, through
            // the SAME `proposedViewportsIn` the frame pump used, and only
            // when a resize is actually in flight rather than every frame.
            // The Msg stays AUTHORITATIVE for the terminal it names: it says
            // what that pane's grid is, and re-deriving over the top would
            // make its own cols/rows meaningless. The convergence below is
            // purely additive — it only touches panes the Msg did not name.
            commitPaneViewport(model, fx, size.terminal_ref, size.cols, size.rows);
            const workspace = model.wsAt(size.window) orelse &model.primary;
            const proposals = projection.proposedViewportsIn(model, workspace, size.size);
            for (proposals.slice()) |proposal| {
                if (proposal.terminal.eql(size.terminal_ref)) continue;
                commitPaneViewport(model, fx, proposal.terminal, proposal.cols, proposal.rows);
            }
            drainEveryPane(model, fx);
        },
        .surface_resized => |surface| {
            const workspace = model.wsAt(surface.window) orelse return;
            workspace.surface_size = surface.size;
            workspace.window_id = surface.window_id;
            if (validScale(surface.scale_factor)) workspace.surface_scale_factor = surface.scale_factor;
        },
        .search_tick => {
            // Every pane with an open search, not just the focused one: a
            // background tab's search is still owed its results.
            for (0..model_module.max_terminals) |index| {
                if (model.provider.states[index] != .active) continue;
                const pane = model.provider.slot(index);
                _ = pane.session.searchPump(grid.Session.search_frame_slice_steps);
            }
            // Outbound is drained here for the same reason the viewport arm
            // drains it: this message can be the only one the pump returns for
            // many consecutive frames, and a search must not starve a pane's
            // pending writes.
            drainEveryPane(model, fx);
        },
        .flush_outbound => drainEveryPane(model, fx),
        .selection_autoscroll => handleSelectionAutoscroll(model, fx),
        // `on_chrome` carries no window identity, so it lands on the window
        // input is currently in. That is right for the case it exists for —
        // the user dragged the front window onto a screen with a different
        // menu bar — and the `window_chrome_changed` arm below is the
        // addressed form for everything else.
        .chrome_changed => |chrome| {
            model.ws().chrome_top = chrome.insets.top;
        },
        .window_chrome_changed => |chrome| {
            const workspace = model.wsAt(chrome.window) orelse return;
            workspace.chrome_top = chrome.top;
        },
        .focus_changed => |focused| {
            if (model.focused == focused) return;
            model.focused = focused;
            // Window blur strands every pane's held-key latches, not
            // only the focused one's.
            if (!focused) {
                endAllCaptures(model, fx);
                // The hover underline is armed by a HELD modifier, and a blur
                // is exactly how that key stops being held without this app
                // ever seeing the release. Leaving it armed underlines a link
                // in a window the pointer has left.
                clearHoverLinks(model);
                for (0..max_terminals) |index| {
                    if (model.provider.states[index] == .active) model.provider.slot(index).macos_natural_keys_held = 0;
                }
                model.consumed_shortcut_keys_held = 0;
                for (&model.held_terminal_keys) |*held| held.* = .{};
            }
        },
        // The system crossed sunset (or somebody flipped the switch). Only a
        // config that asked to follow moves; `adoptSystemTheme` is the whole
        // gate, so this arm can stay unconditional.
        //
        // Nothing is written to disk: the file says `auto`, and following is
        // what `auto` means. Writing the resolved name back would turn a
        // subscription into a choice the user never made.
        .appearance_changed => |appearance| {
            _ = model.config.adoptSystemTheme(switch (appearance.color_scheme) {
                .dark => .dark,
                .light => .light,
            });
        },
        // Pinch to size the type, the way every Mac terminal does. The gearbox
        // lives in `Model.pinchStep`; the step itself is exactly the cmd+= /
        // cmd+- path, so the reflow, the clamp at both ends of the range and
        // the PTY resize pump are all already right.
        .pinch => |gesture| {
            const steps = model.pinchStep(gesture.phase, gesture.scale);
            if (steps == 0) return;
            _ = model.stepFontSize(@floatFromInt(steps));
        },
        .files_dropped => |drop| dropPathsIntoTerminal(model, fx, drop),
        .pointer => |pointer| handleTerminalPointer(model, fx, pointer),
        .wheel_fallback => |wheel| {
            if (model.selectedTerminalRef() == null) return;
            const terminal_ref = terminalRefAtPoint(model, wheel.x, wheel.y) orelse return;
            if (model.provider.terminal(terminal_ref)) |pane| {
                handleTerminalPointer(model, fx, .{
                    .terminal_id = provider_contract.localId(pane.id) orelse return,
                    .generation = pane.session_generation,
                    .phase = .wheel,
                    .point = geometry.PointF.init(wheel.x, wheel.y),
                    .frame = paneFrameForTerminal(model, pane.id) orelse return,
                    .delta = geometry.OffsetF.init(0, wheel.delta),
                });
                return;
            }
            // A remote terminal has no local emulator to scroll: accumulate
            // against the shared cell metric and ask the provider to move its
            // own viewport.
            const state = model.remoteUi(terminal_ref) orelse return;
            if (state.selecting) return;
            // `terminalCellMetricsFor`, not `canvas.terminalCellMetrics`:
            // outside the painter these tokens carry no text-measure provider.
            // Height happens to be the estimate either way (the SDK derives it
            // as `round(font_size * 1.4)` and never measures it), but routing
            // through the same helper as the proposer keeps one answer to
            // "how tall is a row" rather than two that agree by luck.
            const cell_h = @max(1, projection.terminalCellMetricsFor(terminalTokens(model)).height);
            state.wheel_accum += wheel.delta;
            const rows = @trunc(state.wheel_accum / cell_h);
            if (rows != 0) {
                state.wheel_accum -= rows * cell_h;
                const remote = model.phuxForOwner(state.owner) orelse return;
                remote.scrollViewport(state.owner, .{
                    .kind = .delta,
                    .value = -@as(i64, @intFromFloat(rows)),
                }) catch {};
            }
        },
        .copy_selection => {
            const id = model.selectedResourceId() orelse return;
            copySelection(model, fx, id);
        },
        .copy_terminal => |id| copySelection(model, fx, id),
        .paste_terminal => |id| requestPaste(model, fx, id),
        .paste_focused => {
            const id = model.selectedResourceId() orelse return;
            requestPaste(model, fx, id);
        },
        .clipboard => |result| {
            if (!model.copy_inflight) return;
            model.copy_inflight = false;
            if (!model.ownerIsCurrent(model.copy_owner)) return;
            if (model.provider.terminal(model.copy_owner.terminal_ref)) |pane| {
                if (result.outcome == .ok) {
                    // Native terminals keep the copied range highlighted, as
                    // every macOS terminal does. Typing, a new selection, or a
                    // restart clears it; a successful copy does not.
                    pane.selecting = false;
                } else {
                    pane.copied_bytes = 0;
                    pane.copy_failed = true;
                }
                return;
            }
            const state = @import("terminal_interaction.zig").stateForOwner(model, model.copy_owner) orelse return;
            if (result.outcome == .ok) {
                retainSelectionAfterCopy(state);
            } else {
                state.copied_bytes = 0;
                state.copy_failed = true;
            }
        },
        .paste_clipboard => |result| {
            if (!model.paste_inflight) return;
            model.paste_inflight = false;
            if (!model.ownerIsCurrent(model.paste_owner)) return;
            if (result.outcome != .ok) {
                model.paste_failed = true;
                return;
            }
            if (model.provider.terminal(model.paste_owner.terminal_ref)) |pane| {
                if (model.paste_target == .search_needle) {
                    // The field may have closed between the request and the
                    // result; `searchPaste` returns false rather than writing
                    // into a needle nobody is looking at.
                    model.paste_failed = !pane.session.searchPaste(result.text);
                    return;
                }
                if (!pane.acceptsInput()) {
                    model.paste_failed = true;
                    return;
                }
                model.paste_failed = false;
                pasteClipboardText(model, pane, fx, result.text);
                return;
            }
            const remote = model.phuxForOwner(model.paste_owner) orelse return;
            remote.sendPaste(model.paste_owner, result.text, false) catch {
                model.paste_failed = true;
                return;
            };
            model.paste_failed = false;
        },
        .restart => |terminal_ref| {
            // Restart ONLY a genuinely finished session. During
            // `.starting` (spawned, no output yet) or `.live` the pty
            // still holds the key, so respawning would collide on the
            // same key — a rejected exit that strands the running
            // original with no input.
            const pane = model.provider.terminal(terminal_ref) orelse return;
            if (pane.phase != .ended and pane.phase != .failed) return;
            // Keep the clipboard key occupied until cancellation delivers;
            // the generation check above discards the stale result.
            if (model.copy_inflight and model.copy_owner.terminal_ref.eql(pane.id)) fx.cancel(clipboard_key);
            // Keep the read latched until its cancellation terminal
            // arrives, preventing key reuse while the old effect still
            // owns it. The generation fence above discards that result.
            if (model.paste_owner.terminal_ref.eql(pane.id)) {
                model.paste_failed = false;
                if (model.paste_inflight) fx.cancel(paste_clipboard_key);
            }
            endCapturesForTerminal(model, fx, pane.id);
            spawnConfiguredPane(model, pane, fx);
        },
        .focus_pane => |node| {
            const current = model.selectedTree() orelse return;
            if (node >= layout.max_nodes or current.node(node).kind != .leaf) return;
            if (current.focus == node) return;
            current.focus = node;
            endHiddenCaptures(model, fx);
        },
        .select_surface => |surface| {
            if (surface.eql(model.selectedSurface())) return;
            switch (surface) {
                .terminal => |id| {
                    if (!model.selectTerminal(id)) return;
                },
                .web => model.selectWeb(),
            }
            endHiddenCaptures(model, fx);
        },
        // cmd+N and tab cycling address TERMINAL TABS only. Web used to be
        // addressable as "one past the last tab", which meant the chord for a
        // given terminal moved every time a tab opened or closed; it has its
        // own chord (cmd+shift+B) and its own menu item now.
        .select_position => |position| {
            if (position >= model.ws().tab_count) return;
            _ = model.selectTab(position);
            endHiddenCaptures(model, fx);
        },
        .cycle_tab => |delta| {
            if (model.ws().tab_count == 0) return;
            const count: i8 = @intCast(model.ws().tab_count);
            // Cycling from the web surface re-enters the tab list at the tab
            // that was there, rather than stranding the user on a surface the
            // strip no longer shows.
            const current: i8 = @intCast(model.ws().selected_tab);
            const next: usize = @intCast(@mod(current + if (model.ws().web_selected) 0 else delta, count));
            _ = model.selectTab(next);
            endHiddenCaptures(model, fx);
        },
        .new_terminal => {
            // A new terminal is a new TAB. Admission owns the tab bound and
            // this transaction rolls the minted terminal back if it refuses.
            const origin = model.selectedTerminalRef();
            const pane = model.provider.createTerminal() catch {
                // Refused, visibly. See `view.terminalLimitNotice`.
                model.terminal_limit_refused = true;
                return;
            };
            adoptWorkingDirectory(model, origin, pane);
            if (!model.admitTab(pane.id)) {
                _ = model.provider.destroyTerminal(pane.id);
                model.ws().tab_limit_refused = true;
                return;
            }
            _ = model.selectTerminal(pane.id);
            model.terminal_limit_refused = false;
            model.ws().tab_limit_refused = false;
            endHiddenCaptures(model, fx);
            spawnConfiguredPane(model, pane, fx);
        },
        // cmd+N: a WINDOW, with a workspace of its own and one shell in it.
        //
        // The order matters. The slot is minted and made active BEFORE the
        // terminal, because `createTerminal` hands back a pane that
        // `admitTab` then files into whichever window is active — filing it
        // first and switching after would put the new window's shell in the
        // old window's tab strip.
        .new_window => {
            const index = model.freeWindowIndex() orelse {
                // Refused, visibly. See `view.windowLimitNotice`.
                model.window_limit_refused = true;
                return;
            };
            const workspace = model.openWindow(index) orelse {
                model.window_limit_refused = true;
                return;
            };
            const origin = model.selectedTerminalRef();
            const previous = model.active_window;
            model.active_window = index;
            const pane = model.provider.createTerminal() catch {
                // No shell to put in it: the window would open empty and
                // immediately close itself, so it never opens at all. The
                // notice names the reason that actually stopped it — the
                // shell ceiling, not the window one.
                model.terminal_limit_refused = true;
                model.closeWindow(index);
                model.active_window = previous;
                return;
            };
            adoptWorkingDirectory(model, origin, pane);
            if (!workspace.admitTab(pane.id)) {
                _ = model.provider.destroyTerminal(pane.id);
                model.closeWindow(index);
                model.active_window = previous;
                return;
            }
            _ = workspace.selectTerminal(pane.id);
            model.window_limit_refused = false;
            endHiddenCaptures(model, fx);
            spawnConfiguredPane(model, pane, fx);
        },
        // The host resolves a canvas label or a platform window id to an
        // index and sends this; `update` is the only thing that moves
        // `active_window`, so there is exactly one writer.
        .focus_window => |index| {
            if (!model.windowOpen(index)) return;
            if (model.active_window == index) return;
            model.active_window = index;
            endHiddenCaptures(model, fx);
        },
        // The OS closed a window out from under the model (red button, the
        // Window menu). The dismissal precedent: the window is already gone,
        // so this drains its terminals and retires the slot rather than
        // trying to close anything.
        .window_closed => |index| {
            if (!model.windowOpen(index)) return;
            closeWholeWindow(model, fx, index);
        },
        // Addressed to the FOCUSED window's declared label, not the main
        // one. `toggleFullscreenWindow` reads the OS's own answer for that
        // window and SETS the other, so a window the user put into fullscreen
        // from the green button flips back out of it here — no parity state
        // for this app to mirror and get wrong.
        .toggle_fullscreen => fx.toggleFullscreenWindow(scene.windowLabelFor(model.active_window)),
        // cmd+M. Addressed to the focused window's declared label, exactly like
        // fullscreen above — and, exactly like fullscreen, it exists because
        // declaring custom menus replaces the toolkit's stock bar, Minimize
        // included. Without it, the chord every Mac app answers is a chord
        // this one swallows.
        .minimize_window => fx.minimizeWindow(scene.windowLabelFor(model.active_window)),
        // A pick from the menu bar happens while this app is behind whatever
        // the user was actually looking at, so the selection is only half the
        // gesture: `showWindow` unhides, un-minimizes, orders front and
        // activates, which is what makes "go to that terminal" true.
        .tray_select => |index| {
            updateModel(model, .{ .select_position = index }, fx);
            fx.showWindow(scene.windowLabelFor(model.active_window));
        },
        // The derivation behind the menu is consulted after every dispatch, so
        // being dispatched IS the refresh. Nothing else to do.
        .tray_opened => {},
        .close_terminal => {
            const id = model.selectedTerminalRef() orelse return;
            // Remote close detaches only this Cockpit leaf. The provider keeps
            // execution authority and inventory, so the terminal can be
            // admitted again from the switcher.
            closePaneForTerminal(model, fx, id, providerKind(id) == .local);
        },
        .move_terminal => |delta| {
            const id = model.selectedResourceId() orelse return;
            _ = model.moveTerminal(id, delta);
        },
        .toggle_tab_placement => {
            model.tab_placement = if (model.tab_placement == .top) .side else .top;
            endHiddenCaptures(model, fx);
        },
        // Font sizing needs NO effect of its own: the design tokens derive the
        // cell box from the live size, and the onFrame pump already emits one
        // `.viewport` per changed pane, which is what resizes the emulator and
        // the pty. Every pane in every tab follows on its next frame.
        .font_size_step => |delta| _ = model.stepFontSize(@floatFromInt(delta)),
        .font_size_reset => _ = model.resetFontSize(),
        .select_all => selectAllScrollback(model),
        .clear_terminal => clearFocusedTerminal(model, fx),
        .search_open => {
            const pane = focusedLocalPane(model) orelse return;
            // Keyboard selection and the search field both want Escape and
            // both want the keyboard. Only one of them can have either, and
            // the one the user just asked for wins.
            if (pane.selecting) {
                pane.selecting = false;
                pane.session.clearSelection();
            }
            pane.session.searchOpen();
        },
        .search_close => {
            const pane = focusedLocalPane(model) orelse return;
            pane.session.searchClose();
        },
        .search_step => |delta| {
            const pane = focusedLocalPane(model) orelse return;
            _ = pane.session.searchStep(delta >= 0);
        },
        .palette_open => {
            const workspace = model.ws();
            // Idempotent, and deliberately: cmd+shift+P on an open palette
            // must not wipe a needle half typed.
            if (workspace.palette.open) return;
            // The settings surface is the OTHER modal, and the MENU BAR can
            // reach this arm while it is up — the keyboard cannot, because the
            // settings gate in `handleKey` swallows every chord. Without
            // this, Window > "Go to Terminal…" set `palette.open` underneath a
            // settings panel that the view renders INSTEAD of the palette, so
            // nothing appeared; the switcher then ambushed the user on the
            // Escape they pressed to get back to their shell.
            //
            // This mirrors `settings_open`, which already dismisses the
            // palette: the surface the user just asked for wins and the other
            // one goes away VISIBLY, rather than stacking up invisibly behind
            // it. Routed through `settings_close` rather than a bare reset
            // because leaving the panel without committing has to put the
            // previewed theme back, exactly as Escape does.
            if (workspace.settings.open) updateModel(model, .settings_close, fx);
            workspace.palette.reset();
            workspace.palette.open = true;
            syncPaletteHighlight(model, workspace);
        },
        .palette_close => model.ws().palette.reset(),
        .palette_step => |delta| {
            const workspace = model.ws();
            if (!workspace.palette.open) return;
            const count = projection.paletteEntryCountIn(model, workspace);
            if (count == 0) return;
            // Wraps, because a switcher you can walk off the end of makes the
            // last row harder to reach than the first.
            const signed: i32 = @intCast(count);
            const current: i32 = @intCast(@min(workspace.palette.cursor, count - 1));
            workspace.palette.cursor = @intCast(@mod(current + delta, signed));
            syncPaletteHighlight(model, workspace);
        },
        .palette_activate => |destination| {
            // Pointer, Enter, and accessibility all arrive here with the same
            // stable payload. Dismiss the SOURCE workspace before activation
            // can move `active_window`.
            model.ws().palette.reset();
            activatePaletteDestination(model, fx, destination);
        },
        .phux_reconnect => {
            const remote = model.phux() orelse return;
            model.phux_reconnect_after_close = false;
            openPhuxChannel(model, fx, remote.state() != .new);
        },
        .palette_input => |text| {
            const workspace = model.ws();
            if (!workspace.palette.open) return;
            workspace.palette.append(text);
            syncPaletteHighlight(model, workspace);
        },
        .palette_backspace => {
            const workspace = model.ws();
            if (!workspace.palette.open) return;
            workspace.palette.backspace();
            syncPaletteHighlight(model, workspace);
        },
        // One-way, and for this launch only. The band is the app's single
        // chance to say a setting did not apply; re-raising it on the next
        // frame would make it nagware, and re-raising it never would need
        // state on disk that a re-read config file can invalidate.
        .config_notice_dismissed => model.config_notice_dismissed = true,
        .settings_open => {
            const workspace = model.ws();
            // Idempotent, for the same reason `palette_open` is — but here the
            // cost of not being would be worse than a lost needle: reopening
            // would re-snapshot `restore_theme` from the PREVIEWED theme, and
            // Escape would then "cancel" back to the preview instead of to
            // what the user actually had.
            if (workspace.settings.open) return;
            // Two modal surfaces cannot both own the keyboard. The one the
            // user just asked for wins, and the other is dismissed rather than
            // left open behind it holding a half-typed needle.
            workspace.palette.reset();
            workspace.settings.reset();
            workspace.settings.open = true;
            workspace.settings.restore_theme = model.config.theme;
            // Open ON the theme in effect, so the first arrow key is a step
            // away from where the user is rather than a jump to row zero.
            workspace.settings.cursor = theme_module.indexOf(model.config.theme.slice()) orelse 0;
            // Ask the file, ONCE, whether it will take the write this panel is
            // about to promise. Here rather than per frame because a `view` is
            // pure and must not touch a disk, and because the answer only has
            // to be true at the moment the user reads the line.
            workspace.settings.config_writable = model.configFileWritable(model.provider.io);
            workspace.settings.config_exists = model.configFileExists(model.provider.io);
        },
        .settings_close => {
            const workspace = model.ws();
            if (!workspace.settings.open) return;
            // Put the previewed theme back. `setTheme` refuses an unknown
            // name, and the empty string — "no theme was named" — is the one
            // value it accepts that is not a theme, which is exactly the state
            // a first-time user opened the panel in.
            _ = model.config.setTheme(workspace.settings.restore_theme.slice());
            workspace.settings.reset();
        },
        .settings_step => |delta| {
            const workspace = model.ws();
            if (!workspace.settings.open) return;
            const count = theme_module.builtins.len;
            // Wraps, for the reason the palette's step wraps: a list you can
            // walk off the end of makes the last row harder to reach than the
            // first.
            const signed: i32 = @intCast(count);
            const current: i32 = @intCast(@min(workspace.settings.cursor, count - 1));
            workspace.settings.cursor = @intCast(@mod(current + delta, signed));
            // APPLY, right now. Nothing else is needed to repaint: the design
            // tokens are rebuilt from `model.config` on every frame and
            // `Session.snapshot` pushes them into the emulator's defaults on
            // every frame, so the next frame is already the new colour.
            _ = model.config.setTheme(theme_module.builtins[workspace.settings.cursor].name);
        },
        .settings_select => |index| {
            const workspace = model.ws();
            if (!workspace.settings.open) return;
            if (index >= theme_module.builtins.len) return;
            workspace.settings.cursor = index;
            _ = model.config.setTheme(theme_module.builtins[index].name);
        },
        .settings_commit => {
            const workspace = model.ws();
            if (!workspace.settings.open) return;
            _ = model.config.setTheme(theme_module.builtins[
                @min(
                    workspace.settings.cursor,
                    theme_module.builtins.len - 1,
                )
            ].name);
            workspace.settings.reset();
            persistThemeChoice(model);
        },
        .settings_reveal_config => {
            const workspace = model.ws();
            if (!workspace.settings.open or !workspace.settings.config_exists) return;
            if (!model.config_file.enabled()) return;
            fx.hostSend("native-sdk.os.revealPath", model.config_file.path());
        },
        .close_tab => |index| closeTab(model, fx, index),
        // "Close Others", from the tab's own menu. Walked from the END so a
        // tab dropping out from under the walk cannot skip its neighbour, and
        // the kept tab is named by IDENTITY rather than by index for the same
        // reason: every close renumbers everything after it.
        .close_other_tabs => |index| {
            const workspace = model.wsConst();
            const keep = workspace.tabTerminal(index) orelse return;
            var doomed: [max_tabs]TerminalRef = undefined;
            var count: usize = 0;
            var tab = workspace.tab_count;
            while (tab > 0) {
                tab -= 1;
                const id = workspace.tabTerminal(tab) orelse continue;
                if (support.refEql(id, keep)) continue;
                doomed[count] = id;
                count += 1;
            }
            for (doomed[0..count]) |id| {
                const at = model.ws().tabOfTerminal(id) orelse continue;
                closeTab(model, fx, @intCast(at));
            }
        },
        // Move THIS tab, not the selected one: a right-click acts on what is
        // under the pointer. `moveTerminal` takes the terminal rather than the
        // index for the usual reason — the index is a position and the
        // terminal is an identity.
        .move_tab => |request| {
            const id = model.wsConst().tabTerminal(request.index) orelse return;
            _ = model.moveTerminal(id, request.delta);
        },
        .tab_drag => |drag| applyTabDrag(model, drag),
        .hover_tab => |index| model.ws().hovered_tab = index,
        .unhover_tab => model.ws().hovered_tab = model_module.no_hovered_tab,
        .split_right => splitFocusedPane(model, fx, .horizontal),
        .split_down => splitFocusedPane(model, fx, .vertical),
        .split_resized => |resize| {
            const current = model.selectedTree() orelse return;
            current.setFraction(resize.node, resize.value);
        },
        .cycle_pane => |delta| {
            const current = model.selectedTree() orelse return;
            const next = current.cycleFocus(delta) orelse return;
            updateModel(model, .{ .focus_pane = next }, fx);
        },
        .focus_direction => |direction| {
            const current = model.selectedTreeConst() orelse return;
            const chrome = projection.workspaceChrome(model, model.ws().surface_size);
            const next = current.focusDirection(
                chrome.content,
                projection.split_divider_width,
                projection.split_pane_min_width,
                projection.split_pane_min_height,
                direction,
            ) orelse return;
            updateModel(model, .{ .focus_pane = next }, fx);
        },
        // The timer's `outcome` is deliberately not consulted. A rejection
        // (full timer table, no platform timer service) still arrives here
        // exactly once, and the right answer to "the debounce could not be
        // armed" is to save NOW rather than to drop the save.
        .persist_topology => {
            if (!model.state.enabled() or !model.state.pending) return;
            // A write is already on this key; the SDK would reject a second.
            // The result arm restarts the debounce, so nothing is dropped.
            if (model.state.inflight) return;
            writeTopologySnapshot(model, fx);
        },
        .shutdown => {
            // Synchronous, on this thread, through the provider's own `Io`:
            // the effect queue will not be drained again, so a `writeFile`
            // here would be posted to a worker and lost to the exit.
            model.writeWorkspaceState(model.provider.io);
        },
        .topology_persisted => |result| {
            model.state.inflight = false;
            // This result belongs to the snapshot captured when the write was
            // posted. A topology that moved since then has its own fresh retry
            // budget; an old failure cannot spend it, nor can an old success
            // clear a warning owed by newer state.
            if (model.state.inflight_fingerprint != model.state.fingerprint) {
                model.state.pending = true;
                armTopologyPersist(model, fx);
                return;
            }
            if (result.outcome == .ok) {
                model.state.retry_count = 0;
                model.state.write_failed = false;
            } else {
                model.state.pending = true;
                if (model.state.retry_count < topology_persist_retry_limit) {
                    model.state.retry_count += 1;
                    armTopologyPersist(model, fx);
                } else {
                    model.state.write_failed = true;
                }
                return;
            }
            // The shape moved while the write was out. Re-arm rather than
            // writing immediately: whatever moved it may still be moving.
            if (model.state.pending) armTopologyPersist(model, fx);
        },
        .browser_page => |page| {
            model.browser_page = page;
            model.browser_navigation_token +%= 1;
            model.selectWeb();
            endHiddenCaptures(model, fx);
        },
    }
}

/// A real split: mint a NEW terminal and divide the focused pane with it.
/// The old `toggle_split` only flipped a flag and dragged an existing tab
/// into the second slot — which is why splitting "opened up like a different
/// thing" instead of giving a second shell beside the first.
fn splitFocusedPane(model: *Model, fx: *Fx, orientation: layout.Orientation) void {
    const current = model.selectedTree() orelse return;
    const target = current.focus;
    if (target == layout.none or current.node(target).kind != .leaf) return;
    const origin = current.focusedTerminal();
    const pane = model.provider.createTerminal() catch {
        // Refused, visibly — cmd+D at the shell ceiling used to divide the
        // rect and put a permanently blank pane in the new half.
        model.terminal_limit_refused = true;
        return;
    };
    adoptWorkingDirectory(model, origin, pane);
    _ = current.split(target, orientation, pane.id) catch {
        // The tree refused (at its pane ceiling): the terminal minted for it
        // has no home, so it goes back rather than leaking a live shell.
        _ = model.provider.destroyTerminal(pane.id);
        return;
    };
    model.terminal_limit_refused = false;
    endHiddenCaptures(model, fx);
    spawnConfiguredPane(model, pane, fx);
}

/// The bell that rang while nobody was looking, said out loud.
///
/// A bell is the one thing a shell sends ON PURPOSE to get a person's
/// attention — a build finished, a prompt is waiting, an agent wants an
/// answer — and until this the app's whole response was a dot in the tab
/// strip. A dot is a fine answer for a window you are looking at. It is no
/// answer at all for a window behind your browser, which is exactly when a
/// bell is worth ringing.
///
/// So the notification is deliberately NOT the general case: it fires only
/// while the app is in the background, which is the state
/// `acknowledgeVisibleAttention` already treats as "keep this one". A banner
/// for a bell from the pane the user is typing in would be noise, and noise is
/// how notifications get turned off wholesale.
///
/// The EDGE is the caller's `rang_before`, read before the output was fed.
/// Latching on the bell flag alone would re-post on every subsequent chunk of
/// output for as long as the latch stood — one bell, a hundred banners.
///
/// Fire-and-forget: the platform owns whether a banner is drawn (Notification
/// Centre settings, focus modes, permission), so no result Msg could be
/// honest. `Model.recordNotification` is the observable half.
fn notifyBackgroundBell(model: *Model, fx: *Fx, pane: *const Pane, rang_before: bool) void {
    if (model.focused) return;
    if (rang_before or !pane.bellRung()) return;
    var title_storage: [projection.max_terminal_title_bytes]u8 = undefined;
    const title = projection.terminalTitleInto(model, pane.id, &title_storage);
    if (!model.recordNotification(title)) return;
    fx.showNotification(.{
        .title = title,
        .subtitle = scene.app_name,
        .body = "Terminal bell",
    });
}

/// A drop from Finder, delivered into the shell under the pointer.
///
/// A drop IS a paste — of paths the user chose with the mouse instead of with
/// the keyboard — so it goes through the same bracketed-paste encoder cmd+V
/// uses. That is not a shortcut: bracketed paste is what tells a shell, an
/// editor, or a TUI agent that the bytes arriving are DATA rather than typing,
/// and a drop that bypassed it would let a dropped filename with a newline in
/// it run as a command.
///
/// The pointer decides the pane, not the focus: dropping onto the split you
/// are looking at should reach that split, and dragging from Finder never gave
/// this app a chance to be focused first. The drop's own window is adopted for
/// the same reason — the paths landed on a window, so that window is the one
/// the user is addressing.
///
/// Refusals are whole and silent: an unquotable path list (see
/// `shell_words.quotePaths`) and a pane that does not accept input both end
/// here with nothing sent, because there is no half of a path list worth
/// pasting.
fn dropPathsIntoTerminal(model: *Model, fx: *Fx, drop: native_sdk.platform.FileDropEvent) void {
    if (scene.windowIndexForCanvas(drop.view_label)) |window_index| {
        if (model.windowOpen(window_index) and model.active_window != window_index) {
            model.active_window = window_index;
            endHiddenCaptures(model, fx);
        }
    }
    // A drop with no point is a drop the host could not place — the focused
    // pane is the honest answer, and it is the one a keyboard paste would
    // have used.
    const terminal_ref = blk: {
        if (drop.point) |point| {
            if (terminalRefAtPoint(model, point.x, point.y)) |hit| break :blk hit;
        }
        break :blk model.selectedTerminalRef() orelse return;
    };
    const pane = model.provider.terminal(terminal_ref) orelse return;
    if (!pane.acceptsInput()) return;
    var quoted: [shell_words.max_quoted_bytes]u8 = undefined;
    const text = shell_words.quotePaths(drop.paths, &quoted) orelse return;
    // Focus follows the drop. The paths went into THIS pane, and leaving the
    // keyboard pointed somewhere else would put the next keystroke in a
    // different terminal from the words that just appeared.
    if (model.wsConst().selectedTreeConst()) |current| {
        if (current.find(terminal_ref)) |node| updateModel(model, .{ .focus_pane = node }, fx);
    }
    pasteClipboardText(model, pane, fx, text);
}

/// One event of a live tab drag.
///
/// The gesture's own bookkeeping is `Model.TabDrag`; this is the arithmetic
/// that turns a pointer position into swaps. The step is measured against the
/// STRIP's own tab extent — the same derivation the strip lays out with — so a
/// windowed strip whose tabs are narrower than full width steps at the width
/// the user can actually see.
fn applyTabDrag(model: *Model, drag: @FieldType(Msg, "tab_drag")) void {
    const workspace = model.wsConst();
    // Side placement is a vertical rail, and this gesture is horizontal. A
    // drag there is refused whole rather than reinterpreted: guessing that a
    // sideways pointer means a vertical reorder is how a UI gets a reputation
    // for moving things by itself.
    if (model.tab_placement != .top) return;
    const window = projection.visibleTabWindowIn(model, workspace, workspace.surface_size.width - projection.windowPadding(model) * 2);
    switch (drag.phase) {
        // `change`
        0 => {
            if (drag.sourceId >= workspace.tab_count) return;
            var state = model.tab_drag orelse model_module.TabDrag{
                .origin = @intCast(drag.sourceId),
                .current = @intCast(drag.sourceId),
                .anchor_x = drag.x,
            };
            defer model.tab_drag = state;
            if (window.extent <= 0) return;
            const steps_f = std.math.trunc((drag.x - state.anchor_x) / window.extent);
            if (steps_f == 0) return;
            const steps = std.math.lossyCast(i32, steps_f);
            const moved = moveTabBy(model, state.current, steps) orelse return;
            state.anchor_x += @as(f32, @floatFromInt(@as(i32, moved.applied))) * window.extent;
            state.current = moved.landed;
        },
        // `end`: the tabs are already where they look.
        1 => model.tab_drag = null,
        // `cancel`: Escape, or the gesture losing its pointer. Put it back.
        2 => {
            const state = model.tab_drag orelse return;
            model.tab_drag = null;
            const delta = @as(i32, state.origin) - @as(i32, state.current);
            _ = moveTabBy(model, state.current, delta);
        },
        else => {},
    }
}

/// Walk the tab at `index` `delta` places, one swap at a time, stopping at the
/// ends of the strip. Answers where it landed and how far it actually got —
/// which is what re-anchors the drag, so a tab pinned against the last slot
/// does not bank up steps the pointer would have to unwind.
fn moveTabBy(model: *Model, index: u8, delta: i32) ?struct { landed: u8, applied: i32 } {
    if (delta == 0) return .{ .landed = index, .applied = 0 };
    const id = model.wsConst().tabTerminal(index) orelse return null;
    const step: i8 = if (delta > 0) 1 else -1;
    var remaining = @abs(delta);
    var applied: i32 = 0;
    while (remaining > 0) : (remaining -= 1) {
        if (!model.moveTerminal(id, step)) break;
        applied += step;
    }
    const landed = model.wsConst().tabOfTerminal(id) orelse return null;
    return .{ .landed = @intCast(landed), .applied = applied };
}

/// Close a WHOLE tab — every pane in it. Local panes release their owned
/// process; remote panes lose only their Cockpit topology leaf.
fn closeTab(model: *Model, fx: *Fx, index: u8) void {
    closeTabIn(model, fx, model.active_window, index);
}

/// The same, addressed to a specific window — what whole-window close walks.
fn closeTabIn(model: *Model, fx: *Fx, window_index: usize, index: u8) void {
    const workspace = model.wsAtConst(window_index) orelse return;
    const current = workspace.treeConst(index) orelse return;
    var refs: [layout.max_panes]TerminalRef = undefined;
    const count = current.terminals(&refs);
    for (refs[0..count]) |id| {
        closePaneForTerminal(model, fx, id, providerKind(id) == .local);
    }
}

/// The focused terminal's LOCAL pane, or null when focus is on the web
/// surface, on a remote terminal, or on nothing.
fn focusedLocalPane(model: *Model) ?*Pane {
    const terminal_ref = model.selectedTerminalRef() orelse return null;
    return model.provider.terminal(terminal_ref);
}

/// cmd+A: cover the whole SCROLLBACK with a selection the next cmd+C can copy.
///
/// This used to compose the keyboard-selection primitives, which are clamped
/// to the grid, so it could only ever reach the visible screen — and quietly
/// handed back a fraction of the output someone meant to copy. It now delegates
/// to the session's absolute-pin primitive; see `selectAllHistory` for why it
/// leaves keyboard-selection mode disarmed.
fn selectAllScrollback(model: *Model) void {
    const terminal_ref = model.selectedTerminalRef() orelse return;
    const pane = model.provider.terminal(terminal_ref) orelse return;
    if (!pane.session.selectAllHistory()) return;
    pane.selecting = false;
}

/// cmd+K: clear the screen and the scrollback.
///
/// Written as terminal OUTPUT rather than as a new emulator entry point,
/// because that is exactly what it is: `CSI H` homes the cursor, `CSI 2 J`
/// erases the display, `CSI 3 J` erases the saved lines. It is byte-for-byte
/// what `clear` sends, so the emulator ends in a state it already knows how
/// to be in.
fn clearFocusedTerminal(model: *Model, fx: *Fx) void {
    const terminal_ref = model.selectedTerminalRef() orelse return;
    const pane = model.provider.terminal(terminal_ref) orelse return;
    pane.selecting = false;
    pane.session.clearSelection();
    feedOutput(pane, fx, "\x1b[H\x1b[2J\x1b[3J");
    pane.session.scrollToBottom();
    pane.session.refreshScreenText();
    moveResponsesToOutbound(pane, fx);
}

/// Close the pane holding `terminal_ref`, then cascade: the tab goes when it
/// loses its last pane, and the window goes when it loses its last tab.
///
/// `kill_pty` is false when the shell already exited on its own — there is
/// no child left to signal, and the session is freed here either way.
fn closePaneForTerminal(model: *Model, fx: *Fx, terminal_ref: TerminalRef, kill_pty: bool) void {
    // Located across EVERY window, not in the active one: a shell that exits
    // on its own in a background window arrives here as a pty event carrying
    // nothing but its terminal, and closing it against the front window's tab
    // list would silently do nothing.
    const where = model.locateTerminal(terminal_ref) orelse return;
    const workspace = model.wsAt(where.window) orelse return;
    const tab_index = where.tab;
    var current = &workspace.tabs[tab_index];

    endCapturesForTerminal(model, fx, terminal_ref);
    if (model.copy_inflight and model.copy_owner.terminal_ref.eql(terminal_ref)) fx.cancel(clipboard_key);
    if (model.paste_inflight and model.paste_owner.terminal_ref.eql(terminal_ref)) fx.cancel(paste_clipboard_key);

    // The tree promotes the sibling into the parent's rect; nothing else
    // needs to reshape.
    _ = current.closeTerminal(terminal_ref);

    if (model.provider.terminal(terminal_ref)) |pane| {
        const had_live_pty = pane.phase == .starting or pane.phase == .live;
        const pty_key = pane.pty_key;
        // Free the emulator NOW. The old `.closing` tombstone waited on a
        // pty exit that could never arrive, holding a registry slot and a
        // whole session hostage against capacity.
        _ = model.provider.destroyTerminal(terminal_ref);
        if (kill_pty and had_live_pty) fx.ptyKill(pty_key);
    }

    const tab_removed = current.isEmpty();
    if (tab_removed) workspace.dropTab(tab_index);
    endHiddenCaptures(model, fx);

    // Only a local close gives a process slot back. A TAB slot comes back when
    // the final topology leaf closes regardless of provider.
    if (providerKind(terminal_ref) == .local) model.terminal_limit_refused = false;
    if (tab_removed) workspace.tab_limit_refused = false;
    if (workspace.tab_count == 0) retireEmptyWindow(model, fx, where.window);
}

/// A window whose last tab just closed.
///
/// This is the arm that was wrong the moment a second window existed. It used
/// to call `fx.quitApp()` as soon as ANY tab count reached zero, which is
/// right with exactly one window and catastrophic with two: emptying the
/// second window took the whole app down, live shells in the first window
/// included.
///
/// The rule is one sentence. A window whose last tab closes goes away, and the
/// APP goes away only when the last window does — which is true exactly when
/// no secondary window is open AND the main window has no tabs left.
///
/// `closeWindow` is still not enough on its own for that final window: AppKit
/// does not terminate an app when its last window closes unless the delegate
/// opts in, which is what once left a running process with no window and no
/// way back to it. So the close and the quit are both sent, in that order, so
/// the window tears down through the normal path and the shutdown lifecycle
/// that flushes the workspace layout still runs.
fn retireEmptyWindow(model: *Model, fx: *Fx, window_index: usize) void {
    if (window_index == 0) {
        // An emptied MAIN window closes, exactly as a secondary would.
        //
        // macOS apps genuinely vary here and this used to stand on the web
        // surface instead. Closing is the majority behaviour and the one that
        // keeps a single rule for every window: the thing you emptied is the
        // thing that goes away. Standing meant cmd+W on the last tab of the
        // main window left a window on screen showing something the user never
        // asked for, while the identical gesture in any other window closed it.
        //
        // Only when other windows remain. When this is the last window the
        // tail below owns the outcome, which is to close it AND quit - the
        // rule that was already correct and is deliberately untouched.
        var others: usize = 0;
        for (model.secondary) |slot| {
            if (slot != null) others += 1;
        }
        if (others > 0) {
            // `model.closeWindow` is what moves input off a window that just
            // went away (`firstOpenWindow`); doing this by hand would leave
            // `active_window` naming a closed window. The scene's window is
            // not declarative like a secondary's, so it also needs the effect.
            model.closeWindow(window_index);
            fx.closeWindow(scene.main_window_label);
        } else {
            // LAST window: the tail below closes it and quits. Left exactly as
            // it was — the web surface is the state the app is torn down from.
            model.primary.web_selected = true;
        }
    } else {
        // A secondary window is retired DECLARATIVELY: `view.windows` stops
        // naming it and the runtime reconciles it closed. Sending
        // `fx.closeWindow` as well would be two closes for one window, the
        // second against a label that no longer exists.
        model.closeWindow(window_index);
    }
    // A window closing frees a slot, so whatever the last cmd+N was told is
    // no longer true.
    model.window_limit_refused = false;

    var secondaries: usize = 0;
    for (model.secondary) |slot| {
        if (slot != null) secondaries += 1;
    }
    if (secondaries > 0) return;
    if (model.primary_open and model.primary.tab_count > 0) return;

    // Guarded because an emptied main window now closes itself above. Closing
    // it a second time here would be two closes against one label, the second
    // against a window that is already gone.
    if (model.primary_open) {
        model.primary_open = false;
        fx.closeWindow(scene.main_window_label);
    }
    fx.quitApp();
}

/// Close a WHOLE window: local panes release owned processes and remote panes
/// detach presentation. The window itself retires when its final tab leaves.
fn closeWholeWindow(model: *Model, fx: *Fx, window_index: usize) void {
    while (true) {
        const workspace = model.wsAt(window_index) orelse break;
        if (workspace.tab_count == 0) break;
        closeTabIn(model, fx, window_index, 0);
    }
    // The platform window is already gone. The close cascade normally retires
    // the slot; this catches an already-empty workspace.
    if (model.windowOpen(window_index)) retireEmptyWindow(model, fx, window_index);
}
/// ONE copy in flight: the clipboard write reuses a fixed key, so a second
/// request before the first result drains would be rejected as a duplicate —
/// and that rejection would overwrite the first copy's success with
/// `copy_failed`. There is one system clipboard, so this holds ACROSS
/// terminals and providers, not per terminal.
fn copySelection(model: *Model, fx: *Fx, terminal_ref: TerminalRef) void {
    if (model.copy_inflight) return;
    if (model.provider.terminal(terminal_ref)) |pane| {
        pane.copy_failed = false;
        const text = (pane.session.selectionText(pane.session.gpa) catch {
            // Serialization failed with a selection ACTIVE: keep the
            // selection for a retry and say so in the status — a copy that
            // silently does nothing would leave the user pasting stale
            // clipboard content.
            pane.copy_failed = true;
            pane.copied_bytes = 0;
            return;
        }) orelse {
            // No emulator range while the MODEL still holds an anchor: a
            // prior selection re-pin failed and cleared the highlight (see
            // `applySelection`), so this copy has nothing to serialize —
            // that is a failed copy, not a quiet no-op.
            if (pane.session.selectionActive()) {
                pane.copy_failed = true;
                pane.copied_bytes = 0;
            }
            return;
        };
        defer pane.session.gpa.free(text);
        pane.copied_bytes = text.len;
        model.copy_inflight = true;
        model.copy_owner = replicaOwnerForPane(pane);
        fx.writeClipboard(.{
            .key = clipboard_key,
            .text = text,
            .on_result = Fx.clipboardMsg(.clipboard),
        });
        return;
    }

    const state = model.remoteUi(terminal_ref) orelse return;
    state.copy_failed = false;
    const remote = model.phuxForOwner(state.owner) orelse return;
    const text = remote.selectionText(state.owner, std.heap.page_allocator) catch {
        state.copy_failed = true;
        state.copied_bytes = 0;
        return;
    };
    defer std.heap.page_allocator.free(text);
    state.copied_bytes = text.len;
    model.copy_inflight = true;
    model.copy_owner = state.owner;
    fx.writeClipboard(.{
        .key = clipboard_key,
        .text = text,
        .on_result = Fx.clipboardMsg(.clipboard),
    });
    // Keep the range highlighted after submission and completion. A failed
    // write remains retryable; a successful copy follows native terminal
    // convention until typing, a new selection, or restart clears it.
}

/// Commit one pane's grid, local or remote.
///
/// A local commit lands only once the EMULATOR actually took the resize: on an
/// allocation failure the model keeps its old dimensions and the frame pump
/// retries next frame, so the emulator and the pty never disagree about the
/// grid. A commit that changes nothing does no work, which is what makes it
/// safe to call for every pane on every resize dispatch.
fn commitPaneViewport(model: *Model, fx: *Fx, terminal_ref: TerminalRef, cols: u16, rows: u16) void {
    if (model.provider.terminal(terminal_ref)) |pane| {
        if (pane.cols == cols and pane.rows == rows) return;
        if (!pane.session.resize(cols, rows)) return;
        pane.cols = cols;
        pane.rows = rows;
        pane.session.refreshScreenText();
        fx.ptyResize(pane.pty_key, cols, rows);
        return;
    }
    const remote = model.phuxForRef(terminal_ref) orelse return;
    const viewport: Viewport = .{ .cols = cols, .rows = rows };
    if (remote.lastViewport(terminal_ref)) |last| {
        if (last.eql(viewport)) return;
    }
    remote.viewportResize(terminal_ref, viewport) catch {};
}

/// One read in flight: the fixed paste key remains occupied until its result
/// is delivered. A repeated Cmd+V is consumed but cannot issue a duplicate
/// request that would only be rejected.
fn requestPaste(model: *Model, fx: *Fx, terminal_ref: TerminalRef) void {
    if (model.paste_inflight) return;
    model.paste_owner = model.terminalOwner(terminal_ref) orelse return;
    model.paste_failed = false;
    model.paste_target = .terminal;
    if (model.provider.terminal(terminal_ref)) |pane| {
        // An open search field is where a paste GOES, for the same reason
        // committed text does. Deciding it here rather than at the chord
        // means the menu's Edit > Paste agrees with cmd+V for free, and
        // that the two can never drift apart.
        if (pane.session.search.open) {
            model.paste_target = .search_needle;
            model.paste_inflight = true;
            fx.readClipboard(.{
                .key = paste_clipboard_key,
                .on_result = Fx.clipboardMsg(.paste_clipboard),
            });
            return;
        }
        if (!pane.acceptsInput()) {
            model.paste_failed = true;
            return;
        }
    } else {
        const presentation = model.remotePresentation(terminal_ref) orelse {
            model.paste_failed = true;
            return;
        };
        if (presentation.phase != .live) {
            model.paste_failed = true;
            return;
        }
    }
    model.paste_inflight = true;
    fx.readClipboard(.{
        .key = paste_clipboard_key,
        .on_result = Fx.clipboardMsg(.paste_clipboard),
    });
}

/// Encode a clipboard result exactly as this terminal expects, then
/// admit the complete framing and body as one outbound payload. The
/// clipboard result is dispatch scratch, so the mutable staging buffer
/// also gives `encodePaste` room to normalize newlines and replace unsafe
/// control bytes before the result returns.
/// Generic over the effects type for the same reason terminal_runtime.zig is:
/// the TypeScript-core graph delivers a paste through its adapter's effects.
pub fn pasteClipboardText(model: *Model, pane: *Pane, fx: anytype, text: []const u8) void {
    const fence_bytes = "\x1b[200~".len;
    const staging = pane.session.gpa.alloc(u8, text.len + fence_bytes * 2) catch {
        model.paste_failed = true;
        return;
    };
    defer pane.session.gpa.free(staging);

    const body = staging[fence_bytes .. fence_bytes + text.len];
    @memcpy(body, text);
    const parts = vt.input.encodePaste(body, .fromTerminal(&pane.session.term));
    const start = fence_bytes - parts[0].len;
    @memcpy(staging[start..fence_bytes], parts[0]);
    @memcpy(staging[fence_bytes + text.len .. fence_bytes + text.len + parts[2].len], parts[2]);
    const encoded = staging[start .. fence_bytes + text.len + parts[2].len];

    pane.session.scrollToBottom();
    // Retained terminal replies predate this user input and must enter
    // the shared stream first. If they still cannot move, refusing the
    // whole paste is the only ordering-safe outcome.
    moveResponsesToOutbound(pane, fx);
    if (pane.session.response_len > 0 or encoded.len > pane.outbound_buffer.len) {
        pane.outbound_dropped += encoded.len;
        model.paste_failed = true;
        return;
    }
    if (!enqueueOutbound(pane, fx, encoded)) {
        pane.outbound_dropped += encoded.len;
        model.paste_failed = true;
    }
}

// ------------------------------------------------------------- keyboard

pub fn onKey(event: canvas.WidgetKeyboardEvent) ?Msg {
    return .{ .key = event };
}

pub fn onText(event: canvas.WidgetKeyboardEvent) ?Msg {
    return .{ .text = event };
}

pub fn onWheel(wheel: native_sdk.platform.WheelEvent) ?Msg {
    if (wheel.delta_y == 0) return null;
    return .{ .wheel_fallback = .{ .x = wheel.x, .y = wheel.y, .delta = wheel.delta_y } };
}

pub fn onChrome(chrome: native_sdk.platform.WindowChrome) ?Msg {
    return .{ .chrome_changed = chrome };
}

/// Pinch magnification. `.begin` and `.end` carry no scale of their own but
/// are still forwarded: `.begin` is what resets the accumulator, so two
/// gestures never add up into one step.
pub fn onPinch(pinch: native_sdk.platform.PinchEvent) ?Msg {
    return .{ .pinch = pinch };
}

/// A file drop. An empty drop is not a message: the host reports the gesture,
/// and a gesture that carried no paths has nothing for a shell.
pub fn onDrop(drop: native_sdk.platform.FileDropEvent) ?Msg {
    if (drop.paths.len == 0) return null;
    return .{ .files_dropped = drop };
}

/// The system appearance. Forwarded unconditionally — `theme = auto` is the
/// gate, and it lives in the config where the user set it.
pub fn onAppearance(appearance: native_sdk.platform.Appearance) ?Msg {
    return .{ .appearance_changed = appearance };
}

pub fn onLifecycle(event: native_sdk.LifecycleEvent) ?Msg {
    return switch (event) {
        .activate => .{ .focus_changed = true },
        .deactivate => .{ .focus_changed = false },
        // `.stop` is dispatched from the runtime's own shutdown path, BEFORE
        // the app's stop hook, so the model is still whole when it arrives.
        .stop => .shutdown,
        else => null,
    };
}

fn handleKey(model: *Model, fx: *Fx, event: canvas.WidgetKeyboardEvent) void {
    if (!model.focused) return;
    const mods = event.modifiers;
    const primary = mods.hasCommandModifier();

    // A consumed app-shortcut press owns its release too. The latch is
    // window-level because a focus shortcut changes panes before its
    // release arrives.
    if (event.phase == .key_up) {
        const shortcut_mask = appShortcutKeyMask(event.key);
        if (shortcut_mask != 0 and (model.consumed_shortcut_keys_held & shortcut_mask) != 0) {
            model.consumed_shortcut_keys_held &= ~shortcut_mask;
            return;
        }
        const owner = switch (takeHeldTerminalKeyOwner(model, event.key)) {
            .owner => |value| value,
            .consume => return,
            .none => blk: {
                const terminal_ref = model.focusedTerminalRef() orelse return;
                break :blk model.terminalOwner(terminal_ref) orelse return;
            },
        };
        dispatchKeyEvent(model, fx, owner, event, .release);
        return;
    }

    // The settings surface is MODAL, on the same terms the palette below is
    // and for a sharper reason: it is the surface you reach for when the
    // terminal is unreadable, so the one thing it must never do is send the
    // keys meant for it into a shell the user cannot currently see.
    //
    // It sits ABOVE the palette gate because the two can never both be open:
    // `settings_open` dismisses the palette and `palette_open` dismisses
    // settings. If a bug ever made them both open, the one that just took the
    // keyboard should be the one holding it.
    if (model.ws().settings.open) {
        if (keyIs(event.key, "escape")) {
            updateModel(model, .settings_close, fx);
            return;
        }
        if (keyIs(event.key, "enter") or keyIs(event.key, "return")) {
            updateModel(model, .settings_commit, fx);
            return;
        }
        if (keyIs(event.key, "arrowdown") or (mods.control and keyIs(event.key, "n"))) {
            updateModel(model, .{ .settings_step = 1 }, fx);
            return;
        }
        if (keyIs(event.key, "arrowup") or (mods.control and keyIs(event.key, "p"))) {
            updateModel(model, .{ .settings_step = -1 }, fx);
            return;
        }
        // cmd+, again, and the chord's own duplicate delivery, are absorbed:
        // the panel is already open, and `settings_open` is idempotent anyway.
        if (primary and keyIs(event.key, ",")) {
            latchAppShortcut(model, event.key);
            return;
        }
        // Everything else is swallowed, including every cmd-chord. A chord
        // pressed while the settings panel is up is not a chord the user meant
        // for the shell.
        return;
    }

    // The palette is MODAL, and this gate is what makes it so.
    //
    // It sits above every other press arm on purpose. While it is up, no key
    // reaches the shell — not an arrow, not Enter, not a bare letter — because
    // a switcher that leaks its needle into a running program is worse than no
    // switcher. Printable presses are not handled here: they arrive as
    // committed `.text`, which the arm above routes to the needle. That split
    // is the same one the scrollback search already uses.
    if (model.ws().palette.open) {
        if (keyIs(event.key, "escape")) {
            updateModel(model, .palette_close, fx);
            return;
        }
        if (keyIs(event.key, "enter") or keyIs(event.key, "return")) {
            // The identity was fenced when this row became highlighted. A
            // provider catalog rebuild between paint and Enter must not make
            // this cursor select whatever moved into the old numeric slot.
            const destination = model.ws().palette.highlighted orelse {
                updateModel(model, .palette_close, fx);
                return;
            };
            updateModel(model, .{ .palette_activate = destination }, fx);
            return;
        }
        if (keyIs(event.key, "backspace") or keyIs(event.key, "delete")) {
            updateModel(model, .palette_backspace, fx);
            return;
        }
        // Arrows, and the emacs pair every switcher on this platform also
        // answers, so a hand already on ctrl does not have to leave home row.
        if (keyIs(event.key, "arrowdown") or (mods.control and keyIs(event.key, "n"))) {
            updateModel(model, .{ .palette_step = 1 }, fx);
            return;
        }
        if (keyIs(event.key, "arrowup") or (mods.control and keyIs(event.key, "p"))) {
            updateModel(model, .{ .palette_step = -1 }, fx);
            return;
        }
        // cmd+shift+P again, and the chord's own duplicate delivery, are
        // absorbed rather than passed down — the palette is already open and
        // reopening it would clear the needle.
        if (primary and mods.shift and keyIs(event.key, "p")) {
            latchAppShortcut(model, event.key);
            return;
        }
        // Everything else is swallowed. A `cmd`-chord while the palette is up
        // is a chord the user did not mean for the shell.
        return;
    }

    // A registered app shortcut can reach us through both the platform
    // shortcut channel and the canvas key channel. The first delivery owns
    // the physical edge; a duplicate press is consumed until key-up clears it.
    const pressed_shortcut_mask = appShortcutKeyMask(event.key);
    if (pressed_shortcut_mask != 0 and (model.consumed_shortcut_keys_held & pressed_shortcut_mask) != 0) {
        if (primary) return;
        model.consumed_shortcut_keys_held &= ~pressed_shortcut_mask;
    }

    // Tab selection is global and remains available while native WebKit
    // owns the content surface. Releases are latched by physical key above.
    if (primary and !mods.shift and !mods.alt and !mods.control and event.key.len == 1 and event.key[0] >= '1' and event.key[0] <= '5') {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .select_position = event.key[0] - '1' }, fx);
        return;
    }
    if (primary and mods.shift and !mods.alt and !mods.control and keyIs(event.key, "[")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .cycle_tab = -1 }, fx);
        return;
    }
    if (primary and mods.shift and !mods.alt and !mods.control and keyIs(event.key, "]")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .cycle_tab = 1 }, fx);
        return;
    }
    // cmd+D splits right, cmd+shift+D splits down — both always available,
    // including on a fresh window with exactly one terminal. The old gate
    // required two tabs to exist first, which is why the very first split
    // never worked.
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "d")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .split_right, fx);
        return;
    }
    if (primary and mods.shift and !mods.alt and !mods.control and keyIs(event.key, "d")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .split_down, fx);
        return;
    }
    // cmd+[ / cmd+] cycle PANES within the tab; cmd+shift+[ / ] cycle tabs.
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "[")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .cycle_pane = -1 }, fx);
        return;
    }
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "]")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .cycle_pane = 1 }, fx);
        return;
    }
    // Font sizing: cmd+= / cmd+- / cmd+0, the chords every Mac terminal
    // ships. Shift is tolerated on `=` because that key IS `+` with shift and
    // people press cmd+shift+= without thinking.
    if (primary and !mods.alt and !mods.control and (keyIs(event.key, "=") or keyIs(event.key, "+"))) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .font_size_step = 1 }, fx);
        return;
    }
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "-")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .font_size_step = -1 }, fx);
        return;
    }
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "0")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .font_size_reset, fx);
        return;
    }
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "t")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .new_terminal, fx);
        return;
    }
    // cmd+N is a WINDOW, cmd+T a tab — the split every Mac app makes.
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "n")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .new_window, fx);
        return;
    }
    // ctrl+cmd+F, the platform's own fullscreen chord, against the FOCUSED
    // window. It sits with the global chords because it is one: the window is
    // the target whether a terminal, the web surface, or nothing is selected.
    if (primary and mods.control and !mods.shift and !mods.alt and keyIs(event.key, "f")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .toggle_fullscreen, fx);
        return;
    }
    // cmd+F opens scrollback search over the focused terminal; cmd+G and
    // cmd+shift+G step it. Both live with the GLOBAL chords rather than down
    // in the terminal block, because the shortcut latch keys on the physical
    // key regardless of which channel delivered the press — and because the
    // arms themselves already no-op when there is no local terminal focused.
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "f")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .search_open, fx);
        return;
    }
    if (primary and !mods.alt and !mods.control and keyIs(event.key, "g")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .search_step = if (mods.shift) -1 else 1 }, fx);
        return;
    }
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "w")) {
        if (!selectedTerminalCanClose(model)) return;
        latchAppShortcut(model, event.key);
        updateModel(model, .close_terminal, fx);
        return;
    }
    // cmd+shift+B reaches the Web surface. It left the tab strip (a terminal
    // tab strip shows terminals), so it needs a chord of its own or it would
    // only be reachable from the menu.
    if (primary and mods.shift and !mods.alt and !mods.control and keyIs(event.key, "b")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .select_surface = .web }, fx);
        return;
    }
    // cmd+shift+P summons the tab switcher. Needed HERE as well as in
    // `onCommand`: a chord can arrive on the platform shortcut channel or on
    // the canvas key channel, and a canvas-only delivery — which is what the
    // app sees whenever the surface has key — would otherwise fall through to
    // the shell as a bare `p`.
    if (primary and mods.shift and !mods.alt and !mods.control and keyIs(event.key, "p")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .palette_open, fx);
        return;
    }
    // cmd+, opens settings — the chord macOS reserves for exactly this, in
    // every app on the platform. Needed HERE as well as in `onCommand` for the
    // same reason cmd+shift+P is: a canvas-only delivery would otherwise fall
    // through to the shell as a bare comma.
    //
    // Shift is TOLERATED (`<` is the same physical key) but alt and control are
    // not, so this cannot swallow a ctrl+, or opt+, a program is listening for.
    if (primary and !mods.alt and !mods.control and keyIs(event.key, ",")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .settings_open, fx);
        return;
    }
    if (primary and mods.shift and !mods.alt and !mods.control and keyIs(event.key, "arrowleft")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .move_terminal = -1 }, fx);
        return;
    }
    if (primary and mods.shift and !mods.alt and !mods.control and keyIs(event.key, "arrowright")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .move_terminal = 1 }, fx);
        return;
    }
    // cmd+opt+arrows move focus GEOMETRICALLY, the way Ghostty does — the
    // pane that actually lies that way, not the next index in a list.
    if (model.selectedResourceId() != null and primary and mods.alt and !mods.shift and !mods.control) {
        const direction: ?layout.Direction =
            if (keyIs(event.key, "arrowleft")) .left else if (keyIs(event.key, "arrowright")) .right else if (keyIs(event.key, "arrowup")) .up else if (keyIs(event.key, "arrowdown")) .down else null;
        if (direction) |value| {
            latchAppShortcut(model, event.key);
            updateModel(model, .{ .focus_direction = value }, fx);
            return;
        }
    }

    // A webview is a real native input surface. Unclaimed canvas events
    // must never leak into whichever terminal happened to be focused last.
    if (model.selectedResourceId() == null) return;

    const terminal_ref = model.focusedTerminalRef() orelse return;
    if (providerKind(terminal_ref) == .phux) {
        handleRemoteKey(model, fx, terminal_ref, event);
        return;
    }

    // Keyboard input belongs to the active terminal tab.
    const pane = model.provider.terminal(terminal_ref) orelse return;
    const session = pane.session;
    // The search field is MODAL over the terminal. While it is open every
    // key from here down belongs to it — including the ones that would
    // otherwise scroll, select, copy, or reach the child. This is the whole
    // "typing into the field must not reach the shell" contract, stated once
    // as a single early return rather than as a condition repeated on the
    // dozen chords below (which is exactly how that bug gets reintroduced).
    // Everything ABOVE this line is window chrome — tabs, splits, font size,
    // the web surface — and stays live, the way a real search bar does.
    if (session.search.open) {
        handleSearchKey(model, fx, pane, event);
        return;
    }
    // scrollback, restart.
    //
    if (primary and mods.shift and keyIs(event.key, "space")) {
        latchAppShortcut(model, event.key);
        if (pane.selecting) {
            pane.selecting = false;
            session.clearSelection();
        } else {
            pane.selecting = true;
            session.beginSelection(false);
        }
        return;
    }
    if (primary and keyIs(event.key, "c") and (pane.selecting or session.selectionActive())) {
        latchAppShortcut(model, event.key);
        copySelection(model, fx, pane.id);
        return;
    }
    if (primary and keyIs(event.key, "v")) {
        latchAppShortcut(model, event.key);
        requestPaste(model, fx, pane.id);
        return;
    }
    // cmd+A and cmd+K, the two chords every Mac terminal has and this one did
    // not. They sit here rather than with the tab chords because both act on
    // the FOCUSED terminal and mean nothing over the web surface.
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "a")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .select_all, fx);
        return;
    }
    if (primary and !mods.shift and !mods.alt and !mods.control and keyIs(event.key, "k")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .clear_terminal, fx);
        return;
    }
    if (primary and keyIs(event.key, "r") and (pane.phase == .ended or pane.phase == .failed)) {
        latchAppShortcut(model, event.key);
        update(model, .{ .restart = pane.id }, fx);
        return;
    }
    // Scrollback chords pause while a keyboard selection is armed: the
    // selection's anchor and head are VIEWPORT coordinates and the
    // emulator range is pinned to absolute cells, so scrolling under an
    // armed selection would leave the painted caret naming different
    // text than a copy returns. (The chords fall through to the
    // selection block below, where primary+arrows are simply inert.)
    if (!pane.selecting) {
        // shift+PageUp / shift+PageDown page the scrollback. This is the
        // chord terminals have shared for thirty years — xterm, Terminal.app,
        // and Ghostty (`shift+page_up=scroll_page_up`) all ship it — and it
        // was the one genuinely MISSING scrollback binding here.
        if (mods.shift and !primary and !mods.alt and !mods.control and keyIs(event.key, "pageup")) {
            latchAppShortcut(model, event.key);
            session.scrollLines(-@as(isize, pane.rows));
            return;
        }
        if (mods.shift and !primary and !mods.alt and !mods.control and keyIs(event.key, "pagedown")) {
            latchAppShortcut(model, event.key);
            session.scrollLines(@as(isize, pane.rows));
            return;
        }
        if (primary and keyIs(event.key, "arrowup")) {
            latchAppShortcut(model, event.key);
            session.scrollLines(-if (mods.shift) @as(isize, pane.rows) else 1);
            return;
        }
        if (primary and keyIs(event.key, "arrowdown")) {
            latchAppShortcut(model, event.key);
            session.scrollLines(if (mods.shift) @as(isize, pane.rows) else 1);
            return;
        }
        if (primary and keyIs(event.key, "home")) {
            latchAppShortcut(model, event.key);
            session.scrollToTop();
            return;
        }
        if (primary and keyIs(event.key, "end")) {
            latchAppShortcut(model, event.key);
            session.scrollToBottom();
            return;
        }
    }

    if (pane.selecting) {
        if (keyIs(event.key, "escape")) {
            latchAppShortcut(model, event.key);
            pane.selecting = false;
            session.clearSelection();
            return;
        }
        if (keyIs(event.key, "b")) {
            session.toggleSelectionBlock();
            return;
        }
        if (keyIs(event.key, "enter")) {
            latchAppShortcut(model, event.key);
            copySelection(model, fx, pane.id);
            return;
        }
        const step: i32 = 1;
        if (keyIs(event.key, "arrowleft")) return session.moveSelection(-step, 0, mods.shift);
        if (keyIs(event.key, "arrowright")) return session.moveSelection(step, 0, mods.shift);
        if (keyIs(event.key, "arrowup")) return session.moveSelection(0, -step, mods.shift);
        if (keyIs(event.key, "arrowdown")) return session.moveSelection(0, step, mods.shift);
        return;
    }

    if (!pane.acceptsInput()) return;

    // Everything else is terminal input: specials and chords encode
    // through the emulator (application cursor-key mode, kitty
    // protocol, and modifier encodings all honored); plain printable
    // presses arrive through `.text` instead and are ignored here.
    if (pane.session.selectionActive()) pane.session.clearSelection();
    rememberHeldTerminalKey(model, replicaOwnerForPane(pane), event.key);
    encodeKeyEvent(pane, fx, event, .press);
}

/// Every key press that reaches an OPEN search field.
///
/// A handful do something and the rest are SWALLOWED, which is the point:
/// a key with no meaning here is not a key that should fall through to the
/// child. Printable presses do not arrive here at all — they come through the
/// text channel (see the `.text` arm) and are appended to the needle there.
fn handleSearchKey(model: *Model, fx: *Fx, pane: *Pane, event: canvas.WidgetKeyboardEvent) void {
    const primary = event.modifiers.hasCommandModifier();
    // cmd+V fills the needle; cmd+C still copies the TERMINAL's selection.
    //
    // Both sit above the swallowing arms because the search band is modal
    // over the terminal, not over the machine: a field that eats the two
    // chords every Mac app answers reads as broken, and being unable to
    // paste the very string you are looking for is the worst case of it.
    // cmd+C keeps its terminal meaning because the needle is not selectable
    // text — there is nothing else it could copy — and a selection made
    // before cmd+F survives the field being opened.
    if (primary and keyIs(event.key, "v")) {
        latchAppShortcut(model, event.key);
        requestPaste(model, fx, pane.id);
        return;
    }
    if (primary and keyIs(event.key, "c") and (pane.selecting or pane.session.selectionActive())) {
        latchAppShortcut(model, event.key);
        copySelection(model, fx, pane.id);
        return;
    }
    if (keyIs(event.key, "escape")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .search_close, fx);
        return;
    }
    if (keyIs(event.key, "enter")) {
        latchAppShortcut(model, event.key);
        updateModel(model, .{ .search_step = if (event.modifiers.shift) -1 else 1 }, fx);
        return;
    }
    if (keyIs(event.key, "backspace")) {
        // No latch: backspace has no shortcut mask (the u32 latch is full).
        // Its RELEASE is stopped by the search gate in `dispatchKeyEvent`,
        // which is the single funnel every release goes through anyway.
        _ = pane.session.searchBackspace();
        return;
    }
}

fn providerModifiers(event: canvas.WidgetKeyboardEvent) ModifierMask {
    return .{
        .shift = event.modifiers.shift,
        .control = event.modifiers.control,
        .alt = event.modifiers.alt,
        .super = event.modifiers.super and !event.modifiers.control,
    };
}

fn physicalKeyForEvent(event: canvas.WidgetKeyboardEvent) PhysicalKey {
    const key = event.key;
    if (key.len == 1) {
        const byte = std.ascii.toLower(key[0]);
        if (byte >= 'a' and byte <= 'z') return @enumFromInt(20 + byte - 'a');
        if (byte >= '0' and byte <= '9') return @enumFromInt(6 + byte - '0');
        const value: u32 = switch (byte) {
            '`' => 1,
            '\\' => 2,
            '[' => 3,
            ']' => 4,
            ',' => 5,
            '=' => 16,
            '-' => 46,
            '.' => 47,
            '\'' => 48,
            ';' => 49,
            '/' => 50,
            ' ' => 63,
            else => 0,
        };
        return @enumFromInt(value);
    }
    const value: u32 =
        if (keyIs(key, "alt") or keyIs(key, "option")) 51 else if (keyIs(key, "capslock")) 54 else if (keyIs(key, "control") or keyIs(key, "ctrl")) 56 else if (keyIs(key, "meta") or keyIs(key, "command")) 59 else if (keyIs(key, "shift")) 61 else if (keyIs(key, "backspace")) 53 else if (keyIs(key, "enter")) 58 else if (keyIs(key, "tab")) 64 else if (keyIs(key, "delete")) 68 else if (keyIs(key, "end")) 69 else if (keyIs(key, "home")) 71 else if (keyIs(key, "insert")) 72 else if (keyIs(key, "pagedown")) 73 else if (keyIs(key, "pageup")) 74 else if (keyIs(key, "arrowdown")) 75 else if (keyIs(key, "arrowleft")) 76 else if (keyIs(key, "arrowright")) 77 else if (keyIs(key, "arrowup")) 78 else if (keyIs(key, "escape")) 120 else if (key.len >= 2 and (key[0] == 'f' or key[0] == 'F')) blk: {
            const number = std.fmt.parseInt(u8, key[1..], 10) catch break :blk 0;
            break :blk if (number >= 1 and number <= 25) 120 + number else 0;
        } else 0;
    return @enumFromInt(value);
}

fn dispatchKeyEvent(
    model: *Model,
    fx: *Fx,
    owner: ReplicaOwner,
    event: canvas.WidgetKeyboardEvent,
    action: vt.input.KeyAction,
) void {
    if (!model.ownerIsCurrent(owner)) return;
    if (model.provider.terminal(owner.terminal_ref)) |pane| {
        // The search field owns the keyboard, RELEASES included: a child
        // running the kitty protocol would otherwise hear the release of
        // every key typed into the field.
        if (pane.selecting or pane.session.search.open or !pane.acceptsInput()) return;
        encodeKeyEvent(pane, fx, event, action);
        return;
    }
    const remote = model.phuxForOwner(owner) orelse return;
    const physical = physicalKeyForEvent(event);
    if (@intFromEnum(physical) == 0) return;
    var input: KeyInput = .{
        .action = switch (action) {
            .release => .release,
            .repeat => .repeat,
            else => .press,
        },
        .physical = physical,
        .modifiers = providerModifiers(event),
    };
    remote.sendKey(owner, &input) catch {};
}

fn sendRemoteFocus(model: *Model, owner: ReplicaOwner, focused: bool) void {
    const remote = model.phuxForOwner(owner) orelse return;
    remote.sendFocus(owner, focused) catch {};
}

fn beginRemoteSelection(model: *Model, _: TerminalRef, state: *RemoteUiState) void {
    clearRemoteSelection(model, state);
    const presentation = @import("terminal_interaction.zig").presentationForOwner(model, state.owner) orelse return;
    const cursor = presentation.grid.cursor orelse canvas.TerminalCursor{};
    // The selection belongs to the coordinator that minted the pane.
    const remote = model.phuxForOwner(state.owner) orelse return;
    const point: provider_contract.DocumentPoint = .{
        .space = .viewport,
        .row = @as(u32, cursor.y),
        .column = cursor.x,
    };
    const start = remote.createAnchor(state.owner, point) catch return;
    const end = remote.createAnchor(state.owner, point) catch {
        remote.releaseAnchor(state.owner, start);
        return;
    };
    remote.setSelection(state.owner, start, end, false) catch {
        remote.releaseAnchor(state.owner, start);
        remote.releaseAnchor(state.owner, end);
        return;
    };
    state.selecting = true;
    state.rectangle = false;
    state.start_anchor = start.opaque_id;
    state.end_anchor = end.opaque_id;
    state.head_x = cursor.x;
    state.head_y = cursor.y;
}

fn applyRemoteSelection(model: *Model, state: *RemoteUiState) void {
    const remote = model.phuxForOwner(state.owner) orelse return;
    const next = remote.createAnchor(state.owner, .{
        .space = .viewport,
        .row = state.head_y,
        .column = state.head_x,
    }) catch return;
    remote.setSelection(
        state.owner,
        .{ .opaque_id = state.start_anchor },
        next,
        state.rectangle,
    ) catch {
        remote.releaseAnchor(state.owner, next);
        return;
    };
    if (state.end_anchor != 0 and state.end_anchor != state.start_anchor)
        remote.releaseAnchor(state.owner, .{ .opaque_id = state.end_anchor });
    state.end_anchor = next.opaque_id;
}

fn moveRemoteSelection(model: *Model, _: TerminalRef, state: *RemoteUiState, dx: i32, dy: i32) void {
    const presentation = @import("terminal_interaction.zig").presentationForOwner(model, state.owner) orelse return;
    const max_x: i32 = @max(0, @as(i32, presentation.cols) - 1);
    const max_y: i64 = @max(0, @as(i64, presentation.rows) - 1);
    state.head_x = @intCast(std.math.clamp(@as(i32, state.head_x) + dx, 0, max_x));
    state.head_y = @intCast(std.math.clamp(@as(i64, state.head_y) + dy, 0, max_y));
    applyRemoteSelection(model, state);
}

/// What a SUCCESSFUL copy does to the range it copied — and the reason this is
/// one named rule rather than two lines in two arms.
///
/// The local arm has always left the highlight standing (`pane.selecting =
/// false` ends keyboard-selection MODE; the emulator's range, and therefore
/// the wash on screen, survives). The remote arm called `clearRemoteSelection`,
/// which tells the coordinator to drop the range and releases both anchors —
/// so the same cmd+C left a Phux pane bare and a native pane highlighted.
///
/// The behaviour kept is the LOCAL one: the highlight stays. Every Mac
/// terminal a user of this app arrives from (Terminal.app, iTerm2, Ghostty)
/// keeps it, and it is the only confirmation that the copy took the range the
/// user meant — clearing it deletes the evidence at the exact moment the user
/// wants to check it, and makes a re-copy cost a fresh selection. Typing, a new
/// selection, and a restart all still clear it, on both sides.
///
/// Split out as a function over the state alone because the remote clipboard
/// arm is only reachable in a `-Dphux-enabled=true` build: the rule is pinned
/// here, where a default build's tests can reach it.
pub fn retainSelectionAfterCopy(state: *RemoteUiState) void {
    // Keyboard-selection MODE ends — a further arrow key must move the cursor,
    // not extend a range the user has finished with — while the anchors and
    // the coordinator's own range stay exactly where the copy found them.
    state.selecting = false;
}

fn clearRemoteSelection(model: *Model, state: *RemoteUiState) void {
    defer {
        state.selecting = false;
        state.rectangle = false;
        state.start_anchor = 0;
        state.end_anchor = 0;
        state.gesture_handle = 0;
    }
    const remote = model.phuxForOwner(state.owner) orelse return;
    remote.clearSelection(state.owner) catch {};
    if (state.start_anchor != 0)
        remote.releaseAnchor(state.owner, .{ .opaque_id = state.start_anchor });
    if (state.end_anchor != 0 and state.end_anchor != state.start_anchor)
        remote.releaseAnchor(state.owner, .{ .opaque_id = state.end_anchor });
    state.selecting = false;
    state.rectangle = false;
    state.start_anchor = 0;
    state.end_anchor = 0;
}

fn handleRemoteKey(model: *Model, fx: *Fx, terminal_ref: TerminalRef, event: canvas.WidgetKeyboardEvent) void {
    const state = model.remoteUi(terminal_ref) orelse return;
    const mods = event.modifiers;
    const primary = mods.hasCommandModifier();
    const remote = model.phuxForOwner(state.owner) orelse return;

    if (primary and mods.shift and keyIs(event.key, "space")) {
        latchAppShortcut(model, event.key);
        if (state.selecting) clearRemoteSelection(model, state) else beginRemoteSelection(model, terminal_ref, state);
        return;
    }
    if (primary and keyIs(event.key, "c") and state.selecting) {
        latchAppShortcut(model, event.key);
        copySelection(model, fx, terminal_ref);
        return;
    }
    if (primary and keyIs(event.key, "v")) {
        latchAppShortcut(model, event.key);
        requestPaste(model, fx, terminal_ref);
        return;
    }
    if (!state.selecting and primary) {
        if (keyIs(event.key, "arrowup") or keyIs(event.key, "arrowdown")) {
            latchAppShortcut(model, event.key);
            const amount: i64 = if (mods.shift) @intCast((@import("terminal_interaction.zig").presentationForOwner(model, state.owner) orelse return).rows) else 1;
            remote.scrollViewport(state.owner, .{
                .kind = .delta,
                .value = if (keyIs(event.key, "arrowup")) -amount else amount,
            }) catch {};
            return;
        }
        if (keyIs(event.key, "home") or keyIs(event.key, "end")) {
            latchAppShortcut(model, event.key);
            remote.scrollViewport(state.owner, .{
                .kind = if (keyIs(event.key, "home")) .top else .bottom,
            }) catch {};
            return;
        }
    }
    if (state.selecting) {
        if (keyIs(event.key, "escape")) {
            latchAppShortcut(model, event.key);
            clearRemoteSelection(model, state);
            return;
        }
        if (keyIs(event.key, "b")) {
            state.rectangle = !state.rectangle;
            applyRemoteSelection(model, state);
            return;
        }
        if (keyIs(event.key, "enter")) {
            latchAppShortcut(model, event.key);
            copySelection(model, fx, terminal_ref);
            return;
        }
        if (keyIs(event.key, "arrowleft")) return moveRemoteSelection(model, terminal_ref, state, -1, 0);
        if (keyIs(event.key, "arrowright")) return moveRemoteSelection(model, terminal_ref, state, 1, 0);
        if (keyIs(event.key, "arrowup")) return moveRemoteSelection(model, terminal_ref, state, 0, -1);
        if (keyIs(event.key, "arrowdown")) return moveRemoteSelection(model, terminal_ref, state, 0, 1);
        return;
    }
    const presentation = @import("terminal_interaction.zig").presentationForOwner(model, state.owner) orelse return;
    if (presentation.phase != .live) return;
    rememberHeldTerminalKey(model, state.owner, event.key);
    dispatchKeyEvent(model, fx, state.owner, event, .press);
}

fn terminalKeyFingerprint(key: []const u8) u64 {
    var fingerprint: u64 = 14695981039346656037;
    for (key) |byte| {
        fingerprint ^= std.ascii.toLower(byte);
        fingerprint *%= 1099511628211;
    }
    return if (fingerprint == 0) 1 else fingerprint;
}

fn rememberHeldTerminalKey(model: *Model, owner: ReplicaOwner, key: []const u8) void {
    const fingerprint = terminalKeyFingerprint(key);
    var target: usize = @intCast(fingerprint % max_held_terminal_keys);
    for (&model.held_terminal_keys, 0..) |*held, index| {
        if (held.fingerprint == fingerprint) {
            target = index;
            break;
        }
        if (held.fingerprint == 0) target = index;
    }
    model.held_terminal_keys[target] = .{
        .fingerprint = fingerprint,
        .owner = owner,
    };
}

const HeldTerminalKeyOwner = union(enum) {
    none,
    consume,
    owner: ReplicaOwner,
};

fn takeHeldTerminalKeyOwner(model: *Model, key: []const u8) HeldTerminalKeyOwner {
    const fingerprint = terminalKeyFingerprint(key);
    for (&model.held_terminal_keys) |*held| {
        if (held.fingerprint != fingerprint) continue;
        const owner = held.owner;
        held.* = .{};
        if (!model.ownerIsCurrent(owner)) return .consume;
        return .{ .owner = owner };
    }
    return .none;
}

fn latchAppShortcut(model: *Model, key: []const u8) void {
    model.consumed_shortcut_keys_held |= appShortcutKeyMask(key);
}

pub fn appShortcutKeyMask(key: []const u8) u64 {
    if (keyIs(key, "1")) return 1 << 0;
    if (keyIs(key, "2")) return 1 << 1;
    if (keyIs(key, "3")) return 1 << 12;
    if (keyIs(key, "4")) return 1 << 20;
    if (keyIs(key, "5")) return 1 << 21;
    if (keyIs(key, "space")) return 1 << 2;
    if (keyIs(key, "c")) return 1 << 3;
    if (keyIs(key, "r")) return 1 << 4;
    if (keyIs(key, "arrowup")) return 1 << 5;
    if (keyIs(key, "arrowdown")) return 1 << 6;
    if (keyIs(key, "home")) return 1 << 7;
    if (keyIs(key, "end")) return 1 << 8;
    if (keyIs(key, "v")) return 1 << 9;
    if (keyIs(key, "escape")) return 1 << 10;
    if (keyIs(key, "enter")) return 1 << 11;
    if (keyIs(key, "d")) return 1 << 13;
    if (keyIs(key, "[")) return 1 << 14;
    if (keyIs(key, "]")) return 1 << 15;
    if (keyIs(key, "arrowleft")) return 1 << 16;
    if (keyIs(key, "arrowright")) return 1 << 17;
    if (keyIs(key, "t")) return 1 << 18;
    if (keyIs(key, "w")) return 1 << 19;
    // `=` and `+` are ONE physical key: a press reported as `+` and a release
    // reported as `=` (or the reverse, depending on the shift edge) must
    // clear the same latch or the key wedges held forever.
    if (keyIs(key, "=") or keyIs(key, "+")) return 1 << 22;
    if (keyIs(key, "-")) return 1 << 23;
    if (keyIs(key, "0")) return 1 << 24;
    if (keyIs(key, "a")) return 1 << 25;
    if (keyIs(key, "k")) return 1 << 26;
    if (keyIs(key, "b")) return 1 << 27;
    if (keyIs(key, "pageup")) return 1 << 28;
    if (keyIs(key, "pagedown")) return 1 << 29;
    if (keyIs(key, "f")) return 1 << 30;
    if (keyIs(key, "g")) return 1 << 31;
    if (keyIs(key, "n")) return 1 << 32;
    if (keyIs(key, "p")) return 1 << 33;
    // `,` and `<` are ONE physical key, the same way `=` and `+` are above:
    // cmd+, and cmd+shift+, can report either name, and both edges have to
    // clear the same latch or the key wedges held forever.
    if (keyIs(key, ",") or keyIs(key, "<")) return 1 << 34;
    return 0;
}

comptime {
    // The latch was a u32 with every bit spoken for; it is a u64 now, and the
    // three views of the same mask — `consumed_shortcut_keys_held`,
    // `global_shortcut_keys_held`, and `suppressed_canvas_shortcuts` — must
    // be widened TOGETHER, because widening one alone silently drops the top
    // bits of the other two.
    std.debug.assert(@bitSizeOf(@TypeOf(appShortcutKeyMask("f"))) == 64);
}

pub fn commandShortcutKeyMask(name: []const u8) u64 {
    for (cockpit_shortcuts) |shortcut| {
        if (std.mem.eql(u8, name, shortcut.id)) return appShortcutKeyMask(shortcut.key);
    }
    return 0;
}
