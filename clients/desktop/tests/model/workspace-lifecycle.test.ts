import { describe, expect, test } from "bun:test";
import { createRoot } from "solid-js";
import type { DesktopEvent, DesktopPane, DesktopTopology } from "../../native/generated/index";
import { createBridge, type DesktopHost } from "../../src/bridge/desktop";
import { createWorkspace, type Workspace } from "../../src/workspace/controller";
import { leaf, placements, splitAt } from "../../src/workspace/layout";
import {
  defaultDisplay,
  parseLayout,
  saveLayout,
  type SavedLayout,
} from "../../src/workspace/persist";

/** The native boundary only: tests exercise the real bridge and workspace together. */
class LifecycleClient {
  static last: LifecycleClient | undefined;
  handle = "lifecycle-client";
  info = { serverId: "server-a", connectionEpoch: "1", features: [] };
  panes: DesktopPane[] = [];
  ready = new Set<string>();
  views = new Map<string, string>();
  attached: string[] = [];
  destroyed: string[] = [];
  requests: number[] = [];
  queued: DesktopEvent[] = [];
  wake: (handle: string) => void = () => {};
  private nextView = 0;
  private closed = false;

  constructor() {
    LifecycleClient.last = this;
  }
  connect(_options: unknown, activity: (handle: string) => void): void {
    this.wake = activity;
  }
  close(): DesktopEvent[] {
    this.closed = true;
    this.views.clear();
    return [];
  }
  takeEvents(): DesktopEvent[] {
    const events = this.queued;
    this.queued = [];
    return events;
  }
  takePathAnswers(): never[] {
    return [];
  }
  status(): string {
    return "Attached";
  }
  lastError(): null {
    return null;
  }
  connectionEpoch(): string {
    if (this.closed) throw new Error("StaleHandle");
    return this.info.connectionEpoch;
  }
  serverInfo(): { serverId: string; connectionEpoch: string; features: string[] } {
    if (this.closed) throw new Error("StaleHandle");
    return this.info;
  }
  topology(): DesktopTopology {
    return {
      sessions: [{ id: 1, name: "home", windowCount: 1, attachedClientCount: 1 }],
      panes: this.panes,
      focusedPane: this.panes[0]?.terminalId ?? "",
    };
  }
  deliveryFenced(): boolean {
    return false;
  }

  inputReadiness(terminalId: string): { ready: boolean; deliveryFenced: boolean } {
    if (!/^local:\d+$/.test(terminalId)) throw new Error("InvalidResourceId");
    return { ready: this.ready.has(terminalId), deliveryFenced: false };
  }
  createView(terminalId: string): string {
    if (!this.ready.has(terminalId)) throw new Error("terminal has no renderable replica");
    const id = String(++this.nextView);
    this.views.set(id, `${this.info.serverId}/${terminalId}`);
    return id;
  }
  destroyView(id: string): void {
    if (!this.views.delete(id)) throw new Error("unknown view");
    this.destroyed.push(id);
  }
  attachTerminalPreservingGeometry(terminalId: string): number {
    this.attached.push(terminalId);
    return this.attached.length;
  }
  spawnTerminalWithOptions(_options: unknown): number {
    const id = this.requests.length + 1;
    this.requests.push(id);
    return id;
  }
}

function pane(terminalId: string, sessionName = "home"): DesktopPane {
  return {
    terminalId,
    sessionId: 1,
    sessionName,
    windowId: 1,
    windowIndex: 0,
    windowName: "",
    isFocused: false,
  };
}

function saved(root: SavedLayout["tabs"][number]["root"]): SavedLayout {
  return {
    version: 2,
    serverId: "server-a",
    activeTab: "saved-tab",
    tabs: [{ id: "saved-tab", title: "build", root, focusedTerminal: "local:2" }],
    display: defaultDisplay,
  };
}

interface Scenario {
  client: LifecycleClient;
  workspace: Workspace;
  snapshots: SavedLayout[];
  wake: (events?: DesktopEvent[]) => void;
  close: () => void;
}

