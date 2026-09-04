//! Native spatial cockpit composition root.

const std = @import("std");
const runner = @import("runner");
const native_sdk = @import("native_sdk");
const grid = @import("terminal/grid.zig");

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
const view_module = @import("cockpit/native/view.zig");
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

/// The chrome register. See docs/DESIGN_SYSTEM.md, and
/// `src/tests/chrome_register_tests.zig` for what holds it in place.
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
pub const pane_dim_command_id_base = view_module.pane_dim_command_id_base;
pub const pane_focus_command_id_base = view_module.pane_focus_command_id_base;
pub const link_preview_ground_command_id_base = view_module.link_preview_ground_command_id_base;
pub const link_preview_text_command_id_base = view_module.link_preview_text_command_id_base;
pub const link_preview_authority_command_id_base = view_module.link_preview_authority_command_id_base;
pub const linkPreviewCommandReserve = view_module.linkPreviewCommandReserve;
pub const terminalNeedsAttention = projection.terminalNeedsAttention;
pub const tabsRideTitlebarIn = projection.tabsRideTitlebarIn;
pub const paletteRowsIn = projection.paletteRowsIn;
pub const paletteSelectedTabIn = projection.paletteSelectedTabIn;
pub const paletteWindowFor = projection.paletteWindowFor;
pub const PaletteWindow = projection.PaletteWindow;
pub const palette_max_visible_rows = projection.palette_max_visible_rows;
pub const palette_width = view_module.palette_width;
pub const palette_row_height = view_module.palette_row_height;
pub const palette_padding = view_module.palette_padding;
pub const palette_top_inset = view_module.palette_top_inset;
pub const settings_row_height = view_module.settings_row_height;
pub const settings_padding = view_module.settings_padding;
pub const settings_margin = view_module.settings_margin;
pub const field_caret_width = view_module.field_caret_width;
pub const field_caret_height = view_module.field_caret_height;
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
pub const statusItem = view_module.statusItem;
pub const cockpit_status_item = scene.cockpit_status_item;
pub const onDrop = update_module.onDrop;
pub const ConfigFile = model_module.ConfigFile;
pub const settings_width = view_module.settings_width;
pub const settings_panel_label = view_module.settings_panel_label;
pub const settings_sample_label = view_module.settings_sample_label;
pub const settings_phux_transport_label = view_module.settings_phux_transport_label;
pub const paneArgvIn = local.paneArgvIn;
pub const CwdArgv = local.CwdArgv;

pub const view = view_module.view;
pub const viewWindow = view_module.viewWindow;
pub const windowView = view_module.windowView;
pub const declaredWindows = view_module.windows;
pub const buildChrome = view_module.buildChrome;
pub const buildChromeWindow = view_module.buildChromeWindow;
pub const webPanes = view_module.webPanes;
pub const onCommand = view_module.onCommand;
pub const onFrame = view_module.onFrame;
pub const tabPlacementFromText = view_module.tabPlacementFromText;
pub const onTimer = view_module.onTimer;
pub const CockpitHost = host.CockpitHost;
pub const installPostPresentResources = host.installPostPresentResources;
pub const encodeTsSnapshot = ts_snapshot.encode;
pub const TsTabRun = ts_snapshot.TabRun;
pub const ts_snapshot_max_bytes = ts_snapshot.max_bytes;
pub const selection_autoscroll_timer_id = app_types.selection_autoscroll_timer_id;
pub const terminal_font_id = scene.terminal_font_id;
const terminal_bold_font_id = scene.terminal_bold_font_id;
const terminal_italic_font_id = scene.terminal_italic_font_id;
const terminal_bold_italic_font_id = scene.terminal_bold_italic_font_id;
const app_permissions = [_][]const u8{ native_sdk.security.permission_command, native_sdk.security.permission_view };

