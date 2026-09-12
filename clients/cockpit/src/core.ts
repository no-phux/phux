import { Cmd, asciiBytes, utf8Bytes, windowDescriptor } from "@native-sdk/core";
import { type WindowDescriptor, type ScrollState } from "@native-sdk/core/events";
import { applyTextInputEvent, type TextEditState, type TextInputEvent } from "@native-sdk/core/text";
import { type TabCommandState, type TabCommandDecision, initialTabCommands, enqueueTabCommand, enqueueCatalogCommand, enqueueOperationCommand, receiveTabReceipt, unknownTabCommand } from "./tab-commands.ts";
import { type CommandResults, type ResultDecision, initialCommandResults, requestCommandResults, receiveCommandResult, failedCommandResults } from "./command-results.ts";
import { type Appearance, initialAppearance, appearanceRequest, appearanceResponse } from "./appearance.ts";
import { type ActionRow, commandRows, commandDefinition, contextualCommand, containsQuery } from "./commands.ts";
import { type KeybindingPage, type KeybindingRow, initialKeybindings, keybindingRequest, keybindingResponse } from "./keybindings.ts";
import { type Setting, settingsRows, settingRequest, resetSettingRequest, reloadSettingsRequest } from "./settings.ts";
import { windowTarget, windowCommand, windowReceipt } from "./window-navigation.ts";
import { newSessionRequest, newSessionReply } from "./new-session.ts";
import { localToolRequest, localToolReply } from "./local-tools.ts";
import { type MachineState, type MachineRow, initialMachines, requestMachines, machineRequest, machineStatusRequest, receiveMachines, filterMachines, moveMachine, capturedMachine } from "./machines.ts";
import {
  REMOTE_KIND_STATUS,
  REMOTE_KIND_CONNECT,
  REMOTE_KIND_LOCAL,
  REMOTE_KIND_DISCONNECT,
  REMOTE_PHASE_LOCAL,
  REMOTE_PHASE_CONNECTED,
  REMOTE_PHASE_FAILED,
  REMOTE_PHASE_REFUSED,
  remoteRequest,
  remoteReply,
  remoteStatusLine,
} from "./remote-hosts.ts";
import {
  SESSION_KIND_DESCRIBE,
  SESSION_KIND_RENAME,
  SESSION_KIND_STATUS,
  SESSION_KIND_NEW_TAB,
  SESSION_KIND_DISMISS,
  SESSION_KIND_DESCRIBE_ROW,
  SESSION_KIND_RENAME_ROW,
  sessionRowRequest,
  sessionRowTarget,
  SESSION_PHASE_READY,
  SESSION_PHASE_PENDING,
  SESSION_PHASE_RENAMED,
  sessionRequest,
  sessionReply,
} from "./session.ts";
import {
  DIR_KIND_OPEN,
  DIR_KIND_PAGE,
  DIR_KIND_DESCEND,
  DIR_KIND_PARENT,
  DIR_KIND_HERE,
  DIR_STATUS_PENDING,
  DIR_STATUS_LISTED,
  DIR_HERE,
  DIR_UP,
  NO_DIRECTORY_REQUEST,
  type DirectoryPage,
  directoryRequest,
  directoryPage,
  directoryRowLabel,
  directoryNotice,
  directoryTitle,
  DIR_OPEN_HERE_UNAVAILABLE_NOTICE,
} from "./directory.ts";
import {
  ENGINE_CHANNEL_KEY,
  type WireU64,
  type SnapshotTab,
  type SecondaryWindow,
  type SnapshotEmptySession,
  type WindowContext,
  type WindowContexts,
  invalidation,
  intent,
  sameU64,
  snapshot,
  navigationScopedRequest,
  navigationAgentsRequest,
  navigationIntent,
  type NavigationRow,
  type NavigationPage,
  navigationPage,
  navigationHostFilter,
  sameBytes,
  type SnapshotAgentRow,
} from "./protocol.ts";

/// One agent session drawn under the terminal tab it runs in. It is not a
/// tab: it carries no slot, takes no selection, and owns no pane. `id` is its
/// order under that tab, which is all a sibling-scoped `key` needs.
export interface AgentRow {
  readonly id: number;
  readonly provider: Uint8Array;
  readonly state: Uint8Array;
  readonly attention: boolean;
  readonly resource: Uint8Array;
  readonly parent: Uint8Array;
  readonly parentIndex: number;
}

/// One drawn row of the side rail: a terminal tab, or an agent session
/// indented under the tab above it. `agent` picks the shape. A terminal row
/// captures its tab target; an agent press uses `parentIndex` to focus the
/// exact parent split.
export interface RailRow {
  readonly id: number;
  readonly index: number;
  readonly label: Uint8Array;
  readonly state: Uint8Array;
  readonly mark: Uint8Array;
  readonly selected: boolean;
  readonly agent: boolean;
  readonly target: Uint8Array;
  readonly parentIndex: number;
  readonly attentionLabel: Uint8Array;
}

export interface Tab {
  readonly id: number;
  readonly index: number;
  readonly slot: number;
  readonly title: Uint8Array;
  readonly cwd: Uint8Array;
  readonly selected: boolean;
  readonly attention: boolean;
  readonly attentionLabel: Uint8Array;
  /// Live, never persisted: the rows come from the snapshot the engine just
  /// sent, so a session that closed is simply absent from the next one.
  readonly agents: readonly AgentRow[];
  readonly target: Uint8Array;
}

/// One catalog row: display bookkeeping and label beside the captured native
/// target. Only those opaque bytes authorize selection.
export interface SwitcherRow {
  readonly id: number;
  readonly index: number;
  readonly label: Uint8Array;
  readonly target: Uint8Array;
  readonly highlighted: boolean;
  readonly current: boolean;
  readonly detail: Uint8Array;
  readonly kind: number;
  readonly host: Uint8Array;
  readonly selectable: boolean;
  readonly disabled: boolean;
  /// A listed session (this Mac's or a peer's): its context menu offers Rename.
  readonly renamable: boolean;
  /// Identity-bound inspection data; empty on ordinary catalog rows.
  readonly resource: Uint8Array;
  readonly parent: Uint8Array;
  readonly nativeId: Uint8Array;
  readonly evidence: Uint8Array;
}

export interface ThemeRow {
  readonly index: number;
  readonly label: Uint8Array;
  readonly active: boolean;
  readonly highlighted: boolean;
}

export interface SettingsChoice {
  readonly index: number;
  readonly label: Uint8Array;
}

/// One secondary window slot, as the engine's snapshot reports it. `open`
/// is presence in the snapshot; a closed slot keeps empty lists so the
/// slot's markup always has something to bind. `slot` on a tab packs the
/// window and the index into one number for the markup's single-value
/// handlers (window * 32 + index; a workspace holds at most sixteen tabs).
export interface WindowState {
  readonly index: number;
  readonly open: boolean;
  readonly tabs: readonly Tab[];
  readonly visibleTabs: readonly Tab[];
  readonly tabWidth: number;
  readonly hasOverflow: boolean;
  readonly overflowLabel: Uint8Array;
  readonly selectedTab: number;
}

export type TabPlacement = "top" | "side";
export type ChannelState = "data" | "closed" | "rejected";

/// What one open window's header and Empty session chrome name. Bound per
/// window from kind 5 when present; otherwise from the kind 3 / kind 4
/// primary labels so older snapshots keep painting.
export interface WindowChromeContext {
  readonly title: Uint8Array;
  readonly detail: Uint8Array;
  readonly emptyName: Uint8Array;
  readonly emptyDetail: Uint8Array;
  readonly emptyPicked: boolean;
  readonly emptyOpening: boolean;
}

export interface Model {
  readonly tabs: readonly Tab[];
  /// The run the band has room for, always holding the selected tab. The
  /// toolkit cannot bound a run by itself, and the core cannot measure a
  /// band; the engine's snapshot carries the shipping projection's answer
  /// and the core slices to it, with a cue for the rest.
  readonly visibleTabs: readonly Tab[];
  readonly tabWidth: number;
  readonly hasOverflow: boolean;
  readonly overflowLabel: Uint8Array;
  readonly selectedTab: number;
  readonly tabPlacement: TabPlacement;
  /// The side rail, flattened: each visible tab followed by the agent rows
  /// running inside it. The compiled markup iterates model slices, not a
  /// field of a `for` item, so the nesting is expressed here as order and
  /// depth rather than as a loop inside a loop.
  readonly railRows: readonly RailRow[];
  readonly workspaceLabel: Uint8Array;
  /// Per-window session/machine header and Empty session labels.
  readonly mainContext: WindowChromeContext;
  readonly window1Context: WindowChromeContext;
  readonly window2Context: WindowChromeContext;
  readonly window3Context: WindowChromeContext;
  readonly window4Context: WindowChromeContext;
  readonly window1RailRows: readonly RailRow[];
  readonly window2RailRows: readonly RailRow[];
  readonly window3RailRows: readonly RailRow[];
  readonly window4RailRows: readonly RailRow[];
  /// Native projection slot for the platform window that owns modal chrome.
  /// Platform ids never cross the seam; the snapshot carries only 0..4.
  readonly activeWindow: number;
  readonly paletteOpen: boolean;
  readonly agentsMode: boolean;
  readonly inspectedResource: Uint8Array;
  readonly agentCountLabel: Uint8Array;
  readonly mainAgentsOpen: boolean;
  readonly window1AgentsOpen: boolean;
  readonly window2AgentsOpen: boolean;
  readonly window3AgentsOpen: boolean;
  readonly window4AgentsOpen: boolean;
  /// One navigator, including the action palette, shares the native input gate.
  /// 0 terminals, 1 sessions, 2 machines, 3 windows, 4 commands.
  readonly navigatorView: number;
  readonly navigatorTitle: Uint8Array;
  readonly actionRows: readonly ActionRow[];
  readonly bindings: KeybindingPage;
  readonly pendingSessionAction: DeferredAction | null;
  readonly pendingSettingsAction: DeferredAction | null;
  readonly retiredSessionToken: Uint8Array;
  readonly navigatorScroll: number;
  readonly navigatorViewport: number;
  readonly commandContextTarget: Uint8Array;
  readonly commandContextWindow: number;
  readonly machines: MachineState;
  readonly machineRows: readonly MachineRow[];
  readonly machinesOperation: number;
  readonly windowActionId: number;
  readonly windowActionPending: boolean;
  readonly mainPaletteOpen: boolean;
  readonly paletteQuery: Uint8Array;
  readonly paletteScope: number;
  readonly paletteHost: Uint8Array;
  readonly paletteHostLabel: Uint8Array;
  readonly navigationScopes: readonly SettingsChoice[];
  readonly coordinatorEndpoint: Uint8Array;
  readonly machineLabel: Uint8Array;
  readonly connectionDetail: Uint8Array;
  readonly paletteAnchor: number;
  readonly paletteFocus: number;
  readonly paletteRows: readonly SwitcherRow[];
  readonly paletteCursor: number;
  readonly paletteSelection: Uint8Array;
  readonly paletteRefreshing: boolean;
  readonly paletteFill: number;
  readonly paletteOffset: number;
  readonly paletteTotal: number;
  readonly palettePrevious: boolean;
  readonly paletteNext: boolean;
  readonly paletteLoading: boolean;
  readonly paletteNotice: Uint8Array;
  /// Connect to Host (remote-hosts.ts): an app-wide modal like the switcher,
  /// presented in whichever window invoked it (the snapshot's active window).
  /// `hostQuery` survives a failure so a retry is one keystroke;
  /// `hostAwaiting` holds the panel open until the engine reports the host
  /// connected or failed.
  readonly hostOpen: boolean;
  readonly toolPurpose: number;
  readonly toolToken: Uint8Array;
  readonly toolLaunchToken: Uint8Array;
  readonly toolLaunchPending: boolean;
  readonly toolOperationId: number;
  readonly toolOperationToken: Uint8Array;
  readonly toolStatusBusy: boolean;
  readonly toolTarget: Uint8Array;
  readonly toolQueued: boolean;
  readonly hostFriendlyName: Uint8Array;
  readonly friendlyAnchor: number;
  readonly friendlyFocus: number;
  readonly configEditorConfirm: boolean;
  readonly pendingToolOpen: boolean;
  readonly mainHostOpen: boolean;
  readonly window1HostOpen: boolean;
  readonly window2HostOpen: boolean;
  readonly window3HostOpen: boolean;
  readonly window4HostOpen: boolean;
  /// Go to Directory (directory.ts): an app-wide modal in the invoking window
  /// like Connect to Host. `dirRequest` is the listing the rows came from,
  /// as four opaque bytes; `dirStarting` accepts the next listing's new ID;
  /// `dirAwaiting` polls on each invalidation until that listing settles;
  /// `dirClosing` closes the picker once Open Here is accepted.
  readonly dirOpen: boolean;
  readonly mainDirOpen: boolean;
  readonly window1DirOpen: boolean;
  readonly window2DirOpen: boolean;
  readonly window3DirOpen: boolean;
  readonly window4DirOpen: boolean;
  readonly dirQuery: Uint8Array;
  readonly dirAnchor: number;
  readonly dirFocus: number;
  readonly dirRequest: Uint8Array;
  readonly dirStarting: boolean;
  readonly dirAwaiting: boolean;
  readonly dirClosing: boolean;
  readonly dirBusy: boolean;
  readonly dirRows: readonly DirRow[];
  readonly dirCursor: number;
  readonly dirOffset: number;
  readonly dirPrevious: boolean;
  readonly dirNext: boolean;
  readonly dirPath: Uint8Array;
  readonly dirNotice: Uint8Array;
  /// The picker's heading: names the satellite a listing comes from.
  readonly dirTitle: Uint8Array;
  /// Open Here can open a tab in this listing, on the coordinator it came
  /// through (the active one, or a peer showing a session).
  readonly dirOpenHere: boolean;
  /// Rename Session (session.ts): an app-wide modal in the invoking window.
  /// The engine names the session on screen and the coordinator that owns
  /// it; `renameAwaiting` asks for the outcome on each snapshot until the
  /// rename settles there.
  readonly renameOpen: boolean;
  readonly creatingSession: boolean;
  readonly newSessionToken: Uint8Array;
  readonly newSessionAwaiting: boolean;
  readonly mainRenameOpen: boolean;
  readonly window1RenameOpen: boolean;
  readonly window2RenameOpen: boolean;
  readonly window3RenameOpen: boolean;
  readonly window4RenameOpen: boolean;
  readonly renameQuery: Uint8Array;
  /// The captured target of the switcher row Rename was chosen on, or empty
  /// for the session on screen. The engine resolves it against the
  /// coordinator that listed the row, never another.
  readonly renameRow: Uint8Array;
  readonly renameAnchor: number;
  readonly renameFocus: number;
  readonly renameTitle: Uint8Array;
  readonly renameNotice: Uint8Array;
  readonly renameBusy: boolean;
  readonly renameAwaiting: boolean;
  /// The Empty session state (ADR-0105, empty_session.zig): the windows the
  /// snapshot says show it, as a mask, and per window whether it is drawn
  /// (it gives way to every modal). A keep-empty session with no windows is
  /// offered with New Tab, never shown as broken.
  readonly emptyWindows: number;
  readonly mainEmptyOpen: boolean;
  readonly window1EmptyOpen: boolean;
  readonly window2EmptyOpen: boolean;
  readonly window3EmptyOpen: boolean;
  readonly window4EmptyOpen: boolean;
  readonly emptyName: Uint8Array;
  readonly emptyDetail: Uint8Array;
  readonly emptyPicked: boolean;
  readonly emptyBusy: boolean;
  readonly emptyNotice: Uint8Array;
  readonly hostQuery: Uint8Array;
  readonly hostAnchor: number;
  readonly hostFocus: number;
  readonly hostNotice: Uint8Array;
  readonly hostBusy: boolean;
  readonly hostAwaiting: boolean;
  /// Last engine answer: phase 0 local .. 4 reconnecting, and the host name.
  readonly hostPhase: number;
  readonly hostName: Uint8Array;
  readonly remoteLine: Uint8Array;
  /// Snapshot connection byte last seen; a change asks for remote status.
  readonly lastConnection: number;
  readonly canReconnect: boolean;
  readonly connectionStatus: Uint8Array;
  readonly window1Status: Uint8Array;
  readonly window2Status: Uint8Array;
  readonly window3Status: Uint8Array;
  readonly window4Status: Uint8Array;
  readonly settingsOpen: boolean;
  readonly settingsQuery: Uint8Array;
  readonly settingsAnchor: number;
  readonly settingsFocus: number;
  readonly settingRows: readonly Setting[];
  readonly settingEditId: number;
  readonly settingEditValue: Uint8Array;
  readonly settingAnchor: number;
  readonly settingFocus: number;
  readonly settingsReloadStage: number;
  readonly settingsNotice: Uint8Array;
  readonly bindingRows: readonly KeybindingRow[];
  readonly noBindingRows: boolean;
  readonly noSettingRows: boolean;
  readonly bindingEditIndex: number;
  readonly bindingEditValue: Uint8Array;
  readonly bindingAnchor: number;
  readonly bindingFocus: number;
  readonly mainSettingsOpen: boolean;
  readonly themes: readonly ThemeRow[];
  readonly settingsCursor: number;
  readonly configExists: boolean;
  readonly configNotice: Uint8Array;
  readonly appearance: Appearance;
  readonly appearanceBusy: boolean;
  readonly settingsSection: number;
  readonly settingsSections: readonly SettingsChoice[];
  readonly cursorChoices: readonly SettingsChoice[];
  readonly placementChoices: readonly SettingsChoice[];
  readonly fontDecrease: number;
  readonly fontIncrease: number;
  readonly navigationAfterSettings: boolean;
  readonly surfaceAfterSettings: number;
  /// A rollback (Cancel, Escape, or leaving for the switcher) is in flight;
  /// if it fails, Settings still closes rather than trapping the keyboard.
  readonly appearanceClosing: boolean;
  // Each secondary slot flattened: a markup template takes scalars and
  // slices as arguments, not records, so the slot's markup binds these.
  readonly window1Open: boolean;
  readonly window1Tabs: readonly Tab[];
  readonly window1TabWidth: number;
  readonly window1HasOverflow: boolean;
  readonly window1OverflowLabel: Uint8Array;
  readonly window1PaletteOpen: boolean;
  readonly window1SettingsOpen: boolean;
  readonly window2Open: boolean;
  readonly window2Tabs: readonly Tab[];
  readonly window2TabWidth: number;
  readonly window2HasOverflow: boolean;
  readonly window2OverflowLabel: Uint8Array;
  readonly window2PaletteOpen: boolean;
  readonly window2SettingsOpen: boolean;
  readonly window3Open: boolean;
  readonly window3Tabs: readonly Tab[];
  readonly window3TabWidth: number;
  readonly window3HasOverflow: boolean;
  readonly window3OverflowLabel: Uint8Array;
  readonly window3PaletteOpen: boolean;
  readonly window3SettingsOpen: boolean;
  readonly window4Open: boolean;
  readonly window4Tabs: readonly Tab[];
  readonly window4TabWidth: number;
  readonly window4HasOverflow: boolean;
  readonly window4OverflowLabel: Uint8Array;
  readonly window4PaletteOpen: boolean;
  readonly window4SettingsOpen: boolean;
  readonly engineConnected: boolean;
  readonly engineSequence: WireU64;
  readonly engineRevision: WireU64;
  readonly tabCommands: TabCommandState;
  readonly commandResults: CommandResults;
  readonly commandNotice: Uint8Array;
  readonly status: Uint8Array;
}

export type Msg =
  | { readonly kind: "command_result_loaded"; readonly body: Uint8Array }
  | { readonly kind: "command_result_failed"; readonly error: Uint8Array }
  | { readonly kind: "select_target"; readonly target: Uint8Array }
  | { readonly kind: "tab_command_completed"; readonly body: Uint8Array }
  | { readonly kind: "tab_command_failed"; readonly error: Uint8Array }
  | { readonly kind: "select_tab"; readonly index: number }
  | { readonly kind: "select_active_tab"; readonly index: number }
  | { readonly kind: "select_slot"; readonly slot: number }
  | { readonly kind: "new_terminal" }
  | { readonly kind: "new_window" }
  | { readonly kind: "window_closed"; readonly window: number }
  | { readonly kind: "close_selected_tab" }
  | { readonly kind: "toggle_tab_placement" }
  | { readonly kind: "palette_open" }
  | { readonly kind: "agents_open" }
  | { readonly kind: "agent_parent"; readonly index: number }
  | { readonly kind: "navigator_open"; readonly view: number }
  | { readonly kind: "commands_open" }
  | { readonly kind: "commands_pick"; readonly index: number }
  | { readonly kind: "keybindings_loaded"; readonly body: Uint8Array }
  | { readonly kind: "new_session_cancelled"; readonly body: Uint8Array }
  | { readonly kind: "new_session_cancel_failed"; readonly error: Uint8Array }
  | { readonly kind: "context_refused" }
  | { readonly kind: "navigator_scrolled"; readonly scroll: ScrollState }
  | { readonly kind: "keybindings_failed"; readonly error: Uint8Array }
  | { readonly kind: "sessions_open" }
  | { readonly kind: "machines_open" }
  | { readonly kind: "windows_open" }
  | { readonly kind: "window_action_loaded"; readonly body: Uint8Array }
  | { readonly kind: "window_action_failed"; readonly error: Uint8Array }
  | { readonly kind: "machines_loaded"; readonly body: Uint8Array }
  | { readonly kind: "machines_failed"; readonly error: Uint8Array }
  | { readonly kind: "machine_pick"; readonly target: Uint8Array }
  | { readonly kind: "machine_disconnect"; readonly target: Uint8Array }
  | { readonly kind: "machine_forget"; readonly target: Uint8Array }
  | { readonly kind: "machine_forget_confirm" }
  | { readonly kind: "machine_forget_cancel" }
  | { readonly kind: "machines_more" }
  | { readonly kind: "palette_scope"; readonly scope: number }
  | { readonly kind: "palette_close" }
  | { readonly kind: "palette_edit"; readonly edit: TextInputEvent }
  | { readonly kind: "palette_move"; readonly delta: number }
  | { readonly kind: "palette_submit" }
  | { readonly kind: "palette_pick"; readonly target: Uint8Array }
  | { readonly kind: "palette_previous" }
  | { readonly kind: "palette_next" }
  | { readonly kind: "palette_retry" }
  | { readonly kind: "navigation_loaded"; readonly body: Uint8Array }
  | { readonly kind: "navigation_failed"; readonly error: Uint8Array }
  | { readonly kind: "reconnect" }
  | { readonly kind: "host_open" }
  | { readonly kind: "add_machine_open" }
  | { readonly kind: "config_edit" }
  | { readonly kind: "host_name_edit"; readonly edit: TextInputEvent }
  | { readonly kind: "local_tool_loaded"; readonly body: Uint8Array }
  | { readonly kind: "local_tool_failed"; readonly error: Uint8Array }
  | { readonly kind: "local_tool_launch_loaded"; readonly body: Uint8Array }
  | { readonly kind: "local_tool_launch_failed"; readonly error: Uint8Array }
  | { readonly kind: "local_tool_status_loaded"; readonly body: Uint8Array }
  | { readonly kind: "local_tool_status_failed"; readonly error: Uint8Array }
  | { readonly kind: "local_tool_acknowledged"; readonly body: Uint8Array }
  | { readonly kind: "local_tool_ack_failed"; readonly error: Uint8Array }
  | { readonly kind: "tool_status_tick"; readonly at: number }
  | { readonly kind: "tool_submit" }
  | { readonly kind: "tool_recheck" }
  | { readonly kind: "settings_save_edit" }
  | { readonly kind: "settings_discard_edit" }
  | { readonly kind: "settings_cancel_edit" }
  | { readonly kind: "host_close" }
  | { readonly kind: "host_edit"; readonly edit: TextInputEvent }
  | { readonly kind: "host_submit" }
  | { readonly kind: "host_local" }
  | { readonly kind: "host_disconnect" }
  | { readonly kind: "host_disconnect_all" }
  | { readonly kind: "dir_open" }
  | { readonly kind: "dir_close" }
  | { readonly kind: "dir_edit"; readonly edit: TextInputEvent }
  | { readonly kind: "dir_submit" }
  | { readonly kind: "dir_pick"; readonly index: number }
  | { readonly kind: "dir_here" }
  | { readonly kind: "dir_previous" }
  | { readonly kind: "dir_next" }
  | { readonly kind: "directory_loaded"; readonly body: Uint8Array }
  | { readonly kind: "directory_failed"; readonly error: Uint8Array }
  | { readonly kind: "rename_open" }
  | { readonly kind: "new_session_open" }
  | { readonly kind: "new_session_loaded"; readonly body: Uint8Array }
  | { readonly kind: "new_session_failed"; readonly error: Uint8Array }
  | { readonly kind: "rename_row"; readonly target: Uint8Array }
  | { readonly kind: "rename_close" }
  | { readonly kind: "rename_edit"; readonly edit: TextInputEvent }
  | { readonly kind: "rename_submit" }
  | { readonly kind: "session_loaded"; readonly body: Uint8Array }
  | { readonly kind: "session_failed"; readonly error: Uint8Array }
  | { readonly kind: "empty_new_tab" }
  | { readonly kind: "empty_dismiss" }
  | { readonly kind: "empty_loaded"; readonly body: Uint8Array }
  | { readonly kind: "empty_failed"; readonly error: Uint8Array }
  | { readonly kind: "remote_loaded"; readonly body: Uint8Array }
  | { readonly kind: "remote_failed"; readonly error: Uint8Array }
  | { readonly kind: "settings_open" }
  | { readonly kind: "settings_query"; readonly edit: TextInputEvent }
  | { readonly kind: "settings_select"; readonly id: number }
  | { readonly kind: "settings_value"; readonly edit: TextInputEvent }
  | { readonly kind: "settings_apply" }
  | { readonly kind: "settings_reset"; readonly id: number }
  | { readonly kind: "settings_reload" }
  | { readonly kind: "settings_edit_configuration" }
  | { readonly kind: "binding_select"; readonly index: number }
  | { readonly kind: "binding_edit"; readonly edit: TextInputEvent }
  | { readonly kind: "binding_apply" }
  | { readonly kind: "binding_reset"; readonly index: number }
  | { readonly kind: "settings_close" }
  | { readonly kind: "settings_move"; readonly delta: number }
  | { readonly kind: "settings_pick"; readonly index: number }
  | { readonly kind: "settings_commit" }
  | { readonly kind: "settings_reveal" }
  | { readonly kind: "settings_section"; readonly section: number }
  | { readonly kind: "settings_font"; readonly direction: number }
  | { readonly kind: "settings_cursor"; readonly index: number }
  | { readonly kind: "settings_placement"; readonly index: number }
  | { readonly kind: "appearance_loaded"; readonly body: Uint8Array }
  | { readonly kind: "appearance_failed"; readonly error: Uint8Array }
  | { readonly kind: "native_command"; readonly command: number }
  // Posted by the native engine for every shell event it consumed: no bytes
  // ride along, the core only learns that the grids beneath it moved.
  | { readonly kind: "engine_wake" }
  | { readonly kind: "snapshot_loaded"; readonly body: Uint8Array }
  | { readonly kind: "snapshot_failed"; readonly error: Uint8Array }
  | {
      readonly kind: "engine_event";
      readonly key: number;
      readonly state: ChannelState;
      readonly bytes: Uint8Array;
      readonly droppedPending: number;
      readonly droppedTotal: number;
    };

