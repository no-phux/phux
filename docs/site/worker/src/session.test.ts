// Local harness for the session object: pairing admission, view routing, and the
// lifetime clocks, without a deployment.
//
// These rules are where the feature can go quietly wrong (a code that stays
// spendable, an attachment's frames routed to nobody, an idle clock that closes
// a live shell), and finding them in production costs a 15-minute deploy each
// time. The fakes are deliberately dumb: a storage map, a socket that records
// what it was sent, and the runtime's accept/get list.

import { beforeEach, describe, expect, mock, test } from "bun:test";
import { readFile } from "node:fs/promises";
import { mintToken } from "./token";

const SESSION_SECRET = "session-test-secret";
const IDLE_KILL_MS = 120_000;
const HARD_MAX_MS = 600_000;

interface Attachment {
  version: 1;
  idleDeadline: number;
  expiresAt: number;
  seq: number;
  role?: "primary" | "attach";
  attachIds?: number[];
}

class FakeSocket {
  /** The other end of this socket: what one sends, the other receives. */
  peer: FakeSocket | null = null;
  sent: (string | Uint8Array)[] = [];
  closed: { code: number; reason: string } | null = null;
  private attachment: Attachment | null = null;

  accept(): void {}

  send(data: string | ArrayBuffer | Uint8Array): void {
    const frame = typeof data === "string" ? data : new Uint8Array(data);
    (this.peer ?? this).sent.push(frame);
  }

  close(code = 1000, reason = ""): void {
    const far = this.peer ?? this;
    if (!far.closed) far.closed = { code, reason };
  }

  serializeAttachment(value: Attachment): void {
    this.attachment = structuredClone(value);
  }

  deserializeAttachment(): Attachment | null {
    return this.attachment ? structuredClone(this.attachment) : null;
  }

  /** Test hook: pretend this view has been silent since `deadline`. */
  stale(deadline = Date.now() - 1): void {
    if (this.attachment) this.attachment.idleDeadline = deadline;
  }

  envelope(): { pair?: { code: string; expiresAt: number }; [key: string]: unknown } {
    const text = this.sent.find((entry): entry is string => typeof entry === "string");
    return text ? (JSON.parse(text) as { pair?: { code: string; expiresAt: number } }) : {};
  }

  output(): string {
    const decoder = new TextDecoder();
    return this.sent
      .filter((entry): entry is Uint8Array => typeof entry !== "string")
      .map((frame) => decoder.decode(frame))
      .join("");
  }

  /** Attachment frames this view received, with the attach id each carried. */
  addressed(): { type: number; attach: number | undefined }[] {
    return this.sent
      .filter((entry): entry is Uint8Array => typeof entry !== "string")
      .map((frame) => ({ type: frameType(frame) ?? -1, attach: attachIdOf(frame) }))
      .filter((frame) => frame.type === 0x81 || frame.type === 0x83);
  }
}

class FakeStorage {
  private readonly values = new Map<string, unknown>();
  alarm: number | null = null;

  async get<T>(key: string): Promise<T | undefined> {
    return structuredClone(this.values.get(key)) as T | undefined;
  }

  async put<T>(key: string, value: T): Promise<void> {
    this.values.set(key, structuredClone(value));
  }

  async deleteAll(): Promise<void> {
    this.values.clear();
  }

  async setAlarm(at: number): Promise<void> {
    this.alarm = at;
  }

  async getAlarm(): Promise<number | null> {
    return this.alarm;
  }

  async deleteAlarm(): Promise<void> {
    this.alarm = null;
  }
}

interface SessionLike {
  prepare(sid: string, mode: string, snapshot?: unknown, reason?: string): Promise<void>;
  fetch(request: Request): Promise<Response>;
  pairStatus(code: string): Promise<{ linked: boolean; expiresAt: number }>;
  webSocketMessage(ws: unknown, message: string | ArrayBuffer): Promise<void>;
  webSocketClose(ws: unknown, code: number, reason: string, clean: boolean): Promise<void>;
  alarm(): Promise<void>;
}

