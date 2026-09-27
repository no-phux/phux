//! Test-only facade for native engine regression coverage.
//! It re-exports the native modules the unit and regression tests under
//! src/tests use; it defines no app entry point.

const std = @import("std");
const grid = @import("terminal/grid.zig");

const support = @import("cockpit/phux_support.zig");
const local = @import("providers/local/provider.zig");
const topology = @import("cockpit/topology.zig");
const layout = @import("cockpit/layout.zig");
const model_module = @import("cockpit/model.zig");
const session_state = @import("cockpit/session_state.zig");
const startup = @import("cockpit/startup.zig");
const projection = @import("cockpit/native/workspace_projection.zig");
const pointer = @import("cockpit/pointer_input.zig");
const scene = @import("cockpit/native/scene.zig");
const terminal_painter = @import("cockpit/native/terminal_painter.zig");
const ts_snapshot = @import("cockpit/native/ts_snapshot.zig");
const config_module = @import("config/config.zig");

pub const LocalResourceId = support.LocalResourceId;
pub const RemoteResourceId = support.RemoteResourceId;
pub const TerminalRef = support.TerminalRef;
pub const MouseButton = support.MouseButton;
pub const PhuxProvider = support.PhuxProvider;
pub const phux_enabled = support.phux_enabled;

pub const Pane = local.Pane;
pub const max_terminals = local.max_terminals;
pub const max_tabs = topology.max_tabs;
pub const max_panes_per_tab = layout.max_panes;
pub const clipboard_key = local.clipboard_key;
pub const outbound_buffer_bytes = local.outbound_buffer_bytes;
pub const initialTerminalRef = local.initialTerminalRef;
pub const ptyKey = local.ptyKey;

pub const TabPlacement = topology.TabPlacement;
pub const TopologySnapshot = topology.TopologySnapshot;
pub const primarySnapshotSelection = topology.primarySelection;
pub const PersistedTopologySnapshot = topology.PersistedTopologySnapshot;
pub const SnapshotCwd = topology.SnapshotCwd;
pub const max_snapshot_cwd_bytes = topology.max_snapshot_cwd_bytes;
pub const Tree = layout.Tree;
pub const Kind = layout.Kind;
pub const Orientation = layout.Orientation;
pub const LayoutPane = layout.Pane;
pub const topology_snapshot_version = topology.topology_snapshot_version;
pub const migrateTopologySnapshot = topology.migrateTopologySnapshot;

pub const Model = model_module.Model;
pub const Workspace = model_module.Workspace;
pub const max_windows = model_module.max_windows;
pub const reconcileRemoteRefs = model_module.reconcileRemoteRefs;
pub const initialModelWithPhux = model_module.initialModelWithPhux;
pub const initialModel = model_module.initialModel;
pub const initialModelWithIo = model_module.initialModelWithIo;
pub const restoreModel = model_module.restoreModel;
pub const deinitModel = model_module.deinitModel;

pub const state_file_name = session_state.file_name;
pub const release_state_file_name = session_state.release_file_name;
pub const max_state_bytes = session_state.max_state_bytes;
pub const serializeWorkspaceState = session_state.serialize;
pub const parseWorkspaceState = session_state.parse;

/// Geometry exports retained for native engine tests. Declarative chrome is
/// audited by `ts-chrome-parity` in `native_extension.zig`.
pub const header_height = projection.header_height;
pub const config_notice_bytes = projection.config_notice_bytes;
pub const configNoticeRevealed = projection.configNoticeRevealed;
pub const configNoticeLine = projection.configNoticeLine;
pub const chrome_command_envelope = projection.chrome_command_envelope;
pub const cockpitTokens = projection.cockpitTokens;
pub const terminalTokens = projection.terminalTokens;
pub const workspaceChrome = projection.workspaceChrome;
pub const resolvePanes = projection.resolvePanes;
pub const paneFrames = projection.paneFrames;
pub const tabTriggerHeight = projection.tabTriggerHeight;
pub const linkPreviewCommandReserve = terminal_painter.linkPreviewCommandReserve;