export const viewUnbound = [
  "pendingSessionAction", "pendingSettingsAction", "retiredSessionToken", "new_session_cancelled", "new_session_cancel_failed", "context_refused",
  "select_tab",
  "settingsAnchor",
  "settingsFocus",
  "settingAnchor",
  "settingFocus",
  "bindingAnchor",
  "bindingFocus",
  "settingsReloadStage",
  "surfaceAfterSettings",
  "navigatorViewport",
  "paletteFill",
  "paletteRefreshing",
  "paletteSelection",
  "toolToken",
  "toolLaunchToken", "toolLaunchPending", "local_tool_launch_loaded", "local_tool_launch_failed",
  "toolOperationId", "toolOperationToken", "toolStatusBusy", "tool_status_tick",
  "local_tool_status_loaded", "local_tool_status_failed", "local_tool_acknowledged", "local_tool_ack_failed",
  "friendlyAnchor",
  "friendlyFocus",
  "pendingToolOpen",
  "local_tool_loaded",
  "local_tool_failed",
  "newSessionToken",
  "newSessionAwaiting",
  "new_session_loaded",
  "new_session_failed",
  "bindings",
  "commandContextTarget",
  "commandContextWindow",
  "keybindings_loaded",
  "keybindings_failed",
  "machinesOperation",
  "windowActionId",
  "windowActionPending",
  "window_action_loaded",
  "window_action_failed",
  "machines_loaded",
  "machines_failed",
  "navigator_open",
  "select_slot",
  "tabCommands",
  "commandResults",
  "command_result_loaded",
  "command_result_failed",
  "tab_command_completed",
  "tab_command_failed",
  "selectedTab",
  "activeWindow",
  "paletteOpen",
  "settingsOpen",
  "select_active_tab",
  "window_closed",
  "paletteAnchor",
  "paletteFocus",
  "paletteCursor",
  "paletteOffset",
  "paletteTotal",
  "paletteLoading",
  "navigation_loaded",
  "navigation_failed",
  "settingsCursor",
  "palette_move",
  "settings_move",
  "appearance_loaded",
  "appearance_failed",
  "navigationAfterSettings",
  "appearanceClosing",
  "engineConnected",
  "engineSequence",
  "engineRevision",
  "status",
  "engine_event",
  "engine_wake",
  "snapshot_loaded",
  "snapshot_failed",
  "native_command",
  "hostOpen",
  "hostAnchor",
  "hostFocus",
  "hostBusy",
  "hostAwaiting",
  "hostPhase",
  "hostName",
  "remoteLine",
  "lastConnection",
  "remote_loaded",
  "remote_failed",
  "dirOpen",
  "dirAnchor",
  "dirFocus",
  "dirRequest",
  "dirStarting",
  "dirAwaiting",
  "dirClosing",
  "dirBusy",
  "dirCursor",
  "dirOffset",
  "dir_open",
  "directory_loaded",
  "directory_failed",
  "renameOpen",
  "renameAnchor",
  "renameFocus",
  "renameBusy",
  "renameAwaiting",
  "renameRow",
  "rename_open",
  "session_loaded",
  "session_failed",
  "emptyWindows",
  "empty_loaded",
  "empty_failed",
] as const;

const ZERO_U64: WireU64 = { hi: 0, lo: 0 };

function overflowLabel(hidden: number): Uint8Array {
  // "+N" for N in 1..255 without string building or division, neither of
  // which the compiled subset offers on an integer path: peel hundreds and
  // tens by subtraction.
  let rest = hidden;
  let hundreds = 0;
  while (rest >= 100) {
    rest -= 100;
    hundreds += 1;
  }
  let tens = 0;
  while (rest >= 10) {
    rest -= 10;
    tens += 1;
  }
  const digits = hundreds > 0 ? 3 : tens > 0 ? 2 : 1;
  const out = new Uint8Array(1 + digits);
  out[0] = 43;
  let at = 1;
  if (hundreds > 0) {
    out[at] = 48 + hundreds;
    at += 1;
  }
  if (hundreds > 0 || tens > 0) {
    out[at] = 48 + tens;
    at += 1;
  }
  out[at] = 48 + rest;
  return out;
}

function decimalPlace(index: number): number {
  if (index === 0) return 10000;
  if (index === 1) return 1000;
  if (index === 2) return 100;
  if (index === 3) return 10;
  return 1;
}

function decimalBytes(value: number): Uint8Array {
  const out = new Uint8Array(5);
  let rest = value >= 0 && value <= 65535 ? Math.trunc(value) : 0;
  let first = 4;
  for (let i = 0; i < 5; i += 1) {
    const place = decimalPlace(i);
    let digit = 0;
    // Division would taint the caller's navigation indices as AOT floats.
    while (rest >= place) {
      rest -= place;
      digit += 1;
    }
    out[i] = digit + 48;
    if (digit > 0 && first === 4) first = i;
  }
  return out.subarray(first);
}

function joinBytes(head: Uint8Array, mid: Uint8Array, tail: Uint8Array): Uint8Array {
  const out = new Uint8Array(head.length + mid.length + tail.length);
  let at = 0;
  for (let i = 0; i < head.length; i += 1) {
    out[at] = head[i];
    at += 1;
  }
  for (let i = 0; i < mid.length; i += 1) {
    out[at] = mid[i];
    at += 1;
  }
  for (let i = 0; i < tail.length; i += 1) {
    out[at] = tail[i];
    at += 1;
  }
  return out;
}

const NO_BYTES = new Uint8Array(0);
const TOOL_QUEUED_NOTICE = asciiBytes("Opening a dedicated local terminal...");
const TOOL_STATUS_NOTICE = asciiBytes("Local tool receipt unavailable. Checking This Mac again; the tool will not be resubmitted.");

/// U+25CF BLACK CIRCLE. Deliberately the same quiet dot the tab strip uses
/// for a terminal asking for attention: an agent waiting on an answer is the
/// same claim on a person, not a louder one.
const ATTENTION_MARK = utf8Bytes("\u25cf");
// Typed empties: a bare `[]` in a record literal is inferred as number[] and
// the record then fails to be a Model at the union boundary, at runtime.
const NO_ROWS: readonly SwitcherRow[] = [];
const NO_TABS: readonly Tab[] = [];
const NO_THEMES: readonly ThemeRow[] = [];
const NO_ACTION_ROWS: readonly ActionRow[] = [];
const NO_MACHINE_ROWS: readonly MachineRow[] = [];
const NO_SETTING_ROWS: readonly Setting[] = [];
const NO_BINDING_ROWS: readonly KeybindingRow[] = [];

function paletteState(model: Model): TextEditState {
  return {
    text: model.paletteQuery,
    selection: { anchor: model.paletteAnchor, focus: model.paletteFocus },
    composition: null,
  };
}

function requestNavigation(model: Model, offset: number): Model {
  const at = offset >= 0 && offset <= 65535 ? Math.trunc(offset) : 0;
  if (model.agentsMode) {
    return { ...model, inspectedResource: NO_BYTES, paletteOffset: at, paletteRows: NO_ROWS, paletteCursor: 0, paletteLoading: true,
      palettePrevious: false, paletteNext: false, paletteNotice: asciiBytes("Loading workspace...") };
  }
  const append = at > model.paletteOffset;
  return { ...model, inspectedResource: NO_BYTES, paletteOffset: at, paletteRows: append ? model.paletteRows : NO_ROWS,
    paletteSelection: append ? model.paletteSelection : NO_BYTES, paletteCursor: append ? model.paletteCursor : 0,
    paletteLoading: true, paletteRefreshing: append ? model.paletteRefreshing : false,
    palettePrevious: false, paletteNext: false, paletteNotice: asciiBytes("Loading work...") };
}

function closePalette(model: Model): Model {
  return scopeOverlays({ ...model, agentsMode: false, inspectedResource: NO_BYTES, paletteOpen: false, paletteQuery: NO_BYTES, paletteRows: NO_ROWS, paletteCursor: 0 });
}

function refreshNavigation(model: Model): Model {
  if (model.agentsMode) {
    const refreshed = requestNavigation(model, model.paletteOffset);
    return { ...refreshed, inspectedResource: model.inspectedResource, paletteCursor: model.paletteCursor };
  }
  const wanted = Math.max(24, model.paletteRows.length);
  return { ...model, paletteOffset: 0, paletteLoading: true, paletteRefreshing: true,
    paletteFill: wanted >= 24 && wanted <= 65535 ? Math.trunc(wanted) : 24 };
}

/// A painted pick carries its own captured bytes; keyboard submission uses the
/// highlighted current row. A current row the engine marked unselectable never
/// submits. A held pick absent from the page stays native-validated.
function navigationTarget(model: Model, msg: Msg): Uint8Array {
  if (!model.paletteOpen) return NO_BYTES;
  if (msg.kind === "palette_pick") return currentRowRefuses(model, msg.target) ? NO_BYTES : msg.target;
  if (!model.engineConnected || model.paletteRefreshing || model.paletteCursor >= model.paletteRows.length) return NO_BYTES;
  const row = model.paletteRows[model.paletteCursor];
  return row.selectable ? row.target : NO_BYTES;
}

function currentRowRefuses(model: Model, target: Uint8Array): boolean {
  for (const row of model.paletteRows) {
    if (sameBytes(row.target, target)) return !row.selectable;
  }
  return false;
}

function navigationRequestFor(model: Model): Uint8Array {
  return model.agentsMode
    ? navigationAgentsRequest(model.engineRevision, model.paletteOffset)
    : scopedNavigationRequest(model);
}

function validAgentParent(model: Model, index: number): boolean {
  if (!model.engineConnected || index === 65535) return false;
  for (const row of model.railRows) {
    if (row.agent && row.parentIndex === index) return true;
  }
  return false;
}

function failedAgentNavigation(model: Model): Model {
  if (model.agentsMode) return { ...model, paletteRows: NO_ROWS, paletteLoading: false,
    palettePrevious: false, paletteNext: false, paletteNotice: asciiBytes("Agent catalog unavailable. Refresh to inspect again.") };
  if (model.paletteOffset > 0) return requestNavigation(model, 0);
  return { ...model, paletteLoading: false, paletteNotice: asciiBytes("Workspace unavailable. Retry to refresh.") };
}

function navigationPageMatches(model: Model, page: NavigationPage): boolean {
  if (!sameU64(page.revision, model.engineRevision)) return false;
  if (page.agents !== model.agentsMode) return false;
  if (page.offset !== model.paletteOffset) return false;
  if (!sameBytes(page.query, model.paletteQuery)) return false;
  if (model.agentsMode) return true;
  return page.scope === model.paletteScope && sameBytes(page.host, model.paletteHost);
}

function inspectionIdentity(rows: readonly NavigationRow[]): Uint8Array {
  return rows.length === 0 ? NO_BYTES : rows[0].resource;
}

function inspectionRetargeted(model: Model, rows: readonly NavigationRow[]): boolean {
  if (!model.agentsMode || model.inspectedResource.length === 0) return false;
  return !sameBytes(model.inspectedResource, inspectionIdentity(rows));
}

function loadedNavigation(model: Model, body: Uint8Array): Model {
  if (!model.paletteOpen || !model.engineConnected) return model;
  const page = navigationPage(body);
  if (page === null) return { ...model, paletteLoading: false, paletteNotice: asciiBytes("Workspace unavailable. Retry to refresh.") };
  if (!navigationPageMatches(model, page)) return model;
  if (inspectionRetargeted(model, page.rows)) return { ...model, paletteRows: NO_ROWS, paletteLoading: false,
    palettePrevious: false, paletteNext: false, paletteNotice: asciiBytes("Agent changed or closed. Refresh to inspect the current catalog.") };
  const total = page.total >= 0 && page.total <= 65535 ? Math.trunc(page.total) : 0;
  if (model.agentsMode) {
    const loaded: Model = { ...model, inspectedResource: inspectionIdentity(page.rows), paletteRows: switcherRows(page.rows), paletteTotal: total, paletteLoading: false,
      palettePrevious: page.offset > 0, paletteNext: page.offset + page.rows.length < total,
      paletteNotice: navigationNotice(true, model.paletteScope, total, page.offset) };
    if (page.rows.length === 0) return loaded;
    return highlightNavigation(loaded, Math.min(model.paletteCursor, page.rows.length - 1));
  }
  const rows = navigationRowsForPage(model, page);
  const loaded: Model = { ...model, paletteRows: rows, paletteTotal: total, paletteLoading: false,
    palettePrevious: false, paletteNext: page.offset + page.rows.length < total,
    paletteNotice: navigationNotice(false, model.paletteScope, total, page.offset) };
  return reconcileNavigationSelection(loaded);
}

function navigationRowsForPage(model: Model, page: NavigationPage): readonly SwitcherRow[] {
  const incoming = switcherRows(page.rows);
  if (page.offset === 0) return incoming;
  const rows: SwitcherRow[] = [];
  for (const row of model.paletteRows) rows.push(row);
  for (const row of incoming) rows.push(row);
  return rows;
}

function reconcileNavigationSelection(model: Model): Model {
  if (model.paletteSelection.length === 0 && !model.paletteRefreshing) return highlightNavigation(model, 0);
  for (let i = 0; i < model.paletteRows.length; i += 1) {
    if (sameBytes(model.paletteRows[i].target, model.paletteSelection)) return highlightNavigation(model, i);
  }
  const rows: SwitcherRow[] = [];
  for (const row of model.paletteRows) rows.push({ ...row, highlighted: false });
  return { ...model, paletteRows: rows, paletteCursor: 65535 };
}

function receiveNavigation(model: Model, body: Uint8Array): Model {
  if (model.navigatorView === 2 || model.navigatorView === 4) return model;
  const loaded = loadedNavigation(model, body);
  if (loaded === model || loaded.paletteLoading) return loaded;
  // Agent inspection pages one resource at a time; never auto-fill like the
  // everyday navigator's append-until-viewport path.
  if (!loaded.agentsMode && loaded.paletteNext && loaded.paletteRows.length < loaded.paletteFill) {
    return requestNavigation(loaded, loaded.paletteOffset + navigationPageSize(loaded));
  }
  return { ...loaded, paletteRefreshing: false };
}

function currentNavigationPage(model: Model, page: NavigationPage): boolean {
  return sameU64(page.revision, model.engineRevision) && page.offset === model.paletteOffset &&
    sameBytes(page.query, model.paletteQuery) && page.scope === model.paletteScope && sameBytes(page.host, model.paletteHost);
}

function switcherRows(rows: readonly NavigationRow[]): readonly SwitcherRow[] {
  const result: SwitcherRow[] = [];
  for (const row of rows) {
    const index = row.index >= 0 && row.index <= 65535 ? Math.trunc(row.index) : 0;
    const kind = row.kind >= 0 && row.kind <= 5 ? Math.trunc(row.kind) : 0;
    result.push({ id: index, index, kind, label: row.label, detail: row.detail, host: row.host,
      highlighted: row.highlighted, current: row.current && row.kind !== 5, selectable: row.selectable, disabled: !row.selectable, target: row.target,
      renamable: kind === 2 && row.selectable && sessionRowTarget(row.target),
      resource: row.resource, parent: row.parent, nativeId: row.nativeId, evidence: row.evidence });
  }
  return result;
}

function navigationNotice(agents: boolean, scope: number, total: number, offset: number): Uint8Array {
  if (agents) {
    if (total === 0) return asciiBytes("No agent resources in the attached catalog");
    return joinBytes(asciiBytes("Agent "), decimalBytes(offset + 1), joinBytes(asciiBytes(" of "), decimalBytes(total), asciiBytes(" / Last reported state")));
  }
  if (scope === 4) return total === 0 ? asciiBytes("No matching windows") : asciiBytes("Choose a window or tab to bring existing work forward");
  if (scope === 2) return total === 0 ? asciiBytes("No known terminal hosts on this connection") : asciiBytes("Hosts represented by known terminals");
  if (scope === 1) return total === 0 ? asciiBytes("No matching Phux sessions") : asciiBytes("Select a session to open its workspace");
  return total === 0 ? asciiBytes("No matching terminals or sessions") : asciiBytes("Enter to open  /  Escape to return");
}

function scopedNavigationRequest(model: Model): Uint8Array {
  return navigationScopedRequest(model.engineRevision, model.paletteOffset, model.paletteQuery, model.paletteScope, model.paletteHost);
}

function chooseNavigationScope(model: Model, scope: number): Model {
  if (!model.paletteOpen || !(scope >= 0 && scope <= 2)) return model;
  return requestNavigation({ ...model, paletteScope: Math.trunc(scope), paletteHost: NO_BYTES, paletteHostLabel: NO_BYTES,
    paletteQuery: NO_BYTES, paletteAnchor: 0, paletteFocus: 0 }, 0);
}

/// A known-host token only narrows the view to its exact raw host; it is never
/// enqueued as catalog authority, even when held across a replacement page.
function hostNavigation(model: Model, host: Uint8Array): Model {
  const exact = host.slice();
  const label = exact.length === 0 ? asciiBytes("Coordinator") : exact;
  return requestNavigation({ ...model, paletteScope: 3, paletteHost: exact, paletteHostLabel: label,
    paletteQuery: NO_BYTES, paletteAnchor: 0, paletteFocus: 0 }, 0);
}

function moveNavigation(model: Model, delta: number): Model {
  if (!model.paletteOpen || !model.engineConnected || model.paletteRefreshing) return model;
  if (model.paletteCursor === 65535) return highlightNavigation(model, 0);
  const next = model.paletteCursor + (delta >= 0 ? 1 : -1);
  if (next < 0 && model.palettePrevious) return previousNavigation(model);
  if (next >= model.paletteRows.length && model.paletteNext) return requestNavigation(model, model.paletteOffset + navigationPageSize(model));
  return highlightNavigation(model, next);
}

function previousNavigation(model: Model): Model {
  const previous = requestNavigation(model, model.paletteOffset - 4);
  return { ...previous, paletteCursor: 3 };
}

function highlightNavigation(model: Model, next: number): Model {
  if (next < 0 || next >= model.paletteRows.length) return model;
  const cursor = next >= 0 && next <= 65535 ? Math.trunc(next) : 0;
  const rows: SwitcherRow[] = [];
  for (let i = 0; i < model.paletteRows.length; i += 1) {
    const row = model.paletteRows[i];
    rows.push({ ...row, highlighted: i === cursor });
  }
  return revealNavigator({ ...model, paletteCursor: cursor, paletteRows: rows, paletteSelection: rows[cursor].target }, cursor * 40, 40);
}

function editNavigation(model: Model, edit: TextInputEvent): Model {
  if (!model.paletteOpen) return model;
  const next = applyTextInputEvent(paletteState(model), edit, 64);
  if (next === null) return model;
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 64 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 64 ? Math.trunc(next.selection.focus) : 0;
  return requestNavigation({ ...model, paletteQuery: next.text, paletteAnchor: anchor, paletteFocus: focus }, 0);
}

function browseNavigation(model: Model, msg: Msg): Model {
  if (!model.paletteOpen) return model;
  switch (msg.kind) {
    case "palette_previous": return model.palettePrevious ? requestNavigation(model, model.paletteOffset - 4) : model;
    case "palette_next": return model.paletteNext ? requestNavigation(model, model.paletteOffset + navigationPageSize(model)) : model;
    case "palette_retry": return requestNavigation(model, 0);
    default: return model;
  }
}

function navigationPageSize(model: Model): number {
  if (model.agentsMode) return 1;
  return model.paletteScope === 4 ? 16 : 4;
}

function changeNavigation(model: Model, msg: Msg): Model {
  switch (msg.kind) {
    case "palette_open":
    case "agents_open":
      if (model.paletteOpen && model.agentsMode === (msg.kind === "agents_open")) return model;
      return requestNavigation(scopeOverlays({ ...model, agentsMode: msg.kind === "agents_open", paletteOpen: true, settingsOpen: false, hostOpen: false, hostAwaiting: false, paletteQuery: NO_BYTES, paletteAnchor: 0, paletteFocus: 0,
        navigatorView: 0, navigatorTitle: asciiBytes(msg.kind === "agents_open" ? "Inspect agents" : "Go to Terminal"),
        paletteScope: 0, paletteHost: NO_BYTES, paletteHostLabel: NO_BYTES }), 0);
    case "palette_scope": return chooseNavigationScope(model, msg.scope);
    case "palette_edit": return editNavigation(model, msg.edit);
    case "palette_move": return moveNavigation(model, msg.delta);
    default: return browseNavigation(model, msg);
  }
}

/// One drawn row of Go to Directory: a listing index (or a synthetic role,
/// directory.ts DIR_HERE / DIR_UP) and what it says. A pick echoes only the
/// index; the engine composes the path from the listing it names.
export interface DirRow {
  readonly id: number;
  readonly index: number;
  readonly label: Uint8Array;
  readonly highlighted: boolean;
}

const NO_DIR_ROWS: readonly DirRow[] = [];

/// What one Go to Directory message leaves: the model, the request to send
/// (empty for none), and whether the modal slot changed hands. `update`
/// builds the effects inline, as the compiled subset requires.
interface DirectoryDecision {
  readonly model: Model;
  readonly request: Uint8Array;
  readonly committed: boolean;
}

function directoryDecision(model: Model, request: Uint8Array, committed: boolean): DirectoryDecision {
  return { model, request, committed };
}

function unchangedDirectory(model: Model): DirectoryDecision {
  return directoryDecision(model, NO_BYTES, false);
}

function directoryState(model: Model): TextEditState {
  return {
    text: model.dirQuery,
    selection: { anchor: model.dirAnchor, focus: model.dirFocus },
    composition: null,
  };
}

function pageDirectory(model: Model, offset: number): DirectoryDecision {
  const at = offset >= 0 && offset <= 65535 ? Math.trunc(offset) : 0;
  const next = { ...model, dirOffset: at };
  return directoryDecision(next, directoryRequest(DIR_KIND_PAGE, next.dirRequest, at, 0, next.dirQuery), false);
}