interface View {
  /** The socket the browser would hold. */
  client: FakeSocket;
  /** The socket the object holds, for feeding frames back in. */
  server: FakeSocket;
  envelope: { pair?: { code: string }; [key: string]: unknown };
  response: Response;
}

interface Harness {
  session: SessionLike;
  storage: FakeStorage;
  releases: () => number;
  sessionToken(sid: string): Promise<string>;
  /** Open a view; the object's WebSocketPair is stubbed for the call. */
  open(sid: string, options?: { attach?: string; token?: string }): Promise<View>;
  alarm(): number | null;
}

/** The runtime pieces the object imports; mocked before it loads. */
async function loadSessionDO(): Promise<new (ctx: unknown, env: unknown) => SessionLike> {
  mock.module("cloudflare:workers", () => ({
    DurableObject: class {
      ctx: unknown;
      env: unknown;
      constructor(ctx: unknown, env: unknown) {
        this.ctx = ctx;
        this.env = env;
      }
    },
  }));
  // The edge server's WASM is only touched by a message, and wasm-bindgen's
  // initialization does not settle inside Bun's test runner, so these tests stay
  // on the object's own rules (admission, clocks, view lifetime). Frame routing
  // is verified end to end by scripts/smoke-pair.ts instead.
  const bytes = await readFile(new URL("../edge/phux_edge_bg.wasm", import.meta.url));
  mock.module("../edge/phux_edge_bg.wasm", () => ({ default: new Uint8Array(bytes) }));
  const module = await import("./session");
  return module.SessionDO as unknown as new (ctx: unknown, env: unknown) => SessionLike;
}

async function buildHarness(): Promise<Harness> {
  const SessionDO = await loadSessionDO();
  const storage = new FakeStorage();
  const sockets: FakeSocket[] = [];
  let releases = 0;
  const ctx = {
    storage,
    acceptWebSocket: (ws: FakeSocket) => {
      sockets.push(ws);
    },
    getWebSockets: () => [...sockets],
    blockConcurrencyWhile: <T>(fn: () => Promise<T>) => fn(),
  };
  const env = {
    SESSION_TOKEN_SECRET: SESSION_SECRET,
    IDLE_KILL_MS: String(IDLE_KILL_MS),
    HARD_MAX_MS: String(HARD_MAX_MS),
    GLOBAL_CAP: {
      getByName: () => ({
        release: async () => {
          releases += 1;
        },
      }),
    },
  };
  const session = new SessionDO(ctx, env);
  const globals = globalThis as { WebSocketPair?: unknown };

  return {
    session,
    storage,
    releases: () => releases,
    alarm: () => storage.alarm,
    sessionToken: (sid) =>
      mintToken(SESSION_SECRET, { sid, exp: Date.now() + 30_000, scope: "session" }),
    async open(sid, options = {}) {
      const client = new FakeSocket();
      const server = new FakeSocket();
      server.peer = client;
      client.peer = server;
      const previous = globals.WebSocketPair;
      globals.WebSocketPair = class {
        0 = client;
        1 = server;
      };
      try {
        const headers: Record<string, string> = {
          Upgrade: "websocket",
          "X-Phux-Session": sid,
        };
        if (options.attach) {
          headers["X-Phux-Attach"] = "1";
          headers["X-Phux-Token"] = options.attach;
        } else {
          headers["X-Phux-Token"] = options.token ?? (await this.sessionToken(sid));
        }
        const response = await session.fetch(
          new Request("https://shell.phux.sh/session", { headers }),
        );
        return { client, server, response, envelope: client.envelope() };
      } finally {
        globals.WebSocketPair = previous;
      }
    },
  };
}

let harness: Harness;
beforeEach(async () => {
  harness = await buildHarness();
});

