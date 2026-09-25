import { createSignal, For, onCleanup, onMount, Show, type Accessor, type JSX } from "solid-js";
import { render, resetRender } from "@gpuix/solid";
import type { DesktopClient, DesktopEvent, DesktopPane, DesktopServerInfo } from "../native/generated/index";
import {
  focusedPlacement,
  newId,
  parseLayout,
  saveLayout,
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
  const [matches, setMatches] = createSignal("search idle");
  const [fenced, setFenced] = createSignal(false);
  const [identity, setIdentity] = createSignal<DesktopServerInfo | undefined>();
  const [palette, setPalette] = createSignal(false);
  const [badges, setBadges] = createSignal<Record<string, string>>({});
  const [fontSize, setFontSize] = createSignal(14);
  const [optionAsAlt, setOptionAsAlt] = createSignal(false);
  let owner: DesktopClient | undefined;
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
    props.layouts.write(saveLayout(server.serverId, tabs()));
  }

  function accept(batch: DesktopEvent[]): void {
    for (const event of batch) {
      if (event.kind === "ServerError") setDetail(event.message);
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
    setRevision((value) => value + 1);
  }

  function activity(handle: string): void {
    if (closed || handle !== session().handle) return;
    accept(session().takeEvents());
    refresh();
    ensureTerminal();
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

  function ensureTerminal(): void {
    if (session().status() !== "Attached" || tabs().some((tab) => tab.placements.length > 0)) return;
    const saved = parseLayout(props.layouts.read());
    const server = session().serverInfo();
    if (saved && server && saved.serverId === server.serverId) {
      restore(saved);
      if (tabs().length > 0) return;
    }
    const pane = session().topology()?.panes[0];
    if (!pane) {
      const home = session().topology()?.sessions[0];
      if (home) session().spawnTerminal(home.id);
      return;
    }
    if (session().inputReadiness(pane.terminalId).ready) openTerminal(pane);
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
    setMatches(`${found.length} matches`);
    const first = found[0];
    if (first) session().setViewSelection(focused.viewId, first.start, first.end, false);
  }

  function restore(layout: ReturnType<typeof parseLayout>): void {
    if (!layout) return;
    const live = new Set((session().topology()?.panes ?? []).map((pane) => pane.terminalId));
    const restored = layout.tabs.flatMap((tab) => {
      const placements = tab.terminals.flatMap((terminalId) =>
        live.has(terminalId) && session().inputReadiness(terminalId).ready ? [place(terminalId)] : [],
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

  onMount(() => {
    replace();
    connect();
  });
  onCleanup(() => {
    if (closed) return;
    closed = true;
    for (const tab of tabs()) {
      for (const placement of tab.placements) session().destroyView(placement.viewId);
    }
    accept(session().close());
  });

  return (
    <div style={{ display: "flex", height: "100%", backgroundColor: "#15171c", color: "#eeeeee" }}>
      <div style={{ width: 220, padding: 12, backgroundColor: "#101218" }}>
        <text>{`Status: ${status()}`}</text>
        <text>{detail() || props.socketPath}</text>
        <For each={panes()}>
          {(pane): JSX.Element => (
            <div
              style={{ padding: 8, cursor: "pointer" }}
              onClick={() => openTerminal(pane)}
              onMouseUp={(event) => {
                if ((event.x ?? 0) > 240) dragToWindow(pane.terminalId);
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
          <Command label="New terminal" run={ensureTerminal} />
          <Command label="Another view" run={anotherView} />
          <Command label="Close view" run={closeView} />
          <Command label="Terminate" run={terminate} />
          <Command label="Follow live" run={follow} />
          <Command label="Commands" run={() => setPalette((open) => !open)} />
          <Command label="Smaller" run={() => setFontSize((size) => Math.max(10, size - 1))} />
          <Command label="Larger" run={() => setFontSize((size) => Math.min(28, size + 1))} />
          <Command label="Option as Alt" run={() => setOptionAsAlt((enabled) => !enabled)} />
        </div>
        <Show when={palette()}>
          <text>Close drops the view and leaves the process. Terminate kills that terminal only. Unknown delivery is never resent.</text>
        </Show>
        <Show when={fenced()}>
          <text>Delivery unknown. Wait for a fresh presented frame before trusting another command.</text>
        </Show>
        <div style={{ display: "flex", gap: 8 }}>
          <input
            value={query()}
            onChange={(event) => setQuery(event.value ?? "")}
            style={{ height: 32, width: 240 }}
          />
          <Command label="Search" run={search} />
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

function Command(props: { label: string; run: () => void }): JSX.Element {
  return (
    <div onClick={() => props.run()} style={{ padding: 8, backgroundColor: "#28374d", cursor: "pointer" }}>
      <text>{props.label}</text>
    </div>
  );
}

export function mount(host: Host, socketPath: string, sessionName: string, layouts: LayoutStore): void {
  render((): JSX.Element => <DesktopApp host={host} socketPath={socketPath} sessionName={sessionName} layouts={layouts} />, {
    title: "phux",
    width: 1280,
    height: 800,
    focus: false,
    onUncaughtError: (error) => {
      resetRender();
      throw error;
    },
  });
  process.once("SIGTERM", () => {
    resetRender();
    process.exit(0);
  });
}
