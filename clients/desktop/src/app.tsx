import {
  batch,
  createEffect,
  createMemo,
  createSignal,
  on,
  onCleanup,
  onMount,
  Show,
  untrack,
  type Accessor,
  type JSX,
} from "solid-js";
import { createRoot, render, resetRender, useGpuix } from "@gpuix/solid";
import type { DesktopEvent, DesktopPane, DesktopSearchMatch } from "../native/generated/index";
import { createBridge, type DesktopHost } from "./bridge/desktop";
import { Settings, type GhosttyPanel } from "./settings/settings";
import {
  ConfirmDialog,
  EmptyState,
  RenameDialog,
  StatusBar,
  Toasts,
  type StatusInfo,
  type Toast,
} from "./shell/chrome";
import { keyChord, textFieldHasKeys } from "./shell/keymap";
import { CommandPalette, type PaletteItem } from "./shell/palette";
import { Sidebar, paneTitle, shortPath } from "./shell/sidebar";
import { TabBar, type TabView } from "./shell/tabbar";
import { FindBar } from "./terminal/find";
import { Pane, quotePaths } from "./terminal/pane";
import type { HostAction, TerminalTheme } from "./terminal-element";
import { PaletteContext } from "./ui/controls";
import type { IconName } from "./ui/icons";
import { palette, themeById, themes, type Theme } from "./ui/theme";
import { ghosttyPrefs, ghosttyTheme, parseGhostty } from "./settings/ghostty";
import {
  clampRatio,
  placements,
  type Axis,
  type DeskTab,
  type Placement,
} from "./workspace/layout";
import {
  defaultDisplay,
  parseLayout,
  saveLayout,
  sanitizeDisplay,
  SIDEBAR_MAX,
  SIDEBAR_MIN,
  type DisplayPrefs,
  type SavedLayout,
} from "./workspace/persist";
import { SplitView } from "./workspace/split-view";
import { createWorkspace } from "./workspace/controller";

export interface LayoutStore {
  read(): unknown;
  write(layout: unknown): void;
}

interface AppProps {
  host: DesktopHost;
  socketPath: string;
  sessionName: string;
  layouts: LayoutStore;
  startupError?: string | undefined;
  /** Reads the user's Ghostty config text, or undefined when there is none. */
  readGhostty?: (() => string | undefined) | undefined;
  /** Start the server (and home session) if it is gone; returns the failure, if any. */
  ensureServer?: (() => string | undefined) | undefined;
  /** Write `text` to a new private temporary file named `name`; returns its path. */
  writeTempFile?: ((name: string, text: string) => string) | undefined;
  /** This window's key router; the app installs its shortcut handler here. */
  keys: WindowKeys;
  /** Open another window on the same server (Command-N), optionally showing one terminal. */
  newWindow: (terminalId?: string) => void;
  /** Show or hide the quick terminal window. */
  toggleQuick: () => void;
  /** A secondary window starts with a new terminal, not the restored layout. */
  fresh?: boolean | undefined;
  /** A secondary window opened to show this terminal instead of a new one. */
  initialTerminal?: string | undefined;
}

interface WindowKeys {
  run: (chord: string) => void;
}

export interface Command {
  id: string;
  title: string;
  group: string;
  chord?: string;
  /** Extra chords that run the same command (e.g. ⌘+ beside ⌘=). */
  aliases?: string[];
  icon?: IconName;
  run: () => void;
}

type Modal =
  | { kind: "none" }
  | { kind: "commands"; query?: string }
  | { kind: "goto" }
  | { kind: "settings" }
  | { kind: "rename"; tabId: string; title: string }
  | { kind: "terminate"; terminalId: string; title: string };

interface FindState {
  placementId: string;
  query: string;
  caseSensitive: boolean;
  matches: DesktopSearchMatch[];
  index: number;
}

interface Drag {
  kind: "split" | "sidebar" | "tab";
  id: string;
  axis?: Axis;
  container?: number;
  startX: number;
  moved: boolean;
}

const TOAST_LIMIT = 4;

type WriteRegion = "screen" | "scrollback" | "selection";
type WriteMode = "copy" | "paste" | "open";
const WRITE_REGIONS: readonly WriteRegion[] = ["screen", "scrollback", "selection"];
const WRITE_MODES: readonly WriteMode[] = ["copy", "paste", "open"];
const WRITE_TITLES: Record<WriteRegion, string> = {
  screen: "Screen and Scrollback",
  scrollback: "Scrollback",
  selection: "Selection",
};

/** `write-file:<region>:<mode>` (from a Ghostty `write_*_file` bind), parsed. */
function writeAction(id: string): [WriteRegion, WriteMode] | undefined {
  const [kind, region, mode] = id.split(":");
  if (kind !== "write-file") return undefined;
  const knownRegion = WRITE_REGIONS.find((item) => item === region);
  const knownMode = WRITE_MODES.find((item) => item === mode);
  return knownRegion && knownMode ? [knownRegion, knownMode] : undefined;
}