pub fn appOptions() TerminalApp.Options {
    return .{
        .name = app_name,
        .scene = shell_scene,
        .canvas_label = canvas_label,
        .tokens_fn = cockpitTokens,
        // Register the regular face for the first useful frame. The host adds
        // the three weighted companions after that present, using the pin's
        // late-font seam so the next rebuild replaces transient synthesis
        // without charging four 2.3 MB parses to launch.
        .fonts = scene.cockpit_fonts[0..1],
        .init_fx = update_module.initFx,
        .update_fx = update,
        .view = view,
        .on_key = update_module.onKey,
        .key_release_events = true,
        .on_text = update_module.onText,
        .on_wheel = update_module.onWheel,
        .on_pinch = update_module.onPinch,
        .on_drop = update_module.onDrop,
        .on_appearance = update_module.onAppearance,
        .on_timer = view_module.onTimer,
        .on_chrome = update_module.onChrome,
        .on_lifecycle = update_module.onLifecycle,
        .on_frame = view_module.onFrame,
        .web_panes = webPanes,
        // The menu-bar extra: an install-time shell in the scene, and a live
        // derivation beside `web_panes` and `windows_fn` — same shape, same
        // cadence, model as the only source of truth.
        .status_item = scene.cockpit_status_item,
        .status_item_fn = view_module.statusItem,
        .on_command = onCommand,
        // The declared secondary windows, and their trees. Presence in
        // `windows_fn`'s answer IS visibility, so `Model.closeWindow` closes
        // a window by no longer naming it.
        .windows_fn = view_module.windows,
        .window_view = view_module.windowView,
        .chrome = .{
            .prefix_commands = chrome_command_envelope,
            .variable_prefix = true,
            // `build` still has to be set (it is the non-optional field), but
            // `build_window` is what actually runs: it is the only one that
            // can say WHICH window it is painting, and for an app whose
            // terminals are chrome commands the difference is a second window
            // full of live cells versus a second window painting a tab strip
            // over an empty canvas.
            .build = view_module.buildChrome,
            .build_window = view_module.buildChromeWindow,
        },
    };
}

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

pub fn main(init: std.process.Init) !void {
    var initialized = try startup.initializeModel(std.heap.page_allocator, init);
    var model = initialized.model;
    defer deinitModel(&model);
    const app_state = try std.heap.page_allocator.create(CockpitHost);
    defer std.heap.page_allocator.destroy(app_state);
    app_state.init(std.heap.page_allocator, model, appOptions());
    defer app_state.deinit();
    // The argv can only be written once the model is in the storage it will
    // live in: it holds SLICES into that model's own `cwd_argv`.
    if (initialized.provenance == .restored) applyRestoredWorkingDirectories(&app_state.inner.model, &initialized.restored_snapshot);
    // The runtime's own `.stop` lifecycle already flushes the layout, which is
    // the path that actually fires on a macOS quit. This is the belt to that
    // pair of braces, for a host whose run loop returns without a shutdown
    // event; it runs before `app_state.deinit`, and writing the same bytes
    // twice costs one file write on exit.
    defer app_state.inner.model.writeWorkspaceState(init.io);
    try runner.runWithOptions(app_state.app(), .{
        .app_name = app_name,
        .window_title = app_name,
        .bundle_id = bundle_id,
        .default_frame = geometry.RectF.init(0, 0, scene.window_width, scene.window_height),
        .restore_state = false,
        .js_window_api = false,
        .shortcuts = &cockpit_shortcuts,
        .menus = &cockpit_menus,
        .security = .{
            .permissions = &app_permissions,
            .navigation = .{
                .allowed_origins = &web_origins,
                .external_links = .{ .action = .deny },
            },
        },
    }, init);
}

test "tab placement configuration accepts only documented values" {
    try std.testing.expectEqual(TabPlacement.top, tabPlacementFromText("top").?);
    try std.testing.expectEqual(TabPlacement.side, tabPlacementFromText("side").?);
    try std.testing.expectEqual(TabPlacement.side, tabPlacementFromText("SIDEBAR").?);
    try std.testing.expectEqual(@as(?TabPlacement, null), tabPlacementFromText("left"));
}

