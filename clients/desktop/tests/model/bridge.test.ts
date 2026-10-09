import { describe, expect, test } from "bun:test";
import { createRoot } from "solid-js";
import type {
  DesktopEvent,
  DesktopSession,
  DesktopSpawnOptions,
} from "../../native/generated/index";
import { createBridge, refusedInput, structural, type DesktopHost } from "../../src/bridge/desktop";

/** A native client that records which snapshots a wake read. */
class FakeClient {
  static last: FakeClient | undefined;
  static nextHandle = 0;
  handle = `fake-${++FakeClient.nextHandle}`;
  info = { serverId: "server-a", connectionEpoch: "1", features: [] };
  queued: DesktopEvent[] = [];
  afterDrain: (() => void) | undefined;
  topologyReads = 0;
  sessions: DesktopSession[] = [];
  spawned: DesktopSpawnOptions[] = [];
  wake: (from: string) => void = () => {};

  constructor() {
    FakeClient.last = this;
  }
  connect(_options: unknown, activity: (from: string) => void): void {
    this.wake = activity;
  }
  close(): DesktopEvent[] {
    return [];
  }
  connectionEpoch(): string {
    return this.info.connectionEpoch;
  }
  takeEvents(): DesktopEvent[] {
    const events = this.queued;
    this.queued = [];
    this.afterDrain?.();
    this.afterDrain = undefined;
    return events;
  }
  takePathAnswers(): never[] {
    return [];
  }
  topology(): { sessions: DesktopSession[]; panes: never[] } {
    this.topologyReads += 1;
    return { sessions: this.sessions, panes: [] };
  }
  spawnTerminalWithOptions(options: DesktopSpawnOptions): number {
    this.spawned.push(options);
    return this.spawned.length;
  }
  status(): string {
    return "Attached";
  }
  lastError(): null {
    return null;
  }
  serverInfo(): { serverId: string; connectionEpoch: string; features: string[] } {
    return this.info;
  }
  deliveryFenced(): boolean {
    return false;
  }

  inputReadiness(): { ready: boolean; deliveryFenced: boolean } {
    return { ready: true, deliveryFenced: false };
  }
}

function wake(client: FakeClient, events: DesktopEvent[]): void {
  client.queued = events;
  client.wake(client.handle);
}

function badge(terminalId: string, name = "claude"): Extract<DesktopEvent, { kind: "AgentBadge" }> {
  return {
    kind: "AgentBadge",
    terminalId,
    name,
    state: "working",
    attention: "low",
    stateReading: "recognized",
    attentionReading: "declared",
  };
}

function connecting(): DesktopEvent {
  const event: unknown = { kind: "StatusChanged", status: "Connecting" };
  // SAFETY: DesktopStatus is a string enum; both runtime Connecting and Negotiated project here.
  return event as DesktopEvent;
}

function scenario(
  run: (bridge: ReturnType<typeof createBridge>, client: FakeClient) => void,
): void {
  const DesktopClient: unknown = FakeClient;
  // SAFETY: FakeClient implements every native operation these bridge scenarios use.
  const host = { DesktopClient } as DesktopHost;
  createRoot((dispose) => {
    const bridge = createBridge(host, { socketPath: "/tmp/fake.sock", sessionName: "s" }, 0);
    bridge.connect();
    const client = FakeClient.last;
    if (!client) throw new Error("no client");
    try {
      run(bridge, client);
    } finally {
      bridge.close();
      dispose();
    }
  });
}