function DesktopApp(props: AppProps): JSX.Element {
  const gpuix = useGpuix();
  // Mount-time inputs: the window serves one server socket and session for life.
  const layouts = untrack(() => props.layouts);
  const socketPath = untrack(() => props.socketPath);
  const sessionName = untrack(() => props.sessionName);
  const initial = parseLayout(layouts.read());
  let lastSaved: SavedLayout | undefined = initial;
  const readGhostty = untrack(() => props.readGhostty);
  const ensureServer = untrack(() => props.ensureServer);
  const writeTempFile = untrack(() => props.writeTempFile);
  const ghosttyText = readGhostty?.();
  const startGhostty = parseGhostty(ghosttyText ?? "");
  const [ghostty, setGhostty] = createSignal(startGhostty);
  const [ghosttyFound, setGhosttyFound] = createSignal(ghosttyText !== undefined);
  // First launch follows Ghostty; saved preferences win after that.
  const [prefs, setPrefs] = createSignal<DisplayPrefs>(
    initial?.display ??
      (ghosttyText !== undefined ? ghosttyPrefs(defaultDisplay, startGhostty) : defaultDisplay),
  );
  const extraThemes = createMemo((): Theme[] => {
    const theme = ghosttyTheme(ghostty());
    return theme ? [theme] : [];
  });
  const colors = createMemo(() => palette(themeById(prefs().themeId, extraThemes())));
  const [modal, setModal] = createSignal<Modal>({ kind: "none" });
  const [find, setFind] = createSignal<FindState | undefined>();
  const [toasts, setToasts] = createSignal<Toast[]>([]);
  const [now, setNow] = createSignal(Date.now());
  const [drag, setDrag] = createSignal<Drag | undefined>();
  const [hostAction, setHostAction] = createSignal<{ placementId: string; action: HostAction }>();
  let hostActionId = 0;
  const tabElements = new Map<string, number>();
  const agentStates = new Map<string, string>();
  let toastId = 0;

  const bridge = createBridge(
    untrack(() => props.host),
    { socketPath, sessionName },
  );
  const fresh = untrack(() => props.fresh) === true;
  const initialTerminal = untrack(() => props.initialTerminal);
  const workspace = createWorkspace(
    bridge,
    {
      changed: persist,
      notify: (kind, title, body) => toast({ kind, title, ...(body ? { body } : {}) }),
    },
    { fresh, ...(initialTerminal ? { initialTerminal } : {}) },
  );
  workspace.restore(initial);
  bridge.onEvents(receive);

  // ── Persistence ────────────────────────────────────────────────

  function persist(): void {
    const info = bridge.server();
    if (info && bridge.status() === "Attached") {
      lastSaved = saveLayout(info.serverId, workspace.tabs(), prefs(), workspace.activeId());
    } else if (lastSaved) {
      lastSaved = { ...lastSaved, display: prefs() };
    } else {
      return;
    }
    layouts.write(lastSaved);
  }

  function updatePrefs(change: Partial<DisplayPrefs>): void {
    setPrefs((current) => sanitizeDisplay({ ...current, ...change }));
    if (lastSaved || bridge.server()) persist();
    else layouts.write({ version: 2, serverId: "", tabs: [], display: prefs() });
  }

  // ── Events and notifications ───────────────────────────────────

  function receive(events: DesktopEvent[]): void {
    for (const event of events) {
      if (event.kind === "Closed" && workspace.viewsOf(event.terminalId) > 0) {
        const pane = paneOf(event.terminalId);
        const status =
          event.exitStatus !== undefined
            ? `exit ${event.exitStatus}`
            : event.signal !== undefined
              ? `signal ${event.signal}`
              : "closed";
        toast({ kind: "info", title: `${paneTitle(pane)} ended`, body: status });
      }
      if (event.kind === "Detached")
        toast({ kind: "error", title: "Detached", body: event.message });
      if (event.kind === "AgentBadge") agentChanged(event);
    }
    workspace.handle(events);
  }

  function agentChanged(event: Extract<DesktopEvent, { kind: "AgentBadge" }>): void {
    const previous = agentStates.get(event.terminalId);
    agentStates.set(event.terminalId, event.state);
    if (!prefs().notifications || previous === undefined || previous === event.state) return;
    if (event.state !== "done" && event.state !== "blocked" && event.attention !== "high") return;
    if (workspace.focused()?.terminalId === event.terminalId) return;
    const pane = paneOf(event.terminalId);
    toast({
      kind: "agent",
      agentState: event.state,
      title: `${event.name} ${event.state === "blocked" ? "needs you" : event.state === "done" ? "finished" : event.state}`,
      body: `${paneTitle(pane)} · ${shortPath(pane?.cwd)}`,
      action: () => openTerminal(event.terminalId),
    });
  }

  function toast(input: Omit<Toast, "id" | "at">): void {
    toastId += 1;
    const next: Toast = { ...input, id: toastId, at: Date.now() };
    setToasts((current) => [...current, next].slice(-TOAST_LIMIT));
  }

  function dismiss(id: number): void {
    setToasts((current) => current.filter((item) => item.id !== id));
  }

  createEffect(
    on(bridge.revision, () => {
      workspace.settle();
    }),
  );

  // ── Helpers ────────────────────────────────────────────────────

  function paneOf(terminalId: string | undefined): DesktopPane | undefined {
    return bridge.panes().find((pane) => pane.terminalId === terminalId);
  }

  function openTerminal(terminalId: string): void {
    const pane = paneOf(terminalId);
    if (pane) workspace.open(pane);
    else workspace.reveal(terminalId);
    setModal({ kind: "none" });
  }

  function tabTitle(tab: DeskTab): string {
    if (tab.title) return tab.title;
    const focus =
      placements(tab.root).find((item) => item.id === tab.focusedId) ?? placements(tab.root)[0];
    return paneTitle(paneOf(focus?.terminalId));
  }

  const terminalTheme = createMemo<TerminalTheme>(() => {
    const theme: TerminalTheme = {
      foreground: colors().foreground,
      background: colors().background,
      cursor: colors().cursor,
      selectionForeground: colors().selectionForeground ?? colors().foreground,
      selectionBackground: colors().selection,
    };
    const ansi = colors().palette;
    if (ansi) theme.palette = ansi;
    return theme;
  });

  const font = createMemo(() => ({
    family: prefs().fontFamily,
    size: prefs().fontSize,
    lineHeight: prefs().lineHeight,
    cellWidth: prefs().cellWidth,
    cellHeight: prefs().cellHeight,
  }));

  const tabViews = createMemo<TabView[]>(() =>
    workspace.tabs().map((tab) => {
      const states = placements(tab.root)
        .map((item) => bridge.agents()[item.terminalId]?.state)
        .filter((state): state is string => !!state);
      const strongest = ["blocked", "done", "working", "idle", "unknown"].find((state) =>
        states.includes(state),
      );
      const view: TabView = {
        id: tab.id,
        title: tabTitle(tab),
        panes: placements(tab.root).length,
        zoomed: !!tab.zoomedId,
      };
      if (strongest) view.agentState = strongest;
      return view;
    }),
  );

  const activeLeaves = createMemo(() => {
    const tab = workspace.activeTab();
    return tab ? placements(tab.root) : [];
  });
  const visibleTerminals = createMemo(() => new Set(activeLeaves().map((item) => item.terminalId)));

  // ── Find ───────────────────────────────────────────────────────

  function openFind(): void {
    const focus = workspace.focused();
    if (!focus) return;
    const current = find();
    if (current?.placementId === focus.id) {
      runSearch(current.query, current.caseSensitive);
      return;
    }
    setFind({ placementId: focus.id, query: "", caseSensitive: false, matches: [], index: 0 });
  }

  /** Search for the focused view's selected text (its first line), like Command-E on macOS. */
  function findSelection(): void {
    const focus = workspace.focused();
    if (!focus) return;
    const selected = safe(() => bridge.client().viewSelectionText(focus.viewId), "");
    const query = (selected.split(/\r?\n/).find((line) => line.trim()) ?? "").slice(0, 512);
    if (!query) {
      openFind();
      return;
    }
    const caseSensitive = find()?.caseSensitive ?? false;
    setFind({ placementId: focus.id, query, caseSensitive, matches: [], index: 0 });
    runSearch(query, caseSensitive);
  }

  function runSearch(query: string, caseSensitive: boolean): void {
    const state = find();
    const placement = placementById(state?.placementId);
    if (!state || !placement) return;
    const matches = query
      ? safe(() => bridge.client().searchView(placement.viewId, query, caseSensitive), [])
      : [];
    setFind({ ...state, query, caseSensitive, matches, index: 0 });
    showMatch(placement, matches, 0);
  }

  function stepFind(delta: 1 | -1): void {
    const state = find();
    const placement = placementById(state?.placementId);
    if (!state || !placement) return;
    if (state.matches.length === 0) {
      runSearch(state.query, state.caseSensitive);
      return;
    }
    const index = (state.index + delta + state.matches.length) % state.matches.length;
    setFind({ ...state, index });
    showMatch(placement, state.matches, index);
  }

  function showMatch(placement: Placement, matches: DesktopSearchMatch[], index: number): void {
    const match = matches[index];
    if (!match) {
      safe(() => bridge.client().clearViewSelection(placement.viewId), undefined);
      return;
    }
    safe(() => {
      bridge.client().setViewSelection(placement.viewId, match.start, match.end, false);
      bridge.client().pinViewportView(placement.viewId, match.start);
    }, undefined);
  }

  function closeFind(): void {
    const placement = placementById(find()?.placementId);
    if (placement) safe(() => bridge.client().clearViewSelection(placement.viewId), undefined);
    setFind(undefined);
  }

  function findStatus(): string {
    const state = find();
    if (!state?.query) return "";
    if (state.matches.length === 0) return "no matches";
    return `${state.index + 1} of ${state.matches.length}${state.matches.length >= 4096 ? "+" : ""}`;
  }

  function placementById(id: string | undefined): Placement | undefined {
    if (!id) return undefined;
    for (const tab of workspace.tabs()) {
      const found = placements(tab.root).find((item) => item.id === id);
      if (found) return found;
    }
    return undefined;
  }

  // ── Actions ────────────────────────────────────────────────────

  function reconnect(): void {
    // A server that exited (crash, upgrade, `phux kill-server`) comes back
    // first, so Reconnect recovers without a trip to the command line.
    const failure = bridge.status() === "Attached" ? undefined : ensureServer?.();
    if (failure) toast({ kind: "error", title: "Could not start the phux server", body: failure });
    persist();
    const snapshot = lastSaved;
    workspace.releaseAll();
    setFind(undefined);
    workspace.restore(snapshot);
    bridge.reconnect();
    toast({ kind: "info", title: "Reconnecting", body: socketPath });
  }

  function scrollLines(rows: number): void {
    const focus = workspace.focused();
    if (focus) safe(() => bridge.client().scrollView(focus.viewId, rows), undefined);
  }

  /** Move the active tab one place, wrapping like Ghostty's move_tab. */
  function moveActiveTab(step: 1 | -1): void {
    const list = workspace.tabs();
    const from = list.findIndex((tab) => tab.id === workspace.activeId());
    if (from < 0 || list.length < 2) return;
    workspace.moveTab(from, (from + step + list.length) % list.length);
  }

  function scrollPage(direction: 1 | -1): void {
    const focus = workspace.focused();
    if (!focus) return;
    safe(() => {
      const rows = bridge.client().viewInfo(focus.viewId).rows;
      bridge.client().scrollView(focus.viewId, direction * Math.max(1, rows - 1));
    }, undefined);
  }

  function scrollTop(): void {
    const focus = workspace.focused();
    if (!focus) return;
    safe(() => {
      const total = Number(bridge.client().viewInfo(focus.viewId).scrollTotal);
      bridge
        .client()
        .scrollView(focus.viewId, -Math.min(Number.MAX_SAFE_INTEGER, Math.max(1, total)));
    }, undefined);
  }

  /** Ctrl-L: the shell clears its screen. History is the server's. */
  function clearScreen(): void {
    sendText("\f");
  }

  /**
   * Type `text` into the focused terminal as keys: controls become their keys
   * and ESC before a key becomes Alt (Ghostty's `text:` and `esc:`).
   * commitText refuses controls, so this is the only path for them.
   */
  function sendText(text: string): void {
    const focus = workspace.focused();
    if (!focus) return;
    try {
      bridge.client().typeText(focus.viewId, text);
    } catch (error) {
      toast({ kind: "error", title: "Could not send keys", body: String(error) });
    }
  }

  // ── Ghostty terminal actions ───────────────────────────────────

  /** A platform request (clipboard, open) the focused terminal runs natively once. */
  function requestHost(make: (id: string) => HostAction): void {
    const focus = workspace.focused();
    if (!focus) return;
    hostActionId += 1;
    setHostAction({ placementId: focus.id, action: make(String(hostActionId)) });
  }

  function selectAll(): void {
    const focus = workspace.focused();
    if (focus) safe(() => bridge.client().selectAllView(focus.viewId), false);
  }

  /** Ghostty's jump_to_prompt: needs the shell to mark prompts (OSC 133). */
  function jumpToPrompt(prompts: number): void {
    const focus = workspace.focused();
    if (focus) safe(() => bridge.client().jumpToPromptView(focus.viewId, prompts), undefined);
  }

  /** Ghostty's selection clipboard is the view's own selection: paste it, as a middle click would. */
  function pasteSelection(): void {
    const focus = workspace.focused();
    if (!focus) return;
    const text = safe(() => bridge.client().viewSelectionText(focus.viewId), "");
    if (text) safe(() => bridge.client().pasteView(focus.viewId, text), "");
  }

  /**
   * Ghostty's write_screen_file, write_scrollback_file and
   * write_selection_file: the text goes to a private temporary file whose
   * path is then copied, pasted, or opened in its default app.
   */
  function writeFile(region: WriteRegion, mode: WriteMode): void {
    const focus = workspace.focused();
    if (!focus || !writeTempFile) return;
    let path: string;
    try {
      const client = bridge.client();
      const text =
        region === "selection"
          ? safe(() => client.viewSelectionText(focus.viewId), "")
          : client.viewDocumentText(focus.viewId, region === "scrollback");
      if (!text) {
        toast({
          kind: "info",
          title: region === "selection" ? "Nothing selected" : "Nothing to write",
        });
        return;
      }
      path = writeTempFile(`${region}.txt`, text);
    } catch (error) {
      toast({ kind: "error", title: "Could not write the file", body: String(error) });
      return;
    }
    if (mode === "paste")
      safe(() => bridge.client().pasteView(focus.viewId, quotePaths([path])), "");
    else requestHost((id) => ({ id, kind: mode === "open" ? "open" : "copyText", text: path }));
  }

  function reloadGhostty(): void {
    const text = readGhostty?.();
    setGhosttyFound(text !== undefined);
    setGhostty(parseGhostty(text ?? ""));
    toast(
      text === undefined
        ? { kind: "info", title: "No Ghostty config found" }
        : { kind: "success", title: "Ghostty config reloaded" },
    );
  }

  function follow(): void {
    const focus = workspace.focused();
    if (focus) safe(() => bridge.client().followLiveView(focus.viewId), undefined);
  }

  function openFolder(): void {
    const prompt = gpuix?.renderer.promptForPaths?.({
      directories: true,
      files: false,
      prompt: "Open",
    });
    if (!prompt) return;
    prompt
      .then((paths) => {
        const path = paths?.[0];
        if (path) workspace.newTerminal(path);
      })
      .catch((error: unknown) =>
        toast({ kind: "error", title: "Open folder failed", body: String(error) }),
      );
  }

  /**
   * A full window of its own (keys, resizing, reconnect) opens on this
   * terminal; this window only lets go of its view. The process never stops.
   */
  function moveToWindow(): void {
    const focus = workspace.focused();
    if (!focus) return;
    props.newWindow(focus.terminalId);
    workspace.closePane(focus.id);
  }

  function askTerminate(): void {
    const focus = workspace.focused();
    if (!focus) return;
    setModal({
      kind: "terminate",
      terminalId: focus.terminalId,
      title: paneTitle(paneOf(focus.terminalId)),
    });
  }

  function renameActive(): void {
    const tab = workspace.activeTab();
    if (tab) setModal({ kind: "rename", tabId: tab.id, title: tab.title ?? "" });
  }

  function copyPath(): void {
    const pane = paneOf(workspace.focused()?.terminalId);
    if (!pane?.cwd) return;
    const focus = workspace.focused();
    if (focus)
      safe(() => bridge.client().pasteView(focus.viewId, quotePaths([pane.cwd ?? ""])), "");
  }

  function nextAttention(): void {
    const agents = bridge.agents();
    const order = ["blocked", "done", "working"];
    const target = bridge
      .panes()
      .filter((pane) => order.includes(agents[pane.terminalId]?.state ?? ""))
      .sort(
        (a, b) =>
          order.indexOf(agents[a.terminalId]?.state ?? "") -
            order.indexOf(agents[b.terminalId]?.state ?? "") ||
          (agents[b.terminalId]?.changedAt ?? 0) - (agents[a.terminalId]?.changedAt ?? 0),
      )[0];
    if (target) openTerminal(target.terminalId);
    else toast({ kind: "info", title: "No agent needs attention" });
  }

  const commands: Command[] = [
    {
      id: "palette",
      title: "Command Palette",
      group: "General",
      chord: "cmd+shift+p",
      icon: "command",
      run: () => setModal({ kind: "commands" }),
    },
    {
      id: "goto",
      title: "Go to Terminal",
      group: "General",
      chord: "cmd+p",
      icon: "search",
      run: () => setModal({ kind: "goto" }),
    },
    {
      id: "settings",
      title: "Settings",
      group: "General",
      chord: "cmd+,",
      icon: "settings",
      run: () => setModal({ kind: "settings" }),
    },
    {
      id: "new",
      title: "New Terminal Tab",
      group: "Terminal",
      chord: "cmd+t",
      icon: "plus",
      run: () => workspace.newTerminal(),
    },
    {
      id: "new-home",
      title: "New Terminal in Home Directory",
      group: "Terminal",
      chord: "cmd+shift+t",
      icon: "terminal",
      run: () => workspace.newTerminal(process.env.HOME),
    },
    {
      id: "folder",
      title: "Open Folder…",
      group: "Terminal",
      chord: "cmd+o",
      icon: "folder",
      run: openFolder,
    },
    {
      id: "split-right",
      title: "Split Right",
      group: "Panes",
      chord: "cmd+d",
      icon: "splitRight",
      run: () => workspace.split("row"),
    },
    {
      id: "split-down",
      title: "Split Down",
      group: "Panes",
      chord: "cmd+shift+d",
      icon: "splitDown",
      run: () => workspace.split("column"),
    },
    {
      id: "split-left",
      title: "Split Left",
      group: "Panes",
      icon: "splitRight",
      run: () => workspace.split("row", true),
    },
    {
      id: "split-up",
      title: "Split Up",
      group: "Panes",
      icon: "splitDown",
      run: () => workspace.split("column", true),
    },
    {
      id: "view",
      title: "Open Another View of This Terminal",
      group: "Panes",
      chord: "cmd+alt+d",
      icon: "eye",
      run: () => workspace.duplicateView(),
    },
    {
      id: "close",
      title: "Close Pane",
      group: "Panes",
      chord: "cmd+w",
      icon: "close",
      run: () => workspace.closePane(),
    },
    {
      id: "close-tab",
      title: "Close Tab",
      group: "Tabs",
      chord: "cmd+shift+w",
      icon: "close",
      run: () => {
        const tab = workspace.activeTab();
        if (tab) workspace.closeTab(tab.id);
      },
    },
    {
      id: "zoom",
      title: "Zoom Pane",
      group: "Panes",
      chord: "cmd+shift+enter",
      icon: "maximize",
      run: () => workspace.toggleZoom(),
    },
    {
      id: "equalize",
      title: "Equalize Splits",
      group: "Panes",
      chord: "cmd+ctrl+=",
      icon: "splitRight",
      run: () => {
        workspace.equalizeTab();
        persist();
      },
    },
    {
      id: "pane-left",
      title: "Focus Pane Left",
      group: "Panes",
      chord: "cmd+alt+left",
      run: () => workspace.focusDirection("left"),
    },
    {
      id: "pane-right",
      title: "Focus Pane Right",
      group: "Panes",
      chord: "cmd+alt+right",
      run: () => workspace.focusDirection("right"),
    },
    {
      id: "pane-up",
      title: "Focus Pane Up",
      group: "Panes",
      chord: "cmd+alt+up",
      run: () => workspace.focusDirection("up"),
    },
    {
      id: "pane-down",
      title: "Focus Pane Down",
      group: "Panes",
      chord: "cmd+alt+down",
      run: () => workspace.focusDirection("down"),
    },
    {
      id: "pane-next",
      title: "Next Pane",
      group: "Panes",
      chord: "cmd+]",
      run: () => workspace.cyclePane(1),
    },
    {
      id: "pane-prev",
      title: "Previous Pane",
      group: "Panes",
      chord: "cmd+[",
      run: () => workspace.cyclePane(-1),
    },
    {
      id: "tab-next",
      title: "Next Tab",
      group: "Tabs",
      chord: "cmd+shift+]",
      run: () => workspace.stepTab(1),
    },
    {
      id: "tab-prev",
      title: "Previous Tab",
      group: "Tabs",
      chord: "cmd+shift+[",
      run: () => workspace.stepTab(-1),
    },
    {
      id: "tab-move-left",
      title: "Move Tab Left",
      group: "Tabs",
      run: () => moveActiveTab(-1),
    },
    {
      id: "tab-move-right",
      title: "Move Tab Right",
      group: "Tabs",
      run: () => moveActiveTab(1),
    },
    { id: "rename", title: "Rename Tab…", group: "Tabs", icon: "terminal", run: renameActive },
    {
      id: "window",
      title: "Move Pane to New Window",
      group: "Panes",
      chord: "cmd+alt+n",
      icon: "window",
      run: moveToWindow,
    },
    {
      id: "find",
      title: "Find in Terminal",
      group: "Terminal",
      chord: "cmd+f",
      icon: "search",
      run: openFind,
    },
    {
      id: "find-next",
      title: "Find Next",
      group: "Terminal",
      chord: "cmd+g",
      run: () => (find() ? stepFind(1) : openFind()),
    },
    {
      id: "find-prev",
      title: "Find Previous",
      group: "Terminal",
      chord: "cmd+shift+g",
      run: () => (find() ? stepFind(-1) : openFind()),
    },
    {
      id: "find-selection",
      title: "Use Selection for Find",
      group: "Terminal",
      chord: "cmd+e",
      icon: "search",
      run: findSelection,
    },
    {
      id: "find-close",
      title: "Close Find",
      group: "Terminal",
      run: closeFind,
    },
    {
      id: "follow",
      title: "Scroll to Live Output",
      group: "Terminal",
      chord: "cmd+l",
      icon: "follow",
      run: follow,
    },
    {
      id: "paste-cwd",
      title: "Paste Working Directory",
      group: "Terminal",
      icon: "copy",
      run: copyPath,
    },
    {
      id: "terminate",
      title: "Terminate Terminal Process…",
      group: "Terminal",
      icon: "skull",
      run: askTerminate,
    },
    {
      id: "attention",
      title: "Jump to Agent Needing Attention",
      group: "Agents",
      chord: "cmd+shift+a",
      icon: "bell",
      run: nextAttention,
    },
    {
      id: "sidebar",
      title: "Toggle Sidebar",
      group: "View",
      chord: "cmd+b",
      icon: "sidebar",
      run: () => updatePrefs({ sidebarVisible: !prefs().sidebarVisible }),
    },
    {
      id: "font-up",
      title: "Increase Font Size",
      group: "View",
      chord: "cmd+=",
      aliases: ["cmd+shift+="],
      run: () => updatePrefs({ fontSize: prefs().fontSize + 1 }),
    },
    {
      id: "font-down",
      title: "Decrease Font Size",
      group: "View",
      chord: "cmd+-",
      run: () => updatePrefs({ fontSize: prefs().fontSize - 1 }),
    },
    {
      id: "font-reset",
      title: "Reset Font Size",
      group: "View",
      chord: "cmd+0",
      run: () => updatePrefs({ fontSize: defaultDisplay.fontSize }),
    },
    {
      id: "option-alt",
      title: "Toggle Option as Alt",
      group: "View",
      run: () => updatePrefs({ optionAsAlt: !prefs().optionAsAlt }),
    },
    {
      id: "notify",
      title: "Toggle Agent Notifications",
      group: "Agents",
      icon: "bell",
      run: () => updatePrefs({ notifications: !prefs().notifications }),
    },
    ...(["left", "right", "up", "down"] as const).map((direction): Command => ({
      id: `resize-${direction}`,
      title: `Move Divider ${direction[0]?.toUpperCase() ?? ""}${direction.slice(1)}`,
      group: "Panes",
      run: () => {
        workspace.nudge(direction);
        persist();
      },
    })),
    {
      id: "scroll-page-up",
      title: "Scroll Page Up",
      group: "Terminal",
      chord: "cmd+pageup",
      run: () => scrollPage(-1),
    },
    {
      id: "scroll-page-down",
      title: "Scroll Page Down",
      group: "Terminal",
      chord: "cmd+pagedown",
      run: () => scrollPage(1),
    },
    {
      id: "scroll-top",
      title: "Scroll to Top",
      group: "Terminal",
      chord: "cmd+home",
      run: scrollTop,
    },
    {
      id: "scroll-bottom",
      title: "Scroll to Bottom",
      group: "Terminal",
      chord: "cmd+end",
      icon: "follow",
      run: follow,
    },
    {
      id: "clear",
      title: "Clear Screen",
      group: "Terminal",
      chord: "cmd+k",
      run: clearScreen,
    },
    {
      id: "select-all",
      title: "Select All",
      group: "Terminal",
      chord: "cmd+a",
      run: selectAll,
    },
    {
      id: "copy",
      title: "Copy",
      group: "Terminal",
      icon: "copy",
      run: () => requestHost((id) => ({ id, kind: "copy" })),
    },
    {
      id: "paste",
      title: "Paste",
      group: "Terminal",
      run: () => requestHost((id) => ({ id, kind: "paste" })),
    },
    { id: "paste-selection", title: "Paste Selection", group: "Terminal", run: pasteSelection },
    {
      id: "prompt-prev",
      title: "Jump to Previous Prompt",
      group: "Terminal",
      chord: "cmd+up",
      run: () => jumpToPrompt(-1),
    },
    {
      id: "prompt-next",
      title: "Jump to Next Prompt",
      group: "Terminal",
      chord: "cmd+down",
      run: () => jumpToPrompt(1),
    },
    ...WRITE_REGIONS.map((region): Command => ({
      id: `write-file:${region}:open`,
      title: `Open ${WRITE_TITLES[region]} as a File`,
      group: "Terminal",
      run: () => writeFile(region, "open"),
    })),
    {
      id: "quick-terminal",
      title: "Toggle Quick Terminal",
      group: "View",
      icon: "terminal",
      run: () => props.toggleQuick(),
    },
    {
      id: "new-window",
      title: "New Window",
      group: "View",
      chord: "cmd+n",
      icon: "window",
      run: () => props.newWindow(),
    },
    {
      id: "fullscreen",
      title: "Toggle Full Screen",
      group: "View",
      chord: "cmd+ctrl+f",
      icon: "maximize",
      run: () => gpuix?.renderer.toggleFullscreen?.(),
    },
    {
      id: "maximize",
      title: "Zoom Window",
      group: "View",
      icon: "maximize",
      run: () => gpuix?.renderer.zoomWindow?.(),
    },
    {
      id: "reload-config",
      title: "Reload Ghostty Config",
      group: "Ghostty",
      chord: "cmd+shift+,",
      icon: "refresh",
      run: reloadGhostty,
    },
    {
      id: "import-ghostty",
      title: "Use Ghostty Font, Colors and Padding",
      group: "Ghostty",
      icon: "palette",
      run: () => updatePrefs(ghosttyPrefs(prefs(), ghostty())),
    },
    {
      id: "reconnect",
      title: "Reconnect to Server",
      group: "Connection",
      chord: "cmd+shift+r",
      icon: "refresh",
      run: reconnect,
    },
    ...themes.map((theme): Command => ({
      id: `theme-${theme.id}`,
      title: `Theme: ${theme.name}`,
      group: "Theme",
      icon: "palette",
      run: () => updatePrefs({ themeId: theme.id }),
    })),
    ...Array.from({ length: 9 }, (_, index): Command => ({
      id: `tab-${index + 1}`,
      title: index === 8 ? "Go to Last Tab" : `Go to Tab ${index + 1}`,
      group: "Tabs",
      chord: `cmd+${index + 1}`,
      run: () => workspace.selectIndex(index === 8 ? -1 : index),
    })),
  ];

  /** Commands only a keybind names: typed text, a font size, a line count, or nothing. */
  function boundCommand(id: string): Command | undefined {
    const group = "Ghostty keybinds";
    if (id === "ignore") return { id, title: "Ignore Key", group, run: () => {} };
    if (id.startsWith("send:")) {
      const text = id.slice("send:".length);
      return { id, title: `Send ${JSON.stringify(text)}`, group, run: () => sendText(text) };
    }
    const write = writeAction(id);
    if (write) {
      const [region, mode] = write;
      const verb =
        mode === "copy" ? "Copy Its Path" : mode === "paste" ? "Paste Its Path" : "Open It";
      const title = `Write ${WRITE_TITLES[region]} to a File and ${verb}`;
      return { id, title, group, run: () => writeFile(region, mode) };
    }
    const [name, value = ""] = id.split(":");
    const number = Number(value);
    if (value === "" || !Number.isFinite(number)) return undefined;
    if (name === "font-size")
      return {
        id,
        title: `Font Size ${number}`,
        group,
        run: () => updatePrefs({ fontSize: number }),
      };
    if (name === "scroll-lines")
      return { id, title: `Scroll ${number} Lines`, group, run: () => scrollLines(number) };
    if (name === "prompt-jump")
      return { id, title: `Jump ${number} Prompts`, group, run: () => jumpToPrompt(number) };
    return undefined;
  }

  const byId = new Map(commands.map((command) => [command.id, command]));
  const defaultChords = new Map<string, Command>();
  for (const command of commands) {
    for (const chord of [command.chord, ...(command.aliases ?? [])])
      if (chord) defaultChords.set(chord, command);
  }

  /** Built-in chords, then the Ghostty config's binds and unbinds on top. */
  const keymap = createMemo(() => {
    const map = new Map(defaultChords);
    if (!prefs().ghosttyKeys) return map;
    for (const [chord, id] of ghostty().keybinds) {
      const command = byId.get(id) ?? boundCommand(id);
      if (command) map.set(chord, command);
      else map.delete(chord);
    }
    return map;
  });

  /** The chord to show for a command: the user's Ghostty bind wins. */
  const displayChords = createMemo(() => {
    const shown = new Map<string, string>();
    for (const [chord, command] of keymap()) {
      if (!shown.has(command.id) || command.chord !== chord) {
        const existing = shown.get(command.id);
        if (!existing || existing === command.chord) shown.set(command.id, chord);
      }
    }
    return shown;
  });

  /** Chords without Command that the terminal must hand to the window. */
  const appChords = createMemo(() =>
    [...keymap().keys()].filter((chord) => !chord.startsWith("cmd+")),
  );

  function shortcut(chord: string): void {
    if (chord === "escape") {
      if (modal().kind !== "none") setModal({ kind: "none" });
      else if (find()) closeFind();
      return;
    }
    const command = keymap().get(chord);
    if (!command) return;
    // A text field (a dialog, the palette, this pane's find bar) keeps its own Select All.
    const typing = textFieldHasKeys(
      modal().kind !== "none",
      find()?.placementId,
      workspace.focused()?.id,
    );
    if (command.id === "select-all" && typing) return;
    const open = modal().kind;
    if (open !== "none") {
      setModal({ kind: "none" });
      if (
        (open === "commands" && command.id === "palette") ||
        (open === "goto" && command.id === "goto")
      )
        return;
      if (open === "settings" && command.id === "settings") return;
    }
    command.run();
  }

  const commandItems = createMemo<PaletteItem[]>(() =>
    commands.map((command) => {
      const item: PaletteItem = {
        id: command.id,
        title: command.title,
        group: command.group,
        run: command.run,
      };
      const chord = displayChords().get(command.id);
      if (chord) item.chord = chord;
      if (command.icon) item.icon = command.icon;
      return item;
    }),
  );

  const gotoItems = createMemo<PaletteItem[]>(() => {
    const agents = bridge.agents();
    const tabs = workspace.tabs().map((tab, index): PaletteItem => ({
      id: `tab:${tab.id}`,
      title: tabTitle(tab),
      detail: `Tab ${index + 1} · ${placements(tab.root).length} pane${placements(tab.root).length === 1 ? "" : "s"}`,
      group: "Tab",
      icon: "window",
      run: () => workspace.selectTab(tab.id),
    }));
    const panes = bridge.panes().map((pane): PaletteItem => {
      const agent = agents[pane.terminalId];
      const item: PaletteItem = {
        id: `pane:${pane.terminalId}`,
        title: agent ? `${agent.name} — ${paneTitle(pane)}` : paneTitle(pane),
        detail: `${pane.sessionName} · ${shortPath(pane.cwd) || pane.terminalId}`,
        group: agent ? agent.state : "Terminal",
        icon: "terminal",
        run: () => openTerminal(pane.terminalId),
      };
      if (agent) item.agentState = agent.state;
      return item;
    });
    return [...panes, ...tabs];
  });

  const shortcuts = createMemo(() => [
    ...commands.flatMap((command) => {
      const chord = displayChords().get(command.id);
      return chord && !/^tab-\d$/.test(command.id)
        ? [{ title: command.title, chord, group: command.group }]
        : [];
    }),
    ...[...keymap()].flatMap(([chord, command]) =>
      byId.has(command.id) ? [] : [{ title: command.title, chord, group: command.group }],
    ),
  ]);

  // ── Drags (splits, sidebar, tabs) ──────────────────────────────

  function beginResize(splitId: string, axis: Axis, container: number): void {
    setDrag({ kind: "split", id: splitId, axis, container, startX: 0, moved: false });
  }

  function moved(event: { x?: number; y?: number; pressedButton?: number }): void {
    const current = drag();
    if (!current) return;
    if (event.pressedButton === undefined) {
      endDrag();
      return;
    }
    const x = event.x ?? 0;
    const y = event.y ?? 0;
    if (current.kind === "sidebar") {
      updateSidebarWidth(x);
      return;
    }
    if (current.kind === "split" && current.container !== undefined) {
      const bounds = gpuix?.renderer.getElementBounds?.(current.container);
      if (!bounds || bounds.width <= 0 || bounds.height <= 0) return;
      const ratio =
        current.axis === "row" ? (x - bounds.x) / bounds.width : (y - bounds.y) / bounds.height;
      workspace.resizeSplit(current.id, clampRatio(ratio));
      return;
    }
    if (current.kind === "tab") dragTab(current, x);
  }

  function updateSidebarWidth(x: number): void {
    const width = Math.round(Math.min(SIDEBAR_MAX, Math.max(SIDEBAR_MIN, x)));
    setPrefs((current) => ({ ...current, sidebarWidth: width }));
  }

  function dragTab(current: Drag, x: number): void {
    if (!current.moved && Math.abs(x - current.startX) < 6) return;
    if (!current.moved) setDrag({ ...current, moved: true });
    const order = workspace.tabs();
    const from = order.findIndex((tab) => tab.id === current.id);
    if (from < 0) return;
    let to = 0;
    order.forEach((tab, index) => {
      if (index === from) return;
      const element = tabElements.get(tab.id);
      const bounds =
        element === undefined ? undefined : gpuix?.renderer.getElementBounds?.(element);
      if (bounds && x > bounds.x + bounds.width / 2) to = index < from ? index + 1 : index;
    });
    if (to !== from) workspace.moveTab(from, to);
  }

  function endDrag(): void {
    if (!drag()) return;
    setDrag(undefined);
    persist();
  }

  // ── Derived chrome state ───────────────────────────────────────

  const statusInfo = createMemo<StatusInfo>(() => {
    bridge.revision();
    const focus = workspace.focused();
    const info = focus ? safe(() => bridge.client().viewInfo(focus.viewId), undefined) : undefined;
    const counts: Record<string, number> = {};
    for (const agent of Object.values(bridge.agents()))
      counts[agent.state] = (counts[agent.state] ?? 0) + 1;
    return {
      connection: bridge.status(),
      error: bridge.error(),
      session: `${sessionName} · ${bridge.panes().length} terminals`,
      socket: socketPath,
      geometry: info ? `${info.cols}×${info.rows}` : undefined,
      scrollback: info && !info.atTail ? "scrolled back · ⌘L live" : undefined,
      fenced: focus ? bridge.fenced(focus.terminalId) : false,
      pending: workspace.pending(),
      agents: counts,
      theme: themeById(prefs().themeId, extraThemes()).name,
      fontSize: prefs().fontSize,
    };
  });

  createEffect(() => {
    const tab = workspace.activeTab();
    const title = tab ? tabTitle(tab) : "phux";
    gpuix?.renderer.setWindowTitle?.(tab ? `${title} — phux` : "phux");
  });

  // Close find when its placement disappears or another tab takes over.
  createEffect(() => {
    const state = find();
    if (state && !placementById(state.placementId)) setFind(undefined);
  });

  onMount(() => {
    untrack(() => props.keys).run = shortcut;
    const startupError = untrack(() => props.startupError);
    if (startupError)
      toast({ kind: "error", title: "Could not start the phux server", body: startupError });
    bridge.connect();
    const timer = setInterval(() => {
      const at = Date.now();
      // Only agent ages and toast expiry move with the clock; an idle
      // terminal-only window should not re-render every second.
      const aging = Object.keys(untrack(bridge.agents)).length > 0;
      const expiring = untrack(toasts).length > 0;
      if (!aging && !expiring) return;
      batch(() => {
        if (aging) setNow(at);
        if (expiring) {
          setToasts((current) =>
            current.filter(
              (item) =>
                at - item.at < (item.kind === "agent" || item.kind === "error" ? 12_000 : 5_000),
            ),
          );
        }
      });
    }, 1000);
    onCleanup(() => clearInterval(timer));
  });

  onCleanup(() => {
    persist();
    workspace.releaseAll();
    bridge.close();
  });

  // ── View ───────────────────────────────────────────────────────

  function renderPane(placement: Placement): JSX.Element {
    const tab = (): ReturnType<typeof workspace.activeTab> =>
      workspace
        .tabs()
        .find((item) => placements(item.root).some((leaf) => leaf.id === placement.id));
    const selected = (): boolean => tab()?.focusedId === placement.id;
    return (
      <Pane
        placement={placement}
        clientHandle={bridge.handle()}
        pane={paneOf(placement.terminalId)}
        agent={bridge.agents()[placement.terminalId]}
        selected={selected()}
        inputFocused={
          selected() &&
          !textFieldHasKeys(modal().kind !== "none", find()?.placementId, placement.id)
        }
        sizeOwner={workspace.sizeOwner(placement)}
        showHeader={
          placements(tab()?.root ?? { kind: "leaf", placement }).length > 1 || !!tab()?.zoomedId
        }
        zoomed={tab()?.zoomedId === placement.id}
        fenced={bridge.fenced(placement.terminalId)}
        views={workspace.viewsOf(placement.terminalId)}
        revision={bridge.revision()}
        font={font()}
        theme={terminalTheme()}
        optionAsAlt={prefs().optionAsAlt}
        appChords={appChords()}
        hostAction={hostAction()?.placementId === placement.id ? hostAction()?.action : undefined}
        padding={{ x: prefs().paddingX, y: prefs().paddingY }}
        dim={
          !selected() && placements(tab()?.root ?? { kind: "leaf", placement }).length > 1
            ? 1 - prefs().unfocusedOpacity
            : 0
        }
        focus={() => {
          const owner = tab();
          if (owner) workspace.focusPlacement(owner.id, placement.id);
        }}
        close={() => workspace.closePane(placement.id)}
        splitRight={() => {
          const owner = tab();
          if (owner) workspace.focusPlacement(owner.id, placement.id);
          workspace.split("row");
        }}
        splitDown={() => {
          const owner = tab();
          if (owner) workspace.focusPlacement(owner.id, placement.id);
          workspace.split("column");
        }}
        zoom={() => {
          const owner = tab();
          if (owner) workspace.focusPlacement(owner.id, placement.id);
          workspace.toggleZoom();
        }}
        drop={(paths) => {
          if (paths.length > 0)
            safe(() => bridge.client().pasteView(placement.viewId, quotePaths(paths)), "");
        }}
      >
        <Show when={find()?.placementId === placement.id}>
          <FindBar
            query={find()?.query ?? ""}
            status={findStatus()}
            caseSensitive={find()?.caseSensitive ?? false}
            setQuery={(query) => runSearch(query, find()?.caseSensitive ?? false)}
            step={stepFind}
            toggleCase={() => runSearch(find()?.query ?? "", !(find()?.caseSensitive ?? false))}
            close={closeFind}
          />
        </Show>
      </Pane>
    );
  }

  const zoomedPlacement = createMemo(() => {
    const tab = workspace.activeTab();
    return tab?.zoomedId ? placementById(tab.zoomedId) : undefined;
  });

  return (
    <PaletteContext.Provider value={colors}>
      <div
        style={{
          display: "flex",
          flexDirection: "column",
          width: "100%",
          height: "100%",
          backgroundColor: colors().background,
          cursor:
            drag()?.kind === "sidebar" || (drag()?.kind === "split" && drag()?.axis === "row")
              ? "col-resize"
              : drag()?.kind === "split"
                ? "row-resize"
                : "default",
        }}
        onMouseMove={moved}
        onMouseUp={endDrag}
      >
        <TabBar
          tabs={tabViews()}
          activeId={workspace.activeId()}
          sidebarWidth={prefs().sidebarWidth}
          sidebarVisible={prefs().sidebarVisible}
          select={(id) => workspace.selectTab(id)}
          close={(id) => workspace.closeTab(id)}
          newTab={() => workspace.newTerminal()}
          toggleSidebar={() => updatePrefs({ sidebarVisible: !prefs().sidebarVisible })}
          splitRight={() => workspace.split("row")}
          splitDown={() => workspace.split("column")}
          openPalette={() => setModal({ kind: "commands" })}
          beginDrag={(id, x) => setDrag({ kind: "tab", id, startX: x, moved: false })}
          rename={(id) => {
            const tab = workspace.tabs().find((item) => item.id === id);
            if (tab) setModal({ kind: "rename", tabId: id, title: tab.title ?? tabTitle(tab) });
          }}
          registerTab={(id, element) => {
            if (element === undefined) tabElements.delete(id);
            else tabElements.set(id, element);
          }}
        />
        <div style={{ display: "flex", flexDirection: "row", flexGrow: 1, minHeight: 0 }}>
          <Show when={prefs().sidebarVisible}>
            <Sidebar
              width={prefs().sidebarWidth}
              sessions={bridge.topology()?.sessions ?? []}
              panes={bridge.panes()}
              agents={bridge.agents()}
              homeSession={sessionName}
              focusedTerminal={workspace.focused()?.terminalId}
              visibleTerminals={visibleTerminals()}
              viewCount={(terminalId) => workspace.viewsOf(terminalId)}
              now={now()}
              open={(pane) => openTerminal(pane.terminalId)}
              newTerminal={() => workspace.newTerminal()}
              openSettings={() => setModal({ kind: "settings" })}
              openPalette={() => setModal({ kind: "commands" })}
            />
            <div
              role="separator"
              aria-label="Resize sidebar"
              onMouseDown={(event) =>
                setDrag({ kind: "sidebar", id: "sidebar", startX: event.x ?? 0, moved: false })
              }
              style={{
                width: 4,
                flexShrink: 0,
                cursor: "col-resize",
                backgroundColor:
                  drag()?.kind === "sidebar" ? `${colors().accent}44` : colors().border,
                hover: { backgroundColor: `${colors().accent}33` },
              }}
            />
          </Show>
          <div style={{ display: "flex", flexDirection: "column", flexGrow: 1, minWidth: 0 }}>
            <div
              style={{
                display: "flex",
                flexGrow: 1,
                minHeight: 0,
                padding: activeLeaves().length > 1 ? 4 : 0,
                position: "relative",
              }}
            >
              <Show when={!workspace.activeTab()}>
                <EmptyState
                  status={bridge.status()}
                  error={bridge.error()}
                  socket={socketPath}
                  pending={workspace.pending() > 0}
                  newTerminal={() => workspace.newTerminal()}
                  openFolder={openFolder}
                  openPalette={() => setModal({ kind: "commands" })}
                  reconnect={reconnect}
                />
              </Show>
              <Show when={workspace.activeId() || undefined} keyed>
                {(_tabId: string): JSX.Element => (
                  <>
                    <Show when={zoomedPlacement()} keyed>
                      {(zoomed: Placement): JSX.Element => renderPane(zoomed)}
                    </Show>
                    <Show when={!zoomedPlacement() ? workspace.activeTab() : undefined}>
                      {(tab: Accessor<DeskTab>): JSX.Element => (
                        <SplitView
                          node={tab().root}
                          pane={renderPane}
                          beginResize={beginResize}
                          dragging={drag()?.kind === "split" ? drag()?.id : undefined}
                        />
                      )}
                    </Show>
                  </>
                )}
              </Show>
              <Toasts toasts={toasts()} dismiss={dismiss} />
            </div>
            <StatusBar
              info={statusInfo()}
              follow={follow}
              reconnect={reconnect}
              openSettings={() => setModal({ kind: "settings" })}
            />
          </div>
        </div>
        <ModalLayer
          modal={modal()}
          close={() => setModal({ kind: "none" })}
          commandItems={commandItems()}
          gotoItems={gotoItems()}
          switchToCommands={(query) => setModal({ kind: "commands", query })}
          prefs={prefs()}
          updatePrefs={updatePrefs}
          shortcuts={shortcuts()}
          themes={[...extraThemes(), ...themes]}
          ghostty={{
            found: ghosttyFound(),
            unmapped: ghostty().unmapped,
            font: ghostty().fontFamily,
            apply: () => updatePrefs(ghosttyPrefs(prefs(), ghostty())),
            reload: reloadGhostty,
          }}
          bridge={{
            server: bridge.server(),
            socket: socketPath,
            session: sessionName,
            status: bridge.status(),
          }}
          reconnect={reconnect}
          rename={(tabId, title) => workspace.renameTab(tabId, title)}
          terminate={(terminalId) => workspace.terminate(terminalId)}
        />
      </div>
    </PaletteContext.Provider>
  );
}