// GUARD: first-frame-deferred-fonts
test "the terminal family is bundled whole while startup registers only regular" {
    const options = appOptions();
    // The regular face is enough for a useful first frame; missing companions
    // render through the SDK's synthesis until CockpitHost registers the real
    // faces immediately after that present.
    try std.testing.expectEqual(@as(usize, 1), options.fonts.len);
    try std.testing.expectEqual(terminal_font_id, options.fonts[0].id);
    const expected = [_]struct { id: @TypeOf(terminal_font_id), name: []const u8 }{
        .{ .id = terminal_font_id, .name = "JetBrainsMonoNL Nerd Font Mono Regular" },
        .{ .id = terminal_bold_font_id, .name = "JetBrainsMonoNL Nerd Font Mono Bold" },
        .{ .id = terminal_italic_font_id, .name = "JetBrainsMonoNL Nerd Font Mono Italic" },
        .{ .id = terminal_bold_italic_font_id, .name = "JetBrainsMonoNL Nerd Font Mono Bold Italic" },
    };
    try std.testing.expectEqual(expected.len, scene.cockpit_fonts.len);
    for (expected, 0..) |want, index| {
        const registration = scene.cockpit_fonts[index];
        try std.testing.expectEqual(want.id, registration.id);
        try std.testing.expectEqualStrings(want.name, registration.name);
        // Every one is a real TrueType file, not an empty embed that would
        // register and then render nothing.
        try std.testing.expect(registration.ttf.len > 4);
        try std.testing.expectEqualSlices(u8, &.{ 0, 1, 0, 0 }, registration.ttf[0..4]);
    }
    // Ids must be distinct, or a later registration overwrites an earlier one
    // and a whole weight vanishes.
    for (expected, 0..) |a, i| {
        for (expected, 0..) |b, j| {
            if (i != j) try std.testing.expect(a.id != b.id);
        }
    }

    var unused_model: Model = undefined;
    const chrome = cockpitTokens(&unused_model);
    try std.testing.expectEqual(terminal_font_id, chrome.typography.mono_font_id);
    // The grid's tokens are what name the companions; a carried bold flag
    // paints as bold only because these are set.
    try std.testing.expectEqual(terminal_bold_font_id, chrome.typography.mono_bold_font_id);
    try std.testing.expectEqual(terminal_italic_font_id, chrome.typography.mono_italic_font_id);
    try std.testing.expectEqual(terminal_bold_italic_font_id, chrome.typography.mono_bold_italic_font_id);
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
    _ = @import("tests/terminal_keyboard_tests.zig");
    _ = @import("tests/clipboard_tests.zig");
    _ = @import("tests/outbound_io_tests.zig");
    _ = @import("tests/terminal_lifecycle_tests.zig");
    _ = @import("tests/record_replay_tests.zig");
    _ = @import("tests/terminal_registry_tests.zig");
    _ = @import("tests/topology_persistence_tests.zig");
    _ = @import("tests/workspace_layout_tests.zig");
    _ = @import("tests/workspace_chrome_accessibility_tests.zig");
    _ = @import("tests/surface_routing_tests.zig");
    _ = @import("tests/pointer_selection_tests.zig");
    _ = @import("tests/mouse_protocol_tests.zig");
    _ = @import("tests/adversarial_isolation_tests.zig");
    _ = @import("tests/layout_tree_tests.zig");
    _ = @import("tests/config_tests.zig");
    _ = @import("tests/shell_identity_tests.zig");
    _ = @import("tests/config_wiring_tests.zig");
    _ = @import("tests/settings_theme_tests.zig");
    _ = @import("tests/tab_strip_tests.zig");
    _ = @import("tests/tab_identity_tests.zig");
    _ = @import("tests/ts_snapshot_tests.zig");
    _ = @import("tests/scrollback_search_tests.zig");
    _ = @import("tests/multi_window_tests.zig");
    _ = @import("tests/sdk_surface_tests.zig");
    _ = @import("tests/chrome_register_tests.zig");
}
