import { DurableObject } from "cloudflare:workers";
import initWasm, { EdgeSession } from "../edge/phux_edge.js";
import wasmModule from "../edge/phux_edge_bg.wasm";
import { sessionDeadline, sessionExpiryReason } from "./native-routing";
import { verifyToken } from "./token";
import type { GlobalCapDO } from "./global-cap";
import type { DemoMode, PortfolioSnapshot } from "./portfolio";
import {
  serializeSessionInfo,
  type PublicFallbackReason,
  type SessionInfo,
} from "./session-info";

interface Env {
  GLOBAL_CAP: DurableObjectNamespace<GlobalCapDO>;
  SESSION_TOKEN_SECRET: string;
  IDLE_KILL_MS: string;
  HARD_MAX_MS: string;
}

const CLOSE = {
  IDLE: 4004,
  MAX_LIFETIME: 4005,
  UNAUTHORIZED: 4007,
  INTERNAL: 4011,
} as const;

const BOOT_KEY = "boot-v1";
const CHECKPOINT_KEY = "checkpoint-v1";
const RELEASE_RETRY_MS = 5_000;

interface BootRecord {
  version: 1;
  status: "prepared" | "active" | "closing";
  sid: string;
  mode: DemoMode;
  snapshot?: PortfolioSnapshot;
  info: SessionInfo;
  idleMs: number;
}

interface SocketAttachment {
  version: 1;
  idleDeadline: number;
  expiresAt: number;
}

let wasmReady: Promise<void> | null = null;
function ensureWasm(): Promise<void> {
  wasmReady ??= initWasm(wasmModule).then(() => undefined);
  return wasmReady;
}

