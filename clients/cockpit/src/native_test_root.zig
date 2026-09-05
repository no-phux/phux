//! Test-only facade for native engine regression coverage.
//! The shipping app coordinator is src/core.ts; this module exposes native
//! seams to the pre-cutover Zig regression suite without defining an app entry point.

const std = @import("std");
const native_sdk = @import("native_sdk");
const grid = @import("terminal/grid.zig");
const provider_contract = @import("provider_contract");

const support = @import("cockpit/phux_support.zig");
const local = @import("providers/local/provider.zig");
const topology = @import("cockpit/topology.zig");
const layout = @import("cockpit/layout.zig");
const model_module = @import("cockpit/model.zig");
const session_state = @import("cockpit/session_state.zig");
const startup = @import("cockpit/startup.zig");
const app_types = @import("cockpit/app_types.zig");
const runtime = @import("cockpit/terminal_runtime.zig");
const projection = @import("cockpit/native/workspace_projection.zig");
const pointer = @import("cockpit/pointer_input.zig");
const update_module = @import("cockpit/update.zig");
const scene = @import("cockpit/native/scene.zig");
const terminal_painter = @import("cockpit/native/terminal_painter.zig");
const host = @import("cockpit/native/host.zig");
const ts_snapshot = @import("cockpit/native/ts_snapshot.zig");
const config_module = @import("config/config.zig");
const theme_module = @import("config/theme.zig");

const geometry = native_sdk.geometry;

pub const panic = std.debug.FullPanic(native_sdk.debug.capturePanic);

pub const ProviderId = support.ProviderId;
pub const LocalTerminalId = support.LocalTerminalId;
pub const RemoteTerminalId = support.RemoteTerminalId;
pub const TerminalId = support.TerminalId;
pub const TerminalRef = support.TerminalRef;
pub const Generation = support.Generation;
pub const ReplicaOwner = support.ReplicaOwner;
pub const PixelSize = support.PixelSize;
pub const Viewport = support.Viewport;
pub const KeyAction = support.KeyAction;
pub const PhysicalKey = support.PhysicalKey;
pub const ModifierMask = support.ModifierMask;
pub const KeyInput = support.KeyInput;
pub const MouseAction = support.MouseAction;
pub const MouseButton = support.MouseButton;
pub const MouseInput = support.MouseInput;
pub const ScrollKind = support.ScrollKind;
pub const Scroll = support.Scroll;
pub const Presentation = support.Presentation;
pub const Phase = support.Phase;
pub const PhuxProvider = support.PhuxProvider;
pub const ProviderKind = support.ProviderKind;
pub const phux_enabled = support.phux_enabled;
pub const phux_channel_key = support.phux_channel_key;
pub const pointer_channel_key = support.pointer_channel_key;
pub const max_remote_terminals = support.max_remote_terminals;
pub const providerKind = support.providerKind;
pub const localRef = support.localRef;
pub const refEql = support.refEql;
pub const localId = @import("provider_contract").localId;

pub const Pane = local.Pane;
pub const LocalProvider = local.LocalProvider;
pub const Provider = local.Provider;
pub const max_terminals = local.max_terminals;
pub const max_tabs = topology.max_tabs;
pub const max_panes_per_tab = layout.max_panes;
pub const clipboard_key = local.clipboard_key;
pub const paste_clipboard_key = local.paste_clipboard_key;
pub const outbound_buffer_bytes = local.outbound_buffer_bytes;
pub const initialTerminalId = local.initialTerminalId;
pub const initialTerminalRef = local.initialTerminalRef;
pub const ptyKey = local.ptyKey;
pub const paneArgv = local.paneArgv;