function ModalLayer(props: {
  modal: Modal;
  close: () => void;
  commandItems: PaletteItem[];
  gotoItems: PaletteItem[];
  switchToCommands: (query: string) => void;
  prefs: DisplayPrefs;
  updatePrefs: (change: Partial<DisplayPrefs>) => void;
  shortcuts: { title: string; chord: string; group: string }[];
  themes: Theme[];
  ghostty: GhosttyPanel;
  bridge: {
    server: Parameters<typeof Settings>[0]["server"];
    socket: string;
    session: string;
    status: string;
  };
  reconnect: () => void;
  rename: (tabId: string, title: string) => void;
  terminate: (terminalId: string) => void;
}): JSX.Element {
  return (
    <>
      <Show when={props.modal.kind === "commands" ? props.modal : undefined} keyed>
        {(modal: Extract<Modal, { kind: "commands" }>): JSX.Element => (
          <CommandPalette
            placeholder="Run a command…"
            items={props.commandItems}
            close={props.close}
            initialQuery={modal.query ?? ""}
          />
        )}
      </Show>
      <Show when={props.modal.kind === "goto"}>
        <CommandPalette
          placeholder="Go to a terminal, agent or tab…  (type > for commands)"
          items={props.gotoItems}
          close={props.close}
          onQuery={(query) => {
            if (!query.startsWith(">")) return false;
            props.switchToCommands(query.slice(1));
            return true;
          }}
        />
      </Show>
      <Show when={props.modal.kind === "settings"}>
        <Settings
          prefs={props.prefs}
          update={props.updatePrefs}
          shortcuts={props.shortcuts}
          themes={props.themes}
          ghostty={props.ghostty}
          server={props.bridge.server}
          socket={props.bridge.socket}
          session={props.bridge.session}
          status={props.bridge.status}
          reconnect={props.reconnect}
          close={props.close}
        />
      </Show>
      <Show when={props.modal.kind === "rename" ? props.modal : undefined} keyed>
        {(modal: Extract<Modal, { kind: "rename" }>): JSX.Element => (
          <RenameDialog
            initial={modal.title}
            apply={(title) => props.rename(modal.tabId, title)}
            close={props.close}
          />
        )}
      </Show>
      <Show when={props.modal.kind === "terminate" ? props.modal : undefined} keyed>
        {(modal: Extract<Modal, { kind: "terminate" }>): JSX.Element => (
          <ConfirmDialog
            title={`Terminate ${modal.title}?`}
            body="This ends the process for every view and every client attached to it. Closing a pane only detaches."
            confirm="Terminate"
            run={() => props.terminate(modal.terminalId)}
            close={props.close}
          />
        )}
      </Show>
    </>
  );
}

