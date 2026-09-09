const std = @import("std");
const native_sdk = @import("native_sdk");
const grid = @import("../terminal/grid.zig");
const provider_contract = @import("provider_contract");
const support = @import("phux_support.zig");
const local = @import("../providers/local/provider.zig");
const topology = @import("topology.zig");
const layout = @import("layout.zig");
const config_module = @import("../config/config.zig");
const session_state = @import("session_state.zig");
const url_module = @import("../terminal/url.zig");

const canvas = native_sdk.canvas;
const geometry = native_sdk.geometry;

pub const PhuxProvider = support.PhuxProvider;
pub const TerminalRef = support.TerminalRef;
pub const LocalResourceId = support.LocalResourceId;
pub const ReplicaOwner = support.ReplicaOwner;
pub const Presentation = support.Presentation;
pub const MouseButton = support.MouseButton;
pub const Pane = local.Pane;
pub const LocalProvider = local.LocalProvider;
pub const Config = config_module.Config;
pub const TabPlacement = topology.TabPlacement;
pub const SurfaceSelection = topology.SurfaceSelection;
pub const TopologySnapshot = topology.TopologySnapshot;
pub const SnapshotSelection = topology.SnapshotSelection;
pub const PersistedTopologySnapshot = topology.PersistedTopologySnapshot;
pub const max_terminals = local.max_terminals;
pub const max_tabs = topology.max_tabs;
pub const max_remote_terminals = support.max_remote_terminals;
pub const AgentSession = support.AgentSession;
pub const AgentState = support.AgentState;
pub const max_agent_sessions = support.max_agent_sessions;

pub const max_held_terminal_keys: usize = 16;

/// How much magnification buys one point of type size.
///
/// 12% is roughly a deliberate half-inch of finger travel on a trackpad: small
/// enough that a pinch feels connected to the size, large enough that resting
/// two fingers on the glass and breathing does not resize anybody's terminal.
/// Every step reflows every pane, so the cost of being twitchy here is a
/// SIGWINCH storm rather than a wobbly animation.
pub const pinch_points_per_step: f32 = 0.12;

/// Title ceiling for a posted desktop notification. The title is a terminal's
/// name, which the tab strip already bounds well below this.
pub const max_notification_title_bytes: usize = 128;

/// One tab being dragged along the strip.
///
/// The reorder is applied AS THE POINTER MOVES rather than at the drop: the
/// tab under the cursor is the tab that will be there when the button comes
/// up, so there is nothing to guess and no landing animation to disagree with.
/// That is also why `end` has no work — it only forgets the gesture.
///
/// `anchor_x` is the pointer position at which `current` was last committed,
/// not where the drag began. Re-anchoring on every commit is what makes a long
/// drag a run of single steps instead of an accelerating slide: the distance
/// to the NEXT swap is always one tab, whatever happened before it.
///
/// `origin` is kept for exactly one caller — a cancelled drag (Escape, or the
/// window losing the pointer), which has to put the tab back where the user
/// picked it up.
pub const TabDrag = struct {
    origin: u8,
    current: u8,
    anchor_x: f32,
};

/// Path ceiling for the layout state file, matching the SDK's own
/// `max_effect_file_path_bytes` — a path the write effect would refuse is a
/// path there is no point storing.
pub const max_state_path_bytes: usize = 1024;

/// Layout-persistence bookkeeping.
///
/// The path is resolved ONCE at startup and copied here, because `update` has
/// no environment and no `Io`: by the time a topology change wants writing,
/// the only thing that can still answer "where" is the model.
///
/// `fingerprint` is the topology hash as of the end of the previous `update`.
/// Comparing against it is what makes a save EDGE-triggered — a keystroke
/// leaves it unchanged and arms nothing, while a divider drag changes it every
/// frame and re-arms the same one-shot timer, which is the debounce.
pub const StatePersistence = struct {
    path_storage: [max_state_path_bytes]u8 = undefined,
    path_len: usize = 0,
    fingerprint: u64 = 0,
    /// An existing state file was present but could not be accepted. Its path
    /// remains available for the startup notice, while this latch makes the
    /// entire persistence pipeline read-only for the lifetime of the launch.
    preserve_rejected_existing: bool = false,
    /// A write effect is outstanding on the state-file key. A second write on
    /// a live key is rejected by the SDK, so one waits rather than racing.
    inflight: bool = false,
    /// The topology captured by the outstanding write. Its result may arrive
    /// after another change, and must not spend that newer topology's budget.
    inflight_fingerprint: u64 = 0,
    /// The topology moved while that write was in flight, so another is owed
    /// the moment it lands. A failed write also restores this bit until a
    /// retry succeeds or a clean shutdown flushes the live state.
    pending: bool = false,
    /// Consecutive retries for the current topology. Bounded so a permanently
    /// unwritable destination cannot keep the effect loop busy forever.
    retry_count: u8 = 0,
    /// Every retry for the current topology failed. Success clears the latch;
    /// exhaustion leaves it explicit even though no further timer is armed.
    write_failed: bool = false,

    pub fn path(state: *const StatePersistence) []const u8 {
        return state.path_storage[0..state.path_len];
    }

    /// The one persistence invariant: a destination exists and this launch is
    /// allowed to replace it. Every asynchronous and synchronous write path
    /// asks this before doing any work.
    pub fn enabled(state: *const StatePersistence) bool {
        return state.path_len != 0 and !state.preserve_rejected_existing;
    }

    pub fn rejectedExisting(state: *const StatePersistence) bool {
        return state.path_len != 0 and state.preserve_rejected_existing;
    }

    /// Adopt a resolved path, or disable persistence when there is none to
    /// adopt. A window that cannot find a state directory still runs.
    pub fn setPath(state: *StatePersistence, value: ?[]const u8) void {
        state.preserve_rejected_existing = false;
        const resolved = value orelse "";
        if (resolved.len == 0 or resolved.len > max_state_path_bytes) {
            state.path_len = 0;
            return;
        }
        @memcpy(state.path_storage[0..resolved.len], resolved);
        state.path_len = resolved.len;
    }

    /// Keep the rejected source path for explanation, but retire every route
    /// that could rename, truncate, replace, or retry a write against it.
    pub fn preserveRejectedExisting(state: *StatePersistence, source_path: []const u8) void {
        state.setPath(source_path);
        if (state.path_len == 0) return;
        state.preserve_rejected_existing = true;
        state.inflight = false;
        state.pending = false;
        state.retry_count = 0;
        state.write_failed = false;
    }
};

/// Where the CONFIG file lives, for writing a choice back to it.
///
/// Separate storage from `StatePersistence` above, and deliberately so: layout
/// is state nobody hand-writes and it is rewritten on every split, while this
/// is a file a person edits. They must never be confused for each other, and
/// giving them one shared type would be the first step toward that.
///
/// Empty means "no config file could be resolved", which disables writing
/// rather than failing anything — and which is the default, so every test and
/// every fixture is free of disk traffic until a composition root hands over a
/// real path.
pub const ConfigFile = struct {
    path_storage: [max_state_path_bytes]u8 = undefined,
    path_len: usize = 0,

    pub fn path(file: *const ConfigFile) []const u8 {
        return file.path_storage[0..file.path_len];
    }

    pub fn enabled(file: *const ConfigFile) bool {
        return file.path_len != 0;
    }

    pub fn setPath(file: *ConfigFile, value: ?[]const u8) void {
        const resolved = value orelse "";
        if (resolved.len == 0 or resolved.len > max_state_path_bytes) {
            file.path_len = 0;
            return;
        }
        @memcpy(file.path_storage[0..resolved.len], resolved);
        file.path_len = resolved.len;
    }
};

/// The in-app settings surface's state.
///
/// A WORKSPACE's, not the model's, for exactly the reason `Palette` is: it
/// takes the keyboard while it is up, and a surface open in the window behind
/// must not eat the keys typed in the window in front. What it EDITS is
/// app-wide (`Model.config`), and that asymmetry is the point — one config,
/// one live preview, but modality per window.
///
/// `restore_theme` is what makes the preview safe. Moving the cursor applies a
/// theme immediately, because being able to SEE the effect while choosing is
/// the entire reason this surface exists; Escape then has to be able to put
/// back what was in effect when it opened, and this is the copy that lets it.
pub const Settings = struct {
    open: bool = false,
    /// Row index into `theme.builtins`.
    cursor: usize = 0,
    /// The theme name in effect when the surface opened, restored on cancel.
    /// Empty means "no theme was named", which is itself a state Escape has to
    /// be able to return to.
    restore_theme: config_module.ThemeName = config_module.ThemeName.init(""),
    /// Whether the config file will actually take the write, asked ONCE when
    /// the surface opens (`Model.configFileWritable`).
    ///
    /// The panel names the file it saves to, and that line was a promise it had
    /// no evidence for: against a read-only config the whole gesture applied a
    /// theme live, closed, wrote nothing and said nothing, so the next launch
    /// reverted with no explanation. Knowing BEFORE the commit is what lets the
    /// promise be qualified instead of broken.
    ///
    /// Defaults to true — "no reason to think otherwise" — because a panel that
    /// cried read-only in every test fixture that never opened it would be a
    /// worse lie in the other direction. It is only ever meaningful while
    /// `open` is set, and `reset` puts it back.
    ///
    /// A snapshot rather than a live query for the reason `Dir.access`'s own
    /// documentation gives: this is a time-of-check value, and the write at
    /// commit remains the only thing that decides whether the bytes landed.
    /// The band that reports THAT is the backstop for the seconds in between.
    config_writable: bool = true,
    /// Whether the active config path named an existing file when Settings
    /// opened. A missing target may still be writable, but Finder cannot reveal
    /// a file that has not been created yet.
    config_exists: bool = false,

    pub fn reset(settings: *Settings) void {
        settings.open = false;
        settings.cursor = 0;
        settings.restore_theme = config_module.ThemeName.init("");
        settings.config_writable = true;
        settings.config_exists = false;
    }
};

/// Sentinel for "the pointer is over no tab". `max_tabs` is a valid index in
/// no tab list, so it can never collide with a real hover.
pub const no_hovered_tab: usize = std.math.maxInt(usize);
pub const max_pointer_captures: usize = 8;

pub const HeldTerminalKey = struct {
    fingerprint: u64 = 0,
    owner: ReplicaOwner = .{
        .terminal_ref = provider_contract.localTerminalRef(.terminal_1),
        .generation = .{},
    },
};

pub const MonitorPointerCapture = struct {
    owner: ReplicaOwner,
    button: MouseButton,
};

pub const PointerState = struct {
    queue: support.pointer_module.EventQueue = .{},
    monitor: ?support.pointer_module.Monitor = null,
    capture: ?MonitorPointerCapture = null,
    last_x: f64 = 0,
    last_y: f64 = 0,
};

