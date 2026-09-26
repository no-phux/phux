import { createEffect, createSignal, For, onCleanup, onMount, Show, type Accessor, type JSX } from "solid-js";
import { render, resetRender, useGpuix } from "@gpuix/solid";
import type {
  DesktopClient,
  DesktopEvent,
  DesktopPane,
  DesktopSearchMatch,
  DesktopSession,
  DesktopServerInfo,
} from "../native/generated/index";
import {
  focusedPlacement,
  newId,
  parseLayout,
  saveLayout,
  type DisplayPrefs,
  type DeskTab,
  type Placement,
} from "./workspace";

export interface LayoutStore {
  read(): unknown;
  write(layout: unknown): void;
}

interface Host {
  DesktopClient: new () => DesktopClient;
}

interface AppProps {
  host: Host;
  socketPath: string;
  sessionName: string;
  layouts: LayoutStore;
}

function DesktopApp(props: AppProps): JSX.Element {
  const [status, setStatus] = createSignal("disconnected");
  const [detail, setDetail] = createSignal("");
  const [panes, setPanes] = createSignal<DesktopPane[]>([]);
  const [tabs, setTabs] = createSignal<DeskTab[]>([]);
  const [tabId, setTabId] = createSignal("");
  const [revision, setRevision] = createSignal(0);
  const [query, setQuery] = createSignal("");
  const [hits, setHits] = createSignal<DesktopSearchMatch[]>([]);
  const [hit, setHit] = createSignal(0);
  const [matches, setMatches] = createSignal("search idle");
  const [fenced, setFenced] = createSignal(false);
  const [identity, setIdentity] = createSignal<DesktopServerInfo | undefined>();
  const [palette, setPalette] = createSignal(false);
  const [badges, setBadges] = createSignal<Record<string, string>>({});
  const [fontSize, setFontSize] = createSignal(14);
  const [optionAsAlt, setOptionAsAlt] = createSignal(false);
  let owner: DesktopClient | undefined;
  let pendingNew = false;
  const shortcutBridge: { run: (chord: string) => void } = { run: () => {} };
  shortcutBridge.run = runShortcut;
  const gpuix = useGpuix();
  const [generation, setGeneration] = createSignal(0);
  let closed = false;

  function session(): DesktopClient {
    generation();
    if (!owner) throw new Error("Desktop client is not connected");
    return owner;
  }

  function replace(): void {
    owner = new props.host.DesktopClient();
    setGeneration((value) => value + 1);
  }

  function selectedTab(): DeskTab | undefined {
    return tabs().find((tab) => tab.id === tabId()) ?? tabs()[0];
  }

  function selectedPlacement(): Placement | undefined {
    const tab = selectedTab();
    return tab ? focusedPlacement(tab) : undefined;
  }

  function persist(): void {
    const server = identity();
    if (!server) return;
    props.layouts.write(saveLayout(server.serverId, tabs(), displayPrefs()));
  }

  function accept(batch: DesktopEvent[]): void {
    for (const event of batch) {
      if (event.kind === "ServerError") setDetail(event.message);
      if (event.kind === "SpawnAnswered" && event.error) {
        pendingNew = false;
        setDetail(event.error);
      }
      if (event.kind === "InputDelivery" && event.outcome === "Unknown") setFenced(true);
      if (event.kind === "Closed") removeTerminal(event.terminalId);
      if (event.kind === "AgentBadge") {
        setBadges((current) => ({ ...current, [event.terminalId]: event.name }));
      }
      if (event.kind === "TopologyChanged" || event.kind === "TerminalChanged") watchBadges();
    }
  }

  function removeTerminal(terminalId: string): void {
    setTabs((current) =>
      current
        .map((tab) => ({
          ...tab,
          placements: tab.placements.filter((placement) => placement.terminalId !== terminalId),
        }))
        .filter((tab) => tab.placements.length > 0),
    );
  }

  function refresh(): void {
    setPanes(session().topology()?.panes ?? []);
    const failure = session().lastError();
    setStatus(failure ? `failed: ${failure}` : session().status());
    const info = session().serverInfo();
    if (info) setIdentity(info);
    const placement = selectedPlacement();
    if (placement) setFenced(session().inputReadiness(placement.terminalId).deliveryFenced);
    openPendingTerminal();
    setRevision((value) => value + 1);
  }

  function openPendingTerminal(): void {
    if (!pendingNew) return;
    const open = new Set(tabs().flatMap((tab) => tab.placements.map((item) => item.terminalId)));
    const fresh = panes().find(
      (pane) => !open.has(pane.terminalId) && session().inputReadiness(pane.terminalId).ready,
    );
    if (!fresh) return;
    pendingNew = false;
    openTerminal(fresh);
  }

  function activity(handle: string): void {
    if (closed || handle !== session().handle) return;
    accept(session().takeEvents());
    refresh();
    ensureTerminal();
  }

  function changeFont(delta: number): void {
    setFontSize((size) => Math.min(28, Math.max(10, size + delta)));
    persist();
  }

  function toggleOptionAsAlt(): void {
    setOptionAsAlt((enabled) => !enabled);
    persist();
  }

  function displayPrefs(): DisplayPrefs {
    return { fontSize: fontSize(), optionAsAlt: optionAsAlt() };
  }

  function dragToWindow(terminalId: string): void {
    const tab = selectedTab();
    const placement = tab?.placements.find((item) => item.terminalId === terminalId);
    const opener = globalThis.phuxOpenWindow;
    if (!tab || !placement || !opener) return;
    setTabs((current) =>
      current
        .map((item) =>
          item.id === tab.id
            ? { ...item, placements: item.placements.filter((item) => item.id !== placement.id) }
            : item,
        )
        .filter((item) => item.placements.length > 0),
    );
    opener({
      clientHandle: session().handle,
      terminalId: placement.terminalId,
      viewId: placement.viewId,
      title: placement.terminalId,
    });
  }

  function watchBadges(): void {
    for (const pane of session().topology()?.panes ?? []) session().watchAgent(pane.terminalId);
  }

  function connect(): void {
    session().connect(
      { socketPath: props.socketPath, cols: 120, rows: 36, sessionName: props.sessionName },
      activity,
    );
  }

  function retry(): void {
    if (!closed) {
      closed = true;
      accept(session().close());
      closed = false;
    }
    replace();
    connect();
  }

  function homeSession(): DesktopSession | undefined {
    return session()
      .topology()
      ?.sessions.find((item) => item.name === props.sessionName);
  }

  function ensureTerminal(): void {
    if (session().status() !== "Attached" || tabs().some((tab) => tab.placements.length > 0))
      return;
    const saved = parseLayout(props.layouts.read());
    const server = session().serverInfo();
    if (saved && server && saved.serverId === server.serverId) {
      restore(saved);
      if (tabs().length > 0) return;
    }
    const pane = session()
      .topology()
      ?.panes.find((item) => item.sessionName === props.sessionName);
    if (!pane) {
      const home = homeSession();
      if (home) {
        pendingNew = true;
        session().spawnTerminal(home.id);
      }
      return;
    }
    if (session().inputReadiness(pane.terminalId).ready) openTerminal(pane);
  }

  function newTerminal(): void {
    if (session().status() !== "Attached") return;
    const home = homeSession();
    if (!home) return;
    pendingNew = true;
    session().spawnTerminal(home.id);
  }

  function focusTerminal(pane: DesktopPane): void {
    for (const tab of tabs()) {
      const placement = tab.placements.find((item) => item.terminalId === pane.terminalId);
      if (!placement) continue;
      setTabId(tab.id);
      setTabs((current) =>
        current.map((item) => (item.id === tab.id ? { ...item, focusedId: placement.id } : item)),
      );
      return;
    }
    openTerminal(pane);
  }

  function focusPaneIndex(index: number): void {
    const pane = panes()[index];
    if (pane) focusTerminal(pane);
  }

  function openTerminal(pane: DesktopPane): void {
    const placement = place(pane.terminalId);
    const tab: DeskTab = {
      id: newId("tab"),
      title: pane.title ?? pane.sessionName,
      placements: [placement],
      focusedId: placement.id,
    };
    setTabs((current) => [...current, tab]);
    setTabId(tab.id);
    persist();
  }

  function place(terminalId: string): Placement {
    return { id: newId("place"), terminalId, viewId: session().createView(terminalId) };
  }

  function anotherView(): void {
    const tab = selectedTab();
    const focused = selectedPlacement();
    if (!tab || !focused) return;
    const placement = place(focused.terminalId);
    setTabs((current) =>
      current.map((item) =>
        item.id === tab.id
          ? { ...item, placements: [...item.placements, placement], focusedId: placement.id }
          : item,
      ),
    );
    persist();
  }

  function closeView(): void {
    const tab = selectedTab();
    const focused = selectedPlacement();
    if (!tab || !focused) return;
    session().destroyView(focused.viewId);
    setTabs((current) =>
      current
        .map((item) => {
          if (item.id !== tab.id) return item;
          const placements = item.placements.filter((placement) => placement.id !== focused.id);
          const next = placements[0];
          return { ...item, placements, focusedId: next ? next.id : "" };
        })
        .filter((item) => item.placements.length > 0),
    );
    persist();
  }

  function terminate(): void {
    const focused = selectedPlacement();
    const server = identity();
    if (!focused || !server) return;
    session().terminateTerminal(
      { serverId: server.serverId, connectionEpoch: server.connectionEpoch },
      focused.terminalId,
    );
  }

  function follow(): void {
    const focused = selectedPlacement();
    if (focused) session().followLiveView(focused.viewId);
  }

  function search(): void {
    const focused = selectedPlacement();
    if (!focused) return;
    const found = session().searchView(focused.viewId, query(), false);
    setHits(found);
    setHit(0);
    showHit(found, 0);
  }

  function showHit(found: DesktopSearchMatch[], index: number): void {
    const focused = selectedPlacement();
    const match = found[index];
    if (!focused || !match) {
      setMatches(found.length === 0 ? "0 matches" : "search idle");
      return;
    }
    session().setViewSelection(focused.viewId, match.start, match.end, false);
    setMatches(`${index + 1} of ${found.length}`);
  }

  function runShortcut(chord: string): void {
    if (chord === "t" || chord === "n") newTerminal();
    else if (chord === "r") retry();
    else if (chord === "w") closeView();
    else if (chord === "d") anotherView();
    else if (chord === "f") search();
    else if (chord === "g") stepHit(1);
    else if (chord === "shift+g") stepHit(-1);
    else if (chord === "=" || chord === "+") changeFont(1);
    else if (chord === "-") changeFont(-1);
    else if (chord >= "1" && chord <= "9") focusPaneIndex(Number(chord) - 1);
  }

  function stepHit(delta: number): void {
    const found = hits();
    if (found.length === 0) {
      search();
      return;
    }
    const index = (hit() + delta + found.length) % found.length;
    setHit(index);
    showHit(found, index);
  }

  function restore(layout: ReturnType<typeof parseLayout>): void {
    if (!layout) return;
    const live = new Set((session().topology()?.panes ?? []).map((pane) => pane.terminalId));
    const restored = layout.tabs.flatMap((tab) => {
      const placements = tab.terminals.flatMap((terminalId) =>
        live.has(terminalId) && session().inputReadiness(terminalId).ready
          ? [place(terminalId)]
          : [],
      );
      const focused = placements.find((placement) => placement.terminalId === tab.focusedTerminal);
      const first = placements[0];
      if (!first) return [];
      return [{ id: tab.id, title: tab.title, placements, focusedId: focused?.id ?? first.id }];
    });
    const first = restored[0];
    if (!first) return;
    setTabs(restored);
    setTabId(first.id);
  }

  createEffect(() => {
    const placement = selectedPlacement();
    const pane = panes().find((item) => item.terminalId === placement?.terminalId);
    const title = pane?.title || placement?.terminalId || props.sessionName;
    gpuix?.renderer.setWindowTitle?.(`phux — ${title}`);
  });

  onMount(() => {
    globalThis.phuxShortcut = (chord) => {
      shortcutBridge.run(chord);
    };
    const saved = parseLayout(props.layouts.read());
    if (saved) {
      setFontSize(saved.display.fontSize);
      setOptionAsAlt(saved.display.optionAsAlt);
    }
    replace();
    connect();
  });
  onCleanup(() => {
    globalThis.phuxShortcut = undefined;
    if (closed) return;
    closed = true;
    for (const tab of tabs()) {
      for (const placement of tab.placements) session().destroyView(placement.viewId);
    }
    accept(session().close());
  });

  return (
    <div
      style={{ display: "flex", height: "100%", backgroundColor: "#15171c", color: "#eeeeee" }}
      onKeyDown={(event) => runShortcut(chordFrom(event))}
    >
      <div style={{ width: 220, padding: 12, backgroundColor: "#101218" }}>
        <text>{`Status: ${status()}`}</text>
        <text>{detail() || props.socketPath}</text>
        <For each={panes()}>
          {(pane): JSX.Element => (
            <div
              style={{ padding: 8, cursor: "pointer" }}
              onClick={() => focusTerminal(pane)}
              onMouseDown={(event) => beginPaneDrag(pane.terminalId, event)}
              onMouseUp={(event) => {
                const dragged = endPaneDrag(event);
                if (dragged) dragToWindow(dragged.terminalId);
              }}
            >
              <text>{`${badges()[pane.terminalId] ? `${badges()[pane.terminalId]} · ` : ""}${pane.title ?? pane.terminalId}`}</text>
              <text>{pane.cwd ?? pane.sessionName}</text>
            </div>
          )}
        </For>
      </div>
      <div style={{ display: "flex", flexDirection: "column", flexGrow: 1, padding: 12, gap: 8 }}>
        <div style={{ display: "flex", gap: 8 }}>
          <Command label="Retry" run={retry} />
          <Command label="New terminal" run={newTerminal} />
          <Command label="Another view" run={anotherView} />
          <Command label="Close view" run={closeView} />
          <Command label="Terminate" run={terminate} />
          <Command label="Follow live" run={follow} />
          <Command label="Commands" run={() => setPalette((open) => !open)} />
          <Command label="Smaller" run={() => changeFont(-1)} />
          <Command label="Larger" run={() => changeFont(1)} />
          <Command label="Option as Alt" run={toggleOptionAsAlt} />
        </div>
        <text>⌘T new · ⌘W close · ⌘D split view · ⌘F search · ⌘G next · ⌘R reconnect · ⌘1–9 jump</text>
        <Show when={palette()}>
          <text>
            ⌘T new terminal. ⌘W close view. ⌘D another view. ⌘F search. ⌘G next. ⌘1–9 focus a
            pane. Close leaves the process. Terminate kills that terminal only.
          </text>
        </Show>
        <Show when={fenced()}>
          <text>
            Delivery unknown. Wait for a fresh presented frame before trusting another command.
          </text>
        </Show>
        <div style={{ display: "flex", gap: 8 }}>
          <input
            value={query()}
            onChange={(event) => setQuery(event.value ?? "")}
            style={{ height: 32, width: 240 }}
          />
          <Command label="Search" run={search} />
          <Command label="Next" run={() => stepHit(1)} />
          <Command label="Previous" run={() => stepHit(-1)} />
          <text>{matches()}</text>
        </div>
        <Show when={selectedTab()}>
          {(tab: Accessor<DeskTab>): JSX.Element => (
            <div style={{ display: "flex", gap: 8, flexGrow: 1 }}>
              <For each={tab().placements}>
                {(placement): JSX.Element => (
                  <phux-terminal
                    clientHandle={session().handle}
                    terminalId={placement.terminalId}
                    viewId={placement.viewId}
                    paintRevision={revision()}
                    focused={placement.id === tab().focusedId}
                    optionAsAlt={optionAsAlt()}
                    font={{ family: "Menlo", size: fontSize(), lineHeight: 1.25 }}
                    style={{ width: 640, height: 480, flexGrow: 1 }}
                  />
                )}
              </For>
            </div>
          )}
        </Show>
      </div>
    </div>
  );
}