/// Go to Directory takes the one modal slot: the switcher, Connect to Host
/// and Settings give way to it as they do to each other.
function openDirectory(model: Model): DirectoryDecision {
  if (model.dirOpen) return unchangedDirectory(model);
  const base = model.paletteOpen ? closePalette(model) : model;
  const next = scopeOverlays({ ...base, dirOpen: true, hostOpen: false, hostAwaiting: false, settingsOpen: false,
    dirQuery: NO_BYTES, dirAnchor: 0, dirFocus: 0, dirRequest: NO_DIRECTORY_REQUEST, dirStarting: true,
    dirAwaiting: false, dirClosing: false, dirBusy: true, dirRows: NO_DIR_ROWS, dirCursor: 0, dirOffset: 0,
    dirPrevious: false, dirNext: false, dirPath: NO_BYTES, dirNotice: asciiBytes("Listing..."),
    dirTitle: asciiBytes("Go to Directory"), dirOpenHere: true });
  return directoryDecision(next, directoryRequest(DIR_KIND_OPEN, NO_DIRECTORY_REQUEST, 0, 0, NO_BYTES), true);
}

function closeDirectory(model: Model): DirectoryDecision {
  return directoryDecision(scopeOverlays({ ...model, dirOpen: false, dirQuery: NO_BYTES, dirRows: NO_DIR_ROWS,
    dirBusy: false, dirAwaiting: false, dirStarting: false, dirClosing: false }), NO_BYTES, true);
}

/// Another modal opening while the picker is up takes its slot.
function displaceDirectory(model: Model, msg: Msg): Model {
  if (!model.dirOpen) return model;
  if (msg.kind !== "palette_open" && msg.kind !== "host_open" && msg.kind !== "settings_open" && msg.kind !== "rename_open") return model;
  return closeDirectory(model).model;
}

/// What one Rename Session message leaves, as for Go to Directory: the
/// model, the request to send (empty for none), and whether the modal slot
/// changed hands.
interface RenameDecision {
  readonly model: Model;
  readonly request: Uint8Array;
  readonly committed: boolean;
}

function renameDecision(model: Model, request: Uint8Array, committed: boolean): RenameDecision {
  return { model, request, committed };
}

function renameState(model: Model): TextEditState {
  return {
    text: model.renameQuery,
    selection: { anchor: model.renameAnchor, focus: model.renameFocus },
    composition: null,
  };
}

/// Rename Session takes the one modal slot. It asks the engine which session
/// is on screen before offering a name; Settings, which has a preview to roll
/// back, keeps the slot until it closes.
function openRename(model: Model): RenameDecision {
  if (model.renameOpen || model.settingsOpen) return renameDecision(model, NO_BYTES, false);
  const base = model.paletteOpen ? closePalette(model) : model;
  const next = scopeOverlays({ ...base, renameOpen: true, hostOpen: false, hostAwaiting: false, renameRow: NO_BYTES,
    renameQuery: NO_BYTES, renameAnchor: 0, renameFocus: 0, renameBusy: true, renameAwaiting: false,
    renameTitle: asciiBytes("Rename Session"), renameNotice: asciiBytes("Looking for the session on screen...") });
  return renameDecision(next, sessionRequest(SESSION_KIND_DESCRIBE, NO_BYTES), true);
}

/// Rename from a switcher row's context menu: the same panel, naming that
/// row's session by the row's own captured target. It never falls back to
/// the session on screen; a row that is not a session row opens nothing.
function openRenameRow(model: Model, target: Uint8Array): RenameDecision {
  if (model.renameOpen || model.settingsOpen || !sessionRowTarget(target)) return renameDecision(model, NO_BYTES, false);
  const row = target.slice();
  const base = model.paletteOpen ? closePalette(model) : model;
  const next = scopeOverlays({ ...base, renameOpen: true, hostOpen: false, hostAwaiting: false, renameRow: row,
    renameQuery: NO_BYTES, renameAnchor: 0, renameFocus: 0, renameBusy: true, renameAwaiting: false,
    renameTitle: asciiBytes("Rename Session"), renameNotice: asciiBytes("Looking for the session...") });
  return renameDecision(next, sessionRowRequest(SESSION_KIND_DESCRIBE_ROW, NO_BYTES, row), true);
}

function closeRename(model: Model): RenameDecision {
  return renameDecision(scopeOverlays({ ...model, renameOpen: false, renameBusy: false, renameAwaiting: false }), NO_BYTES, true);
}

/// Another modal opening while Rename Session is up takes its slot.
function displaceRename(model: Model, msg: Msg): Model {
  if (!model.renameOpen) return model;
  if (msg.kind !== "palette_open" && msg.kind !== "host_open" && msg.kind !== "settings_open") return model;
  return closeRename(model).model;
}

function editRename(model: Model, edit: TextInputEvent): Model {
  const next = applyTextInputEvent(renameState(model), edit, model.creatingSession ? 240 : 255);
  if (next === null) return model;
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 255 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 255 ? Math.trunc(next.selection.focus) : 0;
  return { ...model, renameQuery: next.text, renameAnchor: anchor, renameFocus: focus };
}

function submitRename(model: Model): RenameDecision {
  if (model.renameBusy) return renameDecision(model, NO_BYTES, false);
  if (model.renameQuery.length === 0) {
    return renameDecision({ ...model, renameNotice: asciiBytes("Enter a new name for this session.") }, NO_BYTES, false);
  }
  const request = model.renameRow.length > 0
    ? sessionRowRequest(SESSION_KIND_RENAME_ROW, model.renameQuery, model.renameRow)
    : sessionRequest(SESSION_KIND_RENAME, model.renameQuery);
  return renameDecision({ ...model, renameBusy: true, renameNotice: asciiBytes("Renaming...") }, request, false);
}

function renameHeading(name: Uint8Array, host: Uint8Array): Uint8Array {
  if (name.length === 0) return asciiBytes("Rename Session");
  return joinBytes(joinBytes(asciiBytes("Rename "), name, asciiBytes(" on ")), host, NO_BYTES);
}

/// Apply one engine answer. The first names the session and seeds the field
/// with its name; a refusal keeps the panel and says why; a rename the
/// coordinator applied closes it.
function receiveSession(model: Model, body: Uint8Array): RenameDecision {
  if (!model.renameOpen) return renameDecision(model, NO_BYTES, false);
  const reply = sessionReply(body);
  if (reply === null) {
    return renameDecision({ ...model, renameBusy: false, renameAwaiting: false,
      renameNotice: asciiBytes("Rename unavailable. Try again.") }, NO_BYTES, false);
  }
  if (reply.phase === SESSION_PHASE_RENAMED) return closeRename(model);
  if (reply.phase === SESSION_PHASE_PENDING) {
    return renameDecision({ ...model, renameBusy: true, renameAwaiting: true, renameNotice: asciiBytes("Renaming...") }, NO_BYTES, false);
  }
  if (reply.phase === SESSION_PHASE_READY) {
    const seeded = model.renameQuery.length === 0 ? reply.name : model.renameQuery;
    const length = seeded.length;
    const end = length >= 0 && length <= 255 ? Math.trunc(length) : 0;
    return renameDecision({ ...model, renameBusy: false, renameAwaiting: false, renameQuery: seeded,
      renameAnchor: 0, renameFocus: end, renameTitle: renameHeading(reply.name, reply.host),
      renameNotice: asciiBytes("Enter a new name for this session.") }, NO_BYTES, false);
  }
  return renameDecision({ ...model, renameBusy: false, renameAwaiting: false,
    renameNotice: reply.reason.length > 0 ? reply.reason : asciiBytes("Nothing was renamed.") }, NO_BYTES, false);
}

function renameTransition(model: Model, msg: Msg): RenameDecision | null {
  if (model.creatingSession) return null;
  if (msg.kind === "rename_open") return openRename(model);
  if (msg.kind === "rename_row") return openRenameRow(model, msg.target);
  const reply = renameReplyTransition(model, msg);
  if (reply !== null) return reply;
  if (!model.renameOpen) return null;
  switch (msg.kind) {
    case "rename_close":
    case "palette_close": return closeRename(model);
    case "rename_edit": return renameDecision(editRename(model, msg.edit), NO_BYTES, false);
    case "rename_submit": return submitRename(model);
    // The arrows mean nothing to a single field.
    case "palette_move": return renameDecision(model, NO_BYTES, false);
    default: return null;
  }
}

function renameReplyTransition(model: Model, msg: Msg): RenameDecision | null {
  if (msg.kind === "session_loaded") return receiveSession(model, msg.body);
  if (msg.kind !== "session_failed") return null;
  if (!model.renameOpen) return renameDecision(model, NO_BYTES, false);
  return renameDecision({ ...model, renameBusy: false, renameAwaiting: false,
    renameNotice: asciiBytes("Rename unavailable. Try again.") }, NO_BYTES, false);
}

/// The Empty session state gives way to every modal.
function emptyShown(model: Model, bit: number): boolean {
  if (model.paletteOpen || model.hostOpen || model.dirOpen || model.renameOpen || model.settingsOpen) return false;
  return (model.emptyWindows & bit) !== 0;
}

/// The engine's answer to New Tab or Dismiss. A refusal says why and lets
/// New Tab be pressed again; an accepted New Tab waits for the tab to land,
/// which the next snapshots show.
function receiveEmpty(model: Model, body: Uint8Array): Model {
  const reply = sessionReply(body);
  if (reply === null) return { ...model, emptyBusy: false, emptyNotice: asciiBytes("Could not open a tab there. Try again.") };
  if (reply.phase === SESSION_PHASE_PENDING) {
    return { ...model, emptyBusy: true, emptyNotice: joinBytes(asciiBytes("Opening a new tab in "), reply.name, asciiBytes("...")) };
  }
  if (reply.phase === SESSION_PHASE_READY) return { ...model, emptyBusy: false, emptyNotice: NO_BYTES };
  return { ...model, emptyBusy: false, emptyNotice: reply.reason.length > 0 ? reply.reason : asciiBytes("No tab was opened.") };
}

/// The connection under an open picker moved, so its rows named a listing on
/// the old connection. Withdraw them; once connected again, list afresh.
function relistDirectory(model: Model, connected: boolean): Model {
  return { ...model, dirRows: NO_DIR_ROWS, dirPrevious: false, dirNext: false, dirAwaiting: false, dirClosing: false,
    dirStarting: connected, dirBusy: connected, dirRequest: NO_DIRECTORY_REQUEST, dirQuery: NO_BYTES, dirAnchor: 0,
    dirFocus: 0, dirOffset: 0, dirCursor: 0,
    dirNotice: connected ? asciiBytes("Reconnected. Listing again...") : asciiBytes("Waiting for the connection...") };
}

function directoryRows(page: DirectoryPage): readonly DirRow[] {
  const rows: DirRow[] = [];
  for (const row of page.rows) {
    const index = row.index >= 0 && row.index <= 65535 ? Math.trunc(row.index) : 0;
    rows.push({ id: index, index, label: directoryRowLabel(row), highlighted: false });
  }
  return rows.length === 0 ? NO_DIR_ROWS : rows;
}

function highlightDirectory(model: Model, next: number): Model {
  if (next < 0 || next >= model.dirRows.length) return model;
  const cursor = next >= 0 && next <= 3 ? Math.trunc(next) : 0;
  const rows: DirRow[] = [];
  for (let i = 0; i < model.dirRows.length; i += 1) {
    const row = model.dirRows[i];
    rows.push({ ...row, highlighted: i === cursor });
  }
  return { ...model, dirCursor: cursor, dirRows: rows };
}

function showDirectory(model: Model, page: DirectoryPage): Model {
  const total = page.total >= 0 && page.total <= 65535 ? Math.trunc(page.total) : 0;
  const rows = directoryRows(page);
  const shown: Model = { ...model, dirRequest: page.request, dirStarting: false, dirBusy: false,
    dirAwaiting: page.status === DIR_STATUS_PENDING, dirRows: rows, dirCursor: 0,
    dirPath: page.path.length > 0 ? page.path : model.dirPath, dirTitle: directoryTitle(page), dirOpenHere: page.openHere,
    dirPrevious: page.offset > 0, dirNext: page.offset + page.rows.length < total, dirNotice: directoryNotice(page) };
  return highlightDirectory(shown, Math.min(model.dirCursor, rows.length - 1));
}

/// A reply counts only while the picker is open, for the listing it shows
/// (or the one it has just started), and for the page it last asked for. A
/// reply that arrives after Escape, or for a directory already left, is
/// therefore dropped.
function receiveDirectory(model: Model, body: Uint8Array): DirectoryDecision {
  if (!model.dirOpen) return unchangedDirectory(model);
  const page = directoryPage(body);
  if (page === null) {
    return unchangedDirectory({ ...model, dirBusy: false, dirClosing: false,
      dirNotice: asciiBytes("Directory listing unavailable. Try again.") });
  }
  if (!model.dirStarting && !sameBytes(page.request, model.dirRequest)) return unchangedDirectory(model);
  if (model.dirClosing) return closeDirectory(model);
  if (page.offset !== model.dirOffset || !sameBytes(page.query, model.dirQuery)) {
    return unchangedDirectory({ ...model, dirRequest: page.request, dirStarting: false });
  }
  return unchangedDirectory(showDirectory(model, page));
}

/// Open Here refused keeps the picker for another try. A refused descend or
/// parent named a listing the engine has moved past, so show the current one.
function failedDirectory(model: Model): DirectoryDecision {
  if (!model.dirOpen) return unchangedDirectory(model);
  if (model.dirClosing) {
    return unchangedDirectory({ ...model, dirBusy: false, dirClosing: false,
      dirNotice: asciiBytes("Could not open a new tab there. Try again.") });
  }
  if (!model.dirStarting) {
    return unchangedDirectory({ ...model, dirBusy: false, dirNotice: asciiBytes("Directory listing unavailable. Try again.") });
  }
  return directoryDecision({ ...model, dirBusy: false, dirNotice: asciiBytes("That listing changed. Showing the current one.") },
    directoryRequest(DIR_KIND_PAGE, NO_DIRECTORY_REQUEST, 0, 0, NO_BYTES), false);
}

function moveDirectory(model: Model, delta: number): DirectoryDecision {
  if (model.dirBusy || model.dirStarting) return unchangedDirectory(model);
  const next = model.dirCursor + (delta >= 0 ? 1 : -1);
  if (next < 0 && model.dirPrevious) {
    const previous = pageDirectory(model, model.dirOffset - 4);
    return directoryDecision({ ...previous.model, dirCursor: 3 }, previous.request, false);
  }
  if (next >= model.dirRows.length && model.dirNext) {
    const following = pageDirectory(model, model.dirOffset + 4);
    return directoryDecision({ ...following.model, dirCursor: 0 }, following.request, false);
  }
  return unchangedDirectory(highlightDirectory(model, next));
}

function editDirectory(model: Model, edit: TextInputEvent): DirectoryDecision {
  const next = applyTextInputEvent(directoryState(model), edit, 64);
  if (next === null) return unchangedDirectory(model);
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 64 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 64 ? Math.trunc(next.selection.focus) : 0;
  return pageDirectory({ ...model, dirQuery: next.text, dirAnchor: anchor, dirFocus: focus, dirCursor: 0 }, 0);
}

/// Descend or go up: a new listing under a new request ID, which the next
/// reply names and the core adopts.
function startDirectoryListing(model: Model, kind: number, index: number): DirectoryDecision {
  return directoryDecision({ ...model, dirStarting: true, dirBusy: true, dirAwaiting: false, dirQuery: NO_BYTES,
    dirAnchor: 0, dirFocus: 0, dirOffset: 0, dirCursor: 0, dirRows: NO_DIR_ROWS, dirPrevious: false, dirNext: false,
    dirNotice: asciiBytes("Listing...") }, directoryRequest(kind, model.dirRequest, 0, index, NO_BYTES), false);
}

function activateDirectory(model: Model, index: number): DirectoryDecision {
  if (model.dirBusy || model.dirStarting) return unchangedDirectory(model);
  const row = index >= 0 && index <= 65535 ? Math.trunc(index) : 0;
  if (row === DIR_UP) return startDirectoryListing(model, DIR_KIND_PARENT, 0);
  // A listing whose coordinator cannot take a tab now: say so, send nothing.
  if (row === DIR_HERE && !model.dirOpenHere) return unchangedDirectory({ ...model, dirNotice: asciiBytes(DIR_OPEN_HERE_UNAVAILABLE_NOTICE) });
  if (row === DIR_HERE) {
    return directoryDecision({ ...model, dirBusy: true, dirClosing: true, dirNotice: asciiBytes("Opening a new tab...") },
      directoryRequest(DIR_KIND_HERE, model.dirRequest, model.dirOffset, DIR_HERE, model.dirQuery), false);
  }
  return startDirectoryListing(model, DIR_KIND_DESCEND, row);
}

function submitDirectory(model: Model): DirectoryDecision {
  if (model.dirCursor < 0 || model.dirCursor >= model.dirRows.length) return unchangedDirectory(model);
  return activateDirectory(model, model.dirRows[model.dirCursor].index);
}

function browseDirectory(model: Model, forward: boolean): DirectoryDecision {
  if (model.dirBusy) return unchangedDirectory(model);
  if (forward) return model.dirNext ? pageDirectory(model, model.dirOffset + 4) : unchangedDirectory(model);
  return model.dirPrevious ? pageDirectory(model, model.dirOffset - 4) : unchangedDirectory(model);
}

/// Every message the open picker answers. Escape (palette_close) and the
/// arrows (palette_move) reach it through the shared overlay keys.
function openDirectoryTransition(model: Model, msg: Msg): DirectoryDecision | null {
  switch (msg.kind) {
    case "dir_close":
    case "palette_close": return closeDirectory(model);
    case "palette_move": return moveDirectory(model, msg.delta);
    case "dir_edit": return editDirectory(model, msg.edit);
    case "dir_submit": return submitDirectory(model);
    case "dir_pick": return activateDirectory(model, msg.index);
    case "dir_here": return activateDirectory(model, DIR_HERE);
    case "dir_previous": return browseDirectory(model, false);
    case "dir_next": return browseDirectory(model, true);
    default: return null;
  }
}

function directoryTransition(model: Model, msg: Msg): DirectoryDecision | null {
  if (msg.kind === "dir_open") return openDirectory(model);
  if (msg.kind === "directory_loaded") return receiveDirectory(model, msg.body);
  if (msg.kind === "directory_failed") return failedDirectory(model);
  if (!model.dirOpen) return null;
  return openDirectoryTransition(model, msg);
}

function hostState(model: Model): TextEditState {
  return {
    text: model.hostQuery,
    selection: { anchor: model.hostAnchor, focus: model.hostFocus },
    composition: null,
  };
}

function editHost(model: Model, edit: TextInputEvent): Model {
  if (!model.hostOpen) return model;
  const next = applyTextInputEvent(hostState(model), edit, 255);
  if (next === null) return model;
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 255 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 255 ? Math.trunc(next.selection.focus) : 0;
  return { ...model, hostQuery: next.text, hostAnchor: anchor, hostFocus: focus };
}

/// The switcher and the host panel are one modal slot: opening Connect to
/// Host from the switcher replaces it. The previous host stays in the field.
function openHost(model: Model): Model {
  const base = model.paletteOpen ? closePalette(model) : model;
  const length = base.hostQuery.length;
  const end = length >= 0 && length <= 255 ? Math.trunc(length) : 0;
  return scopeOverlays({ ...base, hostOpen: true, settingsOpen: false, hostAnchor: end, hostFocus: end,
    hostNotice: base.remoteLine.length > 0 ? base.remoteLine : asciiBytes("A host registered with phux host add or phux host enroll") });
}

/// Apply one engine answer. A failure keeps the panel and the typed host;
/// a connection the panel was waiting for closes it.
function receiveRemote(model: Model, body: Uint8Array): Model {
  const reply = remoteReply(body);
  if (reply === null) {
    return { ...model, hostBusy: false, hostAwaiting: false, hostNotice: asciiBytes("Connection status unavailable. Try again.") };
  }
  // Refused: nothing changed. Say why in the panel; the connection status
  // stays what it was.
  if (reply.phase === REMOTE_PHASE_REFUSED) {
    return { ...model, hostBusy: false, hostAwaiting: false, hostNotice: reply.reason.length > 0 ? reply.reason : asciiBytes("Nothing changed.") };
  }
  const line = remoteStatusLine(reply);
  const settled = reply.phase === REMOTE_PHASE_CONNECTED || reply.phase === REMOTE_PHASE_FAILED || reply.phase === REMOTE_PHASE_LOCAL;
  const done = model.hostAwaiting && (reply.phase === REMOTE_PHASE_CONNECTED || reply.phase === REMOTE_PHASE_LOCAL);
  return scopeOverlays({
    ...model,
    hostBusy: false,
    hostAwaiting: model.hostAwaiting && !settled,
    hostOpen: model.hostOpen && !done,
    hostPhase: reply.phase >= 0 && reply.phase <= 4 ? Math.trunc(reply.phase) : 0,
    hostName: reply.host,
    remoteLine: line,
    hostNotice: line.length > 0 ? line : asciiBytes("Using this Mac's Phux coordinator"),
    connectionStatus: line.length > 0 ? line : model.connectionStatus,
  });
}

/// While a remote host is selected the status bar names it; a failure shows
/// its reason instead of the generic offline label.
function remoteConnectionStatus(model: Model, local: Uint8Array): Uint8Array {
  if (model.hostPhase === REMOTE_PHASE_LOCAL || model.hostName.length === 0) return local;
  if (model.hostPhase === REMOTE_PHASE_FAILED) return model.remoteLine;
  return joinBytes(model.hostName, asciiBytes(" / "), local);
}

function connectionLabel(state: number): Uint8Array {
  if (state === 0) return asciiBytes("Local terminals");
  if (state === 1) return asciiBytes("Phux connecting...");
  if (state === 2) return asciiBytes("Phux connected");
  if (state === 4) return asciiBytes("Phux connected / Shared workspace unavailable");
  return asciiBytes("Phux offline");
}

function terminalStateLabel(state: number): Uint8Array {
  if (state === 1) return asciiBytes("Loading terminal");
  if (state === 2) return asciiBytes("Recovering terminal: waiting for snapshot");
  if (state === 3) return asciiBytes("Terminal frozen: waiting for recovery");
  if (state === 4) return asciiBytes("Terminal unavailable");
  if (state === 5) return asciiBytes("Terminal ended");
  if (state === 6) return asciiBytes("Loading earlier history");
  if (state === 7) return asciiBytes("Earlier history available");
  return NO_BYTES;
}

function windowStatus(connection: number, terminal: number, refused: boolean): Uint8Array {
  const global = refused ? joinBytes(connectionLabel(connection), asciiBytes(" / Action refused"), NO_BYTES) : connectionLabel(connection);
  if (terminal === 0) return global;
  return joinBytes(global, asciiBytes(" / "), terminalStateLabel(terminal));
}

const MIDDLE_DOT = utf8Bytes(" \u00b7 ");
const NO_WINDOW_CONTEXT: WindowChromeContext = {
  title: NO_BYTES, detail: NO_BYTES, emptyName: NO_BYTES, emptyDetail: NO_BYTES,
  emptyPicked: false, emptyOpening: false,
};

function findWindowContext(records: readonly WindowContext[], window: number): WindowContext | null {
  for (const record of records) {
    if (record.window === window) return record;
  }
  return null;
}

/// State word for the header detail line: unavailable and connection words
/// outrank Empty session's opening/empty markers.
function windowContextStateWord(record: WindowContext): Uint8Array {
  if (record.unavailable) return asciiBytes("Unavailable");
  if (record.connection === 4) return asciiBytes("Workspace unavailable");
  if (record.connection === 1) return asciiBytes("Connecting...");
  if (record.connection === 3) return asciiBytes("Offline");
  if (record.opening) return asciiBytes("Opening...");
  if (record.empty) return asciiBytes("Empty session");
  return NO_BYTES;
}

function windowContextDetail(host: Uint8Array, record: WindowContext): Uint8Array {
  const word = windowContextStateWord(record);
  return word.length === 0 ? host : joinBytes(host, MIDDLE_DOT, word);
}

function unknownWindowContext(): WindowChromeContext {
  return {
    title: asciiBytes("Session unknown"),
    detail: asciiBytes("Window context unavailable"),
    emptyName: NO_BYTES,
    emptyDetail: NO_BYTES,
    emptyPicked: false,
    emptyOpening: false,
  };
}

function chromeFromWindowContext(record: WindowContext): WindowChromeContext {
  const host = record.host.length > 0 ? record.host : asciiBytes("Machine not yet known");
  return {
    title: record.session.length > 0 ? record.session : asciiBytes("Sessions"),
    detail: windowContextDetail(host, record),
    emptyName: record.empty ? record.session : NO_BYTES,
    emptyDetail: record.empty ? joinBytes(asciiBytes("Empty session on "), host, NO_BYTES) : NO_BYTES,
    emptyPicked: record.empty && record.picked,
    emptyOpening: record.empty && record.opening,
  };
}