function scenario(layout: SavedLayout | undefined, run: (context: Scenario) => void): void {
  createRoot((dispose) => {
    const DesktopClient: unknown = LifecycleClient;
    // SAFETY: LifecycleClient implements every native operation these bridge/workspace scenarios use.
    const host = { DesktopClient } as DesktopHost;
    const bridge = createBridge(
      host,
      { socketPath: "/tmp/unused-lifecycle.sock", sessionName: "home" },
      0,
    );
    const snapshots: SavedLayout[] = [];
    const workspace = createWorkspace(bridge, {
      changed: () =>
        snapshots.push(
          saveLayout(
            bridge.server()?.serverId ?? "",
            workspace.tabs(),
            defaultDisplay,
            workspace.activeId(),
          ),
        ),
      notify: () => {},
    });
    workspace.restore(layout);
    bridge.onEvents((events) => workspace.handle(events));
    bridge.connect();
    const client = LifecycleClient.last;
    if (!client) throw new Error("native client was not constructed");
    const wake = (events: DesktopEvent[] = [{ kind: "TopologyChanged" }]): void => {
      client.queued = events;
      client.wake(client.handle);
      workspace.settle();
    };
    try {
      run({ client, workspace, snapshots, wake, close: () => bridge.close() });
    } finally {
      workspace.releaseAll();
      dispose();
    }
  });
}

function leaves(workspace: Workspace): Array<{ id: string; terminalId: string; viewId?: string }> {
  return workspace.tabs().flatMap((tab) => placements(tab.root));
}

const delayedSplit = (): SavedLayout =>
  saved({
    kind: "split",
    axis: "column",
    ratio: 0.3,
    first: { kind: "leaf", terminalId: "local:1" },
    second: { kind: "leaf", terminalId: "local:2" },
  });