pub const TabPlacement = topology.TabPlacement;
pub const SurfaceSelection = topology.SurfaceSelection;
pub const SnapshotSelection = topology.SnapshotSelection;
pub const SnapshotTab = topology.SnapshotTab;
pub const SnapshotNode = topology.SnapshotNode;
pub const TopologySnapshot = topology.TopologySnapshot;
pub const LegacyTopologySnapshotV0 = topology.LegacyTopologySnapshotV0;
pub const LegacyTopologySnapshotV1 = topology.LegacyTopologySnapshotV1;
pub const LegacyTopologySnapshotV2 = topology.LegacyTopologySnapshotV2;
pub const LegacyTopologySnapshotV3 = topology.LegacyTopologySnapshotV3;
pub const SnapshotWindow = topology.SnapshotWindow;
pub const max_snapshot_windows = topology.max_snapshot_windows;
pub const max_snapshot_tabs = topology.max_snapshot_tabs;
pub const primarySnapshotSelection = topology.primarySelection;
pub const PersistedTopologySnapshot = topology.PersistedTopologySnapshot;
pub const SnapshotCwd = topology.SnapshotCwd;
pub const max_snapshot_cwd_bytes = topology.max_snapshot_cwd_bytes;
pub const terminalOffset = topology.terminalOffset;
pub const singleLeafTab = topology.singleLeafTab;
pub const Tree = layout.Tree;
pub const Kind = layout.Kind;
pub const LayoutNodeId = layout.NodeId;
pub const layout_none = layout.none;
pub const Orientation = layout.Orientation;
pub const Direction = layout.Direction;
pub const LayoutPane = layout.Pane;
pub const topology_snapshot_version = topology.topology_snapshot_version;
pub const process_restoration_supported = topology.process_restoration_supported;
pub const migrateTopologySnapshot = topology.migrateTopologySnapshot;

pub const PointerModifiers = model_module.PointerModifiers;
pub const TerminalPointerEvent = model_module.TerminalPointerEvent;
pub const PointerDragMode = model_module.PointerDragMode;
pub const PointerCapture = model_module.PointerCapture;
pub const BrowserPage = model_module.BrowserPage;
pub const Model = model_module.Model;
pub const Workspace = model_module.Workspace;
pub const TerminalLocation = model_module.TerminalLocation;
pub const max_windows = model_module.max_windows;
pub const max_secondary_windows = model_module.max_secondary_windows;
pub const reconcileRemoteRefs = model_module.reconcileRemoteRefs;
pub const initialModelWithPhux = model_module.initialModelWithPhux;
pub const initialModel = model_module.initialModel;
pub const attachPhuxProvider = model_module.attachPhuxProvider;
pub const initialModelWithIo = model_module.initialModelWithIo;
pub const initialProductionModelWithIo = model_module.initialProductionModelWithIo;
pub const restoreModel = model_module.restoreModel;
pub const applyRestoredWorkingDirectories = model_module.applyRestoredWorkingDirectories;
pub const writeWorkspaceState = model_module.writeWorkspaceState;
pub const deinitModel = model_module.deinitModel;
pub const StatePersistence = model_module.StatePersistence;

pub const state_file_name = session_state.file_name;
pub const release_state_file_name = session_state.release_file_name;
pub const max_state_bytes = session_state.max_state_bytes;
pub const serializeWorkspaceState = session_state.serialize;
pub const parseWorkspaceState = session_state.parse;
pub const workspaceStatePath = session_state.joinPath;
pub const topology_state_file_key = update_module.topology_state_file_key;
pub const topology_persist_timer_key = update_module.topology_persist_timer_key;
pub const topology_persist_debounce_ms = update_module.topology_persist_debounce_ms;

pub const Msg = app_types.Msg;
pub const TerminalApp = app_types.TerminalApp;
pub const Fx = app_types.Fx;

pub const moveResponsesToOutbound = runtime.moveResponsesToOutbound;
pub const update = update_module.update;
pub const appShortcutKeyMask = update_module.appShortcutKeyMask;
pub const RemoteUiState = model_module.RemoteUiState;
pub const retainSelectionAfterCopy = update_module.retainSelectionAfterCopy;
pub const remoteFocusTarget = update_module.remoteFocusTarget;