pub const RemoteUiState = struct {
    terminal_ref: ?TerminalRef = null,
    attachment_context: topology.attachments.Context = .{},
    owner: ReplicaOwner = .{
        .terminal_ref = provider_contract.localTerminalRef(.terminal_1),
        .generation = .{},
    },
    selecting: bool = false,
    rectangle: bool = false,
    start_anchor: u64 = 0,
    end_anchor: u64 = 0,
    head_x: u16 = 0,
    head_y: u32 = 0,
    copied_bytes: u64 = 0,
    copy_failed: bool = false,
    wheel_accum: f32 = 0,
    search: @import("native/remote_presentation_commands.zig").Search = .{},
    wheel_accum_x: f32 = 0,
    gesture_handle: u64 = 0,

    fn replaceOwner(state: *RemoteUiState, owner: ReplicaOwner, context: topology.attachments.Context) void {
        const identity: topology.attachments.Reference = .{
            .terminal_ref = owner.terminal_ref,
            .context = state.attachment_context,
        };
        const search: @TypeOf(state.search) = if (context.session_id != 0 and identity.matches(&context)) state.search.replacement() else .{};
        // Document coordinates, clipboard latches and gestures belong to the
        // retired replica. Only the identity-qualified Find field survives.
        state.* = .{
            .terminal_ref = owner.terminal_ref,
            .owner = owner,
            .attachment_context = context,
            .search = search,
        };
    }
};

pub const PointerModifiers = struct {
    shift: bool = false,
    control: bool = false,
    alt: bool = false,
    super: bool = false,
};

pub const TerminalPointerEvent = struct {
    window_id: native_sdk.platform.WindowId = 1,
    terminal_id: LocalResourceId,
    generation: u64,
    phase: canvas.WidgetPointerPhase,
    pointer_id: u64 = 0,
    button: i32 = 0,
    click_count: u8 = 1,
    point: geometry.PointF,
    frame: geometry.RectF,
    delta: geometry.OffsetF = .{},
    modifiers: PointerModifiers = .{},
};

pub const PointerDragMode = enum { local_selection, mouse_report };

/// Destination of an in-flight clipboard read. See `Model.paste_target`.
pub const PasteTarget = enum { terminal, search_needle };

pub const PointerCapture = struct {
    active: bool = false,
    window_id: native_sdk.platform.WindowId = 0,
    terminal_id: LocalResourceId,
    generation: u64 = 0,
    pointer_id: u64 = 0,
    button: i32 = 0,
    mode: PointerDragMode = .local_selection,
    mouse_protocol_fingerprint: u8 = 0,
    frame: geometry.RectF = .{},
    last_point: geometry.PointF = .{},
    modifiers: PointerModifiers = .{},
};

pub const BrowserPage = enum {
    github,
    superlogical,
    article,

    pub fn url(page: BrowserPage) []const u8 {
        return switch (page) {
            .github => "https://github.com/phall1",
            .superlogical => "https://www.superlogical.com/",
            .article => "https://mitchellh.com/writing/superlogical",
        };
    }
};

/// Model-declared secondary windows the toolkit budgets
/// (`canvas_limits.max_ui_app_windows`). Restated here rather than imported
/// because `UiApp` is parameterized on this very model, so naming the app type
/// from inside it is circular; `app_types.zig` carries the comptime assertion
/// that the two agree, which is where the app type is actually in scope.
pub const max_secondary_windows: usize = 4;

/// Windows the app can have open at once: the scene's own plus the
/// secondaries. The ceiling is the TOOLKIT's, not a policy of this app's.
pub const max_windows: usize = 1 + max_secondary_windows;

/// ONE window's workspace: a list of TABS, each owning a `layout.Tree` of
/// panes, plus the surface geometry that window was last measured at.
///
/// The selected tab plus that tree's own `focus` fully determine what is
/// focused — there is no second focus variable to keep in sync. `web_selected`
/// is the one thing outside the tab model: the WebKit surface takes over the
/// content area without belonging to a tab.
///
/// Extracted out of `Model` so a second window is a second workspace rather
/// than a second model: everything below is per-window by construction, and
/// everything that stayed on `Model` (the provider, the config, the clipboard
/// latches, the shortcut latch) is genuinely app-wide.
/// The longest needle the summoned working-set index will hold. A destination
/// is found by a few characters of its name or position; nobody types a
/// sentence here, and a fixed buffer keeps the workspace allocation-free.
pub const max_palette_query_bytes: usize = 64;

/// The summoned working-set index's state.
///
/// It is a WORKSPACE's, not the model's: each window has its own tabs, so each
/// window has its own switcher, and a palette open in the window behind must
/// not eat the keys typed in the window in front.
///
/// Deliberately shaped like the scrollback search beside it — a drawn field
/// with an app-routed needle rather than a text-entry widget. The app already
/// routes every canvas key itself, and the requirement that decides this is
/// that a keystroke aimed at the switcher can never reach the shell. That is a
/// property of the routing, so the field is drawn where the routing lives.
pub const Palette = struct {
    open: bool = false,
    query: [max_palette_query_bytes]u8 = [_]u8{0} ** max_palette_query_bytes,
    query_len: usize = 0,
    /// An index into the FILTERED list, not into `tabs`. Filtering changes
    /// what row 0 means on every keystroke, and an index into the unfiltered
    /// list would silently point at a row the user cannot see.
    cursor: usize = 0,
    /// The stable identity carried by the row currently highlighted. `cursor`
    /// is only navigation geometry; activation never resolves through it.
    highlighted: ?PaletteDestination = null,

    pub fn needle(palette: *const Palette) []const u8 {
        return palette.query[0..palette.query_len];
    }

    pub fn reset(palette: *Palette) void {
        palette.open = false;
        palette.query_len = 0;
        palette.cursor = 0;
        palette.highlighted = null;
    }

    /// Append typed text, dropping anything past the buffer rather than
    /// truncating mid-codepoint on the next comparison.
    pub fn append(palette: *Palette, text: []const u8) void {
        for (text) |byte| {
            if (byte < 0x20 or byte == 0x7f) continue;
            if (palette.query_len >= palette.query.len) return;
            palette.query[palette.query_len] = byte;
            palette.query_len += 1;
        }
        palette.cursor = 0;
    }

    /// Delete one BYTE, then walk back off any UTF-8 continuation bytes so a
    /// multi-byte character leaves as one keystroke rather than as four
    /// mangled ones.
    pub fn backspace(palette: *Palette) void {
        if (palette.query_len == 0) return;
        palette.query_len -= 1;
        while (palette.query_len > 0 and (palette.query[palette.query_len] & 0xc0) == 0x80) {
            palette.query_len -= 1;
        }
        palette.cursor = 0;
    }
};