let paneDrag: { terminalId: string; x: number; y: number } | undefined;

function beginPaneDrag(terminalId: string, event: { x?: number; y?: number }): void {
  paneDrag = { terminalId, x: event.x ?? 0, y: event.y ?? 0 };
}

function endPaneDrag(event: { x?: number; y?: number }): { terminalId: string } | undefined {
  const start = paneDrag;
  paneDrag = undefined;
  if (!start) return undefined;
  const x = event.x ?? 0;
  const y = event.y ?? 0;
  const moved = Math.hypot(x - start.x, y - start.y);
  // The rail is 220px. A click stays in the rail; a drag into the terminal opens a window.
  if (moved < 48 || x < 280) return undefined;
  return { terminalId: start.terminalId };
}

function chordFrom(event: {
  isHeld?: boolean;
  key?: string;
  modifiers?: { cmd: boolean; alt: boolean; ctrl: boolean; shift: boolean };
}): string {
  if (event.isHeld || !event.modifiers?.cmd || event.modifiers.alt || event.modifiers.ctrl) return "";
  const key = event.key;
  if (!key) return "";
  return event.modifiers.shift ? `shift+${key}` : key;
}

function Command(props: { label: string; run: () => void }): JSX.Element {
  return (
    <div
      onClick={() => props.run()}
      style={{ padding: 8, backgroundColor: "#28374d", cursor: "pointer" }}
    >
      <text>{props.label}</text>
    </div>
  );
}

export function mount(
  host: Host,
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
      width: 1280,
      height: 800,
      focus: false,
      onKeyDown(event) {
        globalThis.phuxShortcut?.(chordFrom(event));
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