/// Geometry exports retained for native engine tests. Declarative chrome is
/// audited by `ts-chrome-parity` in `native_extension.zig`.
pub const chrome_band_height = projection.chrome_band_height;
pub const chrome_band_inset = projection.chrome_band_inset;
pub const chrome_control_extent = projection.chrome_control_extent;
pub const chrome_icon_extent = projection.chrome_icon_extent;
pub const chrome_gap = projection.chrome_gap;
pub const chrome_hit_target = projection.chrome_hit_target;
pub const grid_inset = projection.grid_inset;
pub const tab_height = projection.tab_height;
pub const tab_control_extent = projection.tab_control_extent;
pub const tab_marker_extent = projection.tab_marker_extent;
pub const tab_indicator_thickness = projection.tab_indicator_thickness;

pub const header_height = projection.header_height;
pub const side_rail_width = projection.side_rail_width;
pub const side_rail_gap = projection.side_rail_gap;
pub const side_tab_height = projection.side_tab_height;
pub const split_divider_width = projection.split_divider_width;
pub const split_pane_min_width = projection.split_pane_min_width;
pub const split_pane_min_height = projection.split_pane_min_height;
pub const webkit_parking_extent = projection.webkit_parking_extent;
pub const search_bar_height = projection.search_bar_height;
pub const searchRevealed = projection.searchRevealed;
pub const config_notice_height = projection.config_notice_height;
pub const config_notice_bytes = projection.config_notice_bytes;
pub const configNoticeRevealed = projection.configNoticeRevealed;
pub const configNoticeLine = projection.configNoticeLine;
pub const chrome_command_envelope = projection.chrome_command_envelope;
pub const cockpitTokens = projection.cockpitTokens;
pub const terminalTokens = projection.terminalTokens;
pub const terminalTokensFrom = projection.terminalTokensFrom;
pub const terminalCellMetricsFor = projection.terminalCellMetricsFor;
pub const windowPadding = projection.windowPadding;
pub const chromeRevealed = projection.chromeRevealed;
pub const workspaceChrome = projection.workspaceChrome;
pub const workspaceChromeIn = projection.workspaceChromeIn;
pub const resolvePanes = projection.resolvePanes;
pub const resolvePanesIn = projection.resolvePanesIn;
pub const proposedViewportsIn = projection.proposedViewportsIn;
pub const PaneViewport = projection.PaneViewport;
pub const paneFrames = projection.paneFrames;
pub const paneAtPoint = projection.paneAtPoint;
pub const paneFrameFor = projection.paneFrameFor;
pub const tabTriggerHeight = projection.tabTriggerHeight;
pub const TabWindow = projection.TabWindow;
pub const visibleTabWindow = projection.visibleTabWindow;
pub const visibleTabWindowIn = projection.visibleTabWindowIn;
pub const visibleTabRun = projection.visibleTabRun;
pub const tabRunWidthIn = projection.tabRunWidthIn;
pub const tabStripStatusReserveIn = projection.tabStripStatusReserveIn;
pub const tab_strip_notice_reserve = projection.tab_strip_notice_reserve;
pub const tab_strip_save_notice_reserve = projection.tab_strip_save_notice_reserve;
pub const tab_strip_pane_status_reserve = projection.tab_strip_pane_status_reserve;
pub const tab_extent = projection.tab_extent;
pub const tab_min_extent = projection.tab_min_extent;
pub const tab_label_furniture = projection.tab_label_furniture;
pub const tabLabelWidth = projection.tabLabelWidth;
pub const chromeTextWidth = projection.chromeTextWidth;
pub const elideTitleMiddleInto = projection.elideTitleMiddleInto;
pub const max_painted_title_bytes = projection.max_painted_title_bytes;
pub const TabLabelIdentity = projection.TabLabelIdentity;
pub const tabCanClose = projection.tabCanClose;
pub const tabLabelIdentityIn = projection.tabLabelIdentityIn;
pub const pane_dim_command_id_base = terminal_painter.pane_dim_command_id_base;
pub const pane_focus_command_id_base = terminal_painter.pane_focus_command_id_base;
pub const link_preview_ground_command_id_base = terminal_painter.link_preview_ground_command_id_base;
pub const link_preview_text_command_id_base = terminal_painter.link_preview_text_command_id_base;
pub const link_preview_authority_command_id_base = terminal_painter.link_preview_authority_command_id_base;
pub const linkPreviewCommandReserve = terminal_painter.linkPreviewCommandReserve;
pub const terminalNeedsAttention = projection.terminalNeedsAttention;
pub const tabsRideTitlebarIn = projection.tabsRideTitlebarIn;
pub const paletteRowsIn = projection.paletteRowsIn;
pub const paletteSelectedTabIn = projection.paletteSelectedTabIn;
pub const paletteWindowFor = projection.paletteWindowFor;
pub const PaletteWindow = projection.PaletteWindow;
pub const palette_max_visible_rows = projection.palette_max_visible_rows;
pub const titlebar_tab_leading_reserve = projection.titlebar_tab_leading_reserve;
pub const titlebar_tab_band_min = projection.titlebar_tab_band_min;