function legacyWindowContext(
  open: boolean,
  session: Uint8Array,
  host: Uint8Array,
  empty: SnapshotEmptySession,
  bit: number,
): WindowChromeContext {
  if (!open) return NO_WINDOW_CONTEXT;
  const shown = (empty.windows & bit) !== 0;
  return {
    title: session.length > 0 ? session : asciiBytes("Sessions"),
    detail: host.length > 0 ? host : asciiBytes("Machine not yet known"),
    emptyName: shown ? empty.name : NO_BYTES,
    emptyDetail: shown ? joinBytes(asciiBytes("Empty session on "), empty.host, NO_BYTES) : NO_BYTES,
    emptyPicked: shown && empty.picked,
    emptyOpening: shown && empty.opening,
  };
}

function windowChromeContext(
  open: boolean,
  window: number,
  bit: number,
  contexts: WindowContexts,
  session: Uint8Array,
  host: Uint8Array,
  empty: SnapshotEmptySession,
): WindowChromeContext {
  if (!open) return NO_WINDOW_CONTEXT;
  if (!contexts.present) return legacyWindowContext(open, session, host, empty, bit);
  const record = findWindowContext(contexts.records, window);
  return record === null ? unknownWindowContext() : chromeFromWindowContext(record);
}

function emptyMaskFromContexts(contexts: WindowContexts, fallback: number): number {
  if (!contexts.present) return fallback;
  let mask = 0;
  for (const record of contexts.records) {
    if (!record.empty) continue;
    if (!(record.window >= 0 && record.window < 5)) continue;
    mask |= 1 << Math.trunc(record.window);
  }
  return mask >= 0 && mask <= 31 ? Math.trunc(mask) : 0;
}

function windowConnectionStatus(
  open: boolean,
  window: number,
  contexts: WindowContexts,
  connection: number,
  terminal: number,
  refused: boolean,
): Uint8Array {
  if (!contexts.present) return windowStatus(connection, terminal, refused);
  if (!open) return windowStatus(connection, terminal, refused);
  const record = findWindowContext(contexts.records, window);
  if (record === null) return asciiBytes("Connection status unavailable");
  return windowStatus(record.connection, terminal, refused);
}

function engineUnavailable(model: Model, status: Uint8Array): Model {
  return { ...withdrawAgentRows(model), engineConnected: false, status, canReconnect: false,
    connectionStatus: asciiBytes("Connection status unavailable"), paletteRows: NO_ROWS,
    window1Status: asciiBytes("Connection status unavailable"),
    window2Status: asciiBytes("Connection status unavailable"),
    window3Status: asciiBytes("Connection status unavailable"),
    window4Status: asciiBytes("Connection status unavailable"),
    paletteNotice: asciiBytes("Agent state unavailable. Refresh when the engine returns."),
    paletteLoading: false, palettePrevious: false, paletteNext: false };
}

function withdrawAgentRows(model: Model): Model {
  return { ...model, railRows: railRows(stampSlots(model.visibleTabs, 0, NO_AGENTS, 2)),
    tabs: stampSlots(model.tabs, 0, NO_AGENTS, 2), visibleTabs: stampSlots(model.visibleTabs, 0, NO_AGENTS, 2),
    window1Tabs: stampSlots(model.window1Tabs, 1, NO_AGENTS, 2), window2Tabs: stampSlots(model.window2Tabs, 2, NO_AGENTS, 2),
    window3Tabs: stampSlots(model.window3Tabs, 3, NO_AGENTS, 2), window4Tabs: stampSlots(model.window4Tabs, 4, NO_AGENTS, 2) };
}

/// Only the active native window presents the global core-owned modal. The
/// booleans are flattened because `.native` template arguments bind fields,
/// not comparisons; `paletteOpen`/`settingsOpen` remain the keyboard gate.
function scopeOverlays(model: Model): Model {
  const active = model.activeWindow >= 0 && model.activeWindow <= 4 ? Math.trunc(model.activeWindow) : 0;
  const scoped = {
    ...model,
    creatingSession: model.renameOpen && model.creatingSession,
    newSessionAwaiting: model.renameOpen && model.newSessionAwaiting,
    mainEmptyOpen: emptyShown(model, 1),
    window1EmptyOpen: emptyShown(model, 2),
    window2EmptyOpen: emptyShown(model, 4),
    window3EmptyOpen: emptyShown(model, 8),
    window4EmptyOpen: emptyShown(model, 16),
  };
  const navigation = scopePaletteOverlays(scoped, active);
  const settings = scopeSettingsOverlays(navigation, active);
  const host = scopeHostOverlays(settings, active);
  return scopeDirectoryOverlays(scopeRenameOverlays(host, active), active);
}

function scopeRenameOverlays(model: Model, active: number): Model {
  return {
    ...model,
    mainRenameOpen: model.renameOpen && active === 0,
    window1RenameOpen: model.renameOpen && active === 1,
    window2RenameOpen: model.renameOpen && active === 2,
    window3RenameOpen: model.renameOpen && active === 3,
    window4RenameOpen: model.renameOpen && active === 4,
  };
}

function scopeDirectoryOverlays(model: Model, active: number): Model {
  return {
    ...model,
    mainDirOpen: model.dirOpen && active === 0,
    window1DirOpen: model.dirOpen && active === 1,
    window2DirOpen: model.dirOpen && active === 2,
    window3DirOpen: model.dirOpen && active === 3,
    window4DirOpen: model.dirOpen && active === 4,
  };
}

function scopePaletteOverlays(model: Model, active: number): Model {
  const palette = model.paletteOpen && !model.agentsMode;
  const agents = model.paletteOpen && model.agentsMode;
  return {
    ...model,
    mainPaletteOpen: palette && active === 0,
    window1PaletteOpen: palette && active === 1,
    window2PaletteOpen: palette && active === 2,
    window3PaletteOpen: palette && active === 3,
    window4PaletteOpen: palette && active === 4,
    mainAgentsOpen: agents && active === 0,
    window1AgentsOpen: agents && active === 1,
    window2AgentsOpen: agents && active === 2,
    window3AgentsOpen: agents && active === 3,
    window4AgentsOpen: agents && active === 4,
  };
}

function scopeSettingsOverlays(model: Model, active: number): Model {
  return {
    ...model,
    mainSettingsOpen: model.settingsOpen && active === 0,
    window1SettingsOpen: model.settingsOpen && active === 1,
    window2SettingsOpen: model.settingsOpen && active === 2,
    window3SettingsOpen: model.settingsOpen && active === 3,
    window4SettingsOpen: model.settingsOpen && active === 4,
  };
}

function scopeHostOverlays(model: Model, active: number): Model {
  return {
    ...model,
    mainHostOpen: model.hostOpen && active === 0,
    window1HostOpen: model.hostOpen && active === 1,
    window2HostOpen: model.hostOpen && active === 2,
    window3HostOpen: model.hostOpen && active === 3,
    window4HostOpen: model.hostOpen && active === 4,
  };
}

const IN_EFFECT = asciiBytes("  (in effect)");

function themeRows(themes: readonly { readonly index: number; readonly name: Uint8Array }[], active: number, cursor: number): readonly ThemeRow[] {
  return themes.map((theme) => {
    const index = theme.index >= 0 && theme.index <= 32 ? Math.trunc(theme.index) : 0;
    return {
      index,
      label: index === active ? joinBytes(theme.name, IN_EFFECT, NO_BYTES) : theme.name,
      active: index === active,
      highlighted: index === cursor,
    };
  });
}

/// Re-highlight the catalog at `cursor`. A loop rather than a map with a
/// captured cursor: the compiled subset loses an integer proof across the
/// capture, and the cursor also lands in an integer slot.
function highlightThemes(themes: readonly ThemeRow[], cursor: number): readonly ThemeRow[] {
  const out: ThemeRow[] = [];
  for (let i = 0; i < themes.length; i += 1) {
    const t = themes[i];
    out.push({ index: t.index, label: t.label, active: t.active, highlighted: t.index === cursor });
  }
  return out;
}

/// The Configuration line the shipping settings panel shows, from the
/// engine's probe. Same sentences, same conditions (view.zig settingsPanel).
function configNotice(enabled: boolean, probed: boolean, exists: boolean, writable: boolean, refused: boolean, path: Uint8Array): Uint8Array {
  if (!enabled) return asciiBytes("No config location could be resolved; changes apply to this run only.");
  if (!probed) return joinBytes(asciiBytes("Active configuration file: "), path, NO_BYTES);
  if (refused) return joinBytes(path, asciiBytes(" refused the write; changes apply to this run only."), NO_BYTES);
  if (!writable) return joinBytes(path, asciiBytes(" is read-only; changes apply to this run only."), NO_BYTES);
  if (!exists) return joinBytes(path, asciiBytes(" will be created when you save."), NO_BYTES);
  return joinBytes(asciiBytes("Active configuration file: "), path, NO_BYTES);
}

/// The display word for each `agent_sessions.State`, in wire order. The
/// engine sends the ordinal and the core says the word: the vocabulary is
/// closed, so shipping the text per row would be bytes for nothing.
const AGENT_STATE_WORDS: readonly Uint8Array[] = [
  asciiBytes("unknown"),
  asciiBytes("working"),
  asciiBytes("blocked"),
  asciiBytes("done"),
  asciiBytes("gone"),
];

const NO_AGENT_ROWS: readonly AgentRow[] = [];
const NO_AGENTS: readonly SnapshotAgentRow[] = [];
const NO_RAIL_ROWS: readonly RailRow[] = [];

/// The agent rows this window's tab owns, in the order the snapshot listed
/// them. A row whose state ordinal is outside the closed vocabulary is
/// dropped rather than shown as a word the engine never said.
function agentRowsFor(agents: readonly SnapshotAgentRow[], window: number, tab: number, connection: number): readonly AgentRow[] {
  const out: AgentRow[] = [];
  let ordinal = 0;
  for (let i = 0; i < agents.length; i += 1) {
    const row = agents[i];
    if (row.window !== window || row.tab !== tab) continue;
    const projected = projectAgentRow(row, ordinal, connection);
    if (projected === null) continue;
    out.push(projected);
    ordinal += 1;
  }
  return out.length === 0 ? NO_AGENT_ROWS : out;
}

function projectAgentRow(row: SnapshotAgentRow, ordinal: number, connection: number): AgentRow | null {
  const state = row.state;
  if (!(state >= 0 && state < AGENT_STATE_WORDS.length)) return null;
  if (!(ordinal >= 0 && ordinal <= 255)) return null;
  const parentIndex = row.parentIndex;
  return {
    id: Math.trunc(ordinal), provider: row.provider,
    state: reportedAgentState(AGENT_STATE_WORDS[Math.trunc(state)], connection),
    attention: row.attention && connection === 2,
    resource: row.resource, parent: row.parent,
    parentIndex: parentIndex >= 0 && parentIndex < 65535 ? Math.trunc(parentIndex) : 65535,
  };
}

function reportedAgentState(state: Uint8Array, connection: number): Uint8Array {
  if (connection === 2) return state;
  return joinBytes(connection === 3 ? asciiBytes("offline / ") : asciiBytes("stale / "), state, NO_BYTES);
}

function attentionLabel(title: Uint8Array, attention: boolean): Uint8Array {
  return attention ? joinBytes(asciiBytes("Needs attention: "), title, NO_BYTES) : NO_BYTES;
}

function stampSlots(tabs: readonly SnapshotTab[], window: number, agents: readonly SnapshotAgentRow[], connection: number): readonly Tab[] {
  const out: Tab[] = [];
  const w = window >= 0 && window <= 4 ? Math.trunc(window) : 0;
  for (let i = 0; i < tabs.length; i += 1) {
    const t = tabs[i];
    const rawIndex = t.index;
    const rawId = t.id;
    if (!(rawIndex >= 0 && rawIndex <= 31) || !(rawId >= 1 && rawId <= 4294967295)) continue;
    const index = Math.trunc(rawIndex);
    const id = Math.trunc(rawId);
    out.push({ id, index, slot: w * 32 + index, title: t.title, cwd: t.cwd, selected: t.selected, attention: t.attention, attentionLabel: attentionLabel(t.title, t.attention), agents: agentRowsFor(agents, w, index, connection), target: t.target });
  }
  return out;
}

/// The rail: each visible tab, then its agent rows in catalog order. A row
/// that is no longer in the snapshot is simply not built, which is the whole
/// of "the row goes away when the session closes".
function railRows(tabs: readonly Tab[]): readonly RailRow[] {
  const out: RailRow[] = [];
  let ordinal = 0;
  for (let i = 0; i < tabs.length; i += 1) {
    const tab = tabs[i];
    if (!(ordinal >= 0 && ordinal <= 65535)) break;
    out.push({ id: Math.trunc(ordinal), index: tab.index, label: tab.title, state: NO_BYTES, mark: tab.attention ? ATTENTION_MARK : NO_BYTES, selected: tab.selected, agent: false, parentIndex: 65535, target: tab.target, attentionLabel: tab.attentionLabel });
    ordinal += 1;
    const rows = tab.agents;
    for (let j = 0; j < rows.length; j += 1) {
      const row = rows[j];
      if (!(ordinal >= 0 && ordinal <= 65535)) break;
      const label = row.resource.length === 0 ? row.provider : joinBytes(row.provider, asciiBytes(" / "), joinBytes(row.resource, asciiBytes(" under "), row.parent));
      out.push({ id: Math.trunc(ordinal), index: tab.index, label, state: row.state, mark: row.attention ? ATTENTION_MARK : NO_BYTES, selected: false, agent: true, parentIndex: row.parentIndex, target: NO_BYTES, attentionLabel: NO_BYTES });
      ordinal += 1;
    }
  }
  return out;
}

const CLOSED_WINDOW: WindowState = {
  index: 0,
  open: false,
  tabs: [],
  visibleTabs: [],
  tabWidth: 168,
  hasOverflow: false,
  overflowLabel: new Uint8Array(0),
  selectedTab: 0,
};

function closedWindow(index: number): WindowState {
  const at = index >= 0 && index <= 4 ? Math.trunc(index) : 0;
  return { ...CLOSED_WINDOW, index: at };
}

function windowState(index: number, section: SecondaryWindow | null, agents: readonly SnapshotAgentRow[], connection: number): WindowState {
  if (section === null) return closedWindow(index);
  const at = index >= 0 && index <= 4 ? Math.trunc(index) : 0;
  const tabs = stampSlots(section.tabs, at, agents, connection);
  const hidden = tabs.length - section.runCount;
  const selected = section.selectedTab >= 0 && section.selectedTab <= 255 ? Math.trunc(section.selectedTab) : 0;
  const width = section.tabWidth >= 0 && section.tabWidth <= 65535 ? Math.trunc(section.tabWidth) : 168;
  return {
    index: at,
    open: true,
    tabs,
    visibleTabs: sliceRun(tabs, section.runStart, section.runCount),
    tabWidth: width,
    hasOverflow: hidden > 0,
    overflowLabel: hidden > 0 ? overflowLabel(hidden) : new Uint8Array(0),
    selectedTab: selected,
  };
}

// Each slot's descriptor spells its labels literally: the compiled subset
// binds `src/windows/<label>.native` to the literal at build time.
function describeWindow1(): WindowDescriptor {
  return windowDescriptor({
    label: asciiBytes("phux-window-1"),
    canvasLabel: asciiBytes("phux-cockpit-canvas-1"),
    title: asciiBytes("Phux Cockpit TS"),
    width: 1100,
    height: 640,
    minWidth: 900,
    minHeight: 420,
    titlebar: "hidden_inset_tall",
    closePolicy: "quit",
    onCloseCommand: asciiBytes("cockpit.window.closed.1"),
  });
}

function describeWindow2(): WindowDescriptor {
  return windowDescriptor({
    label: asciiBytes("phux-window-2"),
    canvasLabel: asciiBytes("phux-cockpit-canvas-2"),
    title: asciiBytes("Phux Cockpit TS"),
    width: 1100,
    height: 640,
    minWidth: 900,
    minHeight: 420,
    titlebar: "hidden_inset_tall",
    closePolicy: "quit",
    onCloseCommand: asciiBytes("cockpit.window.closed.2"),
  });
}

function describeWindow3(): WindowDescriptor {
  return windowDescriptor({
    label: asciiBytes("phux-window-3"),
    canvasLabel: asciiBytes("phux-cockpit-canvas-3"),
    title: asciiBytes("Phux Cockpit TS"),
    width: 1100,
    height: 640,
    minWidth: 900,
    minHeight: 420,
    titlebar: "hidden_inset_tall",
    closePolicy: "quit",
    onCloseCommand: asciiBytes("cockpit.window.closed.3"),
  });
}

function describeWindow4(): WindowDescriptor {
  return windowDescriptor({
    label: asciiBytes("phux-window-4"),
    canvasLabel: asciiBytes("phux-cockpit-canvas-4"),
    title: asciiBytes("Phux Cockpit TS"),
    width: 1100,
    height: 640,
    minWidth: 900,
    minHeight: 420,
    titlebar: "hidden_inset_tall",
    closePolicy: "quit",
    onCloseCommand: asciiBytes("cockpit.window.closed.4"),
  });
}

/// The secondary windows the engine has open, as platform windows: the same
/// labels the shipping scene declares, so the engine paints, sizes and routes
/// each one through its own table. Presence is liveness. The compiled subset
/// requires an array literal of descriptor calls per return, so every
/// combination of open slots is spelled out.
export function windows(model: Model): readonly WindowDescriptor[] {
  const a = model.window1Open;
  const b = model.window2Open;
  const c = model.window3Open;
  const d = model.window4Open;
  if (a && b && c && d) return [describeWindow1(), describeWindow2(), describeWindow3(), describeWindow4()];
  if (a && b && c && !d) return [describeWindow1(), describeWindow2(), describeWindow3()];
  if (a && b && !c && d) return [describeWindow1(), describeWindow2(), describeWindow4()];
  if (a && b && !c && !d) return [describeWindow1(), describeWindow2()];
  if (a && !b && c && d) return [describeWindow1(), describeWindow3(), describeWindow4()];
  if (a && !b && c && !d) return [describeWindow1(), describeWindow3()];
  if (a && !b && !c && d) return [describeWindow1(), describeWindow4()];
  if (a && !b && !c && !d) return [describeWindow1()];
  if (!a && b && c && d) return [describeWindow2(), describeWindow3(), describeWindow4()];
  if (!a && b && c && !d) return [describeWindow2(), describeWindow3()];
  if (!a && b && !c && d) return [describeWindow2(), describeWindow4()];
  if (!a && b && !c && !d) return [describeWindow2()];
  if (!a && !b && c && d) return [describeWindow3(), describeWindow4()];
  if (!a && !b && c && !d) return [describeWindow3()];
  if (!a && !b && !c && d) return [describeWindow4()];
  return [];
}

/// The OS closed a window. Native retirement already matched the incarnation;
/// withdraw the declaration before the SDK rebuilds so this label cannot
/// recreate a recycled slot. The next snapshot is the post-close disposition.
function windowCommandMsg(name: string): Msg | null {
  if (name === "cockpit.window.closed.1") return { kind: "window_closed", window: 1 };
  if (name === "cockpit.window.closed.2") return { kind: "window_closed", window: 2 };
  if (name === "cockpit.window.closed.3") return { kind: "window_closed", window: 3 };
  if (name === "cockpit.window.closed.4") return { kind: "window_closed", window: 4 };
  return null;
}

function forgetClosedWindow(model: Model, window: number): Model {
  switch (window) {
    case 1: return { ...model, window1Open: false };
    case 2: return { ...model, window2Open: false };
    case 3: return { ...model, window3Open: false };
    case 4: return { ...model, window4Open: false };
    default: return model;
  }
}

function surfaceCommandMsg(name: string): Msg | null {
  if (name === "surface.1") return { kind: "select_active_tab", index: 0 };
  if (name === "surface.2") return { kind: "select_active_tab", index: 1 };
  if (name === "surface.3") return { kind: "select_active_tab", index: 2 };
  if (name === "surface.4") return { kind: "select_active_tab", index: 3 };
  if (name === "surface.5") return { kind: "select_active_tab", index: 4 };
  return null;
}

function navigationCommandMsg(name: string): Msg | null {
  if (name === "commands.open") return { kind: "commands_open" };
  if (name === "navigator.sessions") return { kind: "navigator_open", view: 1 };
  if (name === "navigator.machines") return { kind: "navigator_open", view: 2 };
  if (name === "navigator.windows") return { kind: "navigator_open", view: 3 };
  if (name === "tabs.palette") return { kind: "palette_open" };
  return null;
}

function creationCommandMsg(name: string): Msg | null {
  if (name === "config.edit") return { kind: "config_edit" };
  if (name === "session.new") return { kind: "new_session_open" };
  if (name === "terminal.new") return { kind: "new_terminal" };
  if (name === "window.new") return { kind: "new_window" };
  if (name === "settings.open") return { kind: "settings_open" };
  if (name === "remote.connect") return { kind: "host_open" };
  if (name === "directory.open") return { kind: "dir_open" };
  if (name === "session.rename") return { kind: "rename_open" };
  if (name === "tabs.toggle-placement") return { kind: "toggle_tab_placement" };
  return null;
}

function tabNativeCommand(name: string): Msg | null {
  if (name === "tab.previous") return { kind: "native_command", command: 1 };
  if (name === "tab.next") return { kind: "native_command", command: 2 };
  if (name === "terminal.close") return { kind: "native_command", command: 3 };
  if (name === "pane.split-right") return { kind: "native_command", command: 4 };
  if (name === "pane.split-down") return { kind: "native_command", command: 5 };
  if (name === "pane.previous") return { kind: "native_command", command: 6 };
  if (name === "pane.next") return { kind: "native_command", command: 7 };
  if (name === "tab.move-left") return { kind: "native_command", command: 8 };
  if (name === "tab.move-right") return { kind: "native_command", command: 9 };
  return null;
}

function editingNativeCommand(name: string): Msg | null {
  if (name === "terminal.select-all") return { kind: "native_command", command: 10 };
  if (name === "terminal.copy") return { kind: "native_command", command: 11 };
  if (name === "terminal.paste") return { kind: "native_command", command: 12 };
  if (name === "terminal.clear") return { kind: "native_command", command: 13 };
  if (name === "terminal.find") return { kind: "native_command", command: 14 };
  if (name === "terminal.find-next") return { kind: "native_command", command: 15 };
  if (name === "terminal.find-previous") return { kind: "native_command", command: 16 };
  return null;
}

function presentationNativeCommand(name: string): Msg | null {
  if (name === "view.font-larger") return { kind: "native_command", command: 17 };
  if (name === "view.font-smaller") return { kind: "native_command", command: 18 };
  if (name === "view.font-reset") return { kind: "native_command", command: 19 };
  if (name === "pane.focus-left") return { kind: "native_command", command: 20 };
  if (name === "pane.focus-right") return { kind: "native_command", command: 21 };
  if (name === "pane.focus-up") return { kind: "native_command", command: 22 };
  if (name === "pane.focus-down") return { kind: "native_command", command: 23 };
  if (name === "window.fullscreen") return { kind: "native_command", command: 24 };
  if (name === "window.minimize") return { kind: "native_command", command: 25 };
  return null;
}

export function commandMsg(name: string): Msg | null {
  return windowCommandMsg(name) ?? surfaceCommandMsg(name) ?? navigationCommandMsg(name) ??
    creationCommandMsg(name) ?? tabNativeCommand(name) ?? editingNativeCommand(name) ?? presentationNativeCommand(name);
}

function findSection(sections: readonly SecondaryWindow[], index: number) {
  for (let i = 0; i < sections.length; i += 1) {
    if (sections[i].index === index) return sections[i];
  }
  return null;
}

/// Slice the tab list to the engine's run. Every index is proven whole from
/// the wire (protocol.ts fences the bytes), so the slice needs no more.
function sliceRun(tabs: readonly Tab[], runStart: number, runCount: number): readonly Tab[] {
  const total = tabs.length;
  if (!(runStart >= 0 && runStart <= 255) || !(runCount >= 0 && runCount <= 255)) return tabs;
  const start = Math.trunc(runStart);
  const count = Math.trunc(runCount);
  if (start + count > total) return tabs;
  return tabs.slice(start, start + count);
}

