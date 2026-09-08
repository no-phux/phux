import { describe, expect, test } from "bun:test";
import {
  relaySessionSockets,
  startNative,
  type RelaySocket,
} from "./native-start";

const circuitConfig = { threshold: 3, windowMs: 60_000, openMs: 60_000 };

function harness(prepare: () => Promise<void>, status = 101) {
  const calls: string[] = [];
  const session = {
    async prepare() {
      calls.push("prepare");
      await prepare();
    },
    async fetch() {
      calls.push("fetch");
      return { status };
    },
    async abortStartup() {
      calls.push("abort");
    },
  };
  const cap = {
    async nativeFailed() {
      calls.push("failed");
    },
    async nativeSucceeded() {
      calls.push("succeeded");
    },
  };
  return { calls, session, cap };
}

async function run(
  setup: ReturnType<typeof harness>,
): Promise<Awaited<ReturnType<typeof startNative>>> {
  return startNative({
    sid: "sid",
    expiresAt: Date.now() + 300_000,
    startupTimeoutMs: 8_000,
    edgeTtlMs: 30_000,
    circuitConfig,
    request: new Request("https://example.test/"),
    session: setup.session,
    cap: setup.cap,
  });
}

describe("native start failover", () => {
  test("records success without releasing or destroying", async () => {
    const setup = harness(async () => {});
    expect(await run(setup)).toEqual({ response: { status: 101 } });
    expect(setup.calls).toEqual(["prepare", "fetch", "succeeded"]);
  });

  test("downgrades and destroys on startup error", async () => {
    const setup = harness(async () => {
      throw new Error("start failed");
    });
    expect(await run(setup)).toEqual({ fallbackReason: "startup-error" });
    expect(setup.calls).toEqual(["prepare", "failed", "abort"]);
  });

  test("classifies timeout and non-101 while accounting exactly once", async () => {
    const timeout = harness(async () => {
      throw new DOMException("timed out", "TimeoutError");
    });
    expect(await run(timeout)).toEqual({ fallbackReason: "startup-timeout" });
    expect(timeout.calls.filter((call) => call === "failed")).toHaveLength(1);
    expect(timeout.calls.filter((call) => call === "abort")).toHaveLength(1);

    const non101 = harness(async () => {}, 503);
    expect(await run(non101)).toEqual({ fallbackReason: "pre-upgrade-503" });
    expect(non101.calls).toEqual(["prepare", "fetch", "failed", "abort"]);
  });
});

class FakeSocket implements RelaySocket {
  sent: unknown[] = [];
  closed: [number | undefined, string | undefined][] = [];
  listeners = new Map<string, ((event: never) => void)[]>();

  send(data: unknown) {
    this.sent.push(data);
  }
  close(code?: number, reason?: string) {
    this.closed.push([code, reason]);
  }
  addEventListener(type: "message", listener: (event: MessageEvent) => void): void;
  addEventListener(type: "close", listener: (event: CloseEvent) => void): void;
  addEventListener(type: "error", listener: (event: Event) => void): void;
  addEventListener(type: string, listener: (event: never) => void) {
    const listeners = this.listeners.get(type) ?? [];
    listeners.push(listener);
    this.listeners.set(type, listeners);
  }
  emit(type: string, event: { data?: unknown; code?: number } = {}) {
    for (const listener of this.listeners.get(type) ?? []) listener(event as never);
  }
}

describe("native websocket relay", () => {
  test("sends metadata first and preserves subsequent binary frames", () => {
    const browser = new FakeSocket();
    const upstream = new FakeSocket();
    let closed = 0;
    relaySessionSockets(browser, upstream, "session-info", () => closed++);
    const upstreamBytes = new Uint8Array([1, 2, 3]);
    upstream.emit("message", { data: upstreamBytes });
    const browserBytes = new Uint8Array([4, 5, 6]).buffer;
    browser.emit("message", { data: browserBytes });
    expect(browser.sent).toEqual(["session-info", upstreamBytes]);
    expect(upstream.sent).toEqual([browserBytes]);
    upstream.emit("close", { code: 1000 });
    expect(browser.closed).toEqual([[1000, ""]]);
    expect(closed).toBe(1);
  });

  test("does not expose upstream text or close reasons", () => {
    const browser = new FakeSocket();
    const upstream = new FakeSocket();
    relaySessionSockets(browser, upstream, "session-info", () => {});
    upstream.emit("message", { data: "secret upstream error" });
    upstream.emit("close", { code: 1013 });
    expect(browser.sent).toEqual(["session-info"]);
    expect(browser.closed).toEqual([[1011, "native session protocol error"]]);
  });
});