describe("bridge wakes", () => {
  test("server replacement retires badges before accepting the new server's batch", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1"), badge("local:2")]);
      const original = bridge.agents();
      wake(client, [{ kind: "TopologyChanged" }]);
      expect(bridge.agents()).toEqual(original);

      client.info = { ...client.info, serverId: "server-b", connectionEpoch: "3" };
      wake(client, [connecting(), { kind: "TopologyChanged" }, badge("local:2", "codex")]);
      expect(Object.keys(bridge.agents())).toEqual(["local:2"]);
      expect(bridge.agents()["local:2"]?.name).toBe("codex");
    });
  });

  test("a delayed drain cannot replay a retired server's badge across connection boundaries", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1")]);
      client.info = { ...client.info, serverId: "server-b", connectionEpoch: "2" };
      let received: DesktopEvent[] = [];
      bridge.onEvents((events) => {
        received = events;
      });
      wake(client, [
        badge("local:1", "retired-agent"),
        connecting(),
        connecting(),
        badge("local:2", "current-agent"),
      ]);
      expect(Object.keys(bridge.agents())).toEqual(["local:2"]);
      expect(bridge.agents()["local:2"]?.name).toBe("current-agent");
      expect(received.filter((event) => event.kind === "AgentBadge")).toEqual([
        badge("local:2", "current-agent"),
      ]);
    });
  });

  test("identity advancing after the drain cannot attribute that batch's badges to the new server", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1")]);
      client.afterDrain = () => {
        client.info = { ...client.info, serverId: "server-b", connectionEpoch: "2" };
      };
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1", "retired-agent")]);
      expect(bridge.agents()).toEqual({});
      wake(client, [connecting(), badge("local:1", "current-agent")]);
      expect(bridge.agents()["local:1"]?.name).toBe("current-agent");
    });
  });

  test("an old Connecting event cannot qualify badges when identity advances after the drain", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1")]);
      client.afterDrain = () => {
        client.info = { ...client.info, serverId: "server-b", connectionEpoch: "2" };
      };
      let received: DesktopEvent[] = [];
      bridge.onEvents((events) => {
        received = events;
      });
      wake(client, [connecting(), badge("local:1", "retired-agent")]);
      expect(bridge.server()?.serverId).toBe("server-b");
      expect(bridge.agents()).toEqual({});
      expect(received.filter((event) => event.kind === "AgentBadge")).toEqual([]);
      wake(client, [connecting(), badge("local:1", "current-agent")]);
      expect(bridge.agents()["local:1"]?.name).toBe("current-agent");
    });
  });

  test("same-server epoch changes after the drain cannot replay stale badges", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1")]);
      client.afterDrain = () => {
        client.info = { ...client.info, connectionEpoch: "2" };
      };
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1", "retired-agent")]);
      expect(bridge.agents()).toEqual({});
      wake(client, [badge("local:1", "current-agent")]);
      expect(bridge.agents()["local:1"]?.name).toBe("current-agent");
    });
  });

  test("same-server overflow retires labels even when the connection boundary was dropped", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1"), badge("local:2")]);
      client.info = { ...client.info, connectionEpoch: "2" };
      wake(client, [{ kind: "TopologyChanged" }, badge("local:2", "current-agent")]);
      expect(Object.keys(bridge.agents())).toEqual(["local:2"]);
      expect(bridge.agents()["local:2"]?.name).toBe("current-agent");
    });
  });

  test("same-server connection boundaries rebuild badges from replayed metadata", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1"), badge("local:2")]);
      client.info = { ...client.info, connectionEpoch: "2" };
      wake(client, [connecting(), badge("local:2", "current-agent")]);
      expect(Object.keys(bridge.agents())).toEqual(["local:2"]);
      expect(bridge.agents()["local:2"]?.name).toBe("current-agent");
    });
  });

  test("a stable overflow replacement batch accepts current metadata without a boundary", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1")]);
      client.info = { ...client.info, serverId: "server-b", connectionEpoch: "2" };
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1", "current-agent")]);
      expect(bridge.agents()["local:1"]?.name).toBe("current-agent");
    });
  });

  test("closing a terminal retires only its badge, even before topology catches up", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1"), badge("local:2")]);
      wake(client, [{ kind: "Closed", terminalId: "local:1", reason: 0 }]);
      expect(Object.keys(bridge.agents())).toEqual(["local:2"]);
      wake(client, [badge("local:2", "")]);
      expect(bridge.agents()).toEqual({});
    });
  });

  test("explicit reconnect clears badges and ignores late wakes from the retired client", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1")]);
      bridge.reconnect();
      expect(bridge.agents()).toEqual({});
      expect(bridge.server()).toBeUndefined();
      const revision = bridge.revision();
      wake(client, [badge("local:1")]);
      expect(bridge.agents()).toEqual({});
      expect(bridge.revision()).toBe(revision);
      const replacement = FakeClient.last;
      if (!replacement || replacement === client) throw new Error("expected a new client");
      replacement.info = { ...replacement.info, serverId: "server-b" };
      wake(replacement, [connecting(), badge("local:1", "current-agent")]);
      expect(bridge.server()?.serverId).toBe("server-b");
      expect(bridge.agents()["local:1"]?.name).toBe("current-agent");
    });
  });

  test("paint revisions move only for terminals a wake can repaint", () => {
    scenario((bridge, client) => {
      const paint = (): [number, number] => [
        bridge.paintRevision("local:1"),
        bridge.paintRevision("local:2"),
      ];
      wake(client, [{ kind: "TopologyChanged" }]);
      const [one, two] = paint();
      wake(client, [{ kind: "TerminalChanged", terminalId: "local:1" }]);
      const [output, untouched] = paint();
      expect(output).toBeGreaterThan(one);
      expect(untouched).toBe(two);
      wake(client, [badge("local:2")]);
      expect(paint()).toEqual([output, untouched]);
      wake(client, [{ kind: "PaneSpawned", terminalId: "local:3" }]);
      const [first, second] = paint();
      expect(first).toBeGreaterThan(output);
      expect(second).toBe(first);
      // A wake with no events changed state no event names: repaint all.
      wake(client, []);
      const [third, fourth] = paint();
      expect(third).toBeGreaterThan(first);
      expect(fourth).toBe(third);
    });
  });

  test("a paint revision never repeats, even across a closed terminal", () => {
    scenario((bridge, client) => {
      const seen = [bridge.paintRevision("local:1")];
      const events: DesktopEvent[][] = [
        [{ kind: "TerminalChanged", terminalId: "local:1" }],
        [{ kind: "TerminalChanged", terminalId: "local:2" }],
        [{ kind: "Closed", terminalId: "local:1", reason: 0 }],
        // A retained final frame still arrives as output after Closed.
        [{ kind: "TerminalChanged", terminalId: "local:1" }],
        [{ kind: "TopologyChanged" }],
      ];
      for (const batch of events) {
        wake(client, batch);
        seen.push(bridge.paintRevision("local:1"));
      }
      // Unchanged only where the batch named another terminal's output.
      expect(seen[2]).toBe(seen[1]);
      const moved = seen.filter((_, index) => index !== 2);
      expect(new Set(moved).size).toBe(moved.length);
      expect(moved).toEqual([...moved].sort((a, b) => a - b));
    });
  });

  test("wakes inside one frame coalesce into a single drain at its end", async () => {
    const DesktopClient: unknown = FakeClient;
    // SAFETY: FakeClient implements every native operation these bridge scenarios use.
    const host = { DesktopClient } as DesktopHost;
    const bridge = createBridge(host, { socketPath: "/tmp/fake.sock", sessionName: "s" }, 20);
    bridge.connect();
    const client = FakeClient.last;
    if (!client) throw new Error("no client");
    const start = bridge.revision();
    wake(client, [{ kind: "TerminalChanged", terminalId: "local:1" }]);
    expect(bridge.revision()).toBe(start + 1);
    // Inside the frame: deferred, and repeated wakes add no second drain.
    wake(client, [{ kind: "TerminalChanged", terminalId: "local:1" }]);
    client.wake(client.handle);
    expect(bridge.revision()).toBe(start + 1);
    await Bun.sleep(40);
    expect(bridge.revision()).toBe(start + 2);
    expect(client.queued).toEqual([]);
    // A deferred drain for a retired client must not swallow the new one's wake.
    wake(client, [{ kind: "TerminalChanged", terminalId: "local:1" }]);
    wake(client, []);
    bridge.reconnect();
    const replacement = FakeClient.last;
    if (!replacement || replacement === client) throw new Error("expected a new client");
    const before = bridge.revision();
    wake(replacement, [{ kind: "TerminalChanged", terminalId: "local:1" }]);
    await Bun.sleep(40);
    expect(bridge.revision()).toBeGreaterThan(before);
    expect(replacement.queued).toEqual([]);
    bridge.close();
  });

  test("output and agent badges are not structural; lifecycle is", () => {
    expect(structural({ kind: "TerminalChanged", terminalId: "local:1" })).toBe(false);
    expect(structural(badge("local:1"))).toBe(false);
    expect(structural({ kind: "TopologyChanged" })).toBe(true);
    expect(structural({ kind: "PaneSpawned", terminalId: "local:2" })).toBe(true);
  });

  test("a flood of output never re-reads the topology, but still bumps paint", () => {
    const DesktopClient: unknown = FakeClient;
    // SAFETY: the bridge constructs only DesktopClient and calls only the
    // methods FakeClient implements.
    const host = { DesktopClient } as DesktopHost;
    createRoot((dispose) => {
      const bridge = createBridge(host, { socketPath: "/tmp/fake.sock", sessionName: "s" }, 0);
      bridge.connect();
      const client = FakeClient.last;
      if (!client) throw new Error("no client");
      wake(client, [{ kind: "TopologyChanged" }]);
      expect(client.topologyReads).toBe(1);
      const before = bridge.revision();
      for (let index = 0; index < 50; index += 1) {
        wake(client, [{ kind: "TerminalChanged", terminalId: "local:1" }]);
      }
      wake(client, []);
      expect(client.topologyReads).toBe(1);
      expect(bridge.revision()).toBe(before + 51);
      wake(client, [
        { kind: "TerminalChanged", terminalId: "local:1" },
        { kind: "PaneSpawned", terminalId: "local:2" },
      ]);
      expect(client.topologyReads).toBe(2);
      dispose();
    });
  });

  test("a refused paste has a reason to show; other receipts do not", () => {
    const receipt = (outcome: string, message: string): DesktopEvent => {
      const event: unknown = { kind: "InputDelivery", deliveryId: "7", outcome, message };
      // SAFETY: the literal matches the InputDelivery variant; its const-enum
      // outcome is a plain string at runtime.
      return event as DesktopEvent;
    };
    expect(refusedInput(receipt("Refused", "input exceeds the limit"))).toBe(
      "input exceeds the limit",
    );
    expect(refusedInput(receipt("Refused", ""))).toBe("The terminal did not accept the input.");
    expect(refusedInput(receipt("Delivered", ""))).toBeUndefined();
    expect(refusedInput(receipt("Unknown", "lost"))).toBeUndefined();
    expect(refusedInput({ kind: "TopologyChanged" })).toBeUndefined();
  });
});

