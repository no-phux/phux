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
import { render, resetRender, useGpuix } from "@gpuix/solid";
import type { DesktopEvent, DesktopPane, DesktopSearchMatch } from "../native/generated/index";
import { createBridge, type DesktopHost } from "./bridge/desktop";
import { Settings } from "./settings/settings";
import {
  ConfirmDialog,
  EmptyState,
  RenameDialog,
  StatusBar,
  Toasts,
  type StatusInfo,
  type Toast,
} from "./shell/chrome";
import { chordOf, plainKey } from "./shell/keymap";
import { CommandPalette, type PaletteItem } from "./shell/palette";
import { Sidebar, paneTitle, shortPath } from "./shell/sidebar";
import { TabBar, type TabView } from "./shell/tabbar";
import { FindBar } from "./terminal/find";
import { Pane, quotePaths } from "./terminal/pane";
import type { TerminalTheme } from "./terminal-element";
import { PaletteContext } from "./ui/controls";
import type { IconName } from "./ui/icons";
import { palette, themeById, themes } from "./ui/theme";
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

function DesktopApp(props: AppProps): JSX.Element {
  const gpuix = useGpuix();
  // Mount-time inputs: the window serves one server socket and session for life.
  const layouts = untrack(() => props.layouts);
  const socketPath = untrack(() => props.socketPath);
  const sessionName = untrack(() => props.sessionName);
  const initial = parseLayout(layouts.read());
  let lastSaved: SavedLayout | undefined = initial;
  const [prefs, setPrefs] = createSignal<DisplayPrefs>(initial?.display ?? defaultDisplay);
  const colors = createMemo(() => palette(themeById(prefs().themeId)));
  const [modal, setModal] = createSignal<Modal>({ kind: "none" });
  const [find, setFind] = createSignal<FindState | undefined>();
  const [toasts, setToasts] = createSignal<Toast[]>([]);
  const [now, setNow] = createSignal(Date.now());
  const [drag, setDrag] = createSignal<Drag | undefined>();
  const tabElements = new Map<string, number>();
  const agentStates = new Map<string, string>();
  let toastId = 0;

  const bridge = createBridge(
    untrack(() => props.host),
    { socketPath, sessionName },
  );
  const workspace = createWorkspace(bridge, {
    changed: persist,
    notify: (kind, title, body) => toast({ kind, title, ...(body ? { body } : {}) }),
  });
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

  const terminalTheme = createMemo<TerminalTheme>(() => ({
    foreground: colors().foreground,
    background: colors().background,
    cursor: colors().cursor,
    selectionForeground: colors().foreground,
    selectionBackground: colors().selection,
  }));

  const font = createMemo(() => ({
    family: prefs().fontFamily,
    size: prefs().fontSize,
    lineHeight: prefs().lineHeight,
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
    persist();
    const snapshot = lastSaved;
    workspace.releaseAll();
    setFind(undefined);
    workspace.restore(snapshot);
    bridge.reconnect();
    toast({ kind: "info", title: "Reconnecting", body: socketPath });
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

  function moveToWindow(): void {
    const focus = workspace.focused();
    const opener = globalThis.phuxOpenWindow;
    if (!focus || !opener) return;
    const placement = workspace.detachToWindow(focus.id);
    if (!placement) return;
    opener({
      clientHandle: bridge.handle(),
      terminalId: placement.terminalId,
      viewId: placement.viewId,
      title: paneTitle(paneOf(placement.terminalId)),
      font: font(),
      theme: terminalTheme(),
    });
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

  const byChord = new Map<string, Command>();
  for (const command of commands) {
    for (const chord of [command.chord, ...(command.aliases ?? [])])
      if (chord) byChord.set(chord, command);
  }

  function shortcut(chord: string): void {
    if (chord === "escape") {
      if (modal().kind !== "none") setModal({ kind: "none" });
      else if (find()) closeFind();
      return;
    }
    const command = byChord.get(chord);
    if (!command) return;
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
      if (command.chord) item.chord = command.chord;
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

  const shortcuts = commands
    .filter((command) => command.chord && !/^tab-\d$/.test(command.id))
    .map((command) => ({ title: command.title, chord: command.chord ?? "", group: command.group }));

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
      theme: themeById(prefs().themeId).name,
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
    globalThis.phuxShortcut = shortcut;
    bridge.connect();
    const timer = setInterval(() => {
      const at = Date.now();
      batch(() => {
        setNow(at);
        setToasts((current) =>
          current.filter(
            (item) =>
              at - item.at < (item.kind === "agent" || item.kind === "error" ? 12_000 : 5_000),
          ),
        );
      });
    }, 1000);
    onCleanup(() => clearInterval(timer));
  });

  onCleanup(() => {
    globalThis.phuxShortcut = undefined;
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
        inputFocused={selected() && modal().kind === "none" && find()?.placementId !== placement.id}
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
          shortcuts={shortcuts}
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

export function mount(
  host: DesktopHost,
  socketPath: string,
  sessionName: string,
  layouts: LayoutStore,
): void {
  render(
    (): JSX.Element => (
      <DesktopApp host={host} socketPath={socketPath} sessionName={sessionName} layouts={layouts} />
    ),
    {
      title: "phux",
      appName: "phux",
      width: 1320,
      height: 840,
      minWidth: 640,
      minHeight: 420,
      titlebarTransparent: true,
      trafficLightX: 14,
      trafficLightY: 14,
      focus: process.env.PHUX_DESKTOP_BACKGROUND !== "1",
      onKeyDown(event) {
        // A focused terminal consumes Escape itself; one that reaches the
        // window means an overlay or nothing holds the keyboard.
        const chord = chordOf(event) || (plainKey(event) === "escape" ? "escape" : "");
        if (chord) globalThis.phuxShortcut?.(chord);
      },
      onUncaughtError: (error) => {
        resetRender();
        throw error;
      },
    },
  );
  process.once("SIGTERM", () => {
    resetRender();
    process.exit(0);
  });
}