async function openSession(sid = "sid-1"): Promise<View> {
  await harness.session.prepare(sid, "demo");
  return harness.open(sid);
}

describe("session pairing", () => {
  test("hands the opener one code and the paired view none", async () => {
    const primary = await openSession();
    expect(primary.response.status).toBe(101);
    const code = primary.envelope.pair?.code;
    expect(code).toBeTruthy();
    expect(primary.envelope.pair!.expiresAt).toBeGreaterThan(Date.now());

    const phone = await harness.open("sid-1", { attach: code });
    expect(phone.response.status).toBe(101);
    expect(phone.envelope).not.toHaveProperty("pair");
  });

  test("spends the code once and reports the state to the pair panel", async () => {
    const primary = await openSession();
    const code = primary.envelope.pair!.code;
    expect(await harness.session.pairStatus(code)).toMatchObject({ linked: false });

    await harness.open("sid-1", { attach: code });
    expect(await harness.session.pairStatus(code)).toMatchObject({ linked: true });

    const replay = await harness.open("sid-1", { attach: code });
    expect(replay.client.closed?.code).toBe(4003);
  });

  test("keeps the two credentials apart in both directions", async () => {
    const primary = await openSession("sid-2");
    const code = primary.envelope.pair!.code;

    // A pairing code cannot open a session...
    const asSession = await harness.open("sid-2", { token: code });
    expect(asSession.client.closed?.code).toBe(4007);

    // ...and a session token cannot spend one. The object refuses the scope
    // before it looks at the code (the Worker's front door answers 4003 for a
    // malformed or invalid code, so a browser sees that instead).
    const asAttach = await harness.open("sid-2", { attach: await harness.sessionToken("sid-2") });
    expect(asAttach.client.closed?.code).toBe(4007);
  });

  test("will not attach to a session whose opener has gone", async () => {
    const primary = await openSession("sid-3");
    const code = primary.envelope.pair!.code;
    await harness.session.webSocketClose(primary.server, 1000, "closed", true);
    const late = await harness.open("sid-3", { attach: code });
    expect(late.client.closed?.code).toBe(4003);
  });
});

describe("session lifetime", () => {
  test("schedules the idle clock a full window out, not a short one", async () => {
    const primary = await openSession();
    const alarm = harness.alarm();
    expect(alarm).not.toBeNull();
    // Exactly the idle window: a shorter deadline closes a live shell, which is
    // what a per-socket alarm rewrite can quietly do.
    expect(alarm!).toBeGreaterThan(Date.now() + IDLE_KILL_MS - 5_000);
    expect(alarm!).toBeLessThanOrEqual(Date.now() + IDLE_KILL_MS + 1_000);
  });

  test("closes the session with an idle code and releases its slot once", async () => {
    const primary = await openSession();
    primary.server.stale();
    await harness.session.alarm();
    expect(primary.client.closed?.code).toBe(4004);
    expect(harness.releases()).toBe(1);
  });

  test("a view that leaves does not end the session", async () => {
    const primary = await openSession();
    const phone = await harness.open("sid-1", { attach: primary.envelope.pair!.code });

    await harness.session.webSocketClose(phone.server, 1000, "gone", true);
    expect(phone.client.closed?.code).toBe(1000);
    expect(primary.client.closed).toBeNull();
    expect(harness.releases()).toBe(0);

    // The paired view's idle clock expiring is also just that view.
    phone.server.stale();
    await harness.session.alarm();
    expect(primary.client.closed).toBeNull();
    expect(harness.releases()).toBe(0);
  });

  test("the opener leaving ends the session for every view", async () => {
    const primary = await openSession();
    const phone = await harness.open("sid-1", { attach: primary.envelope.pair!.code });

    await harness.session.webSocketClose(primary.server, 1000, "gone", true);
    expect(phone.client.closed?.code).toBe(1000);
    expect(harness.releases()).toBe(1);
  });
});