describe("workspace restore lifecycle", () => {
  test("cycling and directional focus keep the selected pane visible while zoomed", () => {
    scenario(delayedSplit(), ({ client, workspace, wake }) => {
      client.panes = [pane("local:1"), pane("local:2")];
      client.ready = new Set(["local:1", "local:2"]);
      wake();
      workspace.toggleZoom();
      const original = workspace.focused();
      workspace.cyclePane(1);
      expect(workspace.focused()?.terminalId).toBe("local:1");
      expect(workspace.activeTab()?.zoomedId).toBe(workspace.focused()?.id);
      const focused = workspace.focused();
      if (!focused) throw new Error("expected the newly focused pane");
      expect(workspace.sizeOwner(focused)).toBe(true);
      workspace.focusDirection("down");
      expect(workspace.focused()).toEqual(original);
      expect(workspace.activeTab()?.zoomedId).toBe(original?.id);
      workspace.toggleZoom();
      expect(workspace.activeTab()?.zoomedId).toBeUndefined();
    });
  });

  test("revealing a hidden terminal in an inactive zoomed tab makes it visible", () => {
    scenario(delayedSplit(), ({ client, workspace, wake }) => {
      client.panes = [pane("local:1"), pane("local:2"), pane("local:3")];
      client.ready = new Set(["local:1", "local:2", "local:3"]);
      wake();
      workspace.toggleZoom();
      workspace.open(pane("local:3"));
      expect(workspace.activeId()).not.toBe("saved-tab");
      workspace.reveal("local:1");
      expect(workspace.activeId()).toBe("saved-tab");
      expect(workspace.focused()?.terminalId).toBe("local:1");
      expect(workspace.activeTab()?.zoomedId).toBe(workspace.focused()?.id);
      expect(client.views.size).toBe(3);
    });
  });

  test("a delayed split leaf keeps its position, title, focus and saved snapshot", () => {
    scenario(delayedSplit(), ({ client, workspace, snapshots, wake }) => {
      client.panes = [pane("local:1"), pane("local:2", "other")];
      client.ready.add("local:1");
      wake();
      expect(workspace.tabs().map((tab) => [tab.id, tab.title])).toEqual([["saved-tab", "build"]]);
      expect(leaves(workspace).map((item) => item.terminalId)).toEqual(["local:1", "local:2"]);
      expect(workspace.focused()?.terminalId).toBe("local:2");
      const pendingId = workspace.focused()?.id;
      expect(workspace.focused()?.viewId).toBeUndefined();
      expect(snapshots.at(-1)?.tabs[0]?.root.kind).toBe("split");
      expect(client.attached).toEqual(["local:2"]);
      client.ready.add("local:2");
      wake();
      expect(workspace.tabs()).toHaveLength(1);
      expect(workspace.focused()?.id).toBe(pendingId);
      expect(workspace.focused()?.viewId).toBeDefined();
      const root = workspace.tabs()[0]?.root;
      expect(root?.kind === "split" && [root.axis, root.ratio]).toEqual(["column", 0.3]);
    });
  });

  test("an entirely absent active tab survives topology lag without opening a replacement", () => {
    const layout = delayedSplit();
    layout.tabs.push({
      id: "absent-tab",
      title: "waiting",
      root: { kind: "leaf", terminalId: "local:9" },
      focusedTerminal: "local:9",
    });
    layout.activeTab = "absent-tab";
    scenario(layout, ({ client, workspace, wake }) => {
      client.panes = [pane("local:1")];
      client.ready.add("local:1");
      wake();
      expect(workspace.activeId()).toBe("absent-tab");
      expect(client.requests).toEqual([]);
      expect(leaves(workspace).map((item) => item.terminalId)).toEqual([
        "local:1",
        "local:2",
        "local:9",
      ]);
      client.panes.push(pane("local:9"));
      client.ready.add("local:9");
      wake();
      expect(workspace.activeId()).toBe("absent-tab");
      expect(workspace.focused()?.viewId).toBeDefined();
      expect(workspace.tabs()).toHaveLength(2);
    });
  });

  test("closing an unresolved pane or tab prevents later resurrection", () => {
    scenario(delayedSplit(), ({ client, workspace, wake }) => {
      client.panes = [pane("local:1")];
      client.ready.add("local:1");
      wake();
      workspace.closePane(workspace.focused()?.id);
      client.panes.push(pane("local:2"));
      client.ready.add("local:2");
      wake();
      expect(leaves(workspace).map((item) => item.terminalId)).toEqual(["local:1"]);
      expect(client.views.size).toBe(1);
    });
    scenario(saved({ kind: "leaf", terminalId: "local:2" }), ({ client, workspace, wake }) => {
      wake();
      workspace.closeTab("saved-tab");
      client.panes = [pane("local:2")];
      client.ready.add("local:2");
      wake();
      expect(workspace.tabs()).toEqual([]);
      expect(client.views.size).toBe(0);
    });
  });

  test("Closed removes unresolved leaves, but missing topology does not retire existing views", () => {
    scenario(delayedSplit(), ({ client, workspace, wake }) => {
      client.panes = [pane("local:1")];
      client.ready.add("local:1");
      wake();
      const liveView = leaves(workspace)[0]?.viewId;
      if (liveView === undefined) throw new Error("expected the initial ready view");
      client.panes = [];
      client.ready.clear();
      wake([{ kind: "Closed", terminalId: "local:2", reason: 0 }, { kind: "TopologyChanged" }]);
      expect(leaves(workspace).map((item) => item.terminalId)).toEqual(["local:1"]);
      expect(workspace.focused()?.viewId).toBe(liveView);
      expect(client.destroyed).toEqual([]);
      client.panes = [pane("local:1"), pane("local:2")];
      client.ready.add("local:1");
      client.ready.add("local:2");
      wake();
      expect(leaves(workspace).map((item) => item.terminalId)).toEqual(["local:1"]);
      wake([{ kind: "Closed", terminalId: "local:1", reason: 0 }]);
      expect(workspace.tabs()).toEqual([]);
      expect(client.destroyed).toEqual([liveView]);
    });
  });

  test("duplicate views restore separate handles and the second view's focus after readiness", () => {
    const root = splitAt(
      leaf({ id: "first", terminalId: "local:1", viewId: "old-1" }),
      "first",
      { id: "second", terminalId: "local:1", viewId: "old-2" },
      "row",
    );
    const layout = parseLayout(
      JSON.parse(
        JSON.stringify(
          saveLayout(
            "server-a",
            [{ id: "duplicates", root, focusedId: "second" }],
            defaultDisplay,
            "duplicates",
          ),
        ),
      ),
    );
    scenario(layout, ({ client, workspace, wake }) => {
      client.panes = [pane("local:1", "other")];
      wake();
      expect(leaves(workspace).map((item) => item.id)).toEqual(["first", "second"]);
      expect(workspace.focused()?.id).toBe("second");
      expect(client.attached).toEqual(["local:1"]);
      client.ready.add("local:1");
      wake();
      const items = leaves(workspace);
      expect(new Set(items.map((item) => item.viewId)).size).toBe(2);
      expect(workspace.focused()?.id).toBe("second");
      const [first, second] = items;
      if (!first || !second) throw new Error("duplicate placements were not restored");
      expect(workspace.sizeOwner(second)).toBe(true);
      expect(workspace.sizeOwner(first)).toBe(false);
    });
  });

  test("a same-server empty saved workspace stays empty across restore", () => {
    scenario(
      { version: 2, serverId: "server-a", tabs: [], display: defaultDisplay },
      ({ client, workspace, wake }) => {
        client.panes = [pane("local:1")];
        client.ready.add("local:1");
        wake();
        expect(workspace.tabs()).toEqual([]);
        expect(client.requests).toEqual([]);
        expect(client.views.size).toBe(0);
      },
    );
  });

  test("a new server retires old views, pending destinations and numeric terminal identities", () => {
    scenario(saved({ kind: "leaf", terminalId: "local:1" }), ({ client, workspace, wake }) => {
      client.panes = [pane("local:1")];
      client.ready.add("local:1");
      wake();
      const oldView = workspace.focused()?.viewId;
      if (oldView === undefined) throw new Error("expected the original server's view");
      workspace.split("row");
      wake([{ kind: "SpawnAnswered", requestId: 1, terminalId: "local:2" }]);
      workspace.split("column");
      client.info = { ...client.info, serverId: "server-b", connectionEpoch: "2" };
      client.panes = [pane("local:1"), pane("local:2"), pane("local:3")];
      client.ready = new Set(["local:1", "local:2", "local:3"]);
      wake([
        { kind: "SpawnAnswered", requestId: 2, terminalId: "local:3" },
        { kind: "TopologyChanged" },
      ]);
      expect(client.destroyed).toEqual([oldView]);
      expect(workspace.tabs().map((tab) => tab.id)).not.toContain("saved-tab");
      expect(leaves(workspace).map((item) => item.terminalId)).toEqual(["local:1"]);
      expect(workspace.pending()).toBe(0);
      expect(workspace.focused()?.viewId).not.toBe(oldView);
      expect([...client.views.values()]).toEqual(["server-b/local:1"]);
    });
  });

  test("same-server reconnect epochs keep independent live views", () => {
    scenario(saved({ kind: "leaf", terminalId: "local:1" }), ({ client, workspace, wake }) => {
      client.panes = [pane("local:1")];
      client.ready.add("local:1");
      wake();
      workspace.duplicateView();
      const before = leaves(workspace);
      client.info = { ...client.info, connectionEpoch: "2" };
      wake();
      expect(leaves(workspace)).toEqual(before);
      expect(client.destroyed).toEqual([]);
      expect(workspace.focused()?.id).toBe(before[1]?.id);
    });
  });

  test("an unrecognized saved terminal stays closable without reaching native ID parsing", () => {
    scenario(saved({ kind: "leaf", terminalId: "bad" }), ({ client, workspace, wake }) => {
      client.panes = [pane("local:1")];
      client.ready.add("local:1");
      wake();
      expect(workspace.focused()?.terminalId).toBe("bad");
      expect(workspace.focused()?.viewId).toBeUndefined();
      workspace.duplicateView();
      wake();
      expect(leaves(workspace).map((item) => item.terminalId)).toEqual(["bad", "bad"]);
      workspace.closeTab("saved-tab");
      expect(workspace.tabs()).toEqual([]);
      expect(client.views.size).toBe(0);
    });
  });

  test("native client shutdown preserves the saved layout without reading a retired handle", () => {
    scenario(
      saved({ kind: "leaf", terminalId: "local:1" }),
      ({ client, workspace, snapshots, wake, close }) => {
        client.panes = [pane("local:1")];
        client.ready.add("local:1");
        wake();
        const snapshot = snapshots.at(-1);
        close();
        expect(snapshots.at(-1)).toEqual(snapshot);
        expect(workspace.focused()?.terminalId).toBe("local:1");
      },
    );
  });

  test("a delayed attachment preserves tabs opened and renamed while it was pending", () => {
    scenario(delayedSplit(), ({ client, workspace, snapshots, wake }) => {
      client.panes = [pane("local:1"), pane("local:2", "other"), pane("local:3")];
      client.ready.add("local:1");
      client.ready.add("local:3");
      wake();
      workspace.open(pane("local:3"));
      const active = workspace.activeId();
      workspace.renameTab(active, "new work");
      expect(snapshots.at(-1)?.tabs[0]?.root.kind).toBe("split");
      client.ready.add("local:2");
      wake();
      expect(workspace.activeId()).toBe(active);
      expect(workspace.tabs().map((tab) => tab.title)).toEqual(["build", "new work"]);
      expect(leaves(workspace).map((item) => item.terminalId)).toEqual([
        "local:1",
        "local:2",
        "local:3",
      ]);
      expect(client.views.size).toBe(3);
    });
  });
});