pub const canvas_label = scene.canvas_label;
pub const webview_label = scene.webview_label;
pub const webview_anchor = scene.webview_anchor;
pub const app_name = scene.app_name;
pub const bundle_id = scene.bundle_id;
pub const main_window_label = scene.main_window_label;
pub const window_width = scene.window_width;
pub const window_height = scene.window_height;
pub const window_min_width = scene.window_min_width;
pub const window_min_height = scene.window_min_height;
pub const web_origins = scene.web_origins;
pub const cockpit_shortcuts = scene.cockpit_shortcuts;
pub const cockpit_menus = scene.cockpit_menus;
pub const shell_scene = scene.shell_scene;
pub const secondary_window_labels = scene.secondary_window_labels;
pub const secondary_canvas_labels = scene.secondary_canvas_labels;
pub const windowLabelFor = scene.windowLabelFor;
pub const canvasLabelFor = scene.canvasLabelFor;
pub const windowIndexForCanvas = scene.windowIndexForCanvas;
pub const windowIndexForWindow = scene.windowIndexForWindow;

pub const Config = config_module.Config;
pub const ConfigTabPlacement = config_module.TabPlacement;
pub const configPath = config_module.joinPath;
pub const parseConfig = config_module.parse;
pub const loadConfigOrDefault = config_module.loadOrDefault;
pub const setConfigKey = config_module.setKey;
pub const max_config_bytes = config_module.max_config_bytes;
pub const Theme = theme_module.Theme;
pub const builtin_themes = theme_module.builtins;
pub const themeByName = theme_module.byName;
pub const themeIndexOf = theme_module.indexOf;
pub const contrastRatio = theme_module.contrastRatio;
pub const contrastRatioLuminance = theme_module.contrastRatioLuminance;
pub const relativeLuminance = theme_module.relativeLuminance;
pub const wcag_aa_body_text = theme_module.wcag_aa_body_text;
pub const wcag_aaa_body_text = theme_module.wcag_aaa_body_text;
pub const Legibility = theme_module.Legibility;
pub const legibility = projection.legibility;
pub const legibilityOf = projection.legibilityOf;
pub const Settings = model_module.Settings;
pub const TabDrag = model_module.TabDrag;
pub const pinch_points_per_step = model_module.pinch_points_per_step;
pub const quotePaths = @import("cockpit/shell_words.zig").quotePaths;
pub const theme_auto_dark = theme_module.auto_dark;
pub const theme_auto_light = theme_module.auto_light;
pub const terminalTitleInto = projection.terminalTitleInto;
pub const max_terminal_title_bytes = projection.max_terminal_title_bytes;
pub const cockpit_status_item = scene.cockpit_status_item;
pub const onDrop = update_module.onDrop;
pub const ConfigFile = model_module.ConfigFile;
pub const paneArgvIn = local.paneArgvIn;
pub const CwdArgv = local.CwdArgv;