function safe<T>(run: () => T, fallback: T): T {
  try {
    return run();
  } catch {
    return fallback;
  }
}

const WINDOW = {
  title: "phux",
  appName: "phux",
  width: 1320,
  height: 840,
  minWidth: 640,
  minHeight: 420,
  titlebarTransparent: true,
  trafficLightX: 14,
  trafficLightY: 14,
} as const;

export interface MountOptions {
  socketPath: string;
  sessionName: string;
  layouts: LayoutStore;
  startupError?: string | undefined;
  readGhostty?: (() => string | undefined) | undefined;
  quickLayouts?: LayoutStore | undefined;
  /** Runs `phux server --ensure` and the home session; returns the failure, if any. */
  ensureServer?: (() => string | undefined) | undefined;
  /** Writes a private temporary file for Ghostty's `write_*_file` actions; returns its path. */
  writeTempFile?: ((name: string, text: string) => string) | undefined;
}

export function mount(host: DesktopHost, options: MountOptions): void {
  const { socketPath, sessionName, layouts, startupError, readGhostty, quickLayouts } = options;
  const { ensureServer, writeTempFile } = options;
  const mainKeys: WindowKeys = { run: () => {} };
  const quick: { close?: () => void } = {};

  /**
   * A window with its own connection and workspace. Command-N windows start
   * with a new terminal (or the one a pane moved out with) and save nothing;
   * the quick terminal keeps its own small layout so the same terminal comes
   * back on every toggle.
   */
  function openWindow(options: { quick: boolean; terminalId?: string }): {
    close: () => void;
    isOpen: () => boolean;
  } {
    const keys: WindowKeys = { run: () => {} };
    let root: ReturnType<typeof createRoot> | undefined;
    const renderer = new host.GpuixRenderer((error, event) => {
      if (!error) root?.dispatch(event);
    });
    renderer.init(
      options.quick
        ? { ...WINDOW, title: "phux quick terminal", width: 1100, height: 460, focus: true }
        : { ...WINDOW, focus: true },
    );
    const created = createRoot(renderer, {
      onKeyDown(event) {
        const chord = keyChord(event);
        if (chord) keys.run(chord);
      },
    });
    root = created;
    const store =
      options.quick && quickLayouts ? quickStore(quickLayouts, layouts) : borrowedPrefs(layouts);
    created.render((): JSX.Element => (
      <DesktopApp
        host={host}
        socketPath={socketPath}
        sessionName={sessionName}
        layouts={store}
        readGhostty={readGhostty}
        ensureServer={ensureServer}
        writeTempFile={writeTempFile}
        keys={keys}
        newWindow={newWindow}
        toggleQuick={toggleQuick}
        fresh
        initialTerminal={options.terminalId}
      />
    ));
    let disposed = false;
    const dispose = (): void => {
      if (disposed) return;
      disposed = true;
      clearInterval(watch);
      created.unmount();
    };
    // GPUIX reports no close event: dispose the root, its views and its
    // connection once the user closes the window.
    const watch = setInterval(() => {
      if (!renderer.isWindowOpen()) dispose();
    }, 500);
    return {
      close: () => {
        dispose();
        if (renderer.isWindowOpen()) renderer.closeWindow();
      },
      isOpen: () => !disposed && renderer.isWindowOpen(),
    };
  }

  function newWindow(terminalId?: string): void {
    openWindow(terminalId ? { quick: false, terminalId } : { quick: false });
  }

  let quickWindow: ReturnType<typeof openWindow> | undefined;
  function toggleQuick(): void {
    if (quickWindow?.isOpen()) {
      quickWindow.close();
      quickWindow = undefined;
      return;
    }
    quickWindow = openWindow({ quick: true });
  }
  quick.close = () => quickWindow?.close();

  render(
    (): JSX.Element => (
      <DesktopApp
        host={host}
        socketPath={socketPath}
        sessionName={sessionName}
        layouts={layouts}
        startupError={startupError}
        readGhostty={readGhostty}
        ensureServer={ensureServer}
        writeTempFile={writeTempFile}
        keys={mainKeys}
        newWindow={newWindow}
        toggleQuick={toggleQuick}
      />
    ),
    {
      ...WINDOW,
      focus: process.env.PHUX_DESKTOP_BACKGROUND !== "1",
      onKeyDown(event) {
        // A focused terminal consumes Escape itself; one that reaches the
        // window means an overlay or nothing holds the keyboard.
        const chord = keyChord(event);
        if (chord) mainKeys.run(chord);
      },
      onUncaughtError: (error) => {
        resetRender();
        throw error;
      },
    },
  );
  // Settings > Ghostty "Use Ghostty keybinds" off covers global binds too.
  if (parseLayout(layouts.read())?.display.ghosttyKeys !== false)
    registerGlobalHotkeys(host, readGhostty, toggleQuick);
  process.once("SIGTERM", () => {
    quick.close?.();
    resetRender();
    process.exit(0);
  });
}

