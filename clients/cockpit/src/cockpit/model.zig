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
pub const AgentIdentity = support.AgentIdentity;
pub const AgentState = support.AgentState;
pub const max_agent_sessions = support.max_agent_sessions;

pub const max_held_terminal_keys: usize = 16;

/// Title ceiling for a posted desktop notification. The title is a terminal's
/// name, which the tab strip already bounds well below this.
pub const max_notification_title_bytes: usize = 128;

/// Matches the SDK's `max_effect_file_path_bytes`.
pub const max_state_path_bytes: usize = 1024;

/// Layout-persistence bookkeeping. The path is resolved once at startup.
/// `fingerprint` is the last persisted topology hash, which makes saves
/// edge-triggered and debounced.
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

/// Where the hand-edited config file lives, for writing a choice back to it.
/// Deliberately separate from `StatePersistence`. Empty disables writing.
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

/// The settings surface. Per workspace (it owns the keyboard while open) but
/// it edits the app-wide config, previewing themes live.
pub const Settings = struct {
    open: bool = false,
    /// Row index into `theme.builtins`.
    cursor: usize = 0,
    /// The theme in effect when the surface opened, restored on cancel.
    restore_theme: config_module.ThemeName = config_module.ThemeName.init(""),
    /// Time-of-check answer to "will the config file take the write", asked
    /// when the surface opens; the commit's own result stays authoritative.
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

/// Sentinel for "the pointer is over no tab".
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
        // Only the identity-qualified Find field survives a replica change.
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

/// The SDK's model-declared secondary-window budget; `scene.zig` asserts the
/// two agree.
pub const max_secondary_windows: usize = 4;

/// The scene's own window plus the secondaries.
pub const max_windows: usize = 1 + max_secondary_windows;

pub const max_palette_query_bytes: usize = 64;

/// The switcher's state, per workspace so a palette in one window never eats
/// keys typed in another.
pub const Palette = struct {
    open: bool = false,
    query: [max_palette_query_bytes]u8 = [_]u8{0} ** max_palette_query_bytes,
    query_len: usize = 0,
    /// An index into the filtered list.
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

    /// Append typed text, dropping control bytes and anything past the buffer.
    pub fn append(palette: *Palette, text: []const u8) void {
        for (text) |byte| {
            if (byte < 0x20 or byte == 0x7f) continue;
            if (palette.query_len >= palette.query.len) return;
            palette.query[palette.query_len] = byte;
            palette.query_len += 1;
        }
        palette.cursor = 0;
    }

    /// Delete the last UTF-8 scalar.
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
    /// Palette and settings are never persisted in `topologySnapshot`.
    palette: Palette = .{},
    settings: Settings = .{},
    tab_count: usize = 0,
    selected_tab: usize = 0,
    /// The web surface owns the content area (main window only); independent
    /// of `selected_tab` so returning restores the tab.
    web_selected: bool = false,
    hovered_tab: usize = no_hovered_tab,
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
    /// This window's canvas size and device scale.
    surface_size: geometry.SizeF = geometry.SizeF.init(1100, 640),
    /// A frame has measured `surface_size`; until then it is the default
    /// above, never a size to attach a session with (ADR-0110).
    surface_measured: bool = false,
    surface_scale_factor: f32 = 1,
    /// The platform window id, learned from frame events (zero until the
    /// first frame); shortcuts carry only a window id.
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

    /// The selected tab's focused pane, unfiltered by provider bookkeeping.
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

pub const TerminalLocation = struct { window: usize, tab: usize };
/// Switcher payload for a placed terminal; `terminal_ref` is the identity
/// fence, window and tab where it was when the row was built.
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
    /// A session of a peer coordinator; selecting shows it beside the others.
    peer_session: PeerSession,
    /// A peer that cannot list sessions (visible, never selectable).
    peer_unavailable: support.ProviderId,
};

pub const PeerSession = struct { coordinator: support.ProviderId, id: u32, attachment_id: ?u64 = null };

/// Bound on the session name an `EmptyPick` keeps for display.
pub const max_empty_pick_name_bytes: usize = 64;

/// A peer's keep-empty session with no windows, picked in the switcher
/// (ADR-0105). The window it was picked in shows the Empty session state
/// instead of attaching it (native/empty_session.zig). New Tab sets
/// `tab_requested`, which shows the session on that peer; `tab_queued` once
/// its first tab's spawn is on its way there.
pub const EmptyPick = struct {
    attachment_id: u64 = 0,
    window_epoch: u64 = 0,
    created: bool = false,
    coordinator: support.ProviderId,
    session: u32,
    window: usize,
    tab_requested: bool = false,
    tab_queued: bool = false,
    name: [max_empty_pick_name_bytes]u8 = undefined,
    name_len: u8 = 0,

    /// Kept for the state shown while the peer restarts to attach it, when
    /// its session list is briefly gone. Cut on a UTF-8 boundary.
    pub fn setName(self: *EmptyPick, name: []const u8) void {
        var len = @min(name.len, self.name.len);
        while (len > 0 and len < name.len and (name[len] & 0xc0) == 0x80) len -= 1;
        @memcpy(self.name[0..len], name[0..len]);
        self.name_len = @intCast(len);
    }

    pub fn nameSlice(self: *const EmptyPick) []const u8 {
        return self.name[0..self.name_len];
    }
};

/// Heap-stable ownership for a coordinator attachment. Collection growth never
/// moves its projection or pending lifecycle state. Empty entries may be reused,
/// but their retired channel handles are never reused.
pub const Peer = struct {
    provider: ?*PhuxProvider = null,
    coordinator_context: ?u64 = null,
    session_created_at: ?i64 = null,
    selection_epoch: ?u64 = null,
    workspace: @import("shared_workspace.zig").State = .{},
    reopen: bool = false,
    failed: bool = false,
    restore: ?PeerRestore = null,
    channel_key: u64 = 0,
    closing_key: ?u64 = null,
    retry_key: ?u64 = null,
    retry_delay_ms: u64 = 0,
    listed_since: ?std.Io.Timestamp = null,
};

/// What a peer's remembered host showed at the last quit (ADR-0110), keyed
/// by that peer's own coordinator id (native/peer_restore.zig).
pub const PeerRestore = struct {
    coordinator: support.ProviderId,
    shown: @import("remote_memory.zig").Shown,
    /// A front record not shown yet: it waits for the host's first list,
    /// then (`listed`) for the front window's first measured frame.
    pending: bool,
    listed: bool = false,
    /// A front record whose host's connection failed once before it was
    /// shown: kept for the backoff redial, and dropped on a second failure.
    retried: bool = false,
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
    /// Loaded once at startup, then model state.
    config: Config = .{},
    /// Live cmd+= / cmd+- delta over `config.font_size`, in points.
    font_size_offset: f32 = 0,
    /// Backing storage for each slot's `Pane.argv`, which Restart re-reads.
    cwd_argv: [max_terminals]local.CwdArgv = [_]local.CwdArgv{.{}} ** max_terminals,
    phux_provider: ?*PhuxProvider = null,
    /// Coordinators held beside the active one, each with its own channel. A
    /// peer only lists sessions until one is picked, then attaches and
    /// projects beside the active one. Refs route to the provider that minted
    /// them (`phuxForRef`).
    peers: std.ArrayList(*Peer) = .empty,
    window_attachments: [max_windows]?struct { id: u64, epoch: u64 } = @splat(null),
    /// A peer's empty session picked in the switcher (EmptyPick).
    empty_pick: ?EmptyPick = null,
    empty_picks: [max_windows]?EmptyPick = @splat(null),
    /// Phux could not attach; local terminals stay usable but ephemeral, and
    /// the chrome says so until an attach lands.
    phux_connection_unavailable: bool = false,
    /// A session switch waits for the old channel to close before reusing
    /// its effect key.
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
    /// Window 0's workspace, inline (it always exists; a workspace is large,
    /// so windows 1..N are heap-allocated on demand).
    primary: Workspace = .{},
    /// Windows 1..N; a null slot is a closed window.
    secondary: [max_secondary_windows]?*Workspace = @splat(null),
    /// The window input is addressed to.
    active_window: usize = 0,
    /// The main window can close while secondaries stay open.
    primary_open: bool = true,
    /// Visible refusal latches, cleared by the next success: every window
    /// slot taken, every live shell in use, or a settings commit that did not
    /// reach the config file.
    window_limit_refused: bool = false,
    /// The startup config notice was dismissed (app-wide, not persisted).
    config_notice_dismissed: bool = false,
    terminal_limit_refused: bool = false,
    config_write_refused: bool = false,
    tab_placement: TabPlacement = .top,
    /// Background topology writes must retain committed placement during preview.
    appearance_committed_placement: ?TabPlacement = null,
    focused: bool = true,
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
    /// Where the in-flight clipboard read lands; a needle paste is valid even
    /// against a pane that no longer accepts input.
    paste_target: PasteTarget = .terminal,
    /// Observable record of the last URL handed to the OS.
    opened_url_buf: [url_module.max_url_bytes]u8 = undefined,
    opened_url_len: usize = 0,
    opened_url_count: u32 = 0,
    /// Layout and config file destinations; disabled (no disk) by default.
    state: StatePersistence = .{},
    config_file: ConfigFile = .{},
    /// Observable record of posted desktop notifications.
    notified_title_buf: [max_notification_title_bytes]u8 = undefined,
    notified_title_len: usize = 0,
    notification_count: u32 = 0,
    /// Saved references carry evidence, never live provider ownership.
    attachment_context: topology.attachments.Context = .{},
    saved_attachments: topology.attachments.Table = .{},
    pending_attachments: [topology.attachments.max_references]bool = @splat(false),
    /// Async placements cannot acquire a later workspace that reuses a slot.
    window_epochs: [max_windows]u64 = @splat(0),

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

    /// Record a notification; a title too long to record is not posted.
    pub fn recordNotification(model: *Model, title: []const u8) bool {
        if (title.len == 0 or title.len > model.notified_title_buf.len) return false;
        @memcpy(model.notified_title_buf[0..title.len], title);
        model.notified_title_len = title.len;
        model.notification_count += 1;
        return true;
    }

    /// The active window's workspace, falling back to window 0 if its slot
    /// has closed.
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

    /// Whether window `index` is on screen.
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

    /// The lowest free secondary slot, or null at the ceiling.
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

    /// Retire window `index`; callers have already closed its panes.
    pub fn closeWindow(model: *Model, index: usize) void {
        if (index < max_windows) {
            model.window_epochs[index] +|= 1;
            // A stale binding would leave a reopened window with no provider.
            model.window_attachments[index] = null;
        }
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

    /// The lowest-numbered window still open, or 0 when none is.
    pub fn firstOpenWindow(model: *const Model) usize {
        for (0..max_windows) |index| {
            if (model.windowOpen(index)) return index;
        }
        return 0;
    }

    /// Where a terminal lives, across every window.
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

    /// The first peer: the one a single remote host stands beside.
    pub fn phuxPeer(model: *Model) ?*PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        for (model.peers.items) |entry| if (entry.provider) |peer| return peer;
        return null;
    }

    pub fn phuxPeerConst(model: *const Model) ?*const PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        for (model.peers.items) |entry| if (entry.provider) |peer| return peer;
        return null;
    }

    pub fn phuxPeerAt(model: *Model, slot: usize) ?*PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        if (slot >= model.peers.items.len) return null;
        return model.peers.items[slot].provider;
    }

    pub fn phuxPeerAtConst(model: *const Model, slot: usize) ?*const PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        if (slot >= model.peers.items.len) return null;
        return model.peers.items[slot].provider;
    }

    /// The slot of the peer connected to coordinator `id`.
    pub fn peerSlot(model: *const Model, id: support.ProviderId) ?usize {
        if (comptime !support.phux_enabled) return null;
        if (id == .local) return null;
        for (model.peers.items, 0..) |entry, index| {
            const peer = entry.provider orelse continue;
            if (peer.providerId() == id) return index;
        }
        return null;
    }

    /// The provider that mints refs for coordinator `id`: the active one or
    /// a peer. Null for a local PTY and for a coordinator no longer held.
    pub fn phuxFor(model: *Model, id: support.ProviderId) ?*PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        if (id == .local) return null;
        if (model.phux_provider) |active| if (active.providerId() == id) return active;
        const slot = model.peerSlot(id) orelse return null;
        return model.peers.items[slot].provider;
    }

    pub fn phuxForConst(model: *const Model, id: support.ProviderId) ?*const PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        if (id == .local) return null;
        if (model.phux_provider) |active| if (active.providerId() == id) return active;
        const slot = model.peerSlot(id) orelse return null;
        return model.peers.items[slot].provider;
    }

    /// Where a ref's input, sizing and presentation go: the coordinator
    /// that minted it, and no other.
    pub fn phuxForRef(model: *Model, ref: TerminalRef) ?*PhuxProvider {
        return @constCast(model.phuxForRefConst(ref));
    }

    pub fn phuxForRefConst(model: *const Model, ref: TerminalRef) ?*const PhuxProvider {
        const projected = model.projectedAttachment(ref);
        if (projected.ambiguous) return null;
        if (projected.id) |id| {
            const remote = model.phuxForAttachmentConst(id) orelse return null;
            return if (remote.providerId() == ref.provider_id) remote else null;
        }
        return model.phuxForConst(ref.provider_id);
    }

    /// Stable process-local attachment identity, independent of coordinator ID.
    pub fn phuxForAttachment(model: *Model, id: u64) ?*PhuxProvider {
        return @constCast(model.phuxForAttachmentConst(id));
    }

    pub fn phuxForAttachmentConst(model: *const Model, id: u64) ?*const PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        if (model.phux_provider) |remote| if (remote.context_id == id) return remote;
        for (model.peers.items) |entry| {
            const remote = entry.provider orelse continue;
            if (remote.context_id == id) return remote;
        }
        return null;
    }

    pub fn phuxForOwner(model: *Model, owner: ReplicaOwner) ?*PhuxProvider {
        return @constCast(model.phuxForOwnerConst(owner));
    }

    pub fn phuxForOwnerConst(model: *const Model, owner: ReplicaOwner) ?*const PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        if (owner.source_context == 0) return model.phuxForRefConst(owner.terminal_ref);
        if (model.phux_provider) |remote| if (remote.host.context_id == owner.source_context) return remote;
        for (model.peers.items) |entry| {
            const remote = entry.provider orelse continue;
            if (remote.host.context_id == owner.source_context) return remote;
        }
        return null;
    }

    pub fn phuxForTree(model: *Model, pane_tree: *const layout.Tree) ?*PhuxProvider {
        return @constCast(model.phuxForTreeConst(pane_tree));
    }

    pub fn phuxForTreeConst(model: *const Model, pane_tree: *const layout.Tree) ?*const PhuxProvider {
        const authority = @import("shared_workspace.zig").tabAuthority(pane_tree) orelse return null;
        if (pane_tree.attachment_id) |id| {
            const remote = model.phuxForAttachmentConst(id) orelse return null;
            return if (remote.providerId() == authority) remote else null;
        }
        return model.phuxForConst(authority);
    }

    pub fn peerSlotForAttachment(model: *const Model, id: u64) ?usize {
        if (comptime !support.phux_enabled) return null;
        for (model.peers.items, 0..) |entry, slot| {
            const remote = entry.provider orelse continue;
            if (remote.context_id == id) return slot;
        }
        return null;
    }

    pub fn sharedWorkspaceForAttachment(model: *Model, id: u64) ?*@import("shared_workspace.zig").State {
        if (comptime !support.phux_enabled) return null;
        if (model.phux()) |remote| if (remote.context_id == id) return &model.shared_workspace;
        const slot = model.peerSlotForAttachment(id) orelse return null;
        return &model.peers.items[slot].workspace;
    }

    pub fn bindWindowAttachment(model: *Model, window: usize, id: u64) void {
        if (!model.windowOpen(window)) return;
        model.window_attachments[window] = .{ .id = id, .epoch = model.window_epochs[window] };
    }

    pub fn bindSharedAttachment(model: *Model, remote: *PhuxProvider) void {
        if (comptime !support.phux_enabled) return;
        const state = model.sharedWorkspaceForAttachment(remote.context_id) orelse return;
        if (state.attachment_id != null) return;
        // Migrate only a projection confirmed by this State's connection and
        // session before attachment tags existed.
        if (state.epoch == remote.connectionEpoch() and state.session == remote.selectedSessionId()) model.tagLegacyProjection(remote);
        state.attachment_id = remote.context_id;
    }

    fn tagLegacyProjection(model: *Model, remote: *const PhuxProvider) void {
        for (0..max_windows) |window| {
            const workspace = model.wsAt(window) orelse continue;
            for (workspace.tabs[0..workspace.tab_count]) |*pane_tree| {
                if (pane_tree.attachment_id != null) continue;
                if (@import("shared_workspace.zig").tabAuthority(pane_tree) != remote.providerId()) continue;
                pane_tree.attachment_id = remote.context_id;
            }
        }
    }

    pub fn phuxForWindow(model: *Model, window: usize) ?*PhuxProvider {
        return @constCast(model.phuxForWindowConst(window));
    }

    pub fn phuxForWindowConst(model: *const Model, window: usize) ?*const PhuxProvider {
        if (!model.windowOpen(window)) return null;
        const workspace = model.wsAtConst(window) orelse return null;
        if (workspace.treeConst(workspace.selected_tab)) |pane_tree| {
            if (@import("shared_workspace.zig").tabAuthority(pane_tree)) |id| {
                if (id != .local) return model.phuxForTreeConst(pane_tree);
            }
        }
        if (model.window_attachments[window]) |binding| {
            if (binding.epoch != model.window_epochs[window]) return null;
            return model.phuxForAttachmentConst(binding.id);
        }
        return model.defaultWindowProvider(window, workspace.tab_count == 0);
    }

    fn defaultWindowProvider(model: *const Model, window: usize, empty: bool) ?*const PhuxProvider {
        if (empty and window == model.firstOpenWindow()) return model.phuxConst();
        return model.localPhuxProviderConst();
    }

    pub fn localPhuxProviderConst(model: *const Model) ?*const PhuxProvider {
        if (comptime !support.phux_enabled) return null;
        if (model.phuxConst()) |remote| if (model.isCanonicalLocal(remote)) return remote;
        for (model.peers.items) |entry| {
            const remote = entry.provider orelse continue;
            if (model.isCanonicalLocal(remote)) return remote;
        }
        return null;
    }

    fn isCanonicalLocal(model: *const Model, remote: *const PhuxProvider) bool {
        if (comptime !support.phux_enabled) return false;
        const endpoint = remote.endpointDescriptor();
        if (endpoint != .unix) return false;
        return std.mem.eql(u8, endpoint.unix, model.config.phux_socket.slice());
    }

    const ProjectedAttachment = struct {
        id: ?u64 = null,
        ambiguous: bool = false,

        fn include(self: *ProjectedAttachment, pane_tree: *const layout.Tree, ref: TerminalRef) void {
            if (pane_tree.find(ref) == null) return;
            const id = pane_tree.attachment_id orelse return;
            if (self.id) |previous| if (previous != id) {
                self.ambiguous = true;
            };
            self.id = id;
        }
    };

    fn projectedAttachment(model: *const Model, ref: TerminalRef) ProjectedAttachment {
        var result: ProjectedAttachment = .{};
        for (0..max_windows) |window| {
            if (!model.windowOpen(window)) continue;
            const workspace = model.wsAtConst(window) orelse continue;
            for (workspace.tabs[0..workspace.tab_count]) |*pane_tree| result.include(pane_tree, ref);
        }
        return result;
    }

    /// Whether coordinator `id`'s terminals are on screen: the active one's
    /// always may be; a peer's only while it shows a session. A listing
    /// peer is never attached, so none of its terminals can be.
    pub fn projectsAuthority(model: *const Model, id: support.ProviderId) bool {
        if (comptime !support.phux_enabled) return false;
        if (model.phux_provider) |active| if (active.providerId() == id) return true;
        const slot = model.peerSlot(id) orelse return false;
        return model.peers.items[slot].provider.?.showing();
    }

    /// The projection state for coordinator `id`'s shared workspace.
    pub fn sharedWorkspaceFor(model: *Model, id: support.ProviderId) ?*@import("shared_workspace.zig").State {
        if (comptime !support.phux_enabled) return null;
        if (model.phux_provider) |active| if (active.providerId() == id) return &model.shared_workspace;
        const slot = model.peerSlot(id) orelse return null;
        return &model.peers.items[slot].workspace;
    }

    /// Reserve owned entries before mutating any provider. Callers report
    /// allocation failure without disturbing existing visible work.
    pub fn ensurePeerSlots(model: *Model, count: usize) !void {
        const gpa = std.heap.page_allocator;
        try model.peers.ensureTotalCapacity(gpa, count);
        while (model.peers.items.len < count) {
            const entry = try gpa.create(Peer);
            errdefer gpa.destroy(entry);
            entry.* = .{ .channel_key = try support.allocatePeerHandle() };
            model.peers.appendAssumeCapacity(entry);
        }
    }

    pub fn freePeerSlot(model: *Model) !usize {
        for (model.peers.items, 0..) |entry, slot| {
            if (entry.provider == null) return slot;
        }
        const slot = model.peers.items.len;
        try model.ensurePeerSlots(slot + 1);
        return slot;
    }

    /// The coordinator whose placements carry saved attachment evidence:
    /// the active one. A showing peer's placements are projected from its
    /// own shared workspace on every connection instead.
    pub fn attachmentAuthority(model: *const Model) support.ProviderId {
        if (model.phuxConst()) |active| return active.providerId();
        return .phux;
    }

    /// Whether a ref is the active coordinator's own terminal. Resolves
    /// through the active window's selected tree first, because two
    /// attachments of one coordinator project equal refs and the global
    /// lookup refuses to choose.
    pub fn activeOwnsRef(model: *const Model, ref: TerminalRef) bool {
        return model.phuxForActiveRefConst(ref) == model.phuxConst();
    }

    fn phuxForActiveRefConst(model: *const Model, ref: TerminalRef) ?*const PhuxProvider {
        const pane_tree = model.selectedTreeConst() orelse return model.phuxForRefConst(ref);
        if (pane_tree.find(ref) == null) return model.phuxForRefConst(ref);
        const remote = model.phuxForTreeConst(pane_tree) orelse return null;
        return if (remote.providerId() == ref.provider_id) remote else null;
    }

    /// A tab another coordinator projected. Cockpit's tab and split commands
    /// address the active coordinator, so they refuse such a tab rather than
    /// send one machine's window or terminal to another.
    pub fn foreignTree(model: *const Model, tab: *const layout.Tree) bool {
        if (tab.attachment_id != null) return model.phuxForTreeConst(tab) != model.phuxConst();
        const owner = @import("shared_workspace.zig").tabAuthority(tab) orelse return false;
        return owner != .local and owner != model.attachmentAuthority();
    }

    pub fn foreignTab(model: *const Model, workspace: *const Workspace, index: usize) bool {
        const tab = workspace.treeConst(index) orelse return false;
        return model.foreignTree(tab);
    }

    pub fn containsTerminal(model: *const Model, terminal_ref: TerminalRef) bool {
        if (model.attachmentPending(terminal_ref)) return false;
        return switch (support.providerKind(terminal_ref)) {
            .local => model.provider.contains(terminal_ref),
            .phux => if (model.phuxForRefConst(terminal_ref)) |remote| remote.contains(terminal_ref) else false,
        };
    }

    pub fn terminalOwner(model: *const Model, terminal_ref: TerminalRef) ?ReplicaOwner {
        if (support.providerKind(terminal_ref) == .local) return model.provider.owner(terminal_ref);
        const remote = model.phuxForInteractionConst(terminal_ref) orelse return null;
        if (model.pendingFor(remote, terminal_ref)) return null;
        return remote.owner(terminal_ref);
    }

    pub fn ownerIsCurrent(model: *const Model, owner_value: ReplicaOwner) bool {
        if (support.providerKind(owner_value.terminal_ref) == .local) return model.provider.ownerIsCurrent(owner_value);
        const remote = model.phuxForOwnerConst(owner_value) orelse return false;
        if (model.pendingFor(remote, owner_value.terminal_ref)) return false;
        return remote.ownerIsCurrent(owner_value);
    }

    fn pendingFor(model: *const Model, remote: *const PhuxProvider, ref: TerminalRef) bool {
        return remote == model.phuxConst() and model.attachmentPending(ref);
    }

    /// Initial keyboard/pointer ownership comes from the selected tree. Held
    /// and asynchronous operations use phuxForOwner instead of reacquiring it.
    pub fn phuxForInteractionConst(model: *const Model, ref: TerminalRef) ?*const PhuxProvider {
        if (model.selectedTreeConst()) |pane_tree| {
            if (pane_tree.find(ref) != null) return model.phuxForTreeConst(pane_tree);
        }
        return model.phuxForRefConst(ref);
    }

    /// The agent sessions under one terminal, in catalog order (rows, never
    /// surfaces). Borrowed until the next provider drain.
    pub fn agentSessionsUnder(model: *const Model, terminal_ref: TerminalRef, out: []*const AgentSession) usize {
        if (comptime !support.phux_enabled) return 0;
        if (support.providerKind(terminal_ref) != .phux) return 0;
        const remote = model.phuxForRefConst(terminal_ref) orelse return 0;
        return remote.agentSessionsUnder(terminal_ref, out);
    }

    /// Whether an agent under this terminal is waiting on a person.
    pub fn agentAttention(model: *const Model, terminal_ref: TerminalRef) bool {
        if (comptime !support.phux_enabled) return false;
        if (support.providerKind(terminal_ref) != .phux) return false;
        const remote = model.phuxForRefConst(terminal_ref) orelse return false;
        return remote.agentAttention(terminal_ref);
    }

    /// Whether this identity names an agent session rather than a terminal.
    pub fn isAgentSession(model: *const Model, terminal_ref: TerminalRef) bool {
        if (comptime !support.phux_enabled) return false;
        if (support.providerKind(terminal_ref) != .phux) return false;
        const remote = model.phuxForRefConst(terminal_ref) orelse return false;
        return remote.isAgentSession(terminal_ref);
    }

    pub fn remotePresentation(model: *const Model, terminal_ref: TerminalRef) ?Presentation {
        if (support.providerKind(terminal_ref) != .phux) return null;
        const remote = model.phuxForInteractionConst(terminal_ref) orelse return null;
        if (model.pendingFor(remote, terminal_ref)) return null;
        return remote.presentation(terminal_ref);
    }

    pub fn remotePaintPresentationIn(model: *const Model, pane_tree: *const layout.Tree, ref: TerminalRef) ?Presentation {
        if (comptime !support.phux_enabled) return null;
        if (pane_tree.find(ref) == null) return null;
        const remote = model.phuxForTreeConst(pane_tree) orelse return null;
        if (!model.pendingFor(remote, ref)) return remote.presentation(ref);
        for (model.frozen_paint) |entry| {
            const frozen = entry orelse continue;
            if (frozen.owner.source_context != remote.host.context_id) continue;
            if (frozen.reference.terminal_ref.eql(ref)) return frozen.snapshot.value;
        }
        return null;
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
                if (state.owner.source_context != current_owner.source_context) continue;
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
        return model.remoteUiForOwnerConst(current_owner);
    }

    pub fn remoteUiForOwnerConst(model: *const Model, current_owner: ReplicaOwner) ?*const RemoteUiState {
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
        // Freeze old evidence first, or a reused numeric id would acquire the
        // next server's identity.
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
        const remote = model.phuxForRefConst(ref) orelse return false;
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
        const authority = model.attachmentAuthority();
        for (0..max_windows) |window_index| {
            const workspace = model.wsAtConst(window_index) orelse continue;
            for (workspace.tabs[0..workspace.tab_count]) |current| {
                var refs: [layout.max_panes]TerminalRef = undefined;
                const count = current.terminals(&refs);
                for (refs[0..count]) |ref| {
                    if (ref.terminal_id != .phux) continue;
                    // One context describes one coordinator's connection; a
                    // showing peer's refs must never be saved under it.
                    if (ref.provider_id != authority) continue;
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
    // These address the ACTIVE window; use `wsAt(index)` for a specific one.

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

    /// The surface the content area shows.
    pub fn selectedSurface(model: *const Model) SurfaceSelection {
        const terminal_ref = model.focusedTerminalRef() orelse return .web;
        return .{ .terminal = terminal_ref };
    }

    /// The focused pane's terminal, if a provider still vouches for it (for
    /// routing input).
    pub fn selectedTerminalRef(model: *const Model) ?TerminalRef {
        const id = model.focusedTerminalRef() orelse return null;
        return if (model.containsTerminal(id)) id else null;
    }

    /// The selected tab's focused pane, unfiltered.
    pub fn focusedTerminalRef(model: *const Model) ?TerminalRef {
        return model.wsConst().focusedTerminalRef();
    }

    pub fn focusedPane(model: *Model) ?*Pane {
        const id = model.focusedTerminalRef() orelse return null;
        return model.provider.terminal(id);
    }

    /// The tab index whose tree holds `id` in any pane of the ACTIVE window.
    pub fn tabOfTerminal(model: *const Model, id: TerminalRef) ?usize {
        return model.wsConst().tabOfTerminal(id);
    }

    /// The label identity of a tab: its focused pane's terminal.
    pub fn tabTerminal(model: *const Model, index: usize) ?TerminalRef {
        return model.wsConst().tabTerminal(index);
    }

    /// Give `id` a tab in the active window. Idempotent; a terminal in another
    /// window is refused, never duplicated.
    pub fn admitTab(model: *Model, id: TerminalRef) bool {
        if (model.locateTerminal(id)) |where| return where.window == model.active_window;
        if (!model.canAddPane()) return false;
        model.pruneAttachmentState();
        return model.ws().admitTab(id);
    }

    pub fn dropTab(model: *Model, index: usize) void {
        model.ws().dropTab(index);
        model.pruneAttachmentState();
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

    /// Whether the config band is up: diagnostics exist and were not dismissed.
    pub fn configNoticeVisible(model: *const Model) bool {
        return model.config.diagnostic_count != 0 and !model.config_notice_dismissed;
    }

    /// The live terminal type size: the configured size (cmd+0's origin) plus
    /// the chord-driven offset.
    pub fn fontSize(model: *const Model) f32 {
        return std.math.clamp(
            model.config.fontSize() + model.font_size_offset,
            config_module.min_font_size,
            config_module.max_font_size,
        );
    }

    /// Step the type size; false (and no offset drift) at the clamp.
    pub fn stepFontSize(model: *Model, delta: f32) bool {
        const before = model.fontSize();
        model.font_size_offset += delta;
        const after = model.fontSize();
        if (after == before) {
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

    fn persistedTabPlacement(model: *const Model) topology.TabPlacement {
        return model.appearance_committed_placement orelse model.tab_placement;
    }

    pub fn topologySnapshot(model: *const Model) !TopologySnapshot {
        var snapshot: TopologySnapshot = .{ .tab_placement = model.persistedTabPlacement() };
        // Remote topology has one persisted authority: Phux layout metadata.
        // Older local files are superseded at the first shared publication.
        if (model.shared_workspace.session != 0) return snapshot;
        var written: u8 = 0;
        var windows: u8 = 0;
        // Renumbered densely: a closed middle window must not restore as a hole.
        for (0..max_windows) |window_index| {
            if (!model.windowOpen(window_index)) continue;
            const workspace = model.wsAtConst(window_index) orelse continue;
            snapshot.windows[windows] = try encodeWindow(model, workspace, &snapshot, &written);
            windows += 1;
        }
        snapshot.window_count = windows;
        snapshot.tab_count = written;

        // Directories come from each live pane's last OSC 7 report.
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

    /// A hash of the snapshot's shape across every window. Excludes working
    /// directories so `cd` does not cause writes; every save and the shutdown
    /// flush still record current directories.
    pub fn topologyFingerprint(model: *const Model) u64 {
        var hasher = std.hash.Wyhash.init(0);
        std.hash.autoHash(&hasher, model.persistedTabPlacement());
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

    /// Write the layout synchronously (the shutdown flush, when no effect
    /// queue will drain again). Failure is silent: never block exit on it.
    pub fn writeWorkspaceState(model: *const Model, io: std.Io) void {
        if (!model.state.enabled()) return;
        var bytes: [session_state.max_state_bytes]u8 = undefined;
        const snapshot = model.topologySnapshot() catch return;
        const encoded = session_state.serialize(&snapshot, &bytes) catch return;
        writeFileCreatingParent(io, model.state.path(), encoded) catch return;
    }

    /// Rewrite the `theme` key in the user's config file, preserving every
    /// other byte (a synchronous read-modify-write; the effect queue has no
    /// paired read). A leaf because its two buffers are 128 KB of stack. The
    /// theme stays applied live whatever the outcome; a refusal is returned
    /// so the chrome can say it did not persist.
    pub fn writeConfigTheme(model: *const Model, io: std.Io) ConfigWrite {
        if (!model.config_file.enabled()) return .no_destination;
        // An empty `theme =` would not parse back.
        if (model.config.theme.slice().len == 0) return .no_destination;
        const path = model.config_file.path();

        var source_bytes: [config_module.max_config_bytes]u8 = undefined;
        var rewritten: [config_module.max_config_bytes]u8 = undefined;
        const cwd = std.Io.Dir.cwd();

        // A missing file is ordinary: write a one-line config.
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
        writeFileCreatingParent(io, path, encoded) catch return .refused;
        return .written;
    }

    /// Whether the config file would take a write now (asked when Settings
    /// opens). A missing file counts as writable because it will be created.
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

/// What `Model.writeConfigTheme` did; having no config file is not a failure.
pub const ConfigWrite = enum { written, no_destination, refused };

/// Write first and create the parent directory only if that fails:
/// `createDirPath` rejects a parent reached through a symlink (macOS `/tmp`),
/// which would otherwise lose writes that succeed straight through the link.
fn writeFileCreatingParent(io: std.Io, path: []const u8, data: []const u8) !void {
    const cwd = std.Io.Dir.cwd();
    if (cwd.writeFile(io, .{ .sub_path = path, .data = data })) |_| return else |_| {}
    if (std.fs.path.dirname(path)) |parent| try cwd.createDirPath(io, parent);
    try cwd.writeFile(io, .{ .sub_path = path, .data = data });
}

/// Put every restored pane's shell in its recorded directory. Separate from
/// `restoreModel` because `Pane.argv` points into `Model.cwd_argv`, so it
/// must run on the model in its final storage.
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

/// Push the settings only the emulator can hold: palette overrides, the
/// cursor colour (as an override, so tokens do not overwrite it) and cursor
/// style. Call after `spawnPane`, whose reset would drop them.
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

pub fn restoreModel(gpa: std.mem.Allocator, io: std.Io, persisted: PersistedTopologySnapshot) !Model {
    return restoreModelWithScrollback(gpa, io, persisted, grid.Session.max_scrollback);
}

/// Restore with an explicit scrollback ceiling for the fresh sessions.
pub fn restoreModelWithScrollback(
    gpa: std.mem.Allocator,
    io: std.Io,
    persisted: PersistedTopologySnapshot,
    max_scrollback_bytes: usize,
) !Model {
    const snapshot = try topology.migrateTopologySnapshot(persisted);
    const context_id = try @import("provider_contract").context.allocate();
    const provider = try gpa.create(LocalProvider);
    provider.* = .{ .gpa = gpa, .io = io, .max_scrollback_bytes = max_scrollback_bytes, .context_id = context_id };

    var model: Model = .{
        .provider = provider,
        .tab_placement = snapshot.tab_placement,
        .saved_attachments = snapshot.references,
        .pending_attachments = @splat(true),
    };
    errdefer provider.destroy();
    errdefer for (model.secondary) |slot| if (slot) |workspace| std.heap.page_allocator.destroy(workspace);

    // Local leaves get fresh sessions; remote leaves stay pending until
    // provider evidence matches.
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
    deinitPeers(model);
    for (&model.secondary) |*slot| {
        if (slot.*) |workspace| std.heap.page_allocator.destroy(workspace);
        slot.* = null;
    }
    model.provider.destroy();
}

fn deinitPeers(model: *Model) void {
    for (model.peers.items) |entry| {
        entry.workspace.deinit();
        if (comptime support.phux_enabled) if (entry.provider) |peer| peer.destroy();
        std.heap.page_allocator.destroy(entry);
    }
    model.peers.deinit(std.heap.page_allocator);
}