pub const Workspace = struct {
    tabs: [max_tabs]layout.Tree = [_]layout.Tree{.{}} ** max_tabs,
    /// Process-local projection identities. Positions move and focused panes
    /// change, so neither can safely key the TypeScript tab list.
    tab_ids: [max_tabs]u32 = [_]u32{0} ** max_tabs,
    /// Durable Phux layout identity, distinct from the process-local chrome key.
    shared_ids: [max_tabs]?[16]u8 = @splat(null),
    next_tab_id: u32 = 1,
    /// Invalidates command targets when a retired u32 label ID can be reused.
    /// Saturation permanently disables identity commands in this workspace.
    tab_generation: u64 = 0,
    /// The summoned working-set index. Presentational plus a keyboard mode,
    /// and deliberately NOT part of `topologySnapshot`: an open index is not a
    /// workspace shape and must never be restored on launch.
    palette: Palette = .{},
    /// The settings surface. Not part of `topologySnapshot` for the same
    /// reason the palette is not: an open panel is not a workspace shape.
    settings: Settings = .{},
    tab_count: usize = 0,
    selected_tab: usize = 0,
    /// The web surface owns the content area. Independent of `selected_tab`,
    /// so returning from Web restores the tab that was there. Only the MAIN
    /// window can raise it: its post-present WebView is parented to window 0's
    /// canvas.
    web_selected: bool = false,
    /// The tab the pointer is over, or `no_hovered_tab`. Purely presentational
    /// — it decides whether that tab shows its close `x` — but it lives here
    /// because the view is a pure function of it.
    hovered_tab: usize = no_hovered_tab,
    /// Index of the leftmost tab the strip is scrolled to — the one piece of
    /// memory "scroll into view" needs and a pure derivation cannot have.
    ///
    /// Without it the visible window can only be recomputed from the selection
    /// alone, and the two stateless rules available are both wrong: pinning the
    /// selection to the last slot scrolls a whole tab width on every BACKWARD
    /// step even when the target was already on screen, and recentring scrolls
    /// on every step in both directions. Remembering where the strip already is
    /// makes the honest rule expressible — move only when the selection is out
    /// of view, and only far enough to bring it back.
    ///
    /// Presentational, like `hovered_tab`: a scroll position is not a workspace
    /// shape, so it is deliberately absent from `topologySnapshot` and
    /// `topologyFingerprint`. `projection.visibleTabWindowIn` clamps whatever
    /// it finds here, so a stale value costs at most one frame of scroll
    /// position and can never produce a window that hides the selection.
    tab_window_first: usize = 0,
    /// A cmd+T was refused because this workspace already has every tab slot
    /// it can represent. Per-window because another window may still have room.
    tab_limit_refused: bool = false,
    /// This window's titlebar inset, from its own chrome event.
    chrome_top: f32 = 0,
    /// Shipping markup's measured terminal slot, before terminal padding and
    /// native search/notice bands. Null in the retained native presentation.
    shipping_terminal_space: ?geometry.RectF = null,
    shipping_terminal_size: geometry.SizeF = .{},
    /// Available horizontal tab slot from the same compiled chrome layout.
    /// Includes neither traffic lights nor toolbar controls.
    shipping_tab_strip_width: f32 = 0,
    /// This window's canvas size and device scale. Per-window because two
    /// windows can sit on different monitors at different densities, and every
    /// geometric derivation (pane rects, hit tests, the PTY sizing pump) reads
    /// them.
    surface_size: geometry.SizeF = geometry.SizeF.init(1100, 640),
    surface_scale_factor: f32 = 1,
    /// The platform window id this workspace is currently rendered into,
    /// learned from its own frame events. Zero until the first frame.
    ///
    /// Needed because a SHORTCUT event carries a window id and no label, so it
    /// is the only way "cmd+W in the window I am looking at" resolves to a
    /// workspace. The id is the platform's, not ours: it is not stable across
    /// a close and reopen, which is exactly why it is learned per frame rather
    /// than assigned.
    window_id: native_sdk.platform.WindowId = 0,

    pub fn tree(workspace: *Workspace, index: usize) ?*layout.Tree {
        if (index >= workspace.tab_count) return null;
        return &workspace.tabs[index];
    }

    pub fn treeConst(workspace: *const Workspace, index: usize) ?*const layout.Tree {
        if (index >= workspace.tab_count) return null;
        return &workspace.tabs[index];
    }

    pub fn selectedTree(workspace: *Workspace) ?*layout.Tree {
        if (workspace.web_selected) return null;
        return workspace.tree(workspace.selected_tab);
    }

    pub fn selectedTreeConst(workspace: *const Workspace) ?*const layout.Tree {
        if (workspace.web_selected) return null;
        return workspace.treeConst(workspace.selected_tab);
    }

    /// Keyboard focus follows the selected tab's own focused pane. Unfiltered
    /// on purpose: remote focus publication needs the ref the tree names even
    /// in the instant before the provider's bookkeeping catches up.
    pub fn focusedTerminalRef(workspace: *const Workspace) ?TerminalRef {
        const current = workspace.selectedTreeConst() orelse return null;
        return current.focusedTerminal();
    }

    /// The tab index whose tree holds `id` in any pane.
    pub fn tabOfTerminal(workspace: *const Workspace, id: TerminalRef) ?usize {
        for (workspace.tabs[0..workspace.tab_count], 0..) |candidate, index| {
            if (candidate.find(id) != null) return index;
        }
        return null;
    }

    /// The label identity of a tab: its focused pane's terminal.
    pub fn tabTerminal(workspace: *const Workspace, index: usize) ?TerminalRef {
        const current = workspace.treeConst(index) orelse return null;
        return current.focusedTerminal();
    }

    pub fn tabId(workspace: *const Workspace, index: usize) ?u32 {
        if (index >= workspace.tab_count) return null;
        const id = workspace.tab_ids[index];
        return if (id == 0) null else id;
    }

    fn mintTabId(workspace: *Workspace) u32 {
        while (true) {
            const candidate = workspace.next_tab_id;
            workspace.next_tab_id +%= 1;
            if (workspace.next_tab_id == 0) {
                workspace.next_tab_id = 1;
                workspace.tab_generation +|= 1;
            }
            if (candidate == 0) continue;
            var used = false;
            for (workspace.tab_ids[0..workspace.tab_count]) |existing| {
                if (existing == candidate) {
                    used = true;
                    break;
                }
            }
            if (!used) return candidate;
        }
    }

    pub fn assignRestoredTabId(workspace: *Workspace, index: usize) void {
        if (index >= workspace.tab_count or workspace.tab_ids[index] != 0) return;
        workspace.tab_ids[index] = workspace.mintTabId();
    }

    /// Give `id` a tab of its own. Idempotent: a terminal already living in
    /// some pane keeps the tab it is in.
    pub fn admitTab(workspace: *Workspace, id: TerminalRef) bool {
        if (workspace.tabOfTerminal(id) != null) return true;
        if (workspace.tab_count >= max_tabs) return false;
        workspace.tabs[workspace.tab_count] = layout.Tree.initLeaf(id);
        workspace.tab_ids[workspace.tab_count] = workspace.mintTabId();
        workspace.tab_count += 1;
        return true;
    }

    pub fn dropTab(workspace: *Workspace, index: usize) void {
        if (index >= workspace.tab_count) return;
        if (index < workspace.selected_tab) workspace.selected_tab -= 1;
        var cursor = index;
        while (cursor + 1 < workspace.tab_count) : (cursor += 1) {
            workspace.tabs[cursor] = workspace.tabs[cursor + 1];
            workspace.tab_ids[cursor] = workspace.tab_ids[cursor + 1];
            workspace.shared_ids[cursor] = workspace.shared_ids[cursor + 1];
        }
        workspace.tab_count -= 1;
        workspace.tabs[workspace.tab_count] = .{};
        workspace.tab_ids[workspace.tab_count] = 0;
        workspace.shared_ids[workspace.tab_count] = null;
        if (workspace.tab_count == 0) {
            workspace.selected_tab = 0;
            return;
        }
        if (workspace.selected_tab >= workspace.tab_count) workspace.selected_tab = workspace.tab_count - 1;
    }

    pub fn selectTab(workspace: *Workspace, index: usize) bool {
        if (index >= workspace.tab_count) return false;
        workspace.selected_tab = index;
        workspace.web_selected = false;
        return true;
    }

    /// Select the tab holding `id` AND focus the pane that holds it. This is
    /// what a tab click and cmd+T mean; it can never invent a pane.
    pub fn selectTerminal(workspace: *Workspace, id: TerminalRef) bool {
        const index = workspace.tabOfTerminal(id) orelse return false;
        workspace.selected_tab = index;
        workspace.web_selected = false;
        _ = workspace.tabs[index].focusTerminal(id);
        return true;
    }

    pub fn selectWeb(workspace: *Workspace) void {
        workspace.web_selected = true;
    }

    pub fn moveTerminal(workspace: *Workspace, id: TerminalRef, delta: i8) bool {
        const current = workspace.tabOfTerminal(id) orelse return false;
        const target_signed = @as(isize, @intCast(current)) + delta;
        if (target_signed < 0 or target_signed >= workspace.tab_count) return false;
        const target: usize = @intCast(target_signed);
        std.mem.swap(layout.Tree, &workspace.tabs[current], &workspace.tabs[target]);
        std.mem.swap(u32, &workspace.tab_ids[current], &workspace.tab_ids[target]);
        std.mem.swap(?[16]u8, &workspace.shared_ids[current], &workspace.shared_ids[target]);
        if (workspace.selected_tab == current) {
            workspace.selected_tab = target;
        } else if (workspace.selected_tab == target) {
            workspace.selected_tab = current;
        }
        return true;
    }
};

/// A window and a tab inside it — what a terminal-to-topology lookup has to
/// answer once there is more than one window, because a pty event names a
/// terminal and nothing else.
pub const TerminalLocation = struct { window: usize, tab: usize };
/// Stable switcher payload for a terminal that already has presentation
/// topology. `terminal_ref` is the identity fence; window and tab say where
/// that identity was projected when the row was built.
pub const PlacedTerminalDestination = struct {
    window: u8,
    tab: u8,
    terminal_ref: TerminalRef,
};

/// A working-set destination, carried unchanged by pointer, keyboard, and
/// accessibility activation. No arm borrows a provider catalog index.
pub const PaletteDestination = union(enum) {
    placed_terminal: PlacedTerminalDestination,
    available_terminal: TerminalRef,
    session: u32,
};

/// Replace the bounded remote inventory with the provider's latest complete
/// publication. The inventory ceiling is independent of every workspace's tab
/// ceiling: terminals remain discoverable after presentation fills up.
pub fn reconcileRemoteRefs(
    inventory: *[max_remote_terminals]TerminalRef,
    inventory_count: *usize,
    published: []const TerminalRef,
) void {
    const count = @min(inventory.len, published.len);
    @memcpy(inventory[0..count], published[0..count]);
    inventory_count.* = count;
}