pub const paintTerminalWindow = terminal_painter.paintWindowIndex;
pub const tabPlacementFromText = startup.tabPlacementFromText;
pub const CockpitHost = host.CockpitHost;
pub const installPostPresentResources = host.installPostPresentResources;
pub const encodeTsSnapshot = ts_snapshot.encode;
pub const TsTabRun = ts_snapshot.TabRun;
pub const ts_snapshot_max_bytes = ts_snapshot.max_bytes;
pub const selection_autoscroll_timer_id = app_types.selection_autoscroll_timer_id;
pub const terminal_font_id = scene.terminal_font_id;
pub const PhuxEnvironment = startup.PhuxEnvironment;
pub const resolvePhuxConfig = startup.resolvePhuxConfig;
pub const createPhuxProviderFromConfig = startup.createPhuxProviderFromConfig;
pub const configuredPhuxSocket = startup.configuredPhuxSocket;
pub const configuredPhuxSession = startup.configuredPhuxSession;
pub const resolveConfigPath = startup.resolveConfigPath;
pub const resolveDotfileConfigPath = startup.resolveDotfileConfigPath;
pub const LoadedConfig = startup.LoadedConfig;
pub const resolveStatePath = startup.resolveStatePath;
pub const PersistedStateLoad = startup.PersistedStateLoad;
pub const readPersistedState = startup.readPersistedState;
pub const WorkspaceRestore = startup.WorkspaceRestore;
pub const restoreWorkspace = startup.restoreWorkspace;
pub const WorkspaceStateProvenance = startup.WorkspaceStateProvenance;

/// Minimal test host for native engine regressions that exercise the retained
/// model/update/effects seams through `UiApp`. It deliberately renders no Zig
/// widget chrome: shipping chrome belongs to `core.ts` + `.native` markup.
fn testTerminalSurface(ui: *TerminalApp.Ui, model: *const Model, node: layout.NodeId, terminal_ref: TerminalRef) TerminalApp.Ui.Node {
    const pane = model.provider.terminalConst(terminal_ref);
    const screen = if (pane) |local_pane| local_pane.session.screenText() else "";
    const local_id = provider_contract.localId(terminal_ref);
    if (pane == null or local_id == null) {
        return ui.el(.stack, .{
            .global_key = .{ .index = terminal_painter.terminalPaintIndex(model, terminal_ref) },
            .grow = 1,
            .min_width = split_pane_min_width,
            .min_height = split_pane_min_height,
            .opacity = 0,
            .text = screen,
            .on_press = .{ .focus_pane = node },
            .semantics = .{ .focusable = true, .label = "Terminal" },
        }, .{});
    }
    const local_pane = pane.?;
    const context_menu = [_]TerminalApp.Ui.ContextMenuItem{
        .{ .label = "Copy", .msg = .{ .copy_terminal = local_pane.id }, .enabled = local_pane.session.selectionActive() },
        .{ .label = "Paste", .msg = .{ .paste_terminal = local_pane.id }, .enabled = local_pane.acceptsInput() },
    };
    return ui.terminal(.{
        .global_key = .{ .index = @intCast(@intFromEnum(local_id.?)) },
        .grow = 1,
        .min_width = split_pane_min_width,
        .min_height = split_pane_min_height,
        .opacity = 0,
        .text = screen,
        .on_press = .{ .focus_pane = node },
        .context_menu = &context_menu,
        .context_menu_policy = if (pointer.paneReportsMouse(local_pane)) .disabled else .automatic,
        .semantics = .{ .focusable = true, .label = "Terminal" },
    });
}

fn testSplitResizeHandler(comptime node: layout.NodeId) TerminalApp.Ui.ValueMsgFn {
    return struct {
        fn make(value: f32) Msg {
            return .{ .split_resized = .{ .node = node, .value = value } };
        }
    }.make;
}