describe("bridge spawn", () => {
  const session = (id: number, name: string): DesktopSession => ({
    id,
    name,
    windowCount: 1,
    attachedClientCount: 0,
  });

  test("spawns in the named session, in the window session when unnamed, and nowhere when the id is missing", () => {
    scenario((bridge, client) => {
      // The scenario's target session is "s".
      client.sessions = [session(1, "s"), session(2, "projB")];
      wake(client, [{ kind: "TopologyChanged" }]);
      bridge.spawn({ sessionId: 2, cwd: "/work/projB" });
      bridge.spawn({});
      expect(bridge.spawn({ sessionId: 99 })).toBeUndefined();
      expect(client.spawned.map((options) => options.sessionId)).toEqual([2, 1]);
      expect(client.spawned[0]).toEqual({
        cwd: "/work/projB",
        identity: { serverId: "server-a", connectionEpoch: "1" },
        sessionId: 2,
      });
    });
  });

  test("a requested session still spawns when the home session is absent", () => {
    scenario((bridge, client) => {
      client.sessions = [session(2, "projB")];
      wake(client, [{ kind: "TopologyChanged" }]);
      expect(bridge.spawn({ sessionId: 2 })).toBe(1);
      expect(bridge.spawn({})).toBeUndefined();
      expect(client.spawned.map((options) => options.sessionId)).toEqual([2]);
    });
  });
});