pub const Model = struct {
    provider: *LocalProvider,
    /// The user's config, loaded once at startup and then MODEL STATE: the
    /// font-size chords mutate it, so everything downstream (design tokens,
    /// cell metrics, the PTY sizing pump) reads it from here rather than
    /// from the file it came from.
    config: Config = .{},
    /// Live cmd+= / cmd+- delta over `config.font_size`, in points.
    font_size_offset: f32 = 0,
    /// Backing storage for the cwd-carrying argv of each registry slot.
    ///
    /// `Pane.argv` is a SLICE, and `spawnPane` re-reads it on every Restart,
    /// so the bytes have to outlive the spawn that first used them — stack
    /// storage at the spawn site would dangle the instant it returned. The
    /// storage lives HERE rather than on `Pane` because the pane's layout is
    /// owned by the provider, and because the model outlives every pane in
    /// it: a slot reused by a new terminal is rewritten before that
    /// terminal's first spawn, so no live argv ever aliases a dead one.
    cwd_argv: [max_terminals]local.CwdArgv = [_]local.CwdArgv{.{}} ** max_terminals,
    phux_provider: ?*PhuxProvider = null,
    /// The configured Phux provider could not reach or attach a server-owned
    /// session. Local terminals remain usable but are explicitly ephemeral;
    /// the chrome keeps this difference visible until a complete attach lands.
    phux_connection_unavailable: bool = false,
    /// A deliberate session switch waits for the old channel's close event
    /// before reusing its effect key. Opening immediately races the close and
    /// the runtime correctly rejects the duplicate key.
    phux_reconnect_after_close: bool = false,
    /// Shared topology is a projection of confirmed provider snapshots.
    shared_workspace: @import("shared_workspace.zig").State = .{},
    shared_mutations: @import("shared_mutations.zig").Coordinator = .{},
    /// Selection/search belong to live replicas, not the whole-server catalog.
    remote_ui: [max_terminals]RemoteUiState = [_]RemoteUiState{.{}} ** max_terminals,
    /// Display evidence belongs to the placement, not the current connection.
    frozen_paint: [topology.max_terminals]?FrozenPaint = @splat(null),

    /// The provider's complete bounded terminal publication, independent of
    /// which terminals currently have Cockpit topology leaves.
    remote_inventory: [max_remote_terminals]TerminalRef = undefined,
    remote_inventory_count: usize = 0,
    pointer_state: ?*PointerState = null,
    /// Window 0's workspace, INLINE.
    ///
    /// Deliberately not one of N by-value slots. A `layout.Tree` is 9,432
    /// bytes and there are `max_tabs` of them, so a workspace is ~151 KB;
    /// five of those inside a model that is itself passed by value through
    /// `TerminalApp.init` would be three quarters of a megabyte of stack per
    /// construction, and this repo has already taken a real main-thread stack
    /// overflow from exactly that shape (see the comment on
    /// `tests/support.zig:initTerminalApp`). Window 0 always exists, so it
    /// costs nothing to keep here; windows 1..N are minted on demand.
    primary: Workspace = .{},
    /// Windows 1..`max_secondary_windows`, heap-allocated when cmd+N opens
    /// them and freed when they close. A null slot IS a closed window: the
    /// declared-window set the runtime reconciles is derived from this array,
    /// so there is no second "is it open" flag to drift.
    secondary: [max_secondary_windows]?*Workspace = @splat(null),
    /// Which window input is currently addressed to. Every message that
    /// reshapes a workspace acts on THIS one; the host moves it as input
    /// arrives from a window, and `.focus_window` is the seam a test drives.
    active_window: usize = 0,
    /// Whether the scene's own window is still on screen. cmd+W through the
    /// main window's last tab closes it while secondaries are still open, and
    /// the app quits only once nothing is left.
    primary_open: bool = true,
    /// A cmd+N was refused because every window slot is taken.
    ///
    /// A latch rather than a transient, because the refusal has to be VISIBLE:
    /// a chord that silently does nothing is indistinguishable from a chord
    /// that is not bound. The chrome reads it, and the next successful window
    /// open — or any other window closing — clears it.
    window_limit_refused: bool = false,
    /// The startup config notice has been read and dismissed.
    ///
    /// A latch on the MODEL, not on a workspace: the config is app-wide, and a
    /// notice that had to be dismissed once per window would be four dismissals
    /// for one typo. It is deliberately not persisted either — a config file is
    /// re-read on every launch, so "I already saw this" is only true until the
    /// user edits the file, and the state file cannot know that.
    config_notice_dismissed: bool = false,
    /// A cmd+T or a split was refused because every live shell the effects
    /// layer can back is already running (see `local.max_live_shells`).
    ///
    /// Same latch, same reason, and one worse failure to avoid: before this
    /// existed the chord did not do nothing, it opened a tab whose pane stayed
    /// permanently blank because its spawn had been rejected. Cleared by the
    /// next successful terminal, and by anything that frees a shell.
    terminal_limit_refused: bool = false,
    /// A settings commit could not be written to the config file.
    ///
    /// The same latch as the two above, for the same reason and against the
    /// worst version of the failure they exist to prevent: the theme IS applied
    /// live, so the gesture looks like it worked, and the disagreement between
    /// the running app and the file surfaces one launch later as "it did not
    /// persist" with nothing on screen ever having said so. `writeConfigTheme`
    /// was documented as silent at every failure; this is where that silence
    /// stops. Cleared by the next save that lands.
    config_write_refused: bool = false,
    tab_placement: TabPlacement = .top,
    browser_page: BrowserPage = .github,
    browser_navigation_token: u64 = 0,
    /// The parked WebKit child exists. False through the first useful canvas
    /// present so scene loading cannot put WebKit process startup in front of
    /// the synchronous startup-frame flush; set only after createView lands.
    webview_materialized: bool = false,
    /// Number of weighted font companions installed through the late-font
    /// seam. A count, rather than a bool, makes a partial registration failure
    /// retry from the first face that did not land.
    deferred_font_count: u8 = 0,
    /// The automatic post-present resource attempt has run. Failures never
    /// retry on the frame clock; an explicit Web-surface focus retries instead.
    post_present_resources_attempted: bool = false,
    focused: bool = true,
    consumed_shortcut_keys_held: u64 = 0,
    held_terminal_keys: [max_held_terminal_keys]HeldTerminalKey = [_]HeldTerminalKey{.{}} ** max_held_terminal_keys,
    pointer_captures: [max_pointer_captures]PointerCapture = [_]PointerCapture{.{ .terminal_id = .terminal_1 }} ** max_pointer_captures,
    copy_inflight: bool = false,
    copy_owner: ReplicaOwner = .{
        .terminal_ref = provider_contract.localTerminalRef(.terminal_1),
        .generation = .{},
    },
    paste_inflight: bool = false,
    paste_owner: ReplicaOwner = .{
        .terminal_ref = provider_contract.localTerminalRef(.terminal_1),
        .generation = .{},
    },
    paste_failed: bool = false,
    /// Where the in-flight clipboard read is going to land. A paste issued
    /// while the scrollback search field is up belongs to the NEEDLE, not to
    /// the child process, and the two differ in more than destination: a
    /// needle paste is legitimate against a pane that no longer accepts input,
    /// because searching a dead session's scrollback is ordinary work.
    paste_target: PasteTarget = .terminal,
    /// The last URL handed to the OS to open, and how many have been handed
    /// over. Kept on the model rather than being fire-and-forget so the
    /// gesture is observable — a click that opens the WRONG link, or opens one
    /// when it should not have, is the failure mode worth being able to assert
    /// on, and an effect alone leaves nothing to assert against.
    opened_url_buf: [url_module.max_url_bytes]u8 = undefined,
    opened_url_len: usize = 0,
    opened_url_count: u32 = 0,
    /// Where the layout snapshot goes and what is owed to it. Disabled by
    /// default so every test and every fixture stays free of disk traffic
    /// until a composition root hands it a real path.
    state: StatePersistence = .{},
    /// Where the config file this session loaded from lives, so a choice made
    /// in the settings surface can be written back to it. Disabled by default,
    /// same as `state`.
    config_file: ConfigFile = .{},
    /// The live pinch gesture's accumulated magnification, as a running
    /// product of the platform's `(1 + scale)` deltas minus one.
    ///
    /// A trackpad reports magnification in continuous fractions and this
    /// terminal's type size is a whole number of points, so the two need a
    /// gearbox: the accumulator holds what the fingers have done since the
    /// last step, and `pinchStep` spends it a point at a time. Nothing
    /// downstream reads it — it exists so that a slow pinch is not a stream of
    /// no-ops and a fast one is not a jump.
    pinch_scale: f32 = 0,
    /// How many desktop notifications this session has posted, and for what.
    ///
    /// A bell is fire-and-forget at the platform (the OS owns whether a banner
    /// is drawn, so no result Msg could be honest), which leaves nothing to
    /// assert on — exactly the shape `opened_url_*` already solved. The count
    /// and the last title are the observable evidence that the edge fired
    /// once, and only once, per bell.
    notified_title_buf: [max_notification_title_bytes]u8 = undefined,
    notified_title_len: usize = 0,
    notification_count: u32 = 0,
    /// The live tab drag, or null when no tab is being dragged.
    tab_drag: ?TabDrag = null,
    /// Saved references carry evidence, never live provider ownership.
    attachment_context: topology.attachments.Context = .{},
    saved_attachments: topology.attachments.Table = .{},
    pending_attachments: [topology.attachments.max_references]bool = @splat(false),
    /// Async placements cannot acquire a later workspace that reuses a slot.
    window_epochs: [max_windows]u64 = @splat(0),

    // -------------------------------------------------------- windows

    /// The workspace of the window input is currently addressed to.
    ///
    /// Window 0 is the fallback for an `active_window` whose slot has since
    /// closed: a message that arrives one dispatch after its window went away
    /// must land somewhere real rather than reach through a null.
    /// The last URL handed to the OS, or empty when none has been.
    pub fn openedUrl(model: *const Model) []const u8 {
        return model.opened_url_buf[0..model.opened_url_len];
    }

    /// Record a URL as handed over. False when it does not fit, in which case
    /// nothing is recorded and nothing should be opened either.
    pub fn recordOpenedUrl(model: *Model, value: []const u8) bool {
        if (value.len == 0 or value.len > model.opened_url_buf.len) return false;
        @memcpy(model.opened_url_buf[0..value.len], value);
        model.opened_url_len = value.len;
        model.opened_url_count += 1;
        return true;
    }

    /// The title of the last notification posted, or empty when none has been.
    pub fn notifiedTitle(model: *const Model) []const u8 {
        return model.notified_title_buf[0..model.notified_title_len];
    }

    /// Record a notification as posted. A title too long to record is a
    /// notification NOT posted: the record is the only evidence the gesture
    /// happened, and a banner with no trace of it would be exactly the
    /// unassertable thing this counter exists to prevent.
    pub fn recordNotification(model: *Model, title: []const u8) bool {
        if (title.len == 0 or title.len > model.notified_title_buf.len) return false;
        @memcpy(model.notified_title_buf[0..title.len], title);
        model.notified_title_len = title.len;
        model.notification_count += 1;
        return true;
    }

    /// Fold one pinch delta into the gesture, and answer the whole points of
    /// type size it has earned.
    ///
    /// The platform's `scale` is multiplicative per event (`zoom *= 1 +
    /// scale`), so the accumulator is a product rather than a sum. One point
    /// per `pinch_points_per_step` of magnification is the gearing: it takes a
    /// deliberate pinch to move a step, and a long one keeps stepping without
    /// ever needing gesture-start bookkeeping. The spent portion is taken out
    /// of the accumulator rather than cleared, so the remainder carries into
    /// the next event and a slow pinch loses nothing.
    ///
    /// `.begin` and `.end` carry no magnification of their own; the reset on
    /// `.begin` is what keeps two separate gestures from adding up.
    pub fn pinchStep(model: *Model, phase: native_sdk.platform.PinchPhase, scale: f32) i8 {
        if (phase == .begin) {
            model.pinch_scale = 0;
            return 0;
        }
        if (phase != .change) return 0;
        if (!std.math.isFinite(scale)) return 0;
        model.pinch_scale = (1 + model.pinch_scale) * (1 + scale) - 1;
        const steps = std.math.trunc(model.pinch_scale / pinch_points_per_step);
        if (steps == 0) return 0;
        model.pinch_scale -= steps * pinch_points_per_step;
        return std.math.lossyCast(i8, steps);
    }

    pub fn ws(model: *Model) *Workspace {
        return model.wsAt(model.active_window) orelse &model.primary;
    }

    pub fn wsConst(model: *const Model) *const Workspace {
        return model.wsAtConst(model.active_window) orelse &model.primary;
    }

    pub fn wsAt(model: *Model, index: usize) ?*Workspace {
        if (index == 0) return &model.primary;
        if (index > max_secondary_windows) return null;
        return model.secondary[index - 1];
    }

    pub fn wsAtConst(model: *const Model, index: usize) ?*const Workspace {
        if (index == 0) return &model.primary;
        if (index > max_secondary_windows) return null;
        return model.secondary[index - 1];
    }

    /// Whether window `index` is on screen. Window 0 answers `primary_open`
    /// because its window is the SCENE's and is not declared by the model;
    /// every other window's existence is its slot.
    pub fn windowOpen(model: *const Model, index: usize) bool {
        if (index == 0) return model.primary_open;
        if (index > max_secondary_windows) return false;
        return model.secondary[index - 1] != null;
    }

    pub fn openWindowCount(model: *const Model) usize {
        var count: usize = 0;
        for (0..max_windows) |index| {
            if (model.windowOpen(index)) count += 1;
        }
        return count;
    }

    /// The lowest free secondary slot, or null at the ceiling. Lowest-first so
    /// a closed-and-reopened window reuses its label and its declared canvas
    /// rather than walking up the namespace until it runs out.
    pub fn freeWindowIndex(model: *const Model) ?usize {
        for (model.secondary, 0..) |slot, offset| {
            if (slot == null) return offset + 1;
        }
        return null;
    }

    /// Mint window `index`'s workspace. The caller owns the decision that the
    /// slot is free; a slot already taken is left exactly as it was.
    pub fn openWindow(model: *Model, index: usize) ?*Workspace {
        if (index == 0) {
            model.primary_open = true;
            return &model.primary;
        }
        if (index > max_secondary_windows) return null;
        if (model.secondary[index - 1]) |existing| return existing;
        const workspace = std.heap.page_allocator.create(Workspace) catch return null;
        workspace.* = .{};
        model.secondary[index - 1] = workspace;
        return workspace;
    }

    /// Retire window `index`. The workspace's tabs are NOT torn down here —
    /// every close path drains them through the ordinary pane-close cascade
    /// first, so this only releases storage the model no longer names.
    pub fn closeWindow(model: *Model, index: usize) void {
        if (index < max_windows) model.window_epochs[index] +|= 1;
        if (index == 0) {
            model.primary_open = false;
            model.primary = .{};
        } else if (index <= max_secondary_windows) {
            if (model.secondary[index - 1]) |workspace| {
                std.heap.page_allocator.destroy(workspace);
                model.secondary[index - 1] = null;
            }
        }
        if (model.active_window == index) model.active_window = model.firstOpenWindow();
        model.pruneAttachmentState();
    }

    /// The lowest-numbered window still open, or 0 when none is. Used as the
    /// landing place for input after the active window goes away.
    pub fn firstOpenWindow(model: *const Model) usize {
        for (0..max_windows) |index| {
            if (model.windowOpen(index)) return index;
        }
        return 0;
    }

    /// Where a terminal lives, across EVERY window.
    ///
    /// A pty exit event names a terminal and nothing else, and the terminal
    /// may belong to a window that is not the active one — so the close
    /// cascade cannot ask the active workspace and take null for an answer.
    pub fn locateTerminal(model: *const Model, id: TerminalRef) ?TerminalLocation {
        for (0..max_windows) |index| {
            const workspace = model.wsAtConst(index) orelse continue;
            if (workspace.tabOfTerminal(id)) |tab| return .{ .window = index, .tab = tab };
        }
        return null;
    }

    pub fn phux(model: *Model) ?*PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        return model.phux_provider;
    }

    pub fn phuxConst(model: *const Model) ?*const PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        return model.phux_provider;
    }

    pub fn containsTerminal(model: *const Model, terminal_ref: TerminalRef) bool {
        if (model.attachmentPending(terminal_ref)) return false;
        return switch (support.providerKind(terminal_ref)) {
            .local => model.provider.contains(terminal_ref),
            .phux => if (model.phuxConst()) |remote| remote.contains(terminal_ref) else false,
        };
    }

    pub fn terminalOwner(model: *const Model, terminal_ref: TerminalRef) ?ReplicaOwner {
        if (model.attachmentPending(terminal_ref)) return null;
        return switch (support.providerKind(terminal_ref)) {
            .local => model.provider.owner(terminal_ref),
            .phux => if (model.phuxConst()) |remote| remote.owner(terminal_ref) else null,
        };
    }

    pub fn ownerIsCurrent(model: *const Model, owner_value: ReplicaOwner) bool {
        if (model.attachmentPending(owner_value.terminal_ref)) return false;
        return switch (support.providerKind(owner_value.terminal_ref)) {
            .local => model.provider.ownerIsCurrent(owner_value),
            .phux => if (model.phuxConst()) |remote| remote.ownerIsCurrent(owner_value) else false,
        };
    }

    /// The agent sessions running under one terminal, in catalog order.
    ///
    /// A ROW, never a surface: an AgentSession publishes no replica, so there
    /// is deliberately no path from one of these to a `Presentation`. The
    /// answer borrows the provider's roster and is stable until the next
    /// drain, like every other provider projection here.
    pub fn agentSessionsUnder(model: *const Model, terminal_ref: TerminalRef, out: []*const AgentSession) usize {
        if (comptime !support.phux_enabled) return 0;
        if (support.providerKind(terminal_ref) != .phux) return 0;
        const remote = model.phuxConst() orelse return 0;
        return remote.agentSessionsUnder(terminal_ref, out);
    }

    /// Whether an agent under this terminal is waiting on a person. One SOURCE
    /// for the quiet attention path, read the same way the bell latch is.
    pub fn agentAttention(model: *const Model, terminal_ref: TerminalRef) bool {
        if (comptime !support.phux_enabled) return false;
        if (support.providerKind(terminal_ref) != .phux) return false;
        const remote = model.phuxConst() orelse return false;
        return remote.agentAttention(terminal_ref);
    }

    /// Whether this identity names an agent session rather than a terminal.
    pub fn isAgentSession(model: *const Model, terminal_ref: TerminalRef) bool {
        if (comptime !support.phux_enabled) return false;
        if (support.providerKind(terminal_ref) != .phux) return false;
        const remote = model.phuxConst() orelse return false;
        return remote.isAgentSession(terminal_ref);
    }

    pub fn remotePresentation(model: *const Model, terminal_ref: TerminalRef) ?Presentation {
        if (model.attachmentPending(terminal_ref)) return null;
        if (support.providerKind(terminal_ref) != .phux) return null;
        const remote = model.phuxConst() orelse return null;
        return remote.presentation(terminal_ref);
    }

    /// Paint alone may read the previously proven display while a replacement
    /// is pending. Never consult the provider's new publication in this branch.
    pub fn remotePaintPresentation(model: *const Model, ref: TerminalRef) ?Presentation {
        if (model.locateTerminal(ref) == null) return null;
        if (!model.attachmentPending(ref)) return model.remotePresentation(ref);
        if (comptime !support.phux_enabled) return null;
        for (model.frozen_paint) |entry| {
            const frozen = entry orelse continue;
            if (frozen.reference.terminal_ref.eql(ref)) return frozen.snapshot.value;
        }
        return null;
    }

    pub const FrozenPaint = struct {
        reference: topology.attachments.Reference,
        owner: ReplicaOwner,
        snapshot: *const if (support.phux_enabled) PhuxProvider.FrozenPresentation else void,
    };

    /// Called by the shipping disconnect path BEFORE rejecting context. Pending
    /// or unplaced identities can never overwrite the previous accepted image.
    pub fn captureRemotePaint(model: *Model) void {
        if (comptime !support.phux_enabled) return;
        const remote = model.phuxConst() orelse return;
        model.captureAttachmentContexts() catch return;
        for (model.saved_attachments.entries[0..model.saved_attachments.count]) |entry| {
            const reference = entry orelse continue;
            model.captureRemotePaintReference(remote, reference);
        }
    }

    fn captureRemotePaintReference(model: *Model, remote: *const PhuxProvider, reference: topology.attachments.Reference) void {
        if (comptime !support.phux_enabled) return;
        const ref = reference.terminal_ref;
        if (!reference.matches(&model.attachment_context)) return;
        const value = model.remotePresentation(ref) orelse return;
        // A transport failure may already have frozen the provider, but the
        // model's accepted context and exact owner still prove that image.
        model.releaseRemotePaint(ref);
        for (&model.frozen_paint) |*slot| {
            if (slot.* != null) continue;
            const snapshot = remote.capturePresentation(value.owner) catch return;
            slot.* = .{ .reference = reference, .owner = value.owner, .snapshot = snapshot };
            return;
        }
    }

    fn releaseRemotePaint(model: *Model, ref: TerminalRef) void {
        if (comptime !support.phux_enabled) return;
        for (&model.frozen_paint) |*slot| {
            const frozen = slot.* orelse continue;
            if (!frozen.reference.terminal_ref.eql(ref)) continue;
            @constCast(frozen.snapshot).destroy();
            slot.* = null;
        }
    }

    fn pruneRemotePaint(model: *Model) void {
        for (model.frozen_paint) |entry| {
            const frozen = entry orelse continue;
            const ref = frozen.reference.terminal_ref;
            if (model.locateTerminal(ref) != null and !model.hasLiveRemotePaint(ref)) continue;
            model.releaseRemotePaint(frozen.reference.terminal_ref);
        }
    }

    fn hasLiveRemotePaint(model: *const Model, ref: TerminalRef) bool {
        const presentation = model.remotePresentation(ref) orelse return false;
        return presentation.phase == .live;
    }

    fn clearRemotePaint(model: *Model) void {
        for (model.frozen_paint) |entry| {
            const frozen = entry orelse continue;
            model.releaseRemotePaint(frozen.reference.terminal_ref);
        }
    }

    pub fn remoteUi(model: *Model, terminal_ref: TerminalRef) ?*RemoteUiState {
        const current_owner = model.terminalOwner(terminal_ref) orelse return null;
        var vacant: ?*RemoteUiState = null;
        for (&model.remote_ui) |*state| {
            if (state.terminal_ref) |known| {
                if (!known.eql(terminal_ref)) continue;
                if (!state.owner.eql(current_owner)) state.replaceOwner(current_owner, model.attachment_context);
                // READY can precede the first metadata read. Bind UI created
                // in that interval once this same replica's session is proven.
                if (state.attachment_context.session_id == 0) state.attachment_context = model.attachment_context;
                return state;
            }
            if (vacant == null) vacant = state;
        }
        const state = vacant orelse return null;
        state.* = .{ .terminal_ref = terminal_ref, .owner = current_owner, .attachment_context = model.attachment_context };
        return state;
    }

    pub fn remoteUiConst(model: *const Model, terminal_ref: TerminalRef) ?*const RemoteUiState {
        // Retained presentation may be frozen, but must still belong to this
        // exact published owner. terminalOwner also enforces attachmentPending.
        const current_owner = model.terminalOwner(terminal_ref) orelse return null;
        for (&model.remote_ui) |*state| {
            if (state.terminal_ref == null) continue;
            if (state.owner.eql(current_owner)) return state;
        }
        return null;
    }
    pub fn remoteTerminalRefs(model: *const Model) []const TerminalRef {
        return model.remote_inventory[0..model.remote_inventory_count];
    }

    /// Saved identities survive unavailable providers without acquiring live
    /// ownership. A matching HELLO/ATTACHED context is necessary but readiness
    /// still comes from the provider after subscription/bootstrap.
    pub fn setAttachmentContext(model: *Model, endpoint: []const u8, server_id: []const u8, session_id: u32) !void {
        const context = try topology.attachments.Context.init(endpoint, server_id, session_id);
        // Freeze the old placement evidence before changing the current
        // connection. Otherwise a live leaf's reused numeric ID would acquire
        // the next server's identity on the next save.
        try model.captureAttachmentContexts();
        model.attachment_context = context;
        for (model.saved_attachments.entries[0..model.saved_attachments.count], 0..) |entry, index| {
            if (!entry.?.matches(&context)) model.pending_attachments[index] = true;
        }
    }

    /// Disconnect/rejection invalidates all saved readiness, retaining both
    /// the placement and the original evidence for a later matching server.
    pub fn rejectAttachmentContext(model: *Model) void {
        model.captureAttachmentContexts() catch {};
        model.attachment_context = .{};
        @memset(model.pending_attachments[0..model.saved_attachments.count], true);
    }

    pub fn attachmentPending(model: *const Model, ref: TerminalRef) bool {
        const index = model.saved_attachments.find(ref) orelse return false;
        return model.pending_attachments[index];
    }

    pub fn pendingRestoredRefs(model: *const Model, out: []TerminalRef) usize {
        var count: usize = 0;
        for (model.saved_attachments.entries[0..model.saved_attachments.count], 0..) |entry, index| {
            if (!model.pending_attachments[index]) continue;
            const ref = entry.?.terminal_ref;
            if (model.locateTerminal(ref) == null) continue;
            if (count == out.len) break;
            out[count] = ref;
            count += 1;
        }
        return count;
    }

    pub fn restoredAttachmentMatches(model: *const Model, ref: TerminalRef) bool {
        const index = model.saved_attachments.find(ref) orelse return false;
        return model.saved_attachments.entries[index].?.matches(&model.attachment_context);
    }

    pub fn restoredAttachmentContext(model: *const Model, ref: TerminalRef) ?topology.attachments.Context {
        const index = model.saved_attachments.find(ref) orelse return null;
        return model.saved_attachments.entries[index].?.context;
    }

    pub fn resolveRestoredAttachment(model: *Model, ref: TerminalRef) bool {
        if (!model.restoredAttachmentMatches(ref)) return false;
        const remote = model.phuxConst() orelse return false;
        const presentation = remote.presentation(ref) orelse return false;
        if (presentation.phase != .live) return false;
        const index = model.saved_attachments.find(ref).?;
        model.pending_attachments[index] = false;
        model.releaseRemotePaint(ref);
        return true;
    }

    fn attachmentReference(model: *const Model, ref: TerminalRef) topology.attachments.Reference {
        if (model.saved_attachments.find(ref)) |index| return model.saved_attachments.entries[index].?;
        return .{ .terminal_ref = ref, .context = model.attachment_context };
    }

    fn captureAttachmentContexts(model: *Model) !void {
        var table: topology.attachments.Table = .{};
        var pending: [topology.attachments.max_references]bool = @splat(false);
        for (0..max_windows) |window_index| {
            const workspace = model.wsAtConst(window_index) orelse continue;
            for (workspace.tabs[0..workspace.tab_count]) |current| {
                var refs: [layout.max_panes]TerminalRef = undefined;
                const count = current.terminals(&refs);
                for (refs[0..count]) |ref| {
                    if (ref.terminal_id != .phux) continue;
                    const index = try table.append(model.attachmentReference(ref));
                    pending[index] = model.attachmentPending(ref);
                }
            }
        }
        model.saved_attachments = table;
        model.pending_attachments = pending;
    }

    /// Call after direct pane-tree removals, before admitting new identities.
    /// Moves retain their evidence because their destination still holds the ref.
    pub fn pruneAttachmentState(model: *Model) void {
        model.pruneRemotePaint();
        var retained: topology.attachments.Table = .{};
        var pending: [topology.attachments.max_references]bool = @splat(false);
        for (model.saved_attachments.entries[0..model.saved_attachments.count], 0..) |entry, index| {
            const reference = entry orelse continue;
            if (model.locateTerminal(reference.terminal_ref) == null) continue;
            retained.entries[retained.count] = reference;
            pending[retained.count] = model.pending_attachments[index];
            retained.count += 1;
        }
        model.saved_attachments = retained;
        model.pending_attachments = pending;
    }

    /// Preflight before allocating or splitting: the durable budget covers
    /// both providers and unresolved placements, not only local shell slots.
    pub fn canAddPane(model: *const Model) bool {
        var count: usize = 0;
        for (0..max_windows) |window_index| {
            const workspace = model.wsAtConst(window_index) orelse continue;
            for (workspace.tabs[0..workspace.tab_count]) |current| count += current.paneCount();
        }
        return count < topology.max_terminals;
    }

    // ------------------------------------------------------------ tabs
    //
    // Everything below addresses the ACTIVE window's workspace. These stayed
    // on `Model` rather than being spelled `model.ws().x` at every call site
    // because the two say the same thing and the shorter one keeps the ~120
    // callers honest; the explicit `wsAt(index)` form is what a caller that
    // means a SPECIFIC window (the per-window view, the per-window painter)
    // reaches for, and the type system keeps the two apart.

    pub fn tree(model: *Model, index: usize) ?*layout.Tree {
        return model.ws().tree(index);
    }

    pub fn treeConst(model: *const Model, index: usize) ?*const layout.Tree {
        return model.wsConst().treeConst(index);
    }

    pub fn selectedTree(model: *Model) ?*layout.Tree {
        return model.ws().selectedTree();
    }

    pub fn selectedTreeConst(model: *const Model) ?*const layout.Tree {
        return model.wsConst().selectedTreeConst();
    }

    /// The surface the content area shows. Derived: there is no stored copy
    /// to drift from the tab list.
    pub fn selectedSurface(model: *const Model) SurfaceSelection {
        const terminal_ref = model.focusedTerminalRef() orelse return .web;
        return .{ .terminal = terminal_ref };
    }

    /// The focused pane's terminal, filtered to one a provider still vouches
    /// for. Callers that route INPUT use this: a ref no provider owns must
    /// not be treated as a live selection.
    pub fn selectedTerminalRef(model: *const Model) ?TerminalRef {
        const id = model.focusedTerminalRef() orelse return null;
        return if (model.containsTerminal(id)) id else null;
    }

    pub fn selectedResourceId(model: *const Model) ?TerminalRef {
        return model.selectedTerminalRef();
    }

    /// Keyboard focus follows the selected tab's own focused pane. Selection
    /// and focus were separate fields under the two-pane model and could
    /// disagree; a tree has exactly one focused leaf. Unfiltered on purpose:
    /// remote focus publication needs the ref the tree names even in the
    /// instant before the provider's own bookkeeping catches up.
    pub fn focusedTerminalRef(model: *const Model) ?TerminalRef {
        return model.wsConst().focusedTerminalRef();
    }

    pub fn focusedResourceId(model: *const Model) ?TerminalRef {
        return model.focusedTerminalRef();
    }

    pub fn focusedPane(model: *Model) ?*Pane {
        const id = model.focusedTerminalRef() orelse return null;
        return model.provider.terminal(id);
    }

    /// The tab index whose tree holds `id` in any pane of the ACTIVE window.
    pub fn tabOfTerminal(model: *const Model, id: TerminalRef) ?usize {
        return model.wsConst().tabOfTerminal(id);
    }

    pub fn terminalOrderIndex(model: *const Model, id: TerminalRef) ?usize {
        return model.tabOfTerminal(id);
    }

    /// The label identity of a tab: its focused pane's terminal.
    pub fn tabTerminal(model: *const Model, index: usize) ?TerminalRef {
        return model.wsConst().tabTerminal(index);
    }

    /// Give `id` a tab of its own in the active window. Idempotent: a
    /// terminal already living in some pane keeps the tab it is in.
    ///
    /// A terminal that belongs to ANOTHER window is refused rather than
    /// duplicated: two windows holding one terminal would be two trees
    /// driving one emulator, which is the exact invariant `validate` already
    /// enforces within a window.
    pub fn admitTab(model: *Model, id: TerminalRef) bool {
        if (model.locateTerminal(id)) |where| return where.window == model.active_window;
        if (!model.canAddPane()) return false;
        model.pruneAttachmentState();
        return model.ws().admitTab(id);
    }

    /// Backwards-compatible alias: admitting a terminal now means giving it
    /// a tab.
    pub fn admitToOrder(model: *Model, id: TerminalRef) bool {
        return model.admitTab(id);
    }

    pub fn dropTab(model: *Model, index: usize) void {
        model.ws().dropTab(index);
        model.pruneAttachmentState();
    }

    pub fn dropFromOrder(model: *Model, index: usize) void {
        model.dropTab(index);
    }

    pub fn selectTab(model: *Model, index: usize) bool {
        return model.ws().selectTab(index);
    }

    /// Select the tab holding `id` AND focus the pane that holds it. This is
    /// what a tab click and cmd+T mean; it can never invent a pane.
    pub fn selectTerminal(model: *Model, id: TerminalRef) bool {
        return model.ws().selectTerminal(id);
    }

    pub fn selectWeb(model: *Model) void {
        model.ws().selectWeb();
    }

    pub fn moveTerminal(model: *Model, id: TerminalRef, delta: i8) bool {
        return model.ws().moveTerminal(id, delta);
    }

    /// Drop panes whose terminal no longer exists, then drop tabs that lost
    /// every pane. Called after any provider publication.
    ///
    /// Runs over EVERY window: a provider publication is app-wide, and a
    /// retired terminal has to leave whichever window's tree happened to hold
    /// it, not only the one that is in front.
    pub fn normalizeTopology(model: *Model) void {
        // A missing replica is not a removed shared leaf. The authoritative
        // workspace snapshot, rather than stream readiness, retires those panes.
        if (model.phuxConst() != null) return;
        for (0..max_windows) |window_index| {
            const workspace = model.wsAt(window_index) orelse continue;
            var index: usize = 0;
            while (index < workspace.tab_count) {
                var current = &workspace.tabs[index];
                var refs: [layout.max_panes]TerminalRef = undefined;
                const count = current.terminals(&refs);
                for (refs[0..count]) |candidate| {
                    if (model.attachmentPending(candidate)) continue;
                    if (!model.containsTerminal(candidate)) _ = current.closeTerminal(candidate);
                }
                if (current.isEmpty()) workspace.dropTab(index) else index += 1;
            }
            if (workspace.tab_count == 0) {
                // Only the MAIN window has a web surface to fall back to; a
                // secondary window with no tabs is a window with nothing in
                // it, which the close cascade retires.
                workspace.web_selected = window_index == 0;
                workspace.selected_tab = 0;
                continue;
            }
            if (workspace.selected_tab >= workspace.tab_count) workspace.selected_tab = workspace.tab_count - 1;
        }
        model.pruneAttachmentState();
    }

    /// Reconcile provider inventory and presentation state without admitting
    /// topology. Discovery is not a focus or allocation gesture.
    pub fn reconcileRemoteTerminals(model: *Model) void {
        const remote = model.phuxConst() orelse return;
        var published: [max_remote_terminals]TerminalRef = undefined;
        const published_count = remote.catalogRefs(&published);
        reconcileRemoteRefs(
            &model.remote_inventory,
            &model.remote_inventory_count,
            published[0..published_count],
        );

        for (&model.remote_ui) |*state| {
            const known = state.terminal_ref orelse continue;
            if (!remote.terminalKnown(known) and model.locateTerminal(known) == null) state.* = .{};
        }
        for (model.remoteTerminalRefs()) |terminal_ref| _ = model.remoteUi(terminal_ref);
    }

    /// Whether the config band is up: there is something to say and nobody has
    /// said "seen it" yet. Derived, with no second flag to drift — an empty
    /// diagnostic list can never show a band, whatever the latch says.
    pub fn configNoticeVisible(model: *const Model) bool {
        return model.config.diagnostic_count != 0 and !model.config_notice_dismissed;
    }

    /// The LIVE terminal type size. `font_size_offset` is the live delta the
    /// cmd+= / cmd+- chords own; the config's own size is the origin cmd+0
    /// returns to, which is why the two are separate — resetting to a
    /// hardcoded 13 would throw away the `font-size` the user wrote down.
    pub fn fontSize(model: *const Model) f32 {
        return std.math.clamp(
            model.config.fontSize() + model.font_size_offset,
            config_module.min_font_size,
            config_module.max_font_size,
        );
    }

    /// Step the terminal type size by whole points and say whether it moved.
    /// A step at the clamp is a no-op rather than a silently growing number
    /// that reflows nothing.
    pub fn stepFontSize(model: *Model, delta: f32) bool {
        const before = model.fontSize();
        model.font_size_offset += delta;
        const after = model.fontSize();
        if (after == before) {
            // Undo the step that fell off the end, so the offset cannot
            // accumulate an arbitrarily long way past the clamp and make
            // the first step back a dead key.
            model.font_size_offset -= delta;
            return false;
        }
        return true;
    }

    pub fn resetFontSize(model: *Model) bool {
        if (model.font_size_offset == 0) return false;
        model.font_size_offset = 0;
        return true;
    }

    // ------------------------------------------------------ persistence

    pub fn topologySnapshot(model: *const Model) !TopologySnapshot {
        var snapshot: TopologySnapshot = .{ .tab_placement = model.tab_placement };
        // Remote topology has one persisted authority: Phux layout metadata.
        // Older local files are superseded at the first shared publication.
        if (model.shared_workspace.session != 0) return snapshot;
        var written: u8 = 0;
        var windows: u8 = 0;
        // Windows are written in INDEX order and renumbered densely, because
        // the slot a window happened to occupy is an allocation detail: a
        // session whose second window was closed must not restore as a hole.
        for (0..max_windows) |window_index| {
            if (!model.windowOpen(window_index)) continue;
            const workspace = model.wsAtConst(window_index) orelse continue;
            snapshot.windows[windows] = try encodeWindow(model, workspace, &snapshot, &written);
            windows += 1;
        }
        snapshot.window_count = windows;
        snapshot.tab_count = written;

        // Directories are read off the LIVE panes rather than carried in the
        // tree, because the tree does not know them: a cwd is whatever the
        // shell last reported through OSC 7, and a pane that never reported
        // one records nothing (which restores as "$HOME", the first
        // terminal's behaviour, not as "/").
        model.snapshotWorkingDirectories(&snapshot);

        try snapshot.validate();
        return snapshot;
    }

    fn snapshotWorkingDirectories(model: *const Model, snapshot: *TopologySnapshot) void {
        for (snapshot.tabs[0..snapshot.tab_count]) |tab| {
            for (tab.nodes) |node| {
                if (node.kind != .leaf or !node.has_terminal or node.remote_ref != null) continue;
                const pane = model.provider.terminalConst(local.localRef(node.terminal)) orelse continue;
                snapshot.setCwd(node.terminal, pane.pwd());
            }
        }
    }

    /// A cheap hash of everything the SNAPSHOT would carry about shape:
    /// the tab list, each tab's tree, which tab is selected, and where the
    /// strip sits.
    ///
    /// Deliberately excludes working directories. A cwd changes on every `cd`,
    /// and folding it in here would turn ordinary shell navigation into a disk
    /// write per debounce window for no layout reason. Directories are still
    /// captured accurately, because every save serializes the CURRENT panes
    /// and the shutdown flush always writes one.
    /// Folds in EVERY window: a tab opened in the window behind the one in
    /// front still changes what a restore should produce, so a hash that only
    /// saw the active workspace would leave that change unwritten until
    /// something else happened to move the shape.
    pub fn topologyFingerprint(model: *const Model) u64 {
        var hasher = std.hash.Wyhash.init(0);
        std.hash.autoHash(&hasher, model.tab_placement);
        for (0..max_windows) |window_index| {
            const open = model.windowOpen(window_index);
            std.hash.autoHash(&hasher, open);
            if (!open) continue;
            const workspace = model.wsAtConst(window_index) orelse continue;
            std.hash.autoHash(&hasher, workspace.tab_count);
            std.hash.autoHash(&hasher, workspace.selected_tab);
            std.hash.autoHash(&hasher, workspace.web_selected);
            for (workspace.tabs[0..workspace.tab_count]) |tab| {
                std.hash.autoHash(&hasher, tab.root);
                std.hash.autoHash(&hasher, tab.focus);
                for (tab.nodes) |node| {
                    std.hash.autoHash(&hasher, node.kind);
                    if (node.kind == .free) continue;
                    std.hash.autoHash(&hasher, node.parent);
                    std.hash.autoHash(&hasher, node.first);
                    std.hash.autoHash(&hasher, node.second);
                    std.hash.autoHash(&hasher, node.orientation);
                    // A fraction is a float, which `autoHash` refuses; its bits
                    // are the identity that matters for "did the divider move".
                    std.hash.autoHash(&hasher, @as(u32, @bitCast(node.fraction)));
                    if (node.terminal) |id| {
                        std.hash.autoHash(&hasher, id.hash());
                        if (id.terminal_id == .phux) model.attachmentReference(id).context.hash(&hasher);
                    }
                }
            }
        }
        return hasher.final();
    }

    /// Write the layout SYNCHRONOUSLY, right now, on the calling thread.
    ///
    /// The debounced effect covers everything that happens while the app runs;
    /// this covers the interval between the last change and the debounce expiring
    /// — which is exactly "open a tab, then quit". It cannot be an effect: by the
    /// time shutdown is known, nothing will drain another effect queue, and a
    /// write posted to a worker thread would race the process out the door.
    ///
    /// Failure is silent by design, at every step. An app that refuses to exit
    /// because it could not write a layout file is worse than one that opens with
    /// the previous layout.
    pub fn writeWorkspaceState(model: *const Model, io: std.Io) void {
        if (!model.state.enabled()) return;
        var bytes: [session_state.max_state_bytes]u8 = undefined;
        const snapshot = model.topologySnapshot() catch return;
        const encoded = session_state.serialize(&snapshot, &bytes) catch return;
        const cwd = std.Io.Dir.cwd();
        const path = model.state.path();
        // WRITE FIRST, create the directory only if that fails — the same
        // correction `writeConfigTheme` carries, and for the same measured
        // reason. `createDirPath` stats each existing component WITHOUT
        // following symlinks, so a parent that is a symlink to a directory
        // comes back `.sym_link` rather than `.directory` and the call fails
        // with `error.NotDir`; the `catch return` then swallowed the entire
        // save even though a write straight through that same link would have
        // succeeded. `/tmp` -> `/private/tmp` on macOS is exactly that shape,
        // and `PHUX_COCKPIT_STATE` pointing anywhere under one — or a platform
        // state directory reached through one — lost every layout silently.
        //
        // Trying the write first also means the ordinary case, a directory
        // that already exists in any shape, never consults the directory at
        // all; the mkdir is reserved for the first-run case it is for.
        if (cwd.writeFile(io, .{ .sub_path = path, .data = encoded })) |_| return else |_| {}
        if (std.fs.path.dirname(path)) |parent| cwd.createDirPath(io, parent) catch return;
        cwd.writeFile(io, .{ .sub_path = path, .data = encoded }) catch return;
    }

    /// Write the live `theme` choice back into the user's config file, leaving
    /// every other byte of it alone.
    ///
    /// SYNCHRONOUS, through the provider's own `Io`, rather than through
    /// `fx.writeFile`. The write is a READ-MODIFY-WRITE — `config.setKey`
    /// needs the file's current bytes to preserve the comments and unknown
    /// keys in it — and the effect queue offers a write but no paired read, so
    /// posting this would mean serializing a model back over a person's file.
    /// It runs once per Enter in a settings panel, not per frame.
    ///
    /// A LEAF on purpose: two `max_config_bytes` buffers is 128 KB of stack,
    /// and the SDK copies nothing here, so they must exist at the bottom of
    /// the dispatch frame and nowhere up it.
    ///
    /// NON-FATAL at every failure, exactly like `writeWorkspaceState`: a theme
    /// that could not be written down is still applied live, and refusing to
    /// change the colour because a file was read-only would be a worse answer
    /// than changing it for this run.
    ///
    /// It used to be SILENT as well, which is a different thing and was the
    /// bug. Against a read-only config the panel announced the path it saves
    /// to, applied the theme, closed, wrote nothing, and raised nothing — so
    /// the running app and the file disagreed, and the next launch put the old
    /// theme back with no explanation available anywhere. That is precisely
    /// the "it did not persist" failure this surface exists to avoid, so the
    /// result is RETURNED now and `update.persistThemeChoice` latches it into
    /// a band the user can read.
    ///
    /// Three outcomes rather than a bool: "there is no config file to write"
    /// is not a failure — the panel already says so up front, in place of the
    /// path — and reporting it as one would raise a refusal band on every run
    /// that could not resolve a config location at all.
    pub fn writeConfigTheme(model: *const Model, io: std.Io) ConfigWrite {
        if (!model.config_file.enabled()) return .no_destination;
        // `theme = ` with nothing after it is a `bad_value` to the parser that
        // reads it back. There is no gesture that reaches here with no theme
        // named — the settings cursor always sits on a real one — and this is
        // the guard that keeps it that way rather than trusting it.
        if (model.config.theme.slice().len == 0) return .no_destination;
        const path = model.config_file.path();

        var source_bytes: [config_module.max_config_bytes]u8 = undefined;
        var rewritten: [config_module.max_config_bytes]u8 = undefined;
        const cwd = std.Io.Dir.cwd();

        // A missing file is the ORDINARY case, not an error: most people have
        // never written one, and the first thing this app ever writes for them
        // should be a one-line config rather than a refusal.
        const source: []const u8 = read: {
            var file = cwd.openFile(io, path, .{}) catch break :read "";
            defer file.close(io);
            const read_len = file.readPositionalAll(io, &source_bytes, 0) catch break :read "";
            break :read source_bytes[0..read_len];
        };

        const encoded = config_module.setKey(
            source,
            "theme",
            model.config.theme.slice(),
            &rewritten,
        ) catch return .refused;
        // WRITE FIRST, create the directory only if that fails.
        //
        // Not an optimization — a correctness fix, measured. The obvious
        // order (createDirPath, then write) silently wrote NOTHING for a
        // config under `/tmp` on macOS, because `/tmp` is a symlink to
        // `/private/tmp` and `createDirPath` on it fails; the `catch return`
        // then swallowed the whole save. Reproduced by
        // `settings_theme_tests.zig`, "return keeps the previewed theme and
        // writes it to the config file", which wrote a file, committed a
        // theme, and read the file back unchanged. Trying the write first
        // means the ordinary case — a directory that already exists, in every
        // shape — never consults the directory at all, and the mkdir is
        // reserved for the first-run case it is actually for.
        if (cwd.writeFile(io, .{ .sub_path = path, .data = encoded })) |_| return .written else |_| {}
        if (std.fs.path.dirname(path)) |parent| cwd.createDirPath(io, parent) catch return .refused;
        cwd.writeFile(io, .{ .sub_path = path, .data = encoded }) catch return .refused;
        return .written;
    }

    /// Whether the config file would take a write RIGHT NOW, asked before the
    /// user commits rather than after.
    ///
    /// The panel is the only place this app promises anything about a file, and
    /// it made that promise blind. One `access` at open costs a single syscall
    /// on a surface opened by hand, and it is the difference between "saves to
    /// <path>" and a line the file can actually honour.
    ///
    /// A MISSING file is writable, not unwritable: `writeConfigTheme` creates
    /// it — and its directory — and the overwhelmingly common case of "no
    /// config file yet" must not be dressed up as a permissions problem. If the
    /// creation then fails anyway, the commit's own result is what says so, and
    /// that path is covered by the band rather than by this.
    ///
    /// `enabled() == false` answers true for the same reason `writeConfigTheme`
    /// returns `.no_destination` for it: the panel already replaces the whole
    /// "saves to" line in that state, and a second complaint layered onto it
    /// would be noise.
    pub fn configFileWritable(model: *const Model, io: std.Io) bool {
        if (!model.config_file.enabled()) return true;
        std.Io.Dir.cwd().access(io, model.config_file.path(), .{ .write = true }) catch |err| switch (err) {
            error.FileNotFound => return true,
            else => return false,
        };
        return true;
    }

    pub fn configFileExists(model: *const Model, io: std.Io) bool {
        if (!model.config_file.enabled()) return false;
        std.Io.Dir.cwd().access(io, model.config_file.path(), .{}) catch return false;
        return true;
    }
};

