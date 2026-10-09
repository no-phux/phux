/**
 * Workspace state over the bridge: tabs, split trees, focus, and where a
 * pending terminal lands. Placement is local presentation. Closing a pane or
 * tab releases views and never terminates a process; only `terminate` does.
 */
import { batch, createSignal, type Accessor } from "solid-js";
import type { DesktopEvent, DesktopPane } from "../../native/generated/index";
import type { Bridge, SpawnRequest } from "../bridge/desktop";
import {
  cycle,
  equalize,
  findPlacement,
  focusAfterRemoval,
  focusedPlacement,
  leaf,
  neighbor,
  newId,
  nudge,
  placements,
  refocus,
  removeLeaf,
  reorder,
  setRatio,
  splitAt,
  withoutTerminal,
  type Axis,
  type DeskTab,
  type Direction,
  type LayoutNode,
  type Placement,
} from "./layout";
import type { SavedLayout, SavedNode, SavedTab } from "./persist";
import { placeFor } from "./place";

/** Where a terminal that is not yet ready will be placed. */
export type Destination =
  | { kind: "tab" }
  | { kind: "split"; tabId: string; targetId: string; axis: Axis; before?: boolean };

export interface Workspace {
  tabs: Accessor<DeskTab[]>;
  activeId: Accessor<string>;
  activeTab(): DeskTab | undefined;
  focused(): Placement | undefined;
  pending: Accessor<number>;
  selectTab(id: string): void;
  stepTab(step: 1 | -1): void;
  selectIndex(index: number): void;
  moveTab(from: number, to: number): void;
  renameTab(id: string, title: string): void;
  closeTab(id: string): void;
  focusPlacement(tabId: string, placementId: string): void;
  focusDirection(direction: Direction): void;
  cyclePane(step: 1 | -1): void;
  toggleZoom(): void;
  /** Move the nearest divider along `direction`, like Ghostty's resize_split. */
  nudge(direction: Direction): void;
  equalizeTab(): void;
  resizeSplit(splitId: string, ratio: number): void;
  newTerminal(cwd?: string): void;
  /** Spawn a tab in `sessionId`. Does not substitute another session. */
  newTerminalIn(sessionId: number, cwd?: string): void;
  /** Split the focused pane with a new terminal after it, or `before` it. */
  split(axis: Axis, before?: boolean): void;
  duplicateView(): void;
  closePane(placementId?: string): void;
  reveal(terminalId: string): void;
  open(pane: DesktopPane): void;
  terminate(terminalId: string): void;
  handle(events: DesktopEvent[]): void;
  settle(): void;
  restore(layout: SavedLayout | undefined): void;
  snapshot(): { tabs: DeskTab[]; activeId: string };
  /** Incarnation that owns the current tabs, not merely the latest bridge snapshot. */
  serverId(): string | undefined;
  releaseAll(): void;
  viewsOf(terminalId: string): number;
  /** Whether this visible placement proposes its terminal's PTY size. */
  sizeOwner(placement: Placement): boolean;
}

export interface WorkspaceHooks {
  changed(): void;
  notify(kind: "info" | "error", title: string, body?: string): void;
}

export interface WorkspaceOptions {
  /** A new window: open a fresh terminal rather than the session's home pane. */
  fresh?: boolean;
  /** A window opened to show this existing terminal (Move Pane to New Window). */
  initialTerminal?: string;
}

