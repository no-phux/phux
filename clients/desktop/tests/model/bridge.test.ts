import { describe, expect, test } from "bun:test";
import { createRoot } from "solid-js";
import type { DesktopEvent } from "../../native/generated/index";
import { createBridge, refusedInput, structural, type DesktopHost } from "../../src/bridge/desktop";

/** A native client that records which snapshots a wake read. */
class FakeClient {
  static last: FakeClient | undefined;
  static nextHandle = 0;
  handle = `fake-${++FakeClient.nextHandle}`;
  info = { serverId: "server-a", connectionEpoch: "1", features: [] };
  queued: DesktopEvent[] = [];
  topologyReads = 0;
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
  takeEvents(): DesktopEvent[] {
    const events = this.queued;
    this.queued = [];
    return events;
  }
  takePathAnswers(): never[] {
    return [];
  }
  topology(): { sessions: never[]; panes: never[] } {
    this.topologyReads += 1;
    return { sessions: [], panes: [] };
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
  inputReadiness(): { ready: boolean; deliveryFenced: boolean } {
    return { ready: true, deliveryFenced: false };
  }
}

function wake(client: FakeClient, events: DesktopEvent[]): void {
  client.queued = events;
  client.wake(client.handle);
}

function badge(terminalId: string, name = "claude"): DesktopEvent {
  return { kind: "AgentBadge", terminalId, name, state: "working", attention: "low" };
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
    const bridge = createBridge(host, { socketPath: "/tmp/fake.sock", sessionName: "s" });
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
      client.info = { ...client.info, connectionEpoch: "2" };
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
      wake(client, [
        badge("local:1", "retired-agent"),
        connecting(),
        connecting(),
        badge("local:2", "current-agent"),
      ]);
      expect(Object.keys(bridge.agents())).toEqual(["local:2"]);
      expect(bridge.agents()["local:2"]?.name).toBe("current-agent");
    });
  });

  test("identity advancing after the drain cannot attribute that batch's badges to the new server", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1")]);
      client.info = { ...client.info, serverId: "server-b", connectionEpoch: "2" };
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1", "retired-agent")]);
      expect(bridge.agents()).toEqual({});
      wake(client, [connecting(), badge("local:1", "current-agent")]);
      expect(bridge.agents()["local:1"]?.name).toBe("current-agent");
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

  test("an ambiguous overflow replacement batch waits for later metadata instead of old labels", () => {
    scenario((bridge, client) => {
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1")]);
      client.info = { ...client.info, serverId: "server-b", connectionEpoch: "2" };
      wake(client, [{ kind: "TopologyChanged" }, badge("local:1", "current-agent")]);
      expect(bridge.agents()).toEqual({});
      wake(client, [badge("local:1", "current-agent")]);
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
      const revision = bridge.revision();
      wake(client, [badge("local:1")]);
      expect(bridge.agents()).toEqual({});
      expect(bridge.revision()).toBe(revision);
    });
  });

  test("output and agent badges are not structural; lifecycle is", () => {
    expect(structural({ kind: "TerminalChanged", terminalId: "local:1" })).toBe(false);
    expect(
      structural({
        kind: "AgentBadge",
        terminalId: "local:1",
        name: "claude",
        state: "working",
        attention: "low",
      }),
    ).toBe(false);
    expect(structural({ kind: "TopologyChanged" })).toBe(true);
    expect(structural({ kind: "PaneSpawned", terminalId: "local:2" })).toBe(true);
  });

  test("a flood of output never re-reads the topology, but still bumps paint", () => {
    const DesktopClient: unknown = FakeClient;
    // SAFETY: the bridge constructs only DesktopClient and calls only the
    // methods FakeClient implements.
    const host = { DesktopClient } as DesktopHost;
    createRoot((dispose) => {
      const bridge = createBridge(host, { socketPath: "/tmp/fake.sock", sessionName: "s" });
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