export function initialModel(): [Model, Cmd<Msg>] {
  return [
    {
      tabs: [{ id: 1, index: 0, slot: 0, title: asciiBytes("Terminal 1"), cwd: new Uint8Array(0), selected: true, attention: false, attentionLabel: NO_BYTES, agents: NO_AGENT_ROWS, target: NO_BYTES }],
      visibleTabs: [{ id: 1, index: 0, slot: 0, title: asciiBytes("Terminal 1"), cwd: new Uint8Array(0), selected: true, attention: false, attentionLabel: NO_BYTES, agents: NO_AGENT_ROWS, target: NO_BYTES }],
      tabWidth: 168,
      hasOverflow: false,
      overflowLabel: new Uint8Array(0),
      selectedTab: 0,
      tabPlacement: "top",
      railRows: NO_RAIL_ROWS,
      workspaceLabel: asciiBytes("Sessions"),
      mainContext: {
        title: asciiBytes("Sessions"),
        detail: asciiBytes("Machine not yet known"),
        emptyName: NO_BYTES,
        emptyDetail: NO_BYTES,
        emptyPicked: false,
        emptyOpening: false,
      },
      window1Context: NO_WINDOW_CONTEXT,
      window2Context: NO_WINDOW_CONTEXT,
      window3Context: NO_WINDOW_CONTEXT,
      window4Context: NO_WINDOW_CONTEXT,
      window1RailRows: NO_RAIL_ROWS,
      window2RailRows: NO_RAIL_ROWS,
      window3RailRows: NO_RAIL_ROWS,
      window4RailRows: NO_RAIL_ROWS,
      activeWindow: 0,
      paletteOpen: false,
      agentsMode: false,
      inspectedResource: NO_BYTES,
      agentCountLabel: asciiBytes("Agents..."),
      mainAgentsOpen: false,
      window1AgentsOpen: false,
      window2AgentsOpen: false,
      window3AgentsOpen: false,
      window4AgentsOpen: false,
      navigatorView: 0,
      navigatorTitle: asciiBytes("Go to Terminal"),
      actionRows: NO_ACTION_ROWS,
      bindings: initialKeybindings(),
      commandContextTarget: NO_BYTES,
      commandContextWindow: 0,
      machines: initialMachines(),
      machineRows: NO_MACHINE_ROWS,
      machinesOperation: 0,
      windowActionId: 0,
      windowActionPending: false,
      mainPaletteOpen: false,
      paletteQuery: new Uint8Array(0),
      paletteScope: 0,
      paletteHost: NO_BYTES,
      paletteHostLabel: NO_BYTES,
      navigationScopes: [{ index: 0, label: asciiBytes("All work") }, { index: 1, label: asciiBytes("Sessions") }, { index: 2, label: asciiBytes("Known hosts") }],
      coordinatorEndpoint: NO_BYTES,
      machineLabel: asciiBytes("Machine not yet known"),
      connectionDetail: NO_BYTES,
      paletteAnchor: 0,
      paletteFocus: 0,
      paletteRows: NO_ROWS,
      paletteCursor: 0,
      paletteSelection: NO_BYTES,
      paletteRefreshing: false,
      paletteFill: 24,
      paletteOffset: 0,
      paletteTotal: 0,
      palettePrevious: false,
      paletteNext: false,
      paletteLoading: false,
      paletteNotice: NO_BYTES,
      hostOpen: false,
      toolPurpose: 0,
      toolToken: NO_BYTES,
      toolLaunchToken: NO_BYTES,
      toolLaunchPending: false,
      toolOperationId: 0,
      toolOperationToken: NO_BYTES,
      toolStatusBusy: false,
      toolTarget: NO_BYTES,
      toolQueued: false,
      hostFriendlyName: NO_BYTES,
      friendlyAnchor: 0,
      friendlyFocus: 0,
      configEditorConfirm: false,
      pendingToolOpen: false,
      mainHostOpen: false,
      window1HostOpen: false,
      window2HostOpen: false,
      window3HostOpen: false,
      window4HostOpen: false,
      dirOpen: false,
      mainDirOpen: false,
      window1DirOpen: false,
      window2DirOpen: false,
      window3DirOpen: false,
      window4DirOpen: false,
      dirQuery: new Uint8Array(0),
      dirAnchor: 0,
      dirFocus: 0,
      dirRequest: NO_DIRECTORY_REQUEST,
      dirStarting: false,
      dirAwaiting: false,
      dirClosing: false,
      dirBusy: false,
      dirRows: NO_DIR_ROWS,
      dirCursor: 0,
      dirOffset: 0,
      dirPrevious: false,
      dirNext: false,
      dirPath: new Uint8Array(0),
      dirNotice: new Uint8Array(0),
      dirTitle: asciiBytes("Go to Directory"),
      dirOpenHere: true,
      renameOpen: false,
      creatingSession: false,
      newSessionToken: NO_BYTES,
      newSessionAwaiting: false,
      mainRenameOpen: false,
      window1RenameOpen: false,
      window2RenameOpen: false,
      window3RenameOpen: false,
      window4RenameOpen: false,
      renameQuery: new Uint8Array(0),
      renameRow: new Uint8Array(0),
      renameAnchor: 0,
      renameFocus: 0,
      renameTitle: asciiBytes("Rename Session"),
      renameNotice: new Uint8Array(0),
      renameBusy: false,
      renameAwaiting: false,
      emptyWindows: 0,
      mainEmptyOpen: false,
      window1EmptyOpen: false,
      window2EmptyOpen: false,
      window3EmptyOpen: false,
      window4EmptyOpen: false,
      emptyName: new Uint8Array(0),
      emptyDetail: new Uint8Array(0),
      emptyPicked: false,
      emptyBusy: false,
      emptyNotice: new Uint8Array(0),
      hostQuery: new Uint8Array(0),
      hostAnchor: 0,
      hostFocus: 0,
      hostNotice: NO_BYTES,
      hostBusy: false,
      hostAwaiting: false,
      hostPhase: 0,
      hostName: new Uint8Array(0),
      remoteLine: new Uint8Array(0),
      // No byte value: the first snapshot always asks which host is selected,
      // so a host restored at launch is named from the start.
      lastConnection: 255,
      canReconnect: false,
      connectionStatus: asciiBytes("Starting Cockpit..."),
      window1Status: asciiBytes("Starting Cockpit..."),
      window2Status: asciiBytes("Starting Cockpit..."),
      window3Status: asciiBytes("Starting Cockpit..."),
      window4Status: asciiBytes("Starting Cockpit..."),
      settingsOpen: false,
      settingsQuery: NO_BYTES,
      settingsAnchor: 0,
      settingsFocus: 0,
      settingRows: NO_SETTING_ROWS,
      settingEditId: 65535,
      settingEditValue: NO_BYTES,
      settingAnchor: 0,
      settingFocus: 0,
      settingsReloadStage: 0,
      settingsNotice: NO_BYTES,
      bindingRows: NO_BINDING_ROWS,
      pendingSessionAction: null,
      pendingSettingsAction: null,
      retiredSessionToken: NO_BYTES,
      navigatorScroll: 0,
      navigatorViewport: 0,
      noBindingRows: true,
      noSettingRows: false,
      bindingEditIndex: 65535,
      bindingEditValue: NO_BYTES,
      bindingAnchor: 0,
      bindingFocus: 0,
      mainSettingsOpen: false,
      themes: NO_THEMES,
      settingsCursor: 0,
      configExists: false,
      configNotice: new Uint8Array(0),
      appearance: initialAppearance(),
      appearanceBusy: false,
      settingsSection: 0,
      settingsSections: [
        { index: 0, label: asciiBytes("Appearance") }, { index: 1, label: asciiBytes("Terminal") }, { index: 2, label: asciiBytes("Keyboard") }, { index: 3, label: asciiBytes("Window") }, { index: 4, label: asciiBytes("Advanced") },
      ],
      cursorChoices: [
        { index: 0, label: asciiBytes("Block") }, { index: 1, label: asciiBytes("Bar") }, { index: 2, label: asciiBytes("Underline") },
      ],
      placementChoices: [{ index: 0, label: asciiBytes("Top strip") }, { index: 1, label: asciiBytes("Workspace rail") }],
      fontDecrease: 0,
      fontIncrease: 1,
      navigationAfterSettings: false,
      surfaceAfterSettings: 0,
      appearanceClosing: false,
      window1Open: false,
      window1Tabs: NO_TABS,
      window1TabWidth: 168,
      window1HasOverflow: false,
      window1OverflowLabel: new Uint8Array(0),
      window1PaletteOpen: false,
      window1SettingsOpen: false,
      window2Open: false,
      window2Tabs: NO_TABS,
      window2TabWidth: 168,
      window2HasOverflow: false,
      window2OverflowLabel: new Uint8Array(0),
      window2PaletteOpen: false,
      window2SettingsOpen: false,
      window3Open: false,
      window3Tabs: NO_TABS,
      window3TabWidth: 168,
      window3HasOverflow: false,
      window3OverflowLabel: new Uint8Array(0),
      window3PaletteOpen: false,
      window3SettingsOpen: false,
      window4Open: false,
      window4Tabs: NO_TABS,
      window4TabWidth: 168,
      window4HasOverflow: false,
      window4OverflowLabel: new Uint8Array(0),
      window4PaletteOpen: false,
      window4SettingsOpen: false,
      engineConnected: false,
      engineSequence: ZERO_U64,
      tabCommands: initialTabCommands(),
      commandResults: { ...initialCommandResults(), loading: true },
      commandNotice: NO_BYTES,
      engineRevision: ZERO_U64,
      status: asciiBytes("Starting Cockpit..."),
    },
    Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.channelOpen(ENGINE_CHANNEL_KEY, { event: "engine_event" }),
      Cmd.request("cockpit.snapshot", new Uint8Array(0), {
        key: "cockpit-snapshot",
        ok: "snapshot_loaded",
        err: "snapshot_failed",
      }),
      Cmd.request("cockpit.command-results", new Uint8Array([1, 0]), {
        key: "cockpit-command-results", ok: "command_result_loaded", err: "command_result_failed",
      }),
    ]),
  ];
}

function selectTab(tabs: readonly Tab[], selected: number): readonly Tab[] {
  return tabs.map((tab) => ({ ...tab, selected: tab.index === selected }));
}

function speculateTabTarget(model: Model, target: Uint8Array): Model {
  for (const tab of model.tabs) {
    if (!sameBytes(tab.target, target)) continue;
    const visibleTabs = selectTab(model.visibleTabs, tab.index);
    return { ...model, tabs: selectTab(model.tabs, tab.index), visibleTabs, railRows: railRows(visibleTabs), selectedTab: tab.index };
  }
  return model;
}

function tabCommandNotice(outcome: number): Uint8Array {
  if (outcome === 3) return asciiBytes("Command refused. Refresh the target and try again.");
  if (outcome === 4) return asciiBytes("Command queue full. New command not sent.");
  if (outcome === 5) return asciiBytes("Command outcome unknown; queued commands canceled.");
  if (outcome === 6) return asciiBytes("Command IDs exhausted. Restart Cockpit.");
  if (outcome === 7) return asciiBytes("Command accepted; outcome pending.");
  return NO_BYTES;
}

function tabCommandModel(model: Model, decision: TabCommandDecision): Model {
  const notice = model.commandResults.notice;
  return { ...model, tabCommands: decision.state, commandNotice: notice.length > 0 ? notice : tabCommandNotice(decision.state.outcome) };
}

function freshCommandModel(model: Model, decision: TabCommandDecision): Model {
  return tabCommandModel({ ...model, commandResults: { ...model.commandResults, notice: NO_BYTES } }, decision);
}

interface TabCommandTransition {
  readonly model: Model;
  readonly request: Uint8Array;
}

function tabCommandTransition(model: Model, msg: Msg): TabCommandTransition | null {
  if (msg.kind === "select_target") {
    const decision = enqueueTabCommand(model.tabCommands, msg.target);
    const next = freshCommandModel(model, decision);
    return { model: decision.state.outcome === 1 ? speculateTabTarget(next, msg.target) : next, request: decision.request };
  }
  if (msg.kind === "tab_command_completed") {
    const decision = receiveTabReceipt(model.tabCommands, msg.body);
    return { model: tabCommandModel(model, decision), request: decision.request };
  }
  if (msg.kind === "tab_command_failed") {
    return { model: tabCommandModel(model, unknownTabCommand(model.tabCommands)), request: NO_BYTES };
  }
  const operation = operationIntent(model, msg);
  if (operation.length === 0) return null;
  const decision = enqueueOperationCommand(model.tabCommands, operation);
  return { model: freshCommandModel(model, decision), request: decision.request };
}

function operationIntent(model: Model, msg: Msg): Uint8Array {
  switch (msg.kind) {
    case "new_terminal": return intent(2, model.engineRevision, 0, 255);
    case "new_window": return intent(8, model.engineRevision, 0, 255);
    case "close_selected_tab": return intent(3, model.engineRevision, model.selectedTab, 0);
    case "native_command":
      if (durableNativeCommand(msg.command)) return intent(11, model.engineRevision, msg.command, 255);
      return NO_BYTES;
    default: return NO_BYTES;
  }
}

function durableNativeCommand(command: number): boolean {
  return command === 3 || command === 4 || command === 5 || command === 8 || command === 9;
}

function outcomeNotice(model: Model, decision: ResultDecision): Uint8Array {
  if (decision.state.notice.length > 0) return decision.state.notice;
  if (decision.state.deliveryNotice.length > 0) {
    return model.commandNotice.length > 0 ? model.commandNotice : decision.state.deliveryNotice;
  }
  const recent = decision.state.recent;
  if (recent.length === 0) return tabCommandNotice(model.tabCommands.outcome);
  const last = recent[recent.length - 1];
  if (last.source !== 3 && sameU64(last.id, model.tabCommands.lastId)) return NO_BYTES;
  return tabCommandNotice(model.tabCommands.outcome);
}

function resultTransition(model: Model, msg: Msg): TabCommandTransition | null {
  if (msg.kind === "command_result_failed") {
    const decision = failedCommandResults(model.commandResults);
    return { model: { ...model, commandResults: decision.state, commandNotice: outcomeNotice(model, decision) }, request: decision.request };
  }
  if (msg.kind !== "command_result_loaded") return null;
  const decision = receiveCommandResult(model.commandResults, msg.body);
  return { model: { ...model, commandResults: decision.state, commandNotice: outcomeNotice(model, decision) }, request: decision.request };
}

function legacySlotIntent(revision: WireU64, slot: number): Uint8Array {
  if (!(slot >= 0 && slot <= 159)) return NO_BYTES;
  let window = 0;
  let index = Math.trunc(slot);
  while (index >= 32) {
    index -= 32;
    window += 1;
  }
  return intent(1, revision, index, window);
}

interface AppearanceDecision {
  readonly model: Model;
  readonly request: Uint8Array;
  readonly opening: boolean;
  readonly closed: boolean;
  readonly navigate: boolean;
}

function appearanceDecision(model: Model): AppearanceDecision {
  return { model, request: NO_BYTES, opening: false, closed: false, navigate: false };
}

function requestAppearance(model: Model, action: number, argument: number): AppearanceDecision {
  return { ...appearanceDecision({ ...model, appearanceBusy: true }), request: appearanceRequest(action, argument) };
}

function openAppearance(model: Model): AppearanceDecision {
  if (model.settingsOpen) return appearanceDecision(model);
  const next = scopeOverlays({ ...model, settingsOpen: true, paletteOpen: false, hostOpen: false, hostAwaiting: false, settingsSection: 0,
     navigationAfterSettings: false, surfaceAfterSettings: 0, pendingToolOpen: false, configEditorConfirm: false,
     appearanceClosing: false, appearance: initialAppearance(), appearanceBusy: true,
    settingEditId: 65535, settingsQuery: NO_BYTES, settingsNotice: NO_BYTES, settingsReloadStage: 0,
    settingRows: settingsRows(initialAppearance(), NO_BYTES, 0) });
  return { ...requestAppearance(next, 0, 0), opening: true };
}

function loadedAppearance(model: Model, body: Uint8Array): AppearanceDecision {
  if (!model.settingsOpen) return appearanceDecision(model);
  const appearance = appearanceResponse(body);
  if (appearance === null) return appearanceFailure(model);
  const cursor = appearance.theme < model.themes.length ? appearance.theme : model.settingsCursor;
  const rows = settingsRows(appearance, model.settingsQuery, model.settingsSection);
  const next = scopeOverlays({ ...model, appearance, appearanceBusy: false, appearanceClosing: false, settingsCursor: cursor,
    settingRows: rows, noSettingRows: rows.length === 0,
    themes: highlightThemes(model.themes, cursor), settingsOpen: appearance.active });
  if (model.settingsReloadStage > 0) return advanceSettingsReload(next);
  if (appearance.active) return appearanceDecision({ ...next, pendingToolOpen: false });
  if (model.navigationAfterSettings) return openNavigationAfterAppearance(next);
  return { ...appearanceDecision(next), closed: true };
}

function openNavigationAfterAppearance(model: Model): AppearanceDecision {
  const next = changeNavigation(model, { kind: "palette_open" });
  return { ...appearanceDecision(next), navigate: true };
}

function advanceSettingsReload(model: Model): AppearanceDecision {
  const next = scopeOverlays({ ...model, settingsOpen: true, appearanceBusy: true });
  if (model.settingsReloadStage === 1) return { ...appearanceDecision({ ...next, settingsReloadStage: 2 }), request: reloadSettingsRequest() };
  return requestAppearance({ ...next, settingsReloadStage: 0, settingsNotice: model.appearance.notice }, 0, 0);
}

function failedAppearance(model: Model): Model {
  return { ...model, appearanceBusy: false, appearance: { ...model.appearance,
    notice: asciiBytes("Appearance unavailable. Retry Save or Cancel before continuing.") } };
}

/// Dismissal never depends on a native round trip: a transaction that never
/// began has nothing to roll back, and a failed rollback must still release
/// Settings and the keyboard. A failed edit or Save keeps the preview open.
function dismissLocally(model: Model): AppearanceDecision {
  const next = scopeOverlays({ ...model, settingsOpen: false, appearanceBusy: false, appearanceClosing: false,
    appearance: initialAppearance() });
  if (model.navigationAfterSettings) return openNavigationAfterAppearance(next);
  return { ...appearanceDecision(next), closed: true };
}

function appearanceFailure(model: Model): AppearanceDecision {
  if (!model.settingsOpen) return appearanceDecision(model);
  if (model.appearanceClosing) return dismissLocally(model);
  return appearanceDecision(failedAppearance({ ...model, pendingToolOpen: false, settingsReloadStage: 0 }));
}

function dismissAppearance(model: Model): AppearanceDecision {
  model = { ...model, settingsReloadStage: 0 };
  if (!model.appearance.active && !model.appearanceBusy) return dismissLocally(model);
  return requestAppearance({ ...model, appearanceClosing: true }, 6, 0);
}

/// The placement menu command joins the open transaction so Cancel, Save and
/// the Settings selector all see it; with no transaction it is ignored.
function togglePreviewPlacement(model: Model): AppearanceDecision {
  if (!model.appearance.active) return appearanceDecision(model);
  return requestAppearance(model, 5, model.appearance.placement === 1 ? 0 : 1);
}

function previewTheme(model: Model, index: number): AppearanceDecision {
  if (!(index >= 0 && index < model.themes.length && index <= 32)) return appearanceDecision(model);
  const cursor = Math.trunc(index);
  return requestAppearance({ ...model, settingsCursor: cursor, themes: highlightThemes(model.themes, cursor) }, 1, cursor);
}

function editAppearance(model: Model, msg: Msg): AppearanceDecision | null {
  switch (msg.kind) {
    case "settings_pick": return previewTheme(model, msg.index);
    case "settings_move": return moveThemePreview(model, msg.delta);
    case "settings_font": return requestAppearance(model, msg.direction > 0 ? 2 : 3, 0);
    case "settings_cursor": return requestAppearance(model, 4, msg.index);
    case "settings_placement": return requestAppearance(model, 5, msg.index);
    case "toggle_tab_placement": return togglePreviewPlacement(model);
    case "settings_commit": return requestAppearance(model, 7, 0);
    default: return null;
  }
}

function moveThemePreview(model: Model, delta: number): AppearanceDecision {
  if (model.settingsSection !== 0) return appearanceDecision(model);
  return previewTheme(model, model.settingsCursor + (delta >= 0 ? 1 : -1));
}

function updateAppearance(model: Model, msg: Msg): AppearanceDecision | null {
  if (msg.kind === "settings_open") return openAppearance(model);
  if (msg.kind === "appearance_loaded") return loadedAppearance(model, msg.body);
  if (msg.kind === "appearance_failed") return appearanceFailure(model);
  if (!model.settingsOpen) return null;
  return updateOpenAppearance(model, msg);
}

function updateOpenAppearance(model: Model, msg: Msg): AppearanceDecision | null {
  if (msg.kind === "settings_section") return appearanceDecision({ ...model, settingsSection: msg.section });
  if (msg.kind === "settings_close") return dismissAppearance(model);
  if (msg.kind === "palette_open") return dismissAppearance({ ...model, navigationAfterSettings: true });
  const edited = editAppearance(model, msg);
  if (edited === null) return null;
  return model.appearanceBusy ? appearanceDecision(model) : edited;
}

function activeTabs(model: Model): readonly Tab[] {
  if (model.activeWindow === 1) return model.window1Tabs;
  if (model.activeWindow === 2) return model.window2Tabs;
  if (model.activeWindow === 3) return model.window3Tabs;
  if (model.activeWindow === 4) return model.window4Tabs;
  return model.tabs;
}

function focusedTabTarget(model: Model): Uint8Array {
  // Secondary top strips are viewport slices; their rail projection retains
  // every tab, including a selected tab outside that strip's visible run.
  if (model.activeWindow !== 0) {
    for (const row of activeRailRows(model)) if (row.selected) return row.target;
    return NO_BYTES;
  }
  for (const tab of model.tabs) if (tab.selected) return tab.target;
  return NO_BYTES;
}

function activeRailRows(model: Model): readonly RailRow[] {
  if (model.activeWindow === 1) return model.window1RailRows;
  if (model.activeWindow === 2) return model.window2RailRows;
  if (model.activeWindow === 3) return model.window3RailRows;
  if (model.activeWindow === 4) return model.window4RailRows;
  return model.railRows;
}

function commandContextCurrent(model: Model): boolean {
  return model.commandContextWindow === model.activeWindow && sameBytes(model.commandContextTarget, focusedTabTarget(model));
}

function refreshActions(model: Model, cursor: number): Model {
  const rows = commandRows(model.paletteQuery, cursor, model.engineConnected && activeTabs(model).length > 0, model.workspaceLabel, model.bindings);
  const available: ActionRow[] = [];
  for (const row of rows) {
    const command = commandDefinition(row.index);
    const implemented = command !== null && commandMsg(command.name) !== null;
    available.push({ ...row, disabled: row.disabled || !implemented,
      detail: implemented ? row.detail : asciiBytes("Unavailable in this connection") });
  }
  const selected = cursor >= 0 && cursor <= 65535 ? Math.trunc(cursor) : 0;
  return { ...model, actionRows: available, paletteCursor: selected,
    paletteNotice: rows.length === 0 ? asciiBytes("No matching commands") : asciiBytes("Enter to run  /  Escape to return") };
}

function openActions(model: Model): Model {
  const next = scopeOverlays({ ...model, paletteOpen: true, navigatorView: 4,
    navigatorTitle: asciiBytes("Commands"), paletteQuery: NO_BYTES, paletteAnchor: 0, paletteFocus: 0,
    paletteRows: NO_ROWS, paletteLoading: false, settingsOpen: false, hostOpen: false, hostAwaiting: false,
    dirOpen: false, renameOpen: false });
  const window = model.activeWindow >= 0 && model.activeWindow <= 4 ? Math.trunc(model.activeWindow) : 0;
  return refreshActions({ ...next, commandContextTarget: focusedTabTarget(model), commandContextWindow: window }, 0);
}

function editActions(model: Model, edit: TextInputEvent): Model {
  const state = applyTextInputEvent(paletteState(model), edit, 64);
  if (state === null) return model;
  const anchor = state.selection.anchor >= 0 && state.selection.anchor <= 64 ? Math.trunc(state.selection.anchor) : 0;
  const focus = state.selection.focus >= 0 && state.selection.focus <= 64 ? Math.trunc(state.selection.focus) : 0;
  return refreshActions({ ...model, paletteQuery: state.text, paletteAnchor: anchor, paletteFocus: focus }, 0);
}

function actionMessage(model: Model, index: number): Msg | null {
  for (const row of model.actionRows) {
    if (row.index !== index || row.disabled) continue;
    const definition = commandDefinition(index);
    if (definition === null) return null;
    if (contextualCommand(definition.name) && !commandContextCurrent(model)) return null;
    return commandMsg(definition.name);
  }
  return null;
}

function selectedAction(model: Model, msg: Msg): Msg | null {
  if (msg.kind === "commands_pick") return actionMessage(model, msg.index);
  if (msg.kind !== "palette_submit") return null;
  if (model.paletteCursor >= model.actionRows.length) return null;
  return actionMessage(model, model.actionRows[model.paletteCursor].index);
}

function navigatorDestination(msg: Msg): number {
  if (msg.kind === "navigator_open") return msg.view;
  if (msg.kind === "sessions_open") return 1;
  if (msg.kind === "machines_open") return 2;
  if (msg.kind === "windows_open") return 3;
  return -1;
}

interface NavigatorDecision {
  readonly model: Model;
  /// 0 local transition, 1 commit only, 2 machines request, 3 navigation request.
  readonly effect: number;
  readonly request: Uint8Array;
}