const test_split_resize_handlers: [layout.max_nodes]TerminalApp.Ui.ValueMsgFn = blk: {
    var table: [layout.max_nodes]TerminalApp.Ui.ValueMsgFn = undefined;
    for (0..layout.max_nodes) |index| table[index] = testSplitResizeHandler(@intCast(index));
    break :blk table;
};

fn testPaneSubtree(ui: *TerminalApp.Ui, model: *const Model, tree: *const layout.Tree, node: layout.NodeId) TerminalApp.Ui.Node {
    const entry = tree.node(node);
    return switch (entry.kind) {
        .free => ui.el(.stack, .{}, .{}),
        .leaf => testTerminalSurface(ui, model, node, entry.terminal orelse return ui.el(.stack, .{}, .{})),
        .branch => ui.split(.{
            .split_axis = switch (entry.orientation) {
                .horizontal => .horizontal,
                .vertical => .vertical,
            },
            .grow = 1,
            .min_width = split_pane_min_width,
            .min_height = split_pane_min_height,
            .gap = split_divider_width,
            .value = entry.fraction,
            .on_resize = test_split_resize_handlers[node],
        }, .{
            testPaneSubtree(ui, model, tree, entry.first),
            testPaneSubtree(ui, model, tree, entry.second),
        }),
    };
}

fn testView(ui: *TerminalApp.Ui, model: *const Model) TerminalApp.Ui.Node {
    const ws = model.wsConst();
    const tree = ws.selectedTreeConst() orelse return ui.el(.stack, .{}, .{});
    const chrome = projection.workspaceChromeIn(model, ws, ws.surface_size);
    const inset = projection.windowPadding(model);
    return ui.column(.{ .grow = 1, .padding = inset }, .{
        ui.el(.stack, .{ .height = @max(0, chrome.content.y - inset) }, .{}),
        ui.row(.{ .grow = 1 }, .{
            ui.el(.stack, .{ .width = @max(0, chrome.content.x - inset) }, .{}),
            testPaneSubtree(ui, model, tree, tree.root),
        }),
    });
}

fn testOnFrame(model: *const Model, frame: native_sdk.platform.GpuFrame) ?Msg {
    if (frame.size.width <= 0 or frame.size.height <= 0) return null;
    const window_index = scene.windowIndexForCanvas(frame.label) orelse return null;
    const ws = model.wsAtConst(window_index) orelse return null;
    const frame_scale = if (pointer.validScale(frame.scale_factor)) frame.scale_factor else ws.surface_scale_factor;
    var pending = false;
    for (0..max_terminals) |index| {
        if (model.provider.states[index] != .active) continue;
        const pane = model.provider.slotConst(index);
        if (pane.outbound_len > 0 or pane.session.response_len > 0) pending = true;
    }
    const proposals = projection.proposedViewportsIn(model, ws, frame.size);
    for (proposals.slice()) |proposal| {
        if (!projection.viewportDiffers(model, proposal)) continue;
        return .{ .viewport = .{
            .terminal_ref = proposal.terminal,
            .cols = proposal.cols,
            .rows = proposal.rows,
            .size = frame.size,
            .scale_factor = frame_scale,
            .window = @intCast(window_index),
            .window_id = frame.window_id,
        } };
    }
    if (proposals.incomplete) return if (pending) .flush_outbound else null;
    for (0..max_terminals) |index| {
        if (model.provider.states[index] != .active) continue;
        if (model.provider.slotConst(index).session.searchPending()) return .search_tick;
    }
    if (ws.surface_size.width != frame.size.width or ws.surface_size.height != frame.size.height or
        ws.surface_scale_factor != frame_scale or ws.window_id != frame.window_id)
    {
        return .{ .surface_resized = .{
            .size = frame.size,
            .scale_factor = frame_scale,
            .window = @intCast(window_index),
            .window_id = frame.window_id,
        } };
    }
    return if (pending) .flush_outbound else null;
}