export function createWorkspace(
  bridge: Bridge,
  hooks: WorkspaceHooks,
  options: WorkspaceOptions = {},
): Workspace {
  const [tabs, setTabs] = createSignal<DeskTab[]>([]);
  const [activeId, setActiveId] = createSignal("");
  const [pending, setPending] = createSignal(0);
  /** Spawn request id -> destination, until the server names the terminal. */
  const spawns = new Map<number, Destination>();
  /** Terminal id -> destination, until the runtime reports input-ready. */
  const awaiting = new Map<string, Destination>();
  /** Terminal id -> the placement the user focused last. */
  const [owners, setOwners] = createSignal<Record<string, string>>({});
  let restoring: SavedLayout | undefined;
  let restored = false;
  let serverId: string | undefined;
  const unresolved = new Set<string>();
  const attaching = new Set<string>();
  const closedTerminals = new Set<string>();

  function activeTab(): DeskTab | undefined {
    return tabs().find((tab) => tab.id === activeId()) ?? tabs()[0];
  }

  function focused(): Placement | undefined {
    const tab = activeTab();
    return tab ? focusedPlacement(tab) : undefined;
  }

  function commit(next: DeskTab[], active?: string): void {
    batch(() => {
      setTabs(next);
      if (active !== undefined) setActiveId(active);
      else if (!next.some((tab) => tab.id === activeId())) setActiveId(next[0]?.id ?? "");
    });
    hooks.changed();
  }

  function updateTab(id: string, change: (tab: DeskTab) => DeskTab): void {
    commit(tabs().map((tab) => (tab.id === id ? refocus(change(tab)) : tab)));
  }

  function place(terminalId: string, id = newId("place")): Placement {
    if (bridge.ready(terminalId)) {
      return { id, terminalId, viewId: bridge.client().createView(terminalId) };
    }
    unresolved.add(id);
    return { id, terminalId };
  }

  function release(items: Placement[]): void {
    for (const placement of items) {
      unresolved.delete(placement.id);
      if (!placement.viewId) continue;
      try {
        bridge.client().destroyView(placement.viewId);
      } catch {
        // The client may already be closed; its views died with it.
      }
    }
  }

  function selectTab(id: string): void {
    if (tabs().some((tab) => tab.id === id)) {
      setActiveId(id);
      hooks.changed();
    }
  }

  function selectIndex(index: number): void {
    const list = tabs();
    const tab = index < 0 ? list.at(-1) : list[index];
    if (tab) selectTab(tab.id);
  }

  function stepTab(step: 1 | -1): void {
    const list = tabs();
    const index = list.findIndex((tab) => tab.id === activeTab()?.id);
    const next = list[(index + step + list.length) % list.length];
    if (next) selectTab(next.id);
  }

  function closeTab(id: string): void {
    const tab = tabs().find((item) => item.id === id);
    if (!tab) return;
    const list = tabs();
    const index = list.indexOf(tab);
    release(placements(tab.root));
    const next = list.filter((item) => item.id !== id);
    const neighbour = next[Math.min(index, next.length - 1)];
    commit(next, id === activeId() ? (neighbour?.id ?? "") : activeId());
  }

  function claim(placement: Placement | undefined): void {
    if (!placement) return;
    setOwners((current) =>
      current[placement.terminalId] === placement.id
        ? current
        : { ...current, [placement.terminalId]: placement.id },
    );
  }

  function sizeOwner(placement: Placement): boolean {
    const tab = activeTab();
    if (!tab) return false;
    const visible = tab.zoomedId
      ? placements(tab.root).filter((item) => item.id === tab.zoomedId)
      : placements(tab.root);
    const siblings = visible.filter((item) => item.terminalId === placement.terminalId);
    const claimed = siblings.find((item) => item.id === owners()[placement.terminalId]);
    return (claimed ?? siblings[0])?.id === placement.id;
  }

  function focusPlacement(tabId: string, placementId: string): void {
    const tab = tabs().find((item) => item.id === tabId);
    const placement = tab ? findPlacement(tab.root, placementId) : undefined;
    if (!tab || !placement) return;
    claim(placement);
    if (tab.focusedId === placementId && activeId() === tabId) return;
    commit(
      tabs().map((item) => {
        if (item.id !== tabId) return item;
        // Navigation must move the visible zoom as well as keyboard focus.
        const next = { ...item, focusedId: placementId };
        if (item.zoomedId) next.zoomedId = placementId;
        return next;
      }),
      tabId,
    );
  }

  function focusDirection(direction: Direction): void {
    const tab = activeTab();
    if (!tab) return;
    const target = neighbor(tab.root, tab.focusedId, direction);
    if (target) focusPlacement(tab.id, target);
  }

  function cyclePane(step: 1 | -1): void {
    const tab = activeTab();
    if (!tab) return;
    const target = cycle(tab.root, tab.focusedId, step);
    if (target) focusPlacement(tab.id, target);
  }

  function toggleZoom(): void {
    const tab = activeTab();
    if (!tab || placements(tab.root).length < 2) return;
    updateTab(tab.id, (item) => {
      const next = { ...item };
      if (item.zoomedId) delete next.zoomedId;
      else next.zoomedId = item.focusedId;
      return next;
    });
  }

  function destinationAlive(destination: Destination): boolean {
    if (destination.kind === "tab") return true;
    const tab = tabs().find((item) => item.id === destination.tabId);
    return !!tab && !!findPlacement(tab.root, destination.targetId);
  }

  function land(terminalId: string, destination: Destination): void {
    const placement = place(terminalId);
    claim(placement);
    // A split whose target closed before the reply becomes its own tab, never
    // a split of whatever pane happens to be focused now.
    if (destination.kind === "split" && destinationAlive(destination)) {
      const tabId = destination.tabId;
      commit(
        tabs().map((tab) => {
          if (tab.id !== tabId) return tab;
          const { zoomedId: _zoom, ...rest } = tab;
          return {
            ...rest,
            root: splitAt(
              tab.root,
              destination.targetId,
              placement,
              destination.axis,
              destination.before,
            ),
            focusedId: placement.id,
          };
        }),
        tabId,
      );
      return;
    }
    const tab: DeskTab = { id: newId("tab"), root: leaf(placement), focusedId: placement.id };
    commit([...tabs(), tab], tab.id);
  }

  /**
   * The grid a new terminal will get, predicted from the focused pane, so the
   * shell starts at its real size instead of redrawing after a resize. The
   * pane's fit corrects any off-by-one once it paints.
   */
  function predictedSize(destination: Destination): { cols: number; rows: number } | undefined {
    if (!bridge.server()?.features.includes("spawn_initial_size")) return undefined;
    const placement = focused();
    if (!placement?.viewId) return undefined;
    let info: { cols: number; rows: number };
    try {
      info = bridge.client().viewInfo(placement.viewId);
    } catch {
      return undefined;
    }
    if (info.cols < 4 || info.rows < 4) return undefined;
    if (destination.kind === "tab") return { cols: info.cols, rows: info.rows };
    // Splits are a hairline, not a per-pane header, so neither axis loses rows
    // to chrome. The divider is one column on a horizontal split.
    return destination.axis === "row"
      ? { cols: Math.floor((info.cols - 1) / 2), rows: info.rows }
      : { cols: info.cols, rows: Math.floor((info.rows - 1) / 2) };
  }

  function request(destination: Destination, cwd?: string, sessionId?: number): void {
    const options: SpawnRequest = {};
    const focusedDirectory = focusedCwd();
    const place = placeFor(
      focusedDirectory === undefined ? undefined : { directory: focusedDirectory },
      cwd === undefined ? undefined : { directory: cwd },
    );
    if (place.directory) options.cwd = place.directory;
    // New tabs and splits join the focused pane's session unless the caller
    // named one. The bridge uses the window session only when this is unset.
    const inherited = sessionId ?? focusedPane()?.sessionId;
    if (inherited !== undefined) options.sessionId = inherited;
    const size = predictedSize(destination);
    if (size && size.cols >= 2 && size.rows >= 1) options.initialSize = size;
    const id = bridge.spawn(options);
    if (id === undefined) {
      hooks.notify("error", "Not connected", "Wait for the server, or reconnect with ⌘R.");
      return;
    }
    spawns.set(id, destination);
    setPending(spawns.size + awaiting.size);
  }

  function focusedPane(): DesktopPane | undefined {
    const placement = focused();
    if (!placement) return undefined;
    return bridge.panes().find((pane) => pane.terminalId === placement.terminalId);
  }

  function focusedCwd(): string | undefined {
    return focusedPane()?.cwd ?? undefined;
  }

  function newTerminal(cwd?: string): void {
    request({ kind: "tab" }, cwd);
  }

  function newTerminalIn(sessionId: number, cwd?: string): void {
    request({ kind: "tab" }, cwd, sessionId);
  }

  function split(axis: Axis, before = false): void {
    const tab = activeTab();
    const target = focused();
    if (!tab || !target) {
      newTerminal();
      return;
    }
    request({ kind: "split", tabId: tab.id, targetId: target.id, axis, before }, focusedCwd());
  }

  function duplicateView(): void {
    const tab = activeTab();
    const target = focused();
    if (!tab || !target) return;
    const placement = place(target.terminalId);
    claim(placement);
    // Like a split, a new view must be visible, so it ends any zoom.
    updateTab(tab.id, ({ zoomedId: _zoom, ...item }) => ({
      ...item,
      root: splitAt(item.root, target.id, placement, "row"),
      focusedId: placement.id,
    }));
  }

  function closePane(placementId?: string): void {
    const tab = activeTab();
    const id = placementId ?? tab?.focusedId;
    if (!tab || !id) return;
    const placement = findPlacement(tab.root, id);
    if (!placement) return;
    release([placement]);
    const root = removeLeaf(tab.root, id);
    if (!root) {
      closeTab(tab.id);
      return;
    }
    const order = placements(tab.root).map((item) => item.id);
    updateTab(tab.id, (item) => ({
      ...item,
      root,
      focusedId: focusAfterRemoval(order, item.focusedId, new Set([id])),
    }));
  }

  function reveal(terminalId: string): boolean {
    for (const tab of tabs()) {
      const found = placements(tab.root).find((item) => item.terminalId === terminalId);
      if (found) {
        focusPlacement(tab.id, found.id);
        return true;
      }
    }
    return false;
  }

  function open(pane: DesktopPane): void {
    if (reveal(pane.terminalId)) return;
    if (bridge.ready(pane.terminalId)) {
      land(pane.terminalId, { kind: "tab" });
      return;
    }
    // A pane in another session is not subscribed yet: attach, then place it
    // once the runtime reports it ready.
    bridge.client().attachTerminalPreservingGeometry(pane.terminalId);
    awaiting.set(pane.terminalId, { kind: "tab" });
    setPending(spawns.size + awaiting.size);
  }

  function terminate(terminalId: string): void {
    const info = bridge.server();
    if (!info) return;
    bridge
      .client()
      .terminateTerminal(
        { serverId: info.serverId, connectionEpoch: info.connectionEpoch },
        terminalId,
      );
  }

  function handle(events: DesktopEvent[]): void {
    syncServer();
    for (const event of events) {
      if (event.kind === "SpawnAnswered") answered(event.requestId, event.terminalId, event.error);
      if (event.kind === "AttachAnswered" && event.error) {
        awaiting.delete(event.terminalId);
        hooks.notify("error", "Could not open terminal", event.error);
      }
      if (event.kind === "Closed") closed(event.terminalId);
      if (event.kind === "TerminalKilled" && event.error) {
        hooks.notify("error", "Terminate refused", event.error);
      }
    }
    setPending(spawns.size + awaiting.size);
  }

  function answered(requestId: number, terminalId?: string, error?: string): void {
    const destination = spawns.get(requestId);
    if (!destination) return;
    spawns.delete(requestId);
    if (error || !terminalId) {
      hooks.notify("error", "Could not start a terminal", error ?? "The server named no terminal.");
      return;
    }
    awaiting.set(terminalId, destination);
  }

  function closed(terminalId: string): void {
    closedTerminals.add(terminalId);
    awaiting.delete(terminalId);
    const doomed = tabs().flatMap((tab) =>
      placements(tab.root).filter((placement) => placement.terminalId === terminalId),
    );
    if (doomed.length === 0) return;
    release(doomed);
    commit(withoutTerminal(tabs(), terminalId));
  }

  /** After every drain: place ready terminals and finish a pending restore. */
  function settle(): void {
    syncServer();
    if (bridge.status() !== "Attached") return;
    for (const [terminalId, destination] of awaiting) {
      if (!bridge.ready(terminalId)) continue;
      awaiting.delete(terminalId);
      if (!reveal(terminalId) || destination.kind === "split") land(terminalId, destination);
    }
    setPending(spawns.size + awaiting.size);
    if (!restored) finishRestore();
    resolveRestored();
  }

  function finishRestore(): void {
    const server = bridge.server();
    if (!server) return;
    restored = true;
    const layout = restoring?.serverId === server.serverId ? restoring : undefined;
    restoring = undefined;
    const next = (layout?.tabs ?? []).flatMap(restoreTab);
    if (layout) {
      const active = next.find((tab) => tab.id === layout?.activeTab)?.id ?? next[0]?.id ?? "";
      commit(next, active);
      return;
    }
    openInitial();
  }

  function restoreTab(saved: SavedTab): DeskTab[] {
    const root = rebuild(saved.root);
    if (!root) return [];
    const items = placements(root);
    const focus =
      items.find((placement) => placement.id === saved.focusedId) ??
      items.find((placement) => placement.terminalId === saved.focusedTerminal) ??
      items[0];
    claim(focus);
    const tab: DeskTab = { id: saved.id, root, focusedId: focus?.id ?? "" };
    if (saved.title) tab.title = saved.title;
    return [tab];
  }

  function openInitial(): void {
    const initial = bridge.panes().find((pane) => pane.terminalId === options.initialTerminal);
    if (initial) {
      open(initial);
      return;
    }
    if (options.fresh) {
      request({ kind: "tab" });
      return;
    }
    const home = bridge.panes().find((pane) => pane.sessionName === bridge.target.sessionName);
    if (home && bridge.ready(home.terminalId)) land(home.terminalId, { kind: "tab" });
    else if (home) awaiting.set(home.terminalId, { kind: "tab" });
    else request({ kind: "tab" });
  }

  function rebuild(node: SavedNode): LayoutNode | undefined {
    if (node.kind === "leaf") {
      if (closedTerminals.has(node.terminalId)) return undefined;
      return leaf(place(node.terminalId, node.id));
    }
    const first = rebuild(node.first);
    const second = rebuild(node.second);
    if (!first || !second) return first ?? second;
    return { kind: "split", id: newId("split"), axis: node.axis, ratio: node.ratio, first, second };
  }

  /** Missing is not Closed: keep the leaf and fill only its runtime handle later. */
  function resolveNode(node: LayoutNode, live: Set<string>): LayoutNode {
    if (node.kind === "leaf") {
      const placement = node.placement;
      if (!unresolved.has(placement.id)) return node;
      if (!bridge.ready(placement.terminalId)) {
        attachRestored(placement.terminalId, live);
        return node;
      }
      const ready = place(placement.terminalId, placement.id);
      unresolved.delete(placement.id);
      return leaf(ready);
    }
    const first = resolveNode(node.first, live);
    const second = resolveNode(node.second, live);
    return first === node.first && second === node.second ? node : { ...node, first, second };
  }

  function attachRestored(terminalId: string, live: Set<string>): void {
    if (!live.has(terminalId) || attaching.has(terminalId)) return;
    attaching.add(terminalId);
    bridge.client().attachTerminalPreservingGeometry(terminalId);
  }

  function resolveRestored(): void {
    if (unresolved.size === 0) return;
    const live = new Set(bridge.panes().map((pane) => pane.terminalId));
    // Runs on every drain while a saved pane is unavailable: keep the same
    // array unless a leaf resolved, or every pane re-renders (and the window
    // redraws) on each wake.
    setTabs((current) => {
      let changed = false;
      const next = current.map((tab) => {
        const root = resolveNode(tab.root, live);
        if (root === tab.root) return tab;
        changed = true;
        return { ...tab, root };
      });
      return changed ? next : current;
    });
  }

  function syncServer(): void {
    const current = bridge.server()?.serverId;
    if (!current || current === serverId) return;
    if (serverId !== undefined) {
      releaseAll();
      restoring = undefined;
      restored = false;
    }
    serverId = current;
  }

  function restore(layout: SavedLayout | undefined): void {
    restoring = layout;
    restored = false;
  }

  function releaseAll(): void {
    release(tabs().flatMap((tab) => placements(tab.root)));
    spawns.clear();
    awaiting.clear();
    unresolved.clear();
    attaching.clear();
    closedTerminals.clear();
    batch(() => {
      setTabs([]);
      setActiveId("");
      setOwners({});
      setPending(0);
    });
  }

  return {
    tabs,
    activeId: () => activeTab()?.id ?? "",
    activeTab,
    focused,
    pending,
    selectTab,
    stepTab,
    selectIndex,
    moveTab: (from, to) => commit(reorder(tabs(), from, to)),
    renameTab: (id, title) =>
      updateTab(id, (tab) => {
        const next = { ...tab };
        if (title.trim()) next.title = title.trim().slice(0, 80);
        else delete next.title;
        return next;
      }),
    closeTab,
    focusPlacement,
    focusDirection,
    cyclePane,
    toggleZoom,
    nudge: (direction) => {
      const tab = activeTab();
      if (!tab) return;
      updateTab(tab.id, (item) => ({
        ...item,
        root: nudge(item.root, item.focusedId, direction, 0.04),
      }));
    },
    equalizeTab: () => {
      const tab = activeTab();
      if (tab) updateTab(tab.id, (item) => ({ ...item, root: equalize(item.root) }));
    },
    resizeSplit: (splitId, ratio) => {
      const tab = activeTab();
      if (!tab) return;
      setTabs(
        tabs().map((item) =>
          item.id === tab.id ? { ...item, root: setRatio(item.root, splitId, ratio) } : item,
        ),
      );
    },
    newTerminal,
    newTerminalIn,
    split,
    duplicateView,
    closePane,
    reveal: (terminalId) => {
      reveal(terminalId);
    },
    open,
    terminate,
    handle,
    settle,
    restore,
    snapshot: () => ({ tabs: tabs(), activeId: activeId() }),
    serverId: () => serverId,
    releaseAll,
    sizeOwner,
    viewsOf: (terminalId) =>
      tabs().reduce(
        (count, tab) =>
          count +
          placements(tab.root).filter((placement) => placement.terminalId === terminalId).length,
        0,
      ),
  };
}