function navigatorDecision(model: Model, effect: number, request: Uint8Array): NavigatorDecision {
  return { model: { ...model, machineRows: model.machines.visible,
    noBindingRows: model.bindingRows.length === 0, noSettingRows: model.settingRows.length === 0 }, effect, request };
}

function requestMachineOperation(model: Model, operation: number, target: Uint8Array): NavigatorDecision {
  const machines = requestMachines(model.machines, operation);
  const request = machineRequest(machines, operation, target);
  const browseToken = operation === 7 ? request.slice(2, 14) : NO_BYTES;
  return navigatorDecision({ ...model, machines: { ...machines, browseToken } }, 2, request);
}

function openNavigator(model: Model, view: number): NavigatorDecision {
  const destination = view >= 1 && view <= 3 ? Math.trunc(view) : 1;
  const title = view === 1 ? asciiBytes("Sessions") : view === 2 ? asciiBytes("Machines") : asciiBytes("Windows");
  const next = scopeOverlays({ ...model, paletteOpen: true, navigatorView: destination, navigatorTitle: title,
    settingsOpen: false, hostOpen: false, hostAwaiting: false, dirOpen: false, renameOpen: false,
    paletteQuery: NO_BYTES, paletteAnchor: 0, paletteFocus: 0, paletteRows: NO_ROWS,
    paletteHost: NO_BYTES, paletteHostLabel: NO_BYTES, palettePrevious: false, paletteNext: false,
    paletteScope: view === 3 ? 4 : 1 });
  if (view === 2) return requestMachineOperation({ ...next, paletteLoading: false, paletteNotice: NO_BYTES }, 0, NO_BYTES);
  const loading = requestNavigation(next, 0);
  return navigatorDecision(loading, 3, scopedNavigationRequest(loading));
}

function machineAction(model: Model, target: Uint8Array): NavigatorDecision {
  const row = capturedMachine(model.machines, target);
  if (row === null || row.disabled) return navigatorDecision(model, 0, NO_BYTES);
  const operation = row.connected ? 7 : row.state === 4 ? 3 : 2;
  return requestMachineOperation(model, operation, target);
}

function editMachineSearch(model: Model, edit: TextInputEvent): Model {
  const edited = editActions(model, edit);
  return { ...edited, machines: filterMachines(model.machines, edited.paletteQuery), paletteNotice: NO_BYTES };
}

function forgetMachine(model: Model, target: Uint8Array): Model {
  const row = capturedMachine(model.machines, target);
  if (row === null || !row.canForget) return model;
  return { ...model, machines: { ...model.machines, forgetTarget: target, forgetName: row.name } };
}

function confirmForgetMachine(model: Model): NavigatorDecision {
  const row = capturedMachine(model.machines, model.machines.forgetTarget);
  if (row === null || !row.canForget) return navigatorDecision(model, 0, NO_BYTES);
  return requestMachineOperation(model, 5, row.target);
}

function machinePointerTransition(model: Model, msg: Msg): NavigatorDecision | null {
  switch (msg.kind) {
    case "machine_pick": return machineAction(model, msg.target);
    case "machine_disconnect": {
      const row = capturedMachine(model.machines, msg.target);
      if (row === null || !row.connected) return navigatorDecision(model, 0, NO_BYTES);
      return requestMachineOperation(model, 4, msg.target);
    }
    case "machine_forget": return navigatorDecision(forgetMachine(model, msg.target), 0, NO_BYTES);
    case "machine_forget_confirm": return confirmForgetMachine(model);
    case "machine_forget_cancel": return navigatorDecision({ ...model, machines: { ...model.machines, forgetTarget: NO_BYTES } }, 0, NO_BYTES);
    case "machines_more": return requestMachineOperation(model, 1, NO_BYTES);
    default: return null;
  }
}

function machinesTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (!model.paletteOpen || model.navigatorView !== 2) return null;
  switch (msg.kind) {
    case "palette_edit": return navigatorDecision(editMachineSearch(model, msg.edit), 0, NO_BYTES);
    case "palette_move": return navigatorDecision(moveMachineSelection(model, msg.delta), 0, NO_BYTES);
    case "palette_submit": return machineAction(model, model.machines.selected);
    case "palette_retry": return requestMachineOperation(model, 0, NO_BYTES);
    case "machines_loaded": return receivedMachineInventory(model, msg.body);
    case "machines_failed": return navigatorDecision({ ...model, machines: { ...model.machines, loading: false, failed: true, notice: asciiBytes("Machines unavailable. Refresh to try again.") } }, 0, NO_BYTES);
    default: return machinePointerTransition(model, msg);
  }
}

function receivedMachineInventory(model: Model, body: Uint8Array): NavigatorDecision {
  const machines = receiveMachines(model.machines, body, model.paletteQuery);
  if (machines === model.machines) return { model, effect: 0, request: NO_BYTES };
  const next = { ...model, machines };
  if (model.machines.operation === 7 && !machines.failed && !machines.loading) return openMachineSessions(next);
  const status = continueMachineStatus(next);
  if (status !== null) return status;
  return navigatorDecision(next, 0, NO_BYTES);
}

function continueMachineStatus(model: Model): NavigatorDecision | null {
  const machines = model.machines;
  if (machines.loading || machines.failed) return null;
  if (machines.statusFirst > 0) return requestMachineStatus(model, false);
  if (machines.statusDirty) return requestMachineStatus(model, true);
  return null;
}

function openMachineSessions(model: Model): NavigatorDecision {
  if (model.machines.browseToken.length !== 12) return navigatorDecision(model, 0, NO_BYTES);
  const sessions = openNavigator(model, 1).model;
  const captured = requestNavigation({ ...sessions, paletteScope: 5, paletteHost: model.machines.browseToken }, 0);
  return navigatorDecision(captured, 3, scopedNavigationRequest(captured));
}

function refreshMachineSnapshot(model: Model): NavigatorDecision | null {
  if (!model.paletteOpen || model.navigatorView !== 2 || model.machines.loading) return null;
  if (model.machines.generation === 0 || model.machines.failed) return null;
  return requestMachineStatus(model, true);
}

function requestMachineStatus(model: Model, restart: boolean): NavigatorDecision {
  const previous: MachineState = restart ? { ...model.machines, statusFirst: 0, statusDirty: false } : model.machines;
  const machines = requestMachines(previous, 8);
  return navigatorDecision({ ...model, machines }, 2, machineStatusRequest(machines));
}

function retainMachineInvalidation(model: Model): Model {
  if (!model.paletteOpen || model.navigatorView !== 2) return model;
  if (!model.machines.loading) return model;
  return { ...model, machines: { ...model.machines, statusDirty: true } };
}

function loadedKeybindings(model: Model, body: Uint8Array): NavigatorDecision {
  const bindings = keybindingResponse(body);
  if (bindings === null) return failedKeybindings(model);
  if (model.appearanceClosing || model.settingsReloadStage > 0) return navigatorDecision({ ...model, bindings,
    bindingRows: filteredBindings(bindings, model.settingsQuery), settingsNotice: bindings.notice }, 0, NO_BYTES);
  const next = { ...model, bindings, bindingRows: filteredBindings(bindings, model.settingsQuery), appearanceBusy: false, settingsNotice: bindings.notice };
  if (model.settingsOpen) return navigatorDecision({ ...next, appearanceBusy: true }, 7, appearanceRequest(0, 0));
  return navigatorDecision(refreshActions(next, model.paletteCursor), 0, NO_BYTES);
}

function failedKeybindings(model: Model): NavigatorDecision {
  const notice = asciiBytes("Keyboard shortcuts unavailable. Reopen Keyboard to retry.");
  return navigatorDecision({ ...model, appearanceBusy: false, settingsNotice: notice }, 0, NO_BYTES);
}

function filteredBindings(bindings: KeybindingPage, query: Uint8Array): readonly KeybindingRow[] {
  const rows: KeybindingRow[] = [];
  for (const row of bindings.rows) {
    if (containsQuery(row.label, query) || containsQuery(row.command, query)) rows.push(row);
  }
  return rows;
}

function commandsTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (msg.kind === "commands_open") return navigatorDecision(openActions(model), 4, keybindingRequest(0, 0, NO_BYTES));
  if (msg.kind === "keybindings_loaded") return loadedKeybindings(model, msg.body);
  if (msg.kind === "keybindings_failed") return failedKeybindings(model);
  if (!model.paletteOpen || model.navigatorView !== 4) return null;
  if (msg.kind === "palette_edit") return navigatorDecision(editActions(model, msg.edit), 0, NO_BYTES);
  if (msg.kind === "palette_move") {
    const next = Math.max(0, Math.min(model.actionRows.length - 1, model.paletteCursor + msg.delta));
    const moved = refreshActions(model, next >= 0 && next <= 65535 ? Math.trunc(next) : 0);
    return navigatorDecision(revealNavigator(moved, moved.paletteCursor * 52, 48), 0, NO_BYTES);
  }
  return null;
}

function navigatorTransition(incoming: Model, msg: Msg): NavigatorDecision | null {
  const departure = departureTransition(incoming, msg);
  if (departure !== null) return departure;
  if (msg.kind === "navigator_scrolled") return navigatorDecision({ ...incoming, navigatorScroll: msg.scroll.offsetY, navigatorViewport: msg.scroll.viewportExtentY }, 0, NO_BYTES);
  const settings = settingsTransition(incoming, msg);
  if (settings !== null) return settings;
  const creation = creationSurfaceTransition(incoming, msg);
  if (creation !== null) return creation;
  const destination = navigatorDestination(msg);
  if (destination >= 1 && destination <= 3) return openNavigator(incoming, destination);
  const machines = machinesTransition(incoming, msg);
  if (machines !== null) return machines;
  const commands = commandsTransition(incoming, msg);
  if (commands !== null) return commands;
  return catalogNavigationTransition(incoming, msg);
}

function creationSurfaceTransition(model: Model, msg: Msg): NavigatorDecision | null {
  const status = localToolStatusTransition(model, msg);
  if (status !== null) return status;
  const tool = localToolsTransition(model, msg);
  if (tool !== null) return tool;
  const session = newSessionTransition(model, msg);
  if (session !== null) return session;
  return remoteTransition(model, msg);
}

function navigationInputTransition(model: Model, msg: Msg): NavigatorDecision | null {
  switch (msg.kind) {
    case "palette_open":
    case "agents_open":
    case "palette_edit":
    case "palette_scope":
    case "palette_move":
    case "palette_previous":
    case "palette_next":
    case "palette_retry": return requestCatalogNavigation(model, changeNavigation(model, msg), true);
    default: return null;
  }
}

function requestCatalogNavigation(previous: Model, next: Model, commit: boolean): NavigatorDecision {
  if (next === previous) return { model: previous, effect: 0, request: NO_BYTES };
  if (!next.paletteLoading) return navigatorDecision(next, 0, NO_BYTES);
  const requesting: Model = commit ? { ...next, windowActionPending: false } : next;
  return navigatorDecision(requesting, commit ? 3 : 12, navigationRequestFor(requesting));
}

function pickWindowNavigation(model: Model, target: Uint8Array): NavigatorDecision {
  if (model.windowActionPending || model.windowActionId >= 4294967295) return navigatorDecision(model, 0, NO_BYTES);
  const id = model.windowActionId + 1;
  const nextId = id >= 0 && id <= 4294967295 ? Math.trunc(id) : 0;
  return navigatorDecision({ ...model, windowActionId: nextId, windowActionPending: true }, 10, windowCommand(nextId, target));
}

function pickCatalogNavigation(model: Model, msg: Msg): NavigatorDecision {
  if (model.agentsMode) {
    if (msg.kind === "palette_pick" || model.paletteRows.length === 0) return navigatorDecision(model, 0, NO_BYTES);
    const parent = model.paletteRows[model.paletteCursor].index;
    if (parent === 65535) return navigatorDecision(model, 0, NO_BYTES);
    return navigatorDecision(closePalette(model), 21, navigationIntent(model.engineRevision, parent));
  }
  const target = navigationTarget(model, msg);
  if (target.length === 0) return navigatorDecision(model, 0, NO_BYTES);
  if (windowTarget(target)) return pickWindowNavigation(model, target);
  const host = navigationHostFilter(target);
  if (host !== null) return requestCatalogNavigation(model, hostNavigation(model, host), false);
  const decision = enqueueCatalogCommand(model.tabCommands, target);
  const next = freshCommandModel(model, decision);
  if (decision.state.outcome !== 1) return navigatorDecision(next, 0, NO_BYTES);
  return navigatorDecision(closePalette(next), decision.request.length === 0 ? 1 : 11, decision.request);
}

function receivedWindowAction(model: Model, body: Uint8Array): NavigatorDecision {
  if (!model.windowActionPending || !model.paletteOpen || model.navigatorView !== 3) return { model, effect: 0, request: NO_BYTES };
  const receipt = windowReceipt(body, model.windowActionId);
  if (receipt === 0) return navigatorDecision(model, 0, NO_BYTES);
  if (receipt === 1) return navigatorDecision(closePalette({ ...model, windowActionPending: false }), 1, NO_BYTES);
  return navigatorDecision({ ...model, windowActionPending: false, paletteNotice: asciiBytes("That window or tab is no longer available. Refresh to choose existing work.") }, 0, NO_BYTES);
}

function failedWindowAction(model: Model): NavigatorDecision {
  if (!model.windowActionPending) return { model, effect: 0, request: NO_BYTES };
  return navigatorDecision({ ...model, windowActionPending: false, paletteNotice: asciiBytes("Could not bring that window forward. Refresh and try again.") }, 0, NO_BYTES);
}

function navigationReplyTransition(model: Model, msg: Msg): NavigatorDecision | null {
  switch (msg.kind) {
    case "navigation_loaded": return requestCatalogNavigation(model, receiveNavigation(model, msg.body), false);
    case "window_action_loaded": return receivedWindowAction(model, msg.body);
    case "window_action_failed": return failedWindowAction(model);
    case "navigation_failed": {
      const next = failedAgentNavigation(model);
      if (next.paletteLoading) return requestCatalogNavigation(model, next, false);
      return navigatorDecision(next, 0, NO_BYTES);
    }
    default: return null;
  }
}

function catalogNavigationTransition(incoming: Model, msg: Msg): NavigatorDecision | null {
  // Existing modal reducers own their Escape, field and arrow events.
  if (incoming.settingsOpen) return null;
  const model = displaceRename(displaceDirectory(incoming, msg), msg);
  if (model.dirOpen || model.renameOpen) return null;
  if (msg.kind === "agent_parent") {
    if (!validAgentParent(model, msg.index)) return navigatorDecision(model, 0, NO_BYTES);
    return navigatorDecision(model, 20, navigationIntent(model.engineRevision, msg.index));
  }
  if (msg.kind === "palette_submit" || msg.kind === "palette_pick") return pickCatalogNavigation(model, msg);
  if (msg.kind === "palette_close") return navigatorDecision(closePalette({ ...model, hostOpen: false, hostAwaiting: false }), 14, NO_BYTES);
  const input = navigationInputTransition(model, msg);
  return input === null ? navigationReplyTransition(model, msg) : input;
}

function editSettingsSearch(model: Model, edit: TextInputEvent): Model {
  const state: TextEditState = { text: model.settingsQuery, selection: { anchor: model.settingsAnchor, focus: model.settingsFocus }, composition: null };
  const next = applyTextInputEvent(state, edit, 64);
  if (next === null) return model;
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 64 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 64 ? Math.trunc(next.selection.focus) : 0;
  return { ...model, settingsQuery: next.text, settingsAnchor: anchor, settingsFocus: focus,
    bindingRows: filteredBindings(model.bindings, next.text),
    settingRows: settingsRows(model.appearance, next.text, model.settingsSection) };
}

function selectSetting(model: Model, id: number): Model {
  for (const row of model.settingRows) {
    if (row.id !== id || !row.editable) continue;
    const selected = id >= 0 && id <= 10 ? Math.trunc(id) : 65535;
    const length = row.value.length;
    const end = length >= 0 && length <= 1024 ? Math.trunc(length) : 0;
    return { ...model, settingEditId: selected, settingEditValue: row.value, settingAnchor: 0, settingFocus: end };
  }
  return model;
}

function editSettingValue(model: Model, edit: TextInputEvent): Model {
  const state: TextEditState = { text: model.settingEditValue, selection: { anchor: model.settingAnchor, focus: model.settingFocus }, composition: null };
  const next = applyTextInputEvent(state, edit, 1024);
  if (next === null) return model;
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 1024 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 1024 ? Math.trunc(next.selection.focus) : 0;
  return { ...model, settingEditValue: next.text, settingAnchor: anchor, settingFocus: focus };
}

function chooseSettingsSection(model: Model, section: number): NavigatorDecision {
  if (!(section >= 0 && section <= 4)) return navigatorDecision(model, 0, NO_BYTES);
  const selected = Math.trunc(section);
  const next = { ...model, settingsSection: selected, settingEditId: 65535,
    settingRows: settingsRows(model.appearance, model.settingsQuery, selected) };
  if (selected === 2) return navigatorDecision({ ...next, appearanceBusy: true }, 4, keybindingRequest(0, 0, NO_BYTES));
  return navigatorDecision(next, 0, NO_BYTES);
}

function reloadSettings(model: Model): NavigatorDecision {
  if (model.appearanceBusy) return navigatorDecision(model, 0, NO_BYTES);
  if (model.appearance.dirty) return navigatorDecision({ ...model, settingsNotice: asciiBytes("Save or cancel the preview before reloading the configuration file.") }, 0, NO_BYTES);
  return navigatorDecision({ ...model, appearanceBusy: true, settingsReloadStage: 1 }, 7, appearanceRequest(6, 0));
}

function settingControlTransition(model: Model, msg: Msg): NavigatorDecision | null {
  switch (msg.kind) {
    case "settings_apply": {
      if (model.appearanceBusy || model.settingEditId > 10) return navigatorDecision(model, 0, NO_BYTES);
      return navigatorDecision({ ...model, appearanceBusy: true }, 7, settingRequest(model.settingEditId, model.settingEditValue));
    }
    case "settings_reset": {
      if (model.appearanceBusy || msg.id > 10) return navigatorDecision(model, 0, NO_BYTES);
      return navigatorDecision({ ...model, appearanceBusy: true }, 7, resetSettingRequest(msg.id));
    }
    case "settings_reload": return reloadSettings(model);
    default: return null;
  }
}

function selectBinding(model: Model, index: number): Model {
  for (const row of model.bindings.rows) {
    if (row.index !== index) continue;
    const selected = index >= 0 && index <= 191 ? Math.trunc(index) : 65535;
    const length = row.binding.length;
    const end = length >= 0 && length <= 64 ? Math.trunc(length) : 0;
    return { ...model, bindingEditIndex: selected, bindingEditValue: row.binding, bindingAnchor: 0, bindingFocus: end };
  }
  return model;
}

function editBinding(model: Model, edit: TextInputEvent): Model {
  const state: TextEditState = { text: model.bindingEditValue, selection: { anchor: model.bindingAnchor, focus: model.bindingFocus }, composition: null };
  const next = applyTextInputEvent(state, edit, 64);
  if (next === null) return model;
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 64 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 64 ? Math.trunc(next.selection.focus) : 0;
  return { ...model, bindingEditValue: next.text, bindingAnchor: anchor, bindingFocus: focus };
}

function bindingTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (model.appearanceBusy) return null;
  switch (msg.kind) {
    case "binding_select": return navigatorDecision(selectBinding(model, msg.index), 0, NO_BYTES);
    case "binding_edit": return navigatorDecision(editBinding(model, msg.edit), 0, NO_BYTES);
    case "binding_apply": {
      if (model.bindingEditIndex > 191) return navigatorDecision(model, 0, NO_BYTES);
      return navigatorDecision({ ...model, appearanceBusy: true }, 4, keybindingRequest(1, model.bindingEditIndex, model.bindingEditValue));
    }
    case "binding_reset": return navigatorDecision({ ...model, appearanceBusy: true }, 4, keybindingRequest(2, msg.index, NO_BYTES));
    default: return null;
  }
}

function settingsTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (!model.settingsOpen) return null;
  switch (msg.kind) {
    case "settings_query": return navigatorDecision(editSettingsSearch(model, msg.edit), 0, NO_BYTES);
    case "settings_select": return navigatorDecision(selectSetting(model, msg.id), 0, NO_BYTES);
    case "settings_value": return navigatorDecision(editSettingValue(model, msg.edit), 0, NO_BYTES);
    case "settings_section": return chooseSettingsSection(model, msg.section);
    default: return settingControlTransition(model, msg) ?? bindingTransition(model, msg);
  }
}

function remoteOperation(model: Model, operation: number, target: Uint8Array, notice: Uint8Array): NavigatorDecision {
  return navigatorDecision({ ...model, hostBusy: true, hostAwaiting: true, hostNotice: notice }, 8, remoteRequest(operation, target));
}

function connectRemote(model: Model): NavigatorDecision {
  if (model.hostQuery.length === 0) return navigatorDecision({ ...model, hostNotice: asciiBytes("Enter a registered host, e.g. mini or me@mini") }, 0, NO_BYTES);
  return remoteOperation(model, REMOTE_KIND_CONNECT, model.hostQuery, joinBytes(asciiBytes("Connecting to "), model.hostQuery, asciiBytes("...")));
}

function disconnectRemote(model: Model): NavigatorDecision {
  if (model.hostQuery.length === 0) return navigatorDecision({ ...model, hostNotice: asciiBytes("Enter the host to disconnect, or choose Disconnect All") }, 0, NO_BYTES);
  return remoteOperation(model, REMOTE_KIND_DISCONNECT, model.hostQuery, joinBytes(asciiBytes("Disconnecting "), model.hostQuery, asciiBytes("...")));
}

function remoteIntentTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (!model.hostOpen || model.hostBusy) return null;
  switch (msg.kind) {
    case "host_submit": return connectRemote(model);
    case "host_local": return remoteOperation(model, REMOTE_KIND_LOCAL, NO_BYTES, asciiBytes("Returning to this Mac..."));
    case "host_disconnect": return disconnectRemote(model);
    case "host_disconnect_all": return remoteOperation(model, REMOTE_KIND_DISCONNECT, NO_BYTES, asciiBytes("Disconnecting every remote host..."));
    default: return null;
  }
}

function remoteReplyTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (msg.kind === "remote_failed") return navigatorDecision({ ...model, hostBusy: false, hostAwaiting: false, hostNotice: asciiBytes("Connection status unavailable. Try again.") }, 0, NO_BYTES);
  if (msg.kind !== "remote_loaded") return null;
  const next = receiveRemote(model, msg.body);
  return navigatorDecision(next, model.hostOpen && !next.hostOpen ? 1 : 0, NO_BYTES);
}

function remoteTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (msg.kind === "host_open") {
    if (model.hostOpen) return navigatorDecision(model, 0, NO_BYTES);
    const displaced = displaceRename(displaceDirectory(model, msg), msg);
    return navigatorDecision(openHost({ ...displaced, toolPurpose: 0 }), 9, remoteRequest(REMOTE_KIND_STATUS, NO_BYTES));
  }
  if (msg.kind === "host_close") return navigatorDecision(scopeOverlays({ ...model, hostOpen: false, hostAwaiting: false }), model.hostOpen ? 1 : 0, NO_BYTES);
  if (msg.kind === "host_edit") return navigatorDecision(editHost(model, msg.edit), 0, NO_BYTES);
  const reply = remoteReplyTransition(model, msg);
  return reply === null ? remoteIntentTransition(model, msg) : reply;
}

function describeLocalTool(model: Model, purpose: number): NavigatorDecision {
  const next = scopeOverlays({ ...closePalette(model), settingsOpen: false, renameOpen: false, dirOpen: false,
    hostOpen: true, toolPurpose: purpose === 1 ? 1 : 2, toolToken: NO_BYTES,
    pendingToolOpen: false, hostBusy: true, hostAwaiting: false,
    hostNotice: asciiBytes("Checking local setup..."), hostQuery: purpose === 1 ? model.hostQuery : NO_BYTES });
  return navigatorDecision(next, 6, localToolRequest(1, NO_BYTES, NO_BYTES, NO_BYTES));
}

function launchLocalTool(model: Model): NavigatorDecision {
  if (model.toolLaunchPending || model.toolOperationId > 0) return navigatorDecision({ ...model, hostNotice: asciiBytes("Waiting for the previous local tool receipt. Its status is still being checked.") }, 0, NO_BYTES);
  if (model.hostBusy || model.toolToken.length !== 8) return navigatorDecision(model, 0, NO_BYTES);
  if (model.toolPurpose === 1 && model.hostQuery.length === 0) return navigatorDecision({ ...model, hostNotice: asciiBytes("Enter a hostname or SSH destination.") }, 0, NO_BYTES);
  return navigatorDecision({ ...model, hostBusy: true, toolLaunchPending: true, toolLaunchToken: model.toolToken.slice() }, 19,
    localToolLaunchRequest(model));
}