/**
 * Ghostty's `global:` binds for the quick terminal become system-wide hotkeys.
 * Another app holding the chord (a running Ghostty, say) wins; that is logged,
 * and the in-app binding still works while phux is focused.
 */
function registerGlobalHotkeys(
  host: DesktopHost,
  readGhostty: (() => string | undefined) | undefined,
  toggleQuick: () => void,
): void {
  const text = readGhostty?.();
  if (!text) return;
  const config = parseGhostty(text);
  const chords = [...config.globals].filter(
    (chord) => config.keybinds.get(chord) === "quick-terminal",
  );
  if (chords.length === 0) return;
  let hotkeys: InstanceType<DesktopHost["GlobalHotkeys"]>;
  try {
    hotkeys = new host.GlobalHotkeys();
  } catch (error) {
    console.error(`global hotkeys unavailable: ${String(error)}`);
    return;
  }
  const registered = chords.filter((chord) => {
    try {
      hotkeys.register(chord);
      return true;
    } catch (error) {
      console.error(`global hotkey ${chord} unavailable: ${String(error)}`);
      return false;
    }
  });
  if (registered.length === 0) return;
  setInterval(() => {
    if (hotkeys.takePressed().length > 0) toggleQuick();
  }, 50);
}

/** The quick terminal's own tabs, shown without the sidebar in the main window's style. */
function quickStore(quick: LayoutStore, main: LayoutStore): LayoutStore {
  return {
    read: () => {
      const saved = parseLayout(quick.read());
      const display = parseLayout(main.read())?.display;
      if (!saved && !display) return undefined;
      const prefs = { ...(display ?? saved?.display ?? defaultDisplay), sidebarVisible: false };
      return saved
        ? { ...saved, display: prefs }
        : { version: 2, serverId: "", tabs: [], display: prefs };
    },
    write: (layout) => quick.write(layout),
  };
}

/** A secondary window reads the main window's display preferences and saves nothing. */
function borrowedPrefs(layouts: LayoutStore): LayoutStore {
  return {
    read: () => {
      const main = parseLayout(layouts.read());
      return main ? { version: 2, serverId: "", tabs: [], display: main.display } : undefined;
    },
    write: () => {},
  };
}