function positiveInt(value: string, fallback: number): number {
  const parsed = Number.parseInt(value, 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function attachment(ws: WebSocket): SocketAttachment | null {
  try {
    const value = ws.deserializeAttachment() as Partial<SocketAttachment> | null;
    if (
      value?.version !== 1 ||
      !Number.isFinite(value.idleDeadline) ||
      !Number.isFinite(value.expiresAt)
    ) {
      return null;
    }
    return value as SocketAttachment;
  } catch {
    return null;
  }
}

export class SessionDO extends DurableObject<Env> {
  private session: EdgeSession | null = null;
  private cleanupPromise: Promise<void> | null = null;

  async prepare(
    sid: string,
    mode: DemoMode,
    snapshot?: PortfolioSnapshot,
    fallbackReason?: PublicFallbackReason,
  ): Promise<void> {
    if (await this.ctx.storage.get<BootRecord>(BOOT_KEY)) {
      throw new Error("session was already prepared");
    }
    const hardMs = positiveInt(this.env.HARD_MAX_MS, 600_000);
    const boot: BootRecord = {
      version: 1,
      status: "prepared",
      sid,
      mode,
      snapshot,
      idleMs: positiveInt(this.env.IDLE_KILL_MS, 120_000),
      info: {
        type: "phux.session.v1",
        outcome: "accepted",
        backend: "edge",
        expiresAt: Date.now() + hardMs,
        fallbackReason,
      },
    };
    await this.ctx.storage.put(BOOT_KEY, boot);
  }

  async fetch(request: Request): Promise<Response> {
    const sid = request.headers.get("X-Phux-Session") ?? "";
    const token = request.headers.get("X-Phux-Token") ?? "";
    const claims = await verifyToken(this.env.SESSION_TOKEN_SECRET, token);
    if (!claims || claims.sid !== sid || sid.length === 0) {
      return this.reject(CLOSE.UNAUTHORIZED, "unauthorized session");
    }
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("expected websocket upgrade", { status: 426 });
    }

    const boot = await this.ctx.storage.get<BootRecord>(BOOT_KEY);
    if (!boot || boot.version !== 1 || boot.sid !== sid || boot.status !== "prepared") {
      return this.reject(CLOSE.UNAUTHORIZED, "session was not prepared");
    }

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    const now = Date.now();
    const socketState: SocketAttachment = {
      version: 1,
      idleDeadline: now + boot.idleMs,
      expiresAt: boot.info.expiresAt,
    };
    boot.status = "active";
    await this.ctx.storage.put(BOOT_KEY, boot);
    this.ctx.acceptWebSocket(server);
    server.serializeAttachment(socketState);
    await this.ctx.storage.setAlarm(sessionDeadline(socketState));

    // This text envelope must precede every binary phux frame and is sent only
    // during the one permitted upgrade.
    server.send(serializeSessionInfo(boot.info));
    return new Response(null, { status: 101, webSocket: client });
  }

  async webSocketMessage(ws: WebSocket, message: string | ArrayBuffer): Promise<void> {
    const boot = await this.activeBoot();
    const state = attachment(ws);
    if (!boot || !state) {
      await this.shut(ws, CLOSE.INTERNAL, "server state unavailable");
      return;
    }

    state.idleDeadline = Date.now() + boot.idleMs;
    ws.serializeAttachment(state);
    await this.ctx.storage.setAlarm(sessionDeadline(state));

    if (!(message instanceof ArrayBuffer) || message.byteLength > 65_536) {
      await this.shut(ws, CLOSE.UNAUTHORIZED, "invalid frame");
      return;
    }

    try {
      const session = await this.edgeSession(boot);
      const before = session.checkpoint();
      const frames = session.on_message(new Uint8Array(message)) as Uint8Array[];
      for (const frame of frames) ws.send(frame);
      const after = session.checkpoint();
      if (after !== before) await this.ctx.storage.put(CHECKPOINT_KEY, after);
    } catch {
      await this.shut(ws, CLOSE.INTERNAL, "server error");
    }
  }

  async webSocketClose(
    _ws: WebSocket,
    _code: number,
    _reason: string,
    _wasClean: boolean,
  ): Promise<void> {
    await this.cleanup();
  }

  async webSocketError(_ws: WebSocket, _error: unknown): Promise<void> {
    await this.cleanup();
  }

  async alarm(): Promise<void> {
    const sockets = this.ctx.getWebSockets();
    if (sockets.length === 0) {
      await this.cleanup();
      return;
    }

    const ws = sockets[0];
    const state = attachment(ws);
    if (!state) {
      await this.shut(ws, CLOSE.INTERNAL, "server state unavailable");
      return;
    }
    const now = Date.now();
    const expiry = sessionExpiryReason(now, state);
    if (!expiry) {
      await this.ctx.storage.setAlarm(sessionDeadline(state));
      return;
    }
    if (expiry === "hard") {
      await this.shut(ws, CLOSE.MAX_LIFETIME, "closed: session time limit");
    } else {
      await this.shut(ws, CLOSE.IDLE, "closed: idle");
    }
  }

  private async activeBoot(): Promise<BootRecord | null> {
    const boot = await this.ctx.storage.get<BootRecord>(BOOT_KEY);
    return boot?.version === 1 && boot.status === "active" ? boot : null;
  }

  private async edgeSession(boot: BootRecord): Promise<EdgeSession> {
    if (this.session) return this.session;
    await ensureWasm();
    const snapshot = boot.snapshot ? JSON.stringify(boot.snapshot) : "";
    const checkpoint = await this.ctx.storage.get<string>(CHECKPOINT_KEY);
    this.session = checkpoint
      ? EdgeSession.restore(checkpoint, boot.mode, snapshot)
      : new EdgeSession(100, 24, boot.mode, snapshot);
    return this.session;
  }

  private async shut(ws: WebSocket, code: number, reason: string): Promise<void> {
    try {
      ws.close(code, reason);
    } catch {
      // The socket may already be closed; release is still required.
    }
    await this.cleanup();
  }

  private cleanup(): Promise<void> {
    this.cleanupPromise ??= this.releaseAndDelete().finally(() => {
      this.cleanupPromise = null;
    });
    return this.cleanupPromise;
  }

  private async releaseAndDelete(): Promise<void> {
    const boot = await this.ctx.storage.get<BootRecord>(BOOT_KEY);
    if (!boot) {
      await this.ctx.storage.deleteAlarm();
      return;
    }
    if (boot.status !== "closing") {
      boot.status = "closing";
      await this.ctx.storage.put(BOOT_KEY, boot);
    }
    try {
      await this.env.GLOBAL_CAP.getByName("global").release(boot.sid);
    } catch {
      await this.ctx.storage.setAlarm(Date.now() + RELEASE_RETRY_MS);
      return;
    }
    this.session?.free();
    this.session = null;
    await this.ctx.storage.deleteAll();
  }

  private reject(code: number, reason: string): Response {
    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    server.accept();
    try {
      server.close(code, reason);
    } catch {
      // Ignore an already-closed rejection socket.
    }
    return new Response(null, { status: 101, webSocket: client });
  }
}