function localToolLaunchRequest(model: Model): Uint8Array {
  if (model.toolPurpose === 1) return localToolRequest(3, model.toolToken, model.hostQuery, model.hostFriendlyName);
  return localToolRequest(2, model.toolToken, NO_BYTES, NO_BYTES);
}

function receiveLocalTool(model: Model, body: Uint8Array): NavigatorDecision {
  const reply = localToolReply(body);
  if (reply === null) return navigatorDecision({ ...model, hostBusy: false, hostNotice: asciiBytes("Local setup status unavailable. Try again.") }, 0, NO_BYTES);
  if (model.toolToken.length > 0 && !sameBytes(model.toolToken, reply.token)) return navigatorDecision(model, 0, NO_BYTES);
  const next = { ...model, hostBusy: false, toolToken: reply.token, toolTarget: reply.target, hostNotice: reply.message };
  if (reply.phase === 0 && model.toolPurpose === 2) return launchLocalTool(next);
  return navigatorDecision(next, 0, NO_BYTES);
}

function ownsToolDialog(model: Model, token: Uint8Array): boolean {
  return model.hostOpen && sameBytes(model.toolToken, token);
}

function receiveLocalToolAdmission(model: Model, body: Uint8Array): NavigatorDecision {
  if (!model.toolLaunchPending) return { model, effect: 0, request: NO_BYTES };
  const reply = localToolReply(body);
  if (reply === null) return failedLocalToolAdmission(model);
  if (!sameBytes(reply.token, model.toolLaunchToken)) return { model, effect: 0, request: NO_BYTES };
  const next: Model = { ...model, toolLaunchPending: false };
  if (reply.phase === 1) return queuedLocalTool(next, reply.operation, reply.token);
  return rejectedLocalToolAdmission(next, reply.message);
}

function rejectedLocalToolAdmission(model: Model, message: Uint8Array): NavigatorDecision {
  if (ownsToolDialog(model, model.toolLaunchToken)) return navigatorDecision({ ...model, hostBusy: false, hostNotice: message }, 0, NO_BYTES);
  return navigatorDecision({ ...model, commandNotice: joinBytes(asciiBytes("Local tool was not admitted. Check This Mac before retrying. "), message, NO_BYTES) }, 0, NO_BYTES);
}

function failedLocalToolAdmission(model: Model): NavigatorDecision {
  if (!model.toolLaunchPending) return { model, effect: 0, request: NO_BYTES };
  return rejectedLocalToolAdmission({ ...model, toolLaunchPending: false }, asciiBytes("Could not confirm local launch. Check This Mac before trying again."));
}

function queuedLocalTool(model: Model, operation: number, token: Uint8Array): NavigatorDecision {
  if (!(operation > 0 && operation <= 4294967295)) return rejectedLocalToolAdmission(model,
    asciiBytes("Local launch returned no operation receipt. Check This Mac before trying again."));
  const owned = ownsToolDialog(model, token);
  const next = scopeOverlays({ ...model, hostOpen: owned ? false : model.hostOpen, hostBusy: owned ? false : model.hostBusy,
    toolQueued: true, toolStatusBusy: true,
    toolOperationId: Math.trunc(operation), toolOperationToken: token,
    commandNotice: TOOL_QUEUED_NOTICE });
  return navigatorDecision(next, 15, localToolRequest(4, token, NO_BYTES, NO_BYTES));
}

function pollLocalTool(model: Model): NavigatorDecision {
  if (model.toolOperationId === 0 || model.toolStatusBusy) return navigatorDecision(model, 0, NO_BYTES);
  const kind = model.toolQueued ? 4 : 5;
  return navigatorDecision({ ...model, toolStatusBusy: true }, model.toolQueued ? 17 : 18,
    localToolRequest(kind, model.toolOperationToken, NO_BYTES, NO_BYTES));
}

function receiveLocalToolStatus(model: Model, body: Uint8Array): NavigatorDecision {
  if (!model.toolQueued) return { model, effect: 0, request: NO_BYTES };
  const reply = localToolReply(body);
  if (reply === null) return failedLocalToolStatus(model);
  if (reply.operation !== model.toolOperationId || !sameBytes(reply.token, model.toolOperationToken)) return { model, effect: 0, request: NO_BYTES };
  if (reply.phase === 1) return navigatorDecision({ ...model, toolStatusBusy: false }, 16, NO_BYTES);
  if (!terminalLocalToolPhase(reply.phase)) return failedLocalToolStatus(model);
  const notice = localToolOutcomeNotice(model, reply.phase, reply.message);
  return navigatorDecision({ ...model, toolQueued: false, toolStatusBusy: true, commandNotice: notice }, 18,
    localToolRequest(5, model.toolOperationToken, NO_BYTES, NO_BYTES));
}

function localToolOutcomeNotice(model: Model, phase: number, message: Uint8Array): Uint8Array {
  if (phase === 4) return pendingLocalToolNotice(model.commandNotice) ? NO_BYTES : model.commandNotice;
  const prefix = phase === 5 ? asciiBytes("Local tool outcome unknown. Check This Mac before retrying. ") : asciiBytes("Could not place the local tool. Check This Mac and retry explicitly. ");
  return joinBytes(prefix, message, NO_BYTES);
}

function terminalLocalToolPhase(phase: number): boolean {
  return phase === 2 || phase === 4 || phase === 5;
}

function pendingLocalToolNotice(notice: Uint8Array): boolean {
  return sameBytes(notice, TOOL_QUEUED_NOTICE) || sameBytes(notice, TOOL_STATUS_NOTICE);
}

function failedLocalToolStatus(model: Model): NavigatorDecision {
  if (model.toolOperationId === 0) return { model, effect: 0, request: NO_BYTES };
  return navigatorDecision({ ...model, toolStatusBusy: false,
    commandNotice: model.toolQueued ? TOOL_STATUS_NOTICE : model.commandNotice }, 16, NO_BYTES);
}

function acknowledgeLocalTool(model: Model, body: Uint8Array): NavigatorDecision {
  const reply = localToolReply(body);
  if (reply === null) return failedLocalToolStatus(model);
  if (model.toolQueued || reply.operation !== model.toolOperationId || !sameBytes(reply.token, model.toolOperationToken)) return { model, effect: 0, request: NO_BYTES };
  if (!terminalLocalToolPhase(reply.phase)) return failedLocalToolStatus(model);
  return navigatorDecision({ ...model, toolStatusBusy: false, toolOperationId: 0, toolOperationToken: NO_BYTES }, 0, NO_BYTES);
}

function localToolStatusTransition(model: Model, msg: Msg): NavigatorDecision | null {
  switch (msg.kind) {
    case "local_tool_launch_loaded": return receiveLocalToolAdmission(model, msg.body);
    case "local_tool_launch_failed": return failedLocalToolAdmission(model);
    case "tool_status_tick": return pollLocalTool(model);
    case "local_tool_status_loaded": return receiveLocalToolStatus(model, msg.body);
    case "local_tool_acknowledged": return acknowledgeLocalTool(model, msg.body);
    case "local_tool_status_failed":
    case "local_tool_ack_failed": return failedLocalToolStatus(model);
    default: return null;
  }
}

function editFriendlyName(model: Model, edit: TextInputEvent): Model {
  const state: TextEditState = { text: model.hostFriendlyName, selection: { anchor: model.friendlyAnchor, focus: model.friendlyFocus }, composition: null };
  const next = applyTextInputEvent(state, edit, 128);
  if (next === null) return model;
  const anchor = next.selection.anchor >= 0 && next.selection.anchor <= 128 ? Math.trunc(next.selection.anchor) : 0;
  const focus = next.selection.focus >= 0 && next.selection.focus <= 128 ? Math.trunc(next.selection.focus) : 0;
  return { ...model, hostFriendlyName: next.text, friendlyAnchor: anchor, friendlyFocus: focus };
}

function configEditorTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (msg.kind === "settings_cancel_edit") return navigatorDecision({ ...model, configEditorConfirm: false, pendingToolOpen: false }, 0, NO_BYTES);
  if (model.settingsOpen && model.appearanceBusy) return null;
  if (msg.kind === "settings_save_edit") return navigatorDecision({ ...model, configEditorConfirm: false, pendingToolOpen: true, appearanceBusy: true }, 7, appearanceRequest(7, 0));
  if (msg.kind === "settings_discard_edit") return navigatorDecision({ ...model, configEditorConfirm: false, pendingToolOpen: true, appearanceBusy: true }, 7, appearanceRequest(6, 0));
  if (msg.kind !== "config_edit" && msg.kind !== "settings_edit_configuration") return null;
  if (!model.settingsOpen) return describeLocalTool(model, 2);
  if (model.appearance.dirty) return navigatorDecision({ ...model, configEditorConfirm: true }, 0, NO_BYTES);
  return navigatorDecision({ ...model, pendingToolOpen: true, appearanceBusy: true }, 7, appearanceRequest(6, 0));
}

function localToolsTransition(model: Model, msg: Msg): NavigatorDecision | null {
  const editor = configEditorTransition(model, msg);
  if (editor !== null) return editor;
  if (msg.kind === "add_machine_open") return describeLocalTool(model, 1);
  if (msg.kind === "tool_recheck") return openNavigator(model, 2);
  if (!model.hostOpen || model.toolPurpose === 0) return null;
  const reply = localToolReplyTransition(model, msg);
  if (reply !== null) return reply;
  switch (msg.kind) {
    case "host_name_edit": return navigatorDecision(editFriendlyName(model, msg.edit), 0, NO_BYTES);
    case "tool_submit":
    case "host_submit": return launchLocalTool(model);
    default: return null;
  }
}

function localToolReplyTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (msg.kind === "local_tool_loaded") return receiveLocalTool(model, msg.body);
  if (msg.kind === "local_tool_failed") return navigatorDecision({ ...model, hostBusy: false, hostNotice: asciiBytes("Could not open the local setup terminal. Retry or check your local Phux installation.") }, 0, NO_BYTES);
  return null;
}

function describeNewSession(model: Model): NavigatorDecision {
  const next = scopeOverlays({ ...closePalette(model), hostOpen: false, dirOpen: false,
    renameOpen: true, creatingSession: true, renameBusy: true, renameAwaiting: false,
    renameQuery: NO_BYTES, renameAnchor: 0, renameFocus: 0, newSessionToken: NO_BYTES,
    newSessionAwaiting: false, renameTitle: asciiBytes("New Session"), renameNotice: asciiBytes("Checking destination...") });
  return navigatorDecision(next, 5, newSessionRequest(1, NO_BYTES, NO_BYTES));
}

function receiveNewSession(model: Model, body: Uint8Array): NavigatorDecision {
  const reply = newSessionReply(body);
  if (reply === null) return navigatorDecision({ ...model, renameBusy: false, newSessionAwaiting: false, renameNotice: asciiBytes("Session creation status unavailable. Try again.") }, 0, NO_BYTES);
  if (sameBytes(model.retiredSessionToken, reply.token)) return navigatorDecision(model, 0, NO_BYTES);
  if (model.newSessionToken.length > 0 && !sameBytes(model.newSessionToken, reply.token)) return navigatorDecision(model, 0, NO_BYTES);
  const next = { ...model, newSessionToken: reply.token, renameBusy: reply.phase === 1, newSessionAwaiting: reply.phase === 1,
    renameTitle: joinBytes(asciiBytes("New Session on "), reply.host, NO_BYTES), renameNotice: reply.reason };
  if (reply.phase === 2) return navigatorDecision(scopeOverlays({ ...next, renameOpen: false, creatingSession: false }), 1, NO_BYTES);
  return navigatorDecision(next, 0, NO_BYTES);
}

function submitNewSession(model: Model): NavigatorDecision {
  if (model.renameBusy || model.newSessionToken.length !== 8) return navigatorDecision(model, 0, NO_BYTES);
  if (model.renameQuery.length === 0) return navigatorDecision({ ...model, renameNotice: asciiBytes("Name the work you want to return to.") }, 0, NO_BYTES);
  return navigatorDecision({ ...model, renameBusy: true, newSessionAwaiting: true }, 5,
    newSessionRequest(2, model.newSessionToken, model.renameQuery));
}

function newSessionTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (msg.kind === "new_session_open") return describeNewSession(model);
  if (!model.renameOpen || !model.creatingSession) return null;
  const reply = newSessionReplyTransition(model, msg);
  if (reply !== null) return reply;
  switch (msg.kind) {
    case "rename_edit": return navigatorDecision(editRename(model, msg.edit), 0, NO_BYTES);
    case "rename_submit": return submitNewSession(model);
    case "palette_move": return navigatorDecision(model, 0, NO_BYTES);
    case "rename_close":
    case "palette_close": return cancelSessionForAction(model, { kind: "engine_wake" });
    default: return null;
  }
}

function newSessionReplyTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (msg.kind === "new_session_loaded") return receiveNewSession(model, msg.body);
  if (msg.kind === "new_session_failed") return navigatorDecision({ ...model, renameBusy: false, newSessionAwaiting: false, renameNotice: asciiBytes("Could not confirm session creation. Refresh Sessions before trying again.") }, 0, NO_BYTES);
  return null;
}

function openingSurface(msg: Msg): number {
  const view = navigatorDestination(msg);
  if (view >= 1 && view <= 3) return view + 1;
  let index = 1;
  for (const kind of ["palette_open", "sessions_open", "machines_open", "windows_open", "commands_open", "new_session_open", "add_machine_open", "dir_open", "rename_open", "host_open"]) {
    if (msg.kind === kind) return index;
    index += 1;
  }
  return 0;
}

/// The row dimensions match the native list declarations (40 terminals,
/// 48+4 Commands, and MachineRow.height+4). The viewport is measured by SDK.
function revealNavigator(model: Model, top: number, height: number): Model {
  if (model.navigatorViewport <= 0) return model;
  if (top < model.navigatorScroll) return navigatorOffset(model, top);
  const bottom = top + height - model.navigatorViewport;
  if (bottom > model.navigatorScroll) return navigatorOffset(model, bottom);
  return model;
}

function navigatorOffset(model: Model, offset: number): Model {
  if (!(offset >= 0 && offset <= 9007199254740991)) return model;
  return { ...model, navigatorScroll: Math.trunc(offset) };
}

function moveMachineSelection(model: Model, delta: number): Model {
  const next = { ...model, machines: moveMachine(model.machines, delta, model.paletteQuery) };
  let top = 0;
  for (const row of next.machines.visible) {
    if (row.highlighted) return revealNavigator(next, top, row.height);
    top += row.height + 4;
  }
  return next;
}

function surfaceMessage(surface: number): Msg {
  if (surface === 1) return { kind: "palette_open" };
  if (surface === 2) return { kind: "sessions_open" };
  if (surface === 3) return { kind: "machines_open" };
  if (surface === 4) return { kind: "windows_open" };
  if (surface === 5) return { kind: "commands_open" };
  return toolSurfaceMessage(surface);
}

function toolSurfaceMessage(surface: number): Msg {
  if (surface === 6) return { kind: "new_session_open" };
  if (surface === 7) return { kind: "add_machine_open" };
  if (surface === 8) return { kind: "dir_open" };
  if (surface === 9) return { kind: "rename_open" };
  return { kind: "host_open" };
}

interface PreparedMessage { readonly model: Model; readonly msg: Msg; }
export interface DeferredAction {
  readonly code: number;
  readonly argument: number;
  readonly target: Uint8Array;
  readonly revision: WireU64;
  readonly window: number;
  readonly contextTarget: Uint8Array;
}

function deferredActionCode(msg: Msg): number {
  const surface = openingSurface(msg);
  if (surface > 0) return surface;
  let code = 11;
  for (const kind of ["settings_open", "config_edit", "new_window", "window_closed", "new_terminal", "select_target", "select_tab", "select_active_tab", "select_slot", "palette_pick", "native_command", "close_selected_tab"]) {
    if (msg.kind === kind) return code;
    code += 1;
  }
  return 0;
}

function deferredArgument(msg: Msg): number {
  if (msg.kind === "window_closed") return msg.window;
  if (msg.kind === "select_tab" || msg.kind === "select_active_tab") return msg.index;
  if (msg.kind === "select_slot") return msg.slot;
  if (msg.kind === "native_command") return msg.command;
  return 0;
}

function captureDeferredAction(model: Model, msg: Msg): DeferredAction {
  const rawCode = deferredActionCode(msg);
  const rawArgument = deferredArgument(msg);
  const code = rawCode >= 0 && rawCode <= 22 ? Math.trunc(rawCode) : 0;
  const argument = rawArgument >= 0 && rawArgument <= 65535 ? Math.trunc(rawArgument) : 0;
  const target = msg.kind === "select_target" || msg.kind === "palette_pick" ? msg.target.slice() : NO_BYTES;
  return { code, argument, target, revision: model.engineRevision, window: model.activeWindow,
    contextTarget: focusedTabTarget(model).slice() };
}

function deferredContextRequired(action: DeferredAction): boolean {
  if (action.code <= 5) return false;
  return action.code !== 11;
}

function deferredContextCurrent(model: Model, action: DeferredAction): boolean {
  if (!sameU64(model.engineRevision, action.revision)) return false;
  if (model.activeWindow !== action.window) return false;
  return sameBytes(focusedTabTarget(model), action.contextTarget);
}

function resumeDeferredAction(model: Model, action: DeferredAction): PreparedMessage {
  if (deferredContextRequired(action) && !deferredContextCurrent(model, action)) return {
    model: { ...model, commandNotice: asciiBytes("The command context changed while closing the dialog. Reopen the command in the intended window and try again.") },
    msg: { kind: "context_refused" },
  };
  return { model, msg: deferredActionMessage(action) };
}

function deferredActionMessage(action: DeferredAction): Msg {
  if (action.code === 0) return { kind: "engine_wake" };
  if (action.code <= 10) return surfaceMessage(action.code);
  if (action.code === 11) return { kind: "settings_open" };
  if (action.code === 12) return { kind: "config_edit" };
  if (action.code === 13) return { kind: "new_window" };
  if (action.code === 15) return { kind: "new_terminal" };
  return deferredTargetMessage(action);
}

function deferredTargetMessage(action: DeferredAction): Msg {
  const raw = action.argument;
  const argument = raw >= 0 && raw <= 65535 ? Math.trunc(raw) : 0;
  switch (action.code) {
    case 14: return { kind: "window_closed", window: argument };
    case 16: return { kind: "select_target", target: action.target };
    case 17: return { kind: "select_tab", index: argument };
    case 18: return { kind: "select_active_tab", index: argument };
    case 19: return { kind: "select_slot", slot: argument };
    case 20: return { kind: "palette_pick", target: action.target };
    case 21: return { kind: "native_command", command: argument };
    default: return { kind: "close_selected_tab" };
  }
}

function sessionDisplacingMessage(msg: Msg): boolean {
  return deferredActionCode(msg) > 0;
}

function settingsDisplacingMessage(msg: Msg): boolean {
  if (openingSurface(msg) > 0) return true;
  return msg.kind === "new_window" || msg.kind === "window_closed";
}

function departureTransition(model: Model, msg: Msg): NavigatorDecision | null {
  if (msg.kind === "context_refused") return navigatorDecision(model, 1, NO_BYTES);
  if (model.pendingSessionAction !== null) return waitingSessionDeparture(model, msg);
  if (model.creatingSession && sessionDisplacingMessage(msg)) return cancelSessionForAction(model, msg);
  if (model.settingsOpen && settingsDisplacingMessage(msg)) return cancelSettingsForAction(model, msg);
  return null;
}

function cancelSessionForAction(model: Model, msg: Msg): NavigatorDecision {
  const next = scopeOverlays({ ...model, renameOpen: false, creatingSession: false, newSessionAwaiting: false,
    pendingSessionAction: captureDeferredAction(model, msg), retiredSessionToken: model.newSessionToken });
  if (model.newSessionToken.length !== 8) return navigatorDecision(next, 1, NO_BYTES);
  return navigatorDecision(next, 13, newSessionRequest(4, model.newSessionToken, NO_BYTES));
}

function waitingSessionDeparture(model: Model, msg: Msg): NavigatorDecision | null {
  if (msg.kind === "new_session_cancelled" || msg.kind === "new_session_cancel_failed") return failedSessionCancellation(model);
  if (msg.kind === "palette_close" || msg.kind === "rename_close") return navigatorDecision({ ...model, pendingSessionAction: captureDeferredAction(model, { kind: "engine_wake" }) }, 1, NO_BYTES);
  if (sessionDisplacingMessage(msg)) return navigatorDecision({ ...model, pendingSessionAction: captureDeferredAction(model, msg) }, 0, NO_BYTES);
  if (msg.kind !== "new_session_loaded") return null;
  if (model.newSessionToken.length > 0) return { model, effect: 0, request: NO_BYTES };
  const reply = newSessionReply(msg.body);
  if (reply === null) return navigatorDecision(model, 0, NO_BYTES);
  return navigatorDecision({ ...model, newSessionToken: reply.token, retiredSessionToken: reply.token }, 13, newSessionRequest(4, reply.token, NO_BYTES));
}

function failedSessionCancellation(model: Model): NavigatorDecision {
  return navigatorDecision(scopeOverlays({ ...model, pendingSessionAction: null, renameOpen: true, creatingSession: true,
    renameBusy: false, newSessionAwaiting: false,
    renameNotice: asciiBytes("Could not confirm cancellation. Close New Session to retry before choosing other work.") }), 1, NO_BYTES);
}

function cancelSettingsForAction(model: Model, msg: Msg): NavigatorDecision {
  const next: Model = { ...model, pendingSettingsAction: captureDeferredAction(model, msg), settingsReloadStage: 0, pendingToolOpen: false,
    appearanceClosing: true, appearanceBusy: true, navigationAfterSettings: msg.kind === "palette_open" };
  return navigatorDecision(next, 7, appearanceRequest(6, 0));
}

function resumeSessionAction(model: Model, msg: Msg): PreparedMessage {
  if (!sessionCancellationDone(model, msg)) return { model, msg };
  const action = model.pendingSessionAction;
  const next: Model = { ...model, pendingSessionAction: null };
  return action === null ? { model: next, msg: { kind: "engine_wake" } } : resumeDeferredAction(next, action);
}

function sessionCancellationDone(model: Model, msg: Msg): boolean {
  if (msg.kind === "new_session_cancelled") return validSessionCancellation(model, msg.body);
  if (model.pendingSessionAction === null || model.newSessionToken.length > 0) return false;
  if (msg.kind === "new_session_failed") return true;
  return msg.kind === "new_session_loaded" && newSessionReply(msg.body) === null;
}

function validSessionCancellation(model: Model, body: Uint8Array): boolean {
  if (model.pendingSessionAction === null) return false;
  const reply = newSessionReply(body);
  return reply !== null && reply.phase === 0 && sameBytes(reply.token, model.retiredSessionToken);
}

function resumeSettingsAction(model: Model, msg: Msg): PreparedMessage {
  const action = model.pendingSettingsAction;
  if (action === null) return { model, msg };
  if (msg.kind !== "appearance_loaded" && msg.kind !== "appearance_failed") return { model, msg };
  const waiting = { ...model, navigationAfterSettings: false };
  const decision = msg.kind === "appearance_loaded" ? loadedAppearance(waiting, msg.body) : appearanceFailure(waiting);
  if (!decision.closed) return { model, msg };
  return resumeDeferredAction({ ...decision.model, pendingSettingsAction: null }, action);
}

function prepareContinuations(model: Model, msg: Msg): PreparedMessage {
  const session = resumeSessionAction(model, msg);
  const settings = resumeSettingsAction(session.model, session.msg);
  let next = settings.model;
  const action = settings.msg;
  if (openingSurface(action) > 0) next = { ...next, navigatorScroll: 0, windowActionPending: false };
  if (sessionDisplacingMessage(action)) next = { ...next, windowActionPending: false };
  if (action.kind === "palette_close") next = { ...next, windowActionPending: false };
  if (action.kind === "settings_close") next = { ...next, pendingToolOpen: false, pendingSettingsAction: null,
    navigationAfterSettings: false, settingsReloadStage: 0, surfaceAfterSettings: 0, configEditorConfirm: false };
  return { model: next, msg: action };
}

function commandDeparture(previous: Model, next: Model): boolean {
  if (previous.settingsOpen && !next.settingsOpen) return true;
  return next.paletteOpen && next.navigatorView === 4;
}