fn testOnTimer(id: u64, _: u64) ?Msg {
    return if (id == selection_autoscroll_timer_id) .selection_autoscroll else null;
}

fn testOnCommand(name: []const u8) ?Msg {
    if (std.mem.eql(u8, name, "window.new")) return .new_window;
    if (std.mem.eql(u8, name, "window.fullscreen")) return .toggle_fullscreen;
    if (std.mem.eql(u8, name, "window.minimize")) return .minimize_window;
    if (std.mem.eql(u8, name, "surface.1")) return .{ .select_position = 0 };
    if (std.mem.eql(u8, name, "surface.2")) return .{ .select_position = 1 };
    if (std.mem.eql(u8, name, "surface.3")) return .{ .select_position = 2 };
    if (std.mem.eql(u8, name, "surface.4")) return .{ .select_position = 3 };
    if (std.mem.eql(u8, name, "surface.5")) return .{ .select_position = 4 };
    if (std.mem.eql(u8, name, "tab.previous")) return .{ .cycle_tab = -1 };
    if (std.mem.eql(u8, name, "tab.next")) return .{ .cycle_tab = 1 };
    if (std.mem.eql(u8, name, "pane.split-right")) return .split_right;
    if (std.mem.eql(u8, name, "pane.split-down")) return .split_down;
    if (std.mem.eql(u8, name, "terminal.new")) return .new_terminal;
    if (std.mem.eql(u8, name, "terminal.close")) return .close_terminal;
    if (std.mem.eql(u8, name, "tab.move-left")) return .{ .move_terminal = -1 };
    if (std.mem.eql(u8, name, "tab.move-right")) return .{ .move_terminal = 1 };
    if (std.mem.eql(u8, name, "pane.previous")) return .{ .cycle_pane = -1 };
    if (std.mem.eql(u8, name, "pane.next")) return .{ .cycle_pane = 1 };
    if (std.mem.eql(u8, name, "terminal.copy")) return .copy_selection;
    if (std.mem.eql(u8, name, "terminal.paste")) return .paste_focused;
    if (std.mem.eql(u8, name, "terminal.select-all")) return .select_all;
    if (std.mem.eql(u8, name, "terminal.clear")) return .clear_terminal;
    if (std.mem.eql(u8, name, "terminal.find")) return .search_open;
    if (std.mem.eql(u8, name, "terminal.find-next")) return .{ .search_step = 1 };
    if (std.mem.eql(u8, name, "terminal.find-previous")) return .{ .search_step = -1 };
    if (std.mem.eql(u8, name, "view.font-larger")) return .{ .font_size_step = 1 };
    if (std.mem.eql(u8, name, "view.font-smaller")) return .{ .font_size_step = -1 };
    if (std.mem.eql(u8, name, "view.font-reset")) return .font_size_reset;
    if (std.mem.eql(u8, name, "pane.focus-left")) return .{ .focus_direction = .left };
    if (std.mem.eql(u8, name, "pane.focus-right")) return .{ .focus_direction = .right };
    if (std.mem.eql(u8, name, "pane.focus-up")) return .{ .focus_direction = .up };
    if (std.mem.eql(u8, name, "pane.focus-down")) return .{ .focus_direction = .down };
    return null;
}

fn testBuildWindow(model: *const Model, builder: *native_sdk.canvas.Builder, context: TerminalApp.ChromeContext) anyerror!void {
    const window_index = scene.windowIndexForCanvas(context.canvas_label) orelse return;
    return terminal_painter.paintWindowIndex(
        model,
        builder,
        window_index,
        context.size,
        context.tokens,
        context.window_id,
    );
}

fn testBuildMain(model: *const Model, builder: *native_sdk.canvas.Builder, size: geometry.SizeF, tokens: native_sdk.canvas.DesignTokens) anyerror!void {
    return terminal_painter.paintWindowIndex(model, builder, 0, size, tokens, 0);
}