pub const canvas_label = scene.canvas_label;

pub const Config = config_module.Config;
pub const ConfigTabPlacement = config_module.TabPlacement;
pub const parseConfig = config_module.parse;
pub const loadConfigOrDefault = config_module.loadOrDefault;

pub const tabPlacementFromText = startup.tabPlacementFromText;
pub const encodeTsSnapshot = ts_snapshot.encode;
pub const TsTabRun = ts_snapshot.TabRun;
pub const ts_snapshot_max_bytes = ts_snapshot.max_bytes;
pub const resolvePhuxConfig = startup.resolvePhuxConfig;
pub const createPhuxProviderFromConfig = startup.createPhuxProviderFromConfig;
pub const configuredPhuxSocket = startup.configuredPhuxSocket;
pub const configuredPhuxSession = startup.configuredPhuxSession;
pub const resolveConfigPath = startup.resolveConfigPath;
pub const resolveDotfileConfigPath = startup.resolveDotfileConfigPath;
pub const resolveStatePath = startup.resolveStatePath;
pub const restoreWorkspace = startup.restoreWorkspace;

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
    _ = @import("cockpit/diagnostics.zig");
    _ = @import("cockpit/native/ts_protocol.zig");
    _ = @import("cockpit/native/ts_appearance.zig");
    _ = @import("cockpit/native/new_session.zig");
    _ = @import("cockpit/native/local_tools.zig");
    _ = @import("window_navigation_contract_tests.zig");
    _ = @import("empty_session_window_tests.zig");
    _ = @import("new_session_runtime_tests.zig");
    _ = @import("machine_runtime_tests.zig");
    _ = @import("cockpit/native/remote_status_tests.zig");
    _ = @import("cockpit/native/local_tool_launch.zig");
    _ = @import("cockpit/native/machine_browse.zig");
    _ = @import("cockpit/native/window_contexts.zig");
    _ = @import("tests/app_contract_tests.zig");
    _ = @import("tests/url_detection_tests.zig");
    _ = @import("tests/hyperlink_tests.zig");
    _ = @import("tests/grid_state_tests.zig");
    _ = @import("tests/grid_rendering_tests.zig");
    _ = @import("tests/cell_attribute_tests.zig");
    _ = @import("tests/minimum_contrast_tests.zig");
    _ = @import("tests/provider_identity_tests.zig");
    _ = @import("tests/credential_store_tests.zig");
    _ = @import("tests/terminal_registry_tests.zig");
    _ = @import("tests/topology_persistence_tests.zig");
    _ = @import("tests/workspace_layout_tests.zig");
    _ = @import("tests/adversarial_isolation_tests.zig");
    _ = @import("tests/paint_ceiling_tests.zig");
    _ = @import("terminal/grid.zig");
    _ = @import("cockpit/native/paint_budget.zig");
    _ = @import("tests/layout_tree_tests.zig");
    _ = @import("tests/config_tests.zig");
    _ = @import("tests/shell_identity_tests.zig");
    _ = @import("tests/config_wiring_tests.zig");
    _ = @import("tests/ghostty_config_tests.zig");
    _ = @import("tests/tab_identity_tests.zig");
    _ = @import("tests/ts_snapshot_tests.zig");
    _ = @import("tests/scrollback_search_tests.zig");
    _ = @import("tests/agent_session_rows_tests.zig");
    _ = @import("cockpit/native/remote_hosts.zig");
    _ = @import("tests/remote_host_tests.zig");
    _ = @import("tests/directory_picker_tests.zig");
    _ = @import("cockpit/native/path_picker.zig");
    _ = @import("tests/multi_coordinator_tests.zig");
    _ = @import("tests/side_by_side_tests.zig");
    _ = @import("tests/relaunch_layout_tests.zig");
}