/// What `Model.writeConfigTheme` did. See its doc comment for why "there was
/// nowhere to write" is a third state and not a failure.
pub const ConfigWrite = enum { written, no_destination, refused };

/// Put every restored pane's shell in the directory the snapshot recorded.
///
/// Split out of `restoreModel` rather than done inside it because `Pane.argv`
/// is a SLICE into `Model.cwd_argv`, and `restoreModel` returns its model BY
/// VALUE — an argv written there would point into a model that is about to be
/// copied into its real home and then go out of scope. This runs against the
/// model in its final storage, which is the only place the slice is allowed to
/// point.
pub fn applyRestoredWorkingDirectories(model: *Model, snapshot: *const TopologySnapshot) void {
    for (0..max_terminals) |index| {
        if (model.provider.states[index] != .active) continue;
        const pane = model.provider.slot(index);
        const id = provider_contract.localId(pane.id) orelse continue;
        const cwd = snapshot.cwdFor(id);
        if (cwd.len == 0) continue;
        pane.argv = local.paneArgvIn(cwd, &model.cwd_argv[index]);
    }
}

/// Encode remote leaves as references, never as local shell registry offsets.
fn encodeWindow(model: *const Model, workspace: *const Workspace, snapshot: *TopologySnapshot, written: *u8) !topology.SnapshotWindow {
    var selected: ?u8 = null;
    const first = written.*;
    for (workspace.tabs[0..workspace.tab_count], 0..) |current, index| {
        if (written.* >= topology.max_snapshot_tabs) return error.InvalidTopology;
        snapshot.tabs[written.*] = try encodeTab(model, current, &snapshot.references);
        if (!workspace.web_selected and index == workspace.selected_tab) selected = written.* - first;
        written.* += 1;
    }
    return .{ .tab_count = written.* - first, .selection = if (selected) |value| .{ .tab = value } else .web };
}

