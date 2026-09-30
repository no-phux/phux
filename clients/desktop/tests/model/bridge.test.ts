import { describe, expect, test } from "bun:test";
import { createRoot } from "solid-js";
import type { DesktopEvent } from "../../native/generated/index";
import { createBridge, structural, type DesktopHost } from "../../src/bridge/desktop";

/** A native client that records which snapshots a wake read. */
class FakeClient {
  static last: FakeClient | undefined;
  handle = "fake-1";
  queued: DesktopEvent[] = [];
  topologyReads = 0;
  wake: (from: string) => void = () => {};

  constructor() {
    FakeClient.last = this;
  }
  connect(_options: unknown, activity: (from: string) => void): void {
    this.wake = activity;
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
  serverInfo(): null {
    return null;
  }
  inputReadiness(): { ready: boolean; deliveryFenced: boolean } {
    return { ready: true, deliveryFenced: false };
  }
}

function wake(client: FakeClient, events: DesktopEvent[]): void {
  client.queued = events;
  client.wake(client.handle);
}

describe("bridge wakes", () => {
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
});