export function update(incoming: Model, msg: Msg): Model | [Model, Cmd<Msg>] {
  const prepared = prepareContinuations(incoming, msg);
  const fromCommands = commandDeparture(incoming, prepared.model);
  incoming = prepared.model;
  msg = prepared.msg;
  if (incoming.paletteOpen && incoming.navigatorView === 4) {
    const action = selectedAction(incoming, msg);
    if (action !== null) { incoming = closePalette(incoming); msg = action; }
    else if (msg.kind === "palette_submit" || msg.kind === "commands_pick") return { ...incoming, paletteNotice: asciiBytes("This command is unavailable in the captured context. Reopen Commands to use the current terminal.") };
  }
  const navigator = navigatorTransition(incoming, msg);
  if (navigator !== null) {
    if (navigator.effect === 1) return [navigator.model, Cmd.host("cockpit.committed", NO_BYTES)];
    if (navigator.effect === 2) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.machines", navigator.request, { key: "cockpit-machines", ok: "machines_loaded", err: "machines_failed" }),
    ])];
    if (navigator.effect === 3) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.navigation", navigator.request, { key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed" }),
      Cmd.cancel("cockpit-window-command"),
    ])];
    if (navigator.effect === 4) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.keybindings", navigator.request, { key: "cockpit-keybindings", ok: "keybindings_loaded", err: "keybindings_failed" }),
      Cmd.cancel("cockpit-window-command"),
    ])];
    if (navigator.effect === 5) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.new-session", navigator.request, { key: "cockpit-new-session", ok: "new_session_loaded", err: "new_session_failed" }),
    ])];
    if (navigator.effect === 6) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.local-tools", navigator.request, { key: "cockpit-local-tools", ok: "local_tool_loaded", err: "local_tool_failed" }),
    ])];
    if (navigator.effect === 7) return [navigator.model, Cmd.request("cockpit.appearance", navigator.request, { key: "cockpit-appearance", ok: "appearance_loaded", err: "appearance_failed" })];
    if (navigator.effect === 8) return [navigator.model, Cmd.request("cockpit.remote", navigator.request, { key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed" })];
    if (navigator.effect === 9) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.remote", navigator.request, { key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed" }),
    ])];
    if (navigator.effect === 10) return [navigator.model, Cmd.request("cockpit.window-command", navigator.request, { key: "cockpit-window-command", ok: "window_action_loaded", err: "window_action_failed" })];
    if (navigator.effect === 11) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.tab-command", navigator.request, { key: "cockpit-tab-command", ok: "tab_command_completed", err: "tab_command_failed" }),
    ])];
    if (navigator.effect === 12) return [navigator.model, Cmd.request("cockpit.navigation", navigator.request, { key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed" })];
    if (navigator.effect === 13) return [navigator.model, Cmd.batch([
      Cmd.cancel("cockpit-new-session"),
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.new-session", navigator.request, { key: "cockpit-new-session-cancel", ok: "new_session_cancelled", err: "new_session_cancel_failed" }),
    ])];
    if (navigator.effect === 14) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.cancel("cockpit-window-command"),
    ])];
    if (navigator.effect === 15) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.local-tools", navigator.request, { key: "cockpit-tool-status", ok: "local_tool_status_loaded", err: "local_tool_status_failed" }),
    ])];
    if (navigator.effect === 16) return [navigator.model, Cmd.delay("cockpit-tool-status-tick", 500, "tool_status_tick")];
    if (navigator.effect === 17) return [navigator.model, Cmd.request("cockpit.local-tools", navigator.request, { key: "cockpit-tool-status", ok: "local_tool_status_loaded", err: "local_tool_status_failed" })];
    if (navigator.effect === 18) return [navigator.model, Cmd.request("cockpit.local-tools", navigator.request, { key: "cockpit-tool-status", ok: "local_tool_acknowledged", err: "local_tool_ack_failed" })];
    if (navigator.effect === 19) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.local-tools", navigator.request, { key: "cockpit-tool-launch", ok: "local_tool_launch_loaded", err: "local_tool_launch_failed" }),
    ])];
    if (navigator.effect === 20) return [navigator.model, Cmd.host("cockpit.intent", navigator.request)];
    if (navigator.effect === 21) return [navigator.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.host("cockpit.intent", navigator.request),
    ])];
    return navigator.model;
  }
  // Go to Directory first: while it is open it owns Escape and the arrows.
  const directory = directoryTransition(incoming, msg);
  if (directory !== null) {
    const decided = directory.model;
    if (directory.request.length > 0 && directory.committed) return [decided, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.directory", directory.request, { key: "cockpit-directory", ok: "directory_loaded", err: "directory_failed" }),
    ])];
    if (directory.request.length > 0) {
      return [decided, Cmd.request("cockpit.directory", directory.request, { key: "cockpit-directory", ok: "directory_loaded", err: "directory_failed" })];
    }
    if (directory.committed) return [decided, Cmd.host("cockpit.committed", NO_BYTES)];
    return decided;
  }
  const undirected = displaceDirectory(incoming, msg);
  // Rename Session next: while it is open it owns Escape and its field.
  const rename = renameTransition(undirected, msg);
  if (rename !== null) {
    const decided = rename.model;
    if (rename.request.length > 0 && rename.committed) return [decided, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.session", rename.request, { key: "cockpit-session", ok: "session_loaded", err: "session_failed" }),
    ])];
    if (rename.request.length > 0) {
      return [decided, Cmd.request("cockpit.session", rename.request, { key: "cockpit-session", ok: "session_loaded", err: "session_failed" })];
    }
    if (rename.committed) return [decided, Cmd.host("cockpit.committed", NO_BYTES)];
    return decided;
  }
  const model = displaceRename(undirected, msg);
  const result = resultTransition(model, msg);
  if (result !== null) {
    if (result.request.length === 0) return result.model;
    return [result.model, Cmd.request("cockpit.command-results", result.request, {
      key: "cockpit-command-results", ok: "command_result_loaded", err: "command_result_failed",
    })];
  }
  const appearance = updateAppearance(model, msg);
  if (appearance !== null) {
    const next = appearance.model;
    if (appearance.closed && model.pendingToolOpen) {
      const editor = describeLocalTool(next, 2);
      return [editor.model, Cmd.batch([
        Cmd.host("cockpit.committed", NO_BYTES),
        Cmd.request("cockpit.local-tools", editor.request, { key: "cockpit-local-tools", ok: "local_tool_loaded", err: "local_tool_failed" }),
      ])];
    }
    if (appearance.opening) return [next, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.appearance", appearance.request, { key: "cockpit-appearance", ok: "appearance_loaded", err: "appearance_failed" }),
    ])];
    if (appearance.navigate) return [next, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.navigation", navigationRequestFor(next), {
        key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
      }),
    ])];
    if (appearance.closed) return [next, Cmd.host("cockpit.committed", NO_BYTES)];
    if (appearance.request.length === 0) return next;
    return [next, Cmd.request("cockpit.appearance", appearance.request, { key: "cockpit-appearance", ok: "appearance_loaded", err: "appearance_failed" })];
  }
  const command = tabCommandTransition(model, msg);
  if (command !== null) {
    if (command.request.length === 0) {
      if (fromCommands) return [command.model, Cmd.host("cockpit.committed", NO_BYTES)];
      return command.model;
    }
    if (fromCommands) return [command.model, Cmd.batch([
      Cmd.host("cockpit.committed", NO_BYTES),
      Cmd.request("cockpit.tab-command", command.request, { key: "cockpit-tab-command", ok: "tab_command_completed", err: "tab_command_failed" }),
    ])];
    return [command.model, Cmd.request("cockpit.tab-command", command.request, {
      key: "cockpit-tab-command", ok: "tab_command_completed", err: "tab_command_failed",
    })];
  }
  switch (msg.kind) {
    case "select_tab":
      return [
        {
          ...model,
          tabs: selectTab(model.tabs, msg.index),
          visibleTabs: selectTab(model.visibleTabs, msg.index),
          selectedTab: msg.index,
        },
        Cmd.host("cockpit.intent", intent(1, model.engineRevision, msg.index, 0)),
      ];
    case "select_active_tab":
      return [model, Cmd.host("cockpit.intent", intent(1, model.engineRevision, msg.index, 255))];
    case "select_slot": {
      const payload = legacySlotIntent(model.engineRevision, msg.slot);
      if (payload.length === 0) return model;
      return [model, Cmd.host("cockpit.intent", payload)];
    }
    case "reconnect":
      return [model, Cmd.host("cockpit.intent", intent(12, model.engineRevision, 0, 255))];
    case "empty_new_tab":
      if (model.emptyBusy || model.emptyWindows === 0) return model;
      return [{ ...model, emptyBusy: true, emptyNotice: asciiBytes("Opening a new tab...") },
        Cmd.request("cockpit.session", sessionRequest(SESSION_KIND_NEW_TAB, NO_BYTES), {
          key: "cockpit-session-empty", ok: "empty_loaded", err: "empty_failed",
        })];
    case "empty_dismiss":
      if (!model.emptyPicked) return model;
      return [model, Cmd.request("cockpit.session", sessionRequest(SESSION_KIND_DISMISS, NO_BYTES), {
        key: "cockpit-session-empty", ok: "empty_loaded", err: "empty_failed",
      })];
    case "empty_loaded":
      return receiveEmpty(model, msg.body);
    case "empty_failed":
      return { ...model, emptyBusy: false, emptyNotice: asciiBytes("Could not open a tab there. Try again.") };
    case "window_closed": {
      // Native retirement already matched the actual OS window incarnation.
      // Withdraw its declaration before the SDK rebuilds; this label never
      // authorizes a second lifecycle mutation against a recycled slot.
      return [forgetClosedWindow(model, msg.window), Cmd.request("cockpit.snapshot", NO_BYTES, {
        key: "cockpit-snapshot", ok: "snapshot_loaded", err: "snapshot_failed",
      })];
    }
    case "toggle_tab_placement": {
      const placement: TabPlacement = model.tabPlacement === "top" ? "side" : "top";
      if (fromCommands) return [{ ...model, tabPlacement: placement }, Cmd.batch([
        Cmd.host("cockpit.committed", NO_BYTES),
        Cmd.host("cockpit.intent", intent(4, model.engineRevision, placement === "side" ? 1 : 0, 255)),
      ])];
      return [
        { ...model, tabPlacement: placement },
        Cmd.host("cockpit.intent", intent(4, model.engineRevision, placement === "side" ? 1 : 0, 255)),
      ];
    }
    case "settings_reveal":
      if (!model.settingsOpen || !model.configExists) return model;
      return [model, Cmd.host("cockpit.intent", intent(6, model.engineRevision, 0, 0))];
    case "native_command":
      if (fromCommands) return [model, Cmd.batch([Cmd.host("cockpit.committed", NO_BYTES), Cmd.host("cockpit.intent", intent(11, model.engineRevision, msg.command, 255))])];
      return [model, Cmd.host("cockpit.intent", intent(11, model.engineRevision, msg.command, 255))];
    case "engine_wake":
      return { ...model };
    case "snapshot_loaded": {
      const projected = snapshot(msg.body);
      if (projected === null) {
        return engineUnavailable(model, asciiBytes("BAD SNAPSHOT"));
      }
      // Bits 0..4 are the engine model's own limit and write refusals; bit 7
      // is the seam's: the last intent named a revision the engine had left.
      const refusedMask = projected.flags & 159;
      const rawSelected = projected.selectedTab;
      if (!(rawSelected >= 0 && rawSelected <= 255)) {
        return engineUnavailable(model, asciiBytes("BAD SNAPSHOT"));
      }
      const selectedTab = Math.trunc(rawSelected);
      const rawWidth = projected.tabWidth;
      if (!(rawWidth >= 0 && rawWidth <= 65535)) {
        return engineUnavailable(model, asciiBytes("BAD SNAPSHOT"));
      }
      const tabWidth = Math.trunc(rawWidth);
      const hidden = projected.tabs.length - projected.runCount;
      const active = projected.activeTheme;
      const cursor = model.settingsOpen ? model.settingsCursor : active >= 0 && active <= 32 && active < projected.themes.length ? Math.trunc(active) : 0;
      const refused = (projected.flags & 8) !== 0;
      const connection = projected.connection >= 0 && projected.connection <= 255 ? Math.trunc(projected.connection) : 255;
      const mainTabs = stampSlots(projected.tabs, 0, projected.agents, connection);
      const mainVisible = sliceRun(mainTabs, projected.runStart, projected.runCount);
      const w1 = windowState(1, findSection(projected.secondary, 1), projected.agents, connection);
      const w2 = windowState(2, findSection(projected.secondary, 2), projected.agents, connection);
      const w3 = windowState(3, findSection(projected.secondary, 3), projected.agents, connection);
      const w4 = windowState(4, findSection(projected.secondary, 4), projected.agents, connection);
      // The width crosses a record into an integer slot; the proof is
      // restated at the boundary, once per slot.
      const width1 = w1.tabWidth >= 0 && w1.tabWidth <= 65535 ? Math.trunc(w1.tabWidth) : 168;
      const width2 = w2.tabWidth >= 0 && w2.tabWidth <= 65535 ? Math.trunc(w2.tabWidth) : 168;
      const width3 = w3.tabWidth >= 0 && w3.tabWidth <= 65535 ? Math.trunc(w3.tabWidth) : 168;
      const width4 = w4.tabWidth >= 0 && w4.tabWidth <= 65535 ? Math.trunc(w4.tabWidth) : 168;
      const rawEmpty = projected.emptySession.windows;
      const legacyEmpty = rawEmpty >= 0 && rawEmpty <= 31 ? Math.trunc(rawEmpty) : 0;
      const contexts = projected.windowContexts;
      const emptyMaskRaw = emptyMaskFromContexts(contexts, legacyEmpty);
      const emptyMask = emptyMaskRaw >= 0 && emptyMaskRaw <= 31 ? Math.trunc(emptyMaskRaw) : 0;
      const sessionLabel = projected.currentSession.length > 0 ? projected.currentSession : asciiBytes("Sessions");
      const hostLabel = projected.coordinatorEndpoint.length > 0 ? projected.coordinatorEndpoint : asciiBytes("Machine not yet known");
      const primaryRecord = findWindowContext(contexts.records, 0);
      const mainContext = windowChromeContext(true, 0, 1, contexts, sessionLabel, hostLabel, projected.emptySession);
      const window1Context = windowChromeContext(w1.open, 1, 2, contexts, sessionLabel, hostLabel, projected.emptySession);
      const window2Context = windowChromeContext(w2.open, 2, 4, contexts, sessionLabel, hostLabel, projected.emptySession);
      const window3Context = windowChromeContext(w3.open, 3, 8, contexts, sessionLabel, hostLabel, projected.emptySession);
      const window4Context = windowChromeContext(w4.open, 4, 16, contexts, sessionLabel, hostLabel, projected.emptySession);
      const refusedLocal = refusedMask !== 0;
      const primaryStatus = windowConnectionStatus(true, 0, contexts, projected.connection, projected.terminalStates[0], refusedLocal);
      const emptyPicked = contexts.present
        ? mainContext.emptyPicked || window1Context.emptyPicked || window2Context.emptyPicked || window3Context.emptyPicked || window4Context.emptyPicked
        : projected.emptySession.picked;
      const emptyOpening = contexts.present ? false : projected.emptySession.opening;
      const primaryHost = contexts.present
        ? (primaryRecord !== null && primaryRecord.host.length > 0 ? primaryRecord.host : asciiBytes("Machine not yet known"))
        : hostLabel;
      const synced: Model = {
        ...model,
        activeWindow: projected.activeWindow >= 0 && projected.activeWindow <= 4 ? Math.trunc(projected.activeWindow) : 0,
        window1Open: w1.open,
        window1Tabs: w1.visibleTabs,
        window1TabWidth: width1,
        window1HasOverflow: w1.hasOverflow,
        window1OverflowLabel: w1.overflowLabel,
        window2Open: w2.open,
        window2Tabs: w2.visibleTabs,
        window2TabWidth: width2,
        window2HasOverflow: w2.hasOverflow,
        window2OverflowLabel: w2.overflowLabel,
        window3Open: w3.open,
        window3Tabs: w3.visibleTabs,
        window3TabWidth: width3,
        window3HasOverflow: w3.hasOverflow,
        window3OverflowLabel: w3.overflowLabel,
        window4Open: w4.open,
        window4Tabs: w4.visibleTabs,
        window4TabWidth: width4,
        window4HasOverflow: w4.hasOverflow,
        window4OverflowLabel: w4.overflowLabel,
        themes: themeRows(projected.themes, active, cursor),
        settingsCursor: cursor,
        configExists: projected.configExists,
        configNotice: configNotice(projected.configEnabled, projected.configProbed, projected.configExists, projected.configWritable, refused, projected.configPath),
        tabs: mainTabs,
        visibleTabs: mainVisible,
        railRows: railRows(mainTabs),
        agentCountLabel: joinBytes(asciiBytes("Agents "), decimalBytes(projected.agentTotal), NO_BYTES),
        workspaceLabel: mainContext.title,
        mainContext,
        window1Context,
        window2Context,
        window3Context,
        window4Context,
        coordinatorEndpoint: projected.coordinatorEndpoint,
        machineLabel: primaryHost,
        connectionDetail: projected.connectionDetail,
        window1RailRows: railRows(w1.tabs),
        window2RailRows: railRows(w2.tabs),
        window3RailRows: railRows(w3.tabs),
        window4RailRows: railRows(w4.tabs),
        tabWidth,
        hasOverflow: hidden > 0,
        overflowLabel: hidden > 0 ? overflowLabel(hidden) : new Uint8Array(0),
        selectedTab,
        tabPlacement: projected.tabPlacement === 1 ? "side" : "top",
        engineConnected: true,
        engineSequence: projected.sequence,
        engineRevision: projected.revision,
        canReconnect: projected.connection === 3,
        lastConnection: projected.connection >= 0 && projected.connection <= 255 ? Math.trunc(projected.connection) : 255,
        connectionStatus: contexts.present && primaryRecord === null
          ? asciiBytes("Connection status unavailable")
          : remoteConnectionStatus(model, primaryStatus),
        window1Status: windowConnectionStatus(w1.open, 1, contexts, projected.connection, projected.terminalStates[1], refusedLocal),
        window2Status: windowConnectionStatus(w2.open, 2, contexts, projected.connection, projected.terminalStates[2], refusedLocal),
        window3Status: windowConnectionStatus(w3.open, 3, contexts, projected.connection, projected.terminalStates[3], refusedLocal),
        window4Status: windowConnectionStatus(w4.open, 4, contexts, projected.connection, projected.terminalStates[4], refusedLocal),
        status: refusedMask === 0 ? asciiBytes("READY") : asciiBytes("ACTION REFUSED"),
        emptyWindows: emptyMask,
        emptyName: projected.emptySession.name,
        emptyDetail: joinBytes(asciiBytes("Empty session on "), projected.emptySession.host, NO_BYTES),
        emptyPicked,
        emptyBusy: emptyMask !== 0 && (model.emptyBusy || emptyOpening),
        emptyNotice: emptyMask === 0 ? NO_BYTES : model.emptyNotice,
      };
      // An open Go to Directory names a listing on the connection that just
      // moved: withdraw its rows, and list again once connected.
      const directoryMoved = model.dirOpen && model.lastConnection !== 255 && projected.connection !== model.lastConnection;
      const directoryRelists = directoryMoved && projected.connection === 2;
      const scoped = retainMachineInvalidation(directoryMoved ? relistDirectory(scopeOverlays(synced), directoryRelists) : scopeOverlays(synced));
      const machinePoll = refreshMachineSnapshot(scoped);
      if (machinePoll !== null) return [machinePoll.model, Cmd.request("cockpit.machines", machinePoll.request, { key: "cockpit-machines", ok: "machines_loaded", err: "machines_failed" })];
      // Remote status is asked for only when the connection moved (or a
      // Connect to Host is waiting on it), never once per snapshot.
      const askRemote = projected.connection !== model.lastConnection || model.hostAwaiting;
      if (model.newSessionAwaiting) return [scoped, Cmd.request("cockpit.new-session", newSessionRequest(3, model.newSessionToken, NO_BYTES), {
        key: "cockpit-new-session", ok: "new_session_loaded", err: "new_session_failed",
      })];
      if (!model.paletteOpen || model.navigatorView === 2 || model.navigatorView === 4) {
        // A rename waits on its coordinator: each snapshot asks how it went.
        if (model.renameAwaiting && askRemote) return [scoped, Cmd.batch([
          Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_STATUS, NO_BYTES), {
            key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
          }),
          Cmd.request("cockpit.session", sessionRequest(SESSION_KIND_STATUS, NO_BYTES), {
            key: "cockpit-session", ok: "session_loaded", err: "session_failed",
          }),
        ])];
        if (model.renameAwaiting) return [scoped, Cmd.request("cockpit.session", sessionRequest(SESSION_KIND_STATUS, NO_BYTES), {
          key: "cockpit-session", ok: "session_loaded", err: "session_failed",
        })];
        if (!askRemote) return scoped;
        if (directoryRelists) return [scoped, Cmd.batch([
          Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_STATUS, NO_BYTES), {
            key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
          }),
          Cmd.request("cockpit.directory", directoryRequest(DIR_KIND_OPEN, NO_DIRECTORY_REQUEST, 0, 0, NO_BYTES), {
            key: "cockpit-directory", ok: "directory_loaded", err: "directory_failed",
          }),
        ])];
        return [scoped, Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_STATUS, NO_BYTES), {
          key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
        })];
      }
      if (!askRemote) {
        const refreshed = refreshNavigation(scoped);
        return [refreshed, Cmd.request("cockpit.navigation", navigationRequestFor(refreshed), {
          key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
        })];
      }
      return [refreshNavigation(scoped), Cmd.batch([
        Cmd.request("cockpit.navigation", navigationRequestFor(refreshNavigation(scoped)), {
          key: "cockpit-navigation", ok: "navigation_loaded", err: "navigation_failed",
        }),
        Cmd.request("cockpit.remote", remoteRequest(REMOTE_KIND_STATUS, NO_BYTES), {
          key: "cockpit-remote", ok: "remote_loaded", err: "remote_failed",
        }),
      ])];    }
    case "snapshot_failed":
      return engineUnavailable(model, asciiBytes("ENGINE UNAVAILABLE"));
    case "engine_event": {
      if (msg.state !== "data") {
        return engineUnavailable(model, msg.state === "rejected" ? asciiBytes("ENGINE REFUSED") : asciiBytes("ENGINE CLOSED"));
      }
      const event = invalidation(msg.bytes);
      if (event === null) return { ...model, status: asciiBytes("ENGINE PROTOCOL ERROR") };
      const read = requestCommandResults(model.commandResults);
      const next = {
        ...withdrawAgentRows(model),
        commandResults: read.state,
        engineSequence: event.sequence,
        engineConnected: false,
        status: asciiBytes("SYNCING"),
        // Agent inspection drops the prior page; everyday navigator keeps held
        // painted targets across the fence until the refreshed page lands.
        paletteRows: model.agentsMode ? NO_ROWS : model.paletteRows,
        paletteLoading: model.paletteOpen && (model.agentsMode || (model.navigatorView !== 2 && model.navigatorView !== 4)),
        palettePrevious: model.agentsMode ? false : model.palettePrevious,
        paletteNext: model.agentsMode ? false : model.paletteNext,
        paletteNotice: model.agentsMode ? asciiBytes("Updating workspace and agent state...") : model.paletteNotice,
      };
      // A listing Go to Directory waits on settles in the provider drain
      // that announced this invalidation; ask for its page with the snapshot.
      const pollDirectory = model.dirOpen && model.dirAwaiting;
      const directoryPoll = directoryRequest(DIR_KIND_PAGE, model.dirRequest, model.dirOffset, 0, model.dirQuery);
      if (read.request.length > 0 && pollDirectory) return [next, Cmd.batch([
        Cmd.request("cockpit.snapshot", NO_BYTES, {
          key: "cockpit-snapshot", ok: "snapshot_loaded", err: "snapshot_failed",
        }),
        Cmd.request("cockpit.command-results", read.request, {
          key: "cockpit-command-results", ok: "command_result_loaded", err: "command_result_failed",
        }),
        Cmd.request("cockpit.directory", directoryPoll, { key: "cockpit-directory", ok: "directory_loaded", err: "directory_failed" }),
      ])];
      if (read.request.length > 0) return [next, Cmd.batch([
        Cmd.request("cockpit.snapshot", NO_BYTES, {
          key: "cockpit-snapshot", ok: "snapshot_loaded", err: "snapshot_failed",
        }),
        Cmd.request("cockpit.command-results", read.request, {
          key: "cockpit-command-results", ok: "command_result_loaded", err: "command_result_failed",
        }),
      ])];
      if (pollDirectory) return [next, Cmd.batch([
        Cmd.request("cockpit.snapshot", NO_BYTES, {
          key: "cockpit-snapshot", ok: "snapshot_loaded", err: "snapshot_failed",
        }),
        Cmd.request("cockpit.directory", directoryPoll, { key: "cockpit-directory", ok: "directory_loaded", err: "directory_failed" }),
      ])];
      return [
        next,
        Cmd.request("cockpit.snapshot", new Uint8Array(0), {
          key: "cockpit-snapshot",
          ok: "snapshot_loaded",
          err: "snapshot_failed",
        }),
      ];
    }
    default:
      return model;
  }
}