fn encodeTab(model: *const Model, current: layout.Tree, references: *topology.attachments.Table) !topology.SnapshotTab {
    if (current.isEmpty()) return error.InvalidTopology;
    var tab: topology.SnapshotTab = .{ .root = current.root, .focus = current.focus };
    for (current.nodes, 0..) |node, index| {
        switch (node.kind) {
            .free => continue,
            .leaf => {
                const held = node.terminal orelse return error.InvalidTopology;
                const local_id = provider_contract.localId(held);
                tab.nodes[index] = .{
                    .kind = .leaf,
                    .parent = node.parent,
                    .terminal = local_id orelse .terminal_1,
                    .remote_ref = if (local_id == null) try references.append(model.attachmentReference(held)) else null,
                    .has_terminal = true,
                };
            },
            .branch => tab.nodes[index] = .{
                .kind = .branch,
                .parent = node.parent,
                .orientation = node.orientation,
                .fraction = node.fraction,
                .first = node.first,
                .second = node.second,
            },
        }
    }
    return tab;
}

fn decodeTab(snapshot: *const TopologySnapshot, tab: topology.SnapshotTab) layout.Tree {
    var current: layout.Tree = .{ .root = tab.root, .focus = tab.focus };
    for (tab.nodes, 0..) |node, index| {
        current.nodes[index] = switch (node.kind) {
            .free => .{},
            .leaf => .{
                .kind = .leaf,
                .parent = node.parent,
                .terminal = snapshot.terminalRef(node),
            },
            .branch => .{
                .kind = .branch,
                .parent = node.parent,
                .orientation = node.orientation,
                .fraction = node.fraction,
                .first = node.first,
                .second = node.second,
            },
        };
    }
    return current;
}