pub fn appOptions() TerminalApp.Options {
    return .{
        .name = app_name,
        .scene = shell_scene,
        .canvas_label = canvas_label,
        .tokens_fn = cockpitTokens,
        // Legacy UiApp fixtures call `installPostPresentResources` after the
        // first frame, so only seed the regular face here; shipping registers
        // all four up front in `native_extension.zig`.
        .fonts = scene.cockpit_fonts[0..1],
        .init_fx = update_module.initFx,
        .update_fx = update,
        .view = testView,
        .on_key = update_module.onKey,
        .key_release_events = true,
        .on_text = update_module.onText,
        .on_wheel = update_module.onWheel,
        .on_pinch = update_module.onPinch,
        .on_drop = update_module.onDrop,
        .on_appearance = update_module.onAppearance,
        .on_timer = testOnTimer,
        .on_chrome = update_module.onChrome,
        .on_lifecycle = update_module.onLifecycle,
        .on_frame = testOnFrame,
        .on_command = testOnCommand,
        .chrome = .{
            .prefix_commands = chrome_command_envelope,
            .variable_prefix = true,
            .build = testBuildMain,
            .build_window = testBuildWindow,
        },
    };
}

pub const buildChromeWindow = testBuildWindow;

test "tab placement configuration accepts only documented values" {
    try std.testing.expectEqual(TabPlacement.top, tabPlacementFromText("top").?);
    try std.testing.expectEqual(TabPlacement.side, tabPlacementFromText("side").?);
    try std.testing.expectEqual(TabPlacement.side, tabPlacementFromText("SIDEBAR").?);
    try std.testing.expectEqual(@as(?TabPlacement, null), tabPlacementFromText("left"));
}

test "AppKit pointer buttons map to provider mouse buttons" {
    try std.testing.expectEqual(MouseButton.left, pointer.pointerButton(0));
    try std.testing.expectEqual(MouseButton.right, pointer.pointerButton(1));
    try std.testing.expectEqual(MouseButton.middle, pointer.pointerButton(2));
    try std.testing.expectEqual(MouseButton.button_4, pointer.pointerButton(3));
    try std.testing.expectEqual(MouseButton.button_5, pointer.pointerButton(4));
    try std.testing.expectEqual(MouseButton.none, pointer.pointerButton(std.math.maxInt(u32)));
}

test {
    _ = @import("cockpit/native/ts_protocol.zig");
    _ = @import("tests/app_contract_tests.zig");
    _ = @import("tests/url_detection_tests.zig");
    _ = @import("tests/hyperlink_tests.zig");
    _ = @import("tests/grid_state_tests.zig");
    _ = @import("tests/grid_rendering_tests.zig");
    _ = @import("tests/cell_attribute_tests.zig");
    _ = @import("tests/minimum_contrast_tests.zig");
    _ = @import("tests/provider_identity_tests.zig");
    _ = @import("tests/credential_store_tests.zig");
    _ = @import("tests/terminal_keyboard_tests.zig");
    _ = @import("tests/clipboard_tests.zig");
    _ = @import("tests/outbound_io_tests.zig");
    _ = @import("tests/terminal_lifecycle_tests.zig");
    _ = @import("tests/record_replay_tests.zig");
    _ = @import("tests/terminal_registry_tests.zig");
    _ = @import("tests/topology_persistence_tests.zig");
    _ = @import("tests/workspace_layout_tests.zig");
    _ = @import("tests/pointer_selection_tests.zig");
    _ = @import("tests/mouse_protocol_tests.zig");
    _ = @import("tests/adversarial_isolation_tests.zig");
    _ = @import("tests/layout_tree_tests.zig");
    _ = @import("tests/config_tests.zig");
    _ = @import("tests/shell_identity_tests.zig");
    _ = @import("tests/config_wiring_tests.zig");
    _ = @import("tests/tab_identity_tests.zig");
    _ = @import("tests/ts_snapshot_tests.zig");
    _ = @import("tests/scrollback_search_tests.zig");
}