/// Push the settings only an EMULATOR can hold into one session.
///
/// The split is deliberate. Background, foreground, and the selection wash
/// reach the painter through the design tokens (`terminalTokens`), because
/// those are the ones the emulator composes from its own defaults and an
/// application's OSC 10/11 must still be able to win. These three cannot go
/// that way:
///
///   palette      the ANSI-16 slots are the EMULATOR's (`vt.color.default`),
///                read back verbatim by the projection, so an override has
///                to land in the emulator's dynamic palette;
///   cursor color a user's `cursor-color` is an explicit choice, not a theme
///                default — it is exactly OSC 12's override channel, and
///                using `.default` would have the token accent overwrite it
///                on the next snapshot;
///   cursor style DECSCUSR state, which only the emulator carries.
///
/// Called AFTER `spawnPane`, which hard-resets the emulator: applying before
/// the reset would drop every one of these on the floor.
pub fn applySessionConfig(cfg: *const Config, session: *grid.Session) void {
    for (cfg.palette, 0..) |maybe_color, index| {
        const color = maybe_color orelse continue;
        session.term.colors.palette.set(@intCast(index), .{ .r = color.r, .g = color.g, .b = color.b });
    }
    if (cfg.cursor_color) |color| {
        session.term.colors.cursor.set(.{ .r = color.r, .g = color.g, .b = color.b });
    }
    session.term.setDefaultCursorStyle(switch (cfg.cursor_style) {
        .block => .block,
        .bar => .bar,
        .underline => .underline,
    });
    session.term.setDefaultCursorBlink(cfg.cursor_style_blink);
}

pub fn initialModelWithPhux(session: *grid.Session, phux_provider: ?*PhuxProvider) Model {
    const local_provider = LocalProvider.create(std.heap.page_allocator, session) catch @panic("failed to allocate local terminal provider");
    var pointer_state: ?*PointerState = null;
    if (comptime support.phux_enabled) if (phux_provider != null) {
        pointer_state = std.heap.page_allocator.create(PointerState) catch @panic("failed to allocate pointer monitor state");
        pointer_state.?.* = .{};
    };
    var model: Model = .{
        .provider = local_provider,
        .phux_provider = phux_provider,
        .pointer_state = pointer_state,
    };
    _ = model.admitTab(local.initialTerminalRef(0));
    return model;
}

pub fn initialModel(session: *grid.Session) Model {
    return initialModelWithPhux(session, null);
}

pub fn attachPhuxProvider(model: *Model, phux_provider: ?*PhuxProvider) void {
    if (comptime !support.phux_enabled) return;
    const remote = phux_provider orelse return;
    model.phux_provider = remote;
    if (model.pointer_state != null) return;
    const pointer_state = std.heap.page_allocator.create(PointerState) catch @panic("failed to allocate pointer monitor state");
    pointer_state.* = .{};
    model.pointer_state = pointer_state;
}

pub fn initialModelWithIo(gpa: std.mem.Allocator, io: std.Io, session: *grid.Session) !Model {
    const provider = try LocalProvider.createWithIo(gpa, io, session);
    var model: Model = .{ .provider = provider };
    _ = model.admitTab(local.initialTerminalRef(0));
    return model;
}

pub fn initialProductionModelWithIo(gpa: std.mem.Allocator, io: std.Io, session: *grid.Session) !Model {
    return initialModelWithIo(gpa, io, session);
}

pub fn restoreModel(gpa: std.mem.Allocator, io: std.Io, persisted: PersistedTopologySnapshot) !Model {
    return restoreModelWithScrollback(gpa, io, persisted, grid.Session.max_scrollback);
}

/// Restore with an explicit scrollback ceiling, so a restored window's panes
/// honour `scrollback-limit` too. Every restored leaf gets a fresh session
/// built HERE, which is why the limit has to arrive as a parameter rather than
/// being set on the provider afterwards.
pub fn restoreModelWithScrollback(
    gpa: std.mem.Allocator,
    io: std.Io,
    persisted: PersistedTopologySnapshot,
    max_scrollback_bytes: usize,
) !Model {
    const snapshot = try topology.migrateTopologySnapshot(persisted);
    const provider = try gpa.create(LocalProvider);
    provider.* = .{ .gpa = gpa, .io = io, .max_scrollback_bytes = max_scrollback_bytes };

    var model: Model = .{
        .provider = provider,
        .tab_placement = snapshot.tab_placement,
        .saved_attachments = snapshot.references,
        .pending_attachments = @splat(true),
    };
    errdefer provider.destroy();
    errdefer for (model.secondary) |slot| if (slot) |workspace| std.heap.page_allocator.destroy(workspace);

    // Local leaves get fresh sessions. Remote leaves retain pending identities
    // until provider evidence matches; restoration never allocates their PTYs.
    //
    // Windows are restored in the order they were written, which is the order
    // they were numbered: window 0 into the inline workspace, the rest into
    // freshly minted secondary slots. A snapshot from before windows existed
    // migrates to exactly one window, so it lands entirely in `primary`.
    for (0..snapshot.window_count) |window_index| {
        try restoreWindow(&model, &snapshot, window_index);
    }
    return model;
}

fn restoreWindow(model: *Model, snapshot: *const TopologySnapshot, window_index: usize) !void {
    const workspace = model.openWindow(window_index) orelse return error.WindowCapacityReached;
    const tabs = snapshot.windowTabs(window_index);
    workspace.tab_count = tabs.len;
    workspace.web_selected = snapshot.windows[window_index].selection == .web;
    workspace.selected_tab = switch (snapshot.windows[window_index].selection) {
        .tab => |index| index,
        .web => 0,
    };
    for (tabs, 0..) |tab, tab_index| {
        workspace.tabs[tab_index] = decodeTab(snapshot, tab);
        workspace.assignRestoredTabId(tab_index);
        for (tab.nodes) |node| {
            if (node.kind != .leaf or !node.has_terminal or node.remote_ref != null) continue;
            try restoreLocalPane(model.provider, node.terminal);
        }
    }
}

fn restoreLocalPane(provider: *LocalProvider, terminal: LocalResourceId) !void {
    // Runtime capacity failure propagates, preserving the valid source file.
    if (provider.liveShellCount() >= local.max_live_shells) return error.TerminalCapacityReached;
    const session = try grid.Session.createWithScrollback(provider.gpa, provider.io, 80, 24, provider.max_scrollback_bytes);
    errdefer session.destroy();
    var index: usize = 0;
    while (index < max_terminals and provider.states[index] != .vacant) : (index += 1) {}
    if (index == max_terminals) return error.TerminalCapacityReached;
    provider.slots[index] = .{
        .id = local.localRef(terminal),
        .session = session,
        .pty_key = provider.next_pty_key,
        .argv = local.paneArgv(0),
    };
    provider.states[index] = .active;
    provider.next_pty_key += 1;
    provider.next_terminal_raw = @max(provider.next_terminal_raw, @intFromEnum(terminal) + 1);
}

pub fn deinitModel(model: *Model) void {
    model.shared_workspace.deinit();
    model.clearRemotePaint();
    if (comptime support.phux_enabled) {
        if (model.pointer_state) |pointer_state| {
            if (pointer_state.monitor) |*monitor| monitor.stop();
            std.heap.page_allocator.destroy(pointer_state);
            model.pointer_state = null;
        }
        if (model.phux_provider) |remote| remote.destroy();
        model.phux_provider = null;
    }
    for (&model.secondary) |*slot| {
        if (slot.*) |workspace| std.heap.page_allocator.destroy(workspace);
        slot.* = null;
    }
    model.provider.destroy();
}
