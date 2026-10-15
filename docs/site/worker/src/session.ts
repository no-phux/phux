import { DurableObject } from "cloudflare:workers";
import initWasm, { EdgeSession } from "../edge/phux_edge.js";
import wasmModule from "../edge/phux_edge_bg.wasm";
import { ViewFanout } from "./attach-fanout";
import { PAIR_TTL_MS, pairRejection, type PairRecord } from "./pairing";
import { attachIdOf, frameType, TYPE_ATTACH } from "./wire";
import { sessionDeadline, sessionExpiryReason } from "./native-routing";
import { mintToken, verifyToken } from "./token";
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
  /**
   * A pairing code that cannot be spent. It shares the front door's
   * "could not accept this session" category on purpose: telling a prober
   * whether a code exists, expired, or was already used helps nobody but them.
   */
  BAD_REQUEST: 4003,
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
  /** Connection order, so the newest attached screen stays knowable after a wake. */
  connections: number;
  /**
   * The single-use attach code for a paired second screen. Minted with the
   * session, handed only to the view that opened it, spent on first use.
   */
  pair?: PairRecord;
}

interface SocketAttachment {
  version: 1;
  idleDeadline: number;
  expiresAt: number;
  /** Connection order; a higher number attached later. */
  seq: number;
  /**
   * "primary" is the view that opened the session and owns its slot; "attach" is
   * a paired second screen. Absent means primary, so a socket restored from
   * before pairing existed keeps its meaning.
   */
  role?: "primary" | "attach";
  /** Attach correlation ids this socket opened, for routing after a wake. */
  attachIds?: number[];
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
    const value =
      ws.deserializeAttachment() as Partial<SocketAttachment> | null;
    if (
      value?.version !== 1 ||
      !Number.isFinite(value.idleDeadline) ||
      !Number.isFinite(value.expiresAt)
    ) {
      return null;
    }
    const state = value as SocketAttachment;
    if (!Array.isArray(state.attachIds)) state.attachIds = [];
    return state;
  } catch {
    return null;
  }
}

export class SessionDO extends DurableObject<Env> {
  private session: EdgeSession | null = null;
  private cleanupPromise: Promise<void> | null = null;
  private readonly fanout = new ViewFanout<WebSocket>();

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
    const expiresAt = Date.now() + hardMs;
    const pairExpiresAt = Math.min(expiresAt, Date.now() + PAIR_TTL_MS);
    const boot: BootRecord = {
      version: 1,
      status: "prepared",
      sid,
      mode,
      snapshot,
      idleMs: positiveInt(this.env.IDLE_KILL_MS, 120_000),
      connections: 0,
      info: {
        type: "phux.session.v1",
        outcome: "accepted",
        backend: "edge",
        expiresAt,
        fallbackReason,
      },
      // The view that opened the session gets one attach code to hand to a phone.
      pair: {
        code: await mintToken(this.env.SESSION_TOKEN_SECRET, {
          sid,
          exp: pairExpiresAt,
          scope: "attach",
        }),
        expiresAt: pairExpiresAt,
      },
    };
    await this.ctx.storage.put(BOOT_KEY, boot);
  }

  async fetch(request: Request): Promise<Response> {
    const sid = request.headers.get("X-Phux-Session") ?? "";
    const token = request.headers.get("X-Phux-Token") ?? "";
    const attach = request.headers.get("X-Phux-Attach") === "1";
    const claims = await verifyToken(
      this.env.SESSION_TOKEN_SECRET,
      token,
      attach ? "attach" : "session",
    );
    if (!claims || claims.sid !== sid || sid.length === 0) {
      return this.reject(CLOSE.UNAUTHORIZED, "unauthorized session");
    }
    if (request.headers.get("Upgrade")?.toLowerCase() !== "websocket") {
      return new Response("expected websocket upgrade", { status: 426 });
    }

    const boot = await this.ctx.storage.get<BootRecord>(BOOT_KEY);
    if (!boot || boot.version !== 1 || boot.sid !== sid) {
      return this.reject(CLOSE.UNAUTHORIZED, "session was not prepared");
    }
    if (attach) return this.attachView(boot, token);
    if (boot.status !== "prepared") {
      return this.reject(CLOSE.UNAUTHORIZED, "session was not prepared");
    }

    const pair =
      boot.pair && boot.pair.expiresAt > Date.now() ? boot.pair : undefined;
    const info: SessionInfo = pair
      ? { ...boot.info, pair: { code: pair.code, expiresAt: pair.expiresAt } }
      : boot.info;
    return this.accept(boot.sid, "primary", info);
  }

  /**
   * A paired second screen. The attach code is single-use and spendable only
   * while the view that opened the session is still attached; the session's own
   * slot, idle clock, and hard expiry are untouched, so pairing never consumes
   * admission or budget.
   */
  private async attachView(boot: BootRecord, code: string): Promise<Response> {
    const rejection = await this.ctx.blockConcurrencyWhile(async () => {
      const current = await this.ctx.storage.get<BootRecord>(BOOT_KEY);
      if (!current || current.status !== "active") return "the session has ended";
      const reason = pairRejection(current.pair, code, Date.now());
      if (reason) return reason;
      await this.ctx.storage.put<BootRecord>(BOOT_KEY, {
        ...current,
        pair: { ...current.pair!, claimedAt: Date.now() },
      });
      return null;
    });
    if (rejection) return this.reject(CLOSE.BAD_REQUEST, rejection);
    return this.accept(boot.sid, "attach", boot.info);
  }

  /**
   * Add one view to the session. Re-reads the session record inside the object's
   * single-threaded window so a claim written by `attachView` is never clobbered
   * by this write.
   */
  private async accept(
    sid: string,
    role: "primary" | "attach",
    info: SessionInfo,
  ): Promise<Response> {
    const boot = await this.ctx.blockConcurrencyWhile(async () => {
      const current = await this.ctx.storage.get<BootRecord>(BOOT_KEY);
      if (!current || current.version !== 1 || current.sid !== sid) return null;
      current.connections += 1;
      if (role === "primary") current.status = "active";
      await this.ctx.storage.put(BOOT_KEY, current);
      return current;
    });
    if (!boot) return this.reject(CLOSE.UNAUTHORIZED, "session was not prepared");

    const pair = new WebSocketPair();
    const [client, server] = Object.values(pair);
    const now = Date.now();
    const socketState: SocketAttachment = {
      version: 1,
      idleDeadline: now + boot.idleMs,
      expiresAt: info.expiresAt,
      seq: boot.connections,
      role,
      attachIds: [],
    };
    this.hydrate();
    this.ctx.acceptWebSocket(server);
    server.serializeAttachment(socketState);
    this.fanout.add(server, socketState.seq);
    await this.refreshAlarm();

    // This text envelope must precede every binary phux frame and is sent only
    // during the one permitted upgrade. Only the paired view learns the code.
    server.send(serializeSessionInfo(info));
    return new Response(null, { status: 101, webSocket: client });
  }

  /** Whether a pairing code has been spent, for the pairing panel. */
  async pairStatus(code: string): Promise<{ linked: boolean; expiresAt: number }> {
    const boot = await this.ctx.storage.get<BootRecord>(BOOT_KEY);
    const pair = boot?.pair;
    if (!pair || pair.code !== code) return { linked: false, expiresAt: 0 };
    return { linked: pair.claimedAt !== undefined, expiresAt: pair.expiresAt };
  }

  async webSocketMessage(
    ws: WebSocket,
    message: string | ArrayBuffer,
  ): Promise<void> {
    this.hydrate();
    const boot = await this.activeBoot();
    const state = attachment(ws);
    if (!boot || !state) {
      await this.shut(ws, CLOSE.INTERNAL, "server state unavailable");
      return;
    }

    state.idleDeadline = Date.now() + boot.idleMs;
    ws.serializeAttachment(state);
    await this.refreshAlarm();

    if (!(message instanceof ArrayBuffer) || message.byteLength > 65_536) {
      await this.shut(ws, CLOSE.UNAUTHORIZED, "invalid frame");
      return;
    }

    const frame = new Uint8Array(message);
    if (frameType(frame) === TYPE_ATTACH) {
      const attachId = attachIdOf(frame);
      if (attachId !== undefined && !state.attachIds?.includes(attachId)) {
        state.attachIds = [...(state.attachIds ?? []), attachId];
        ws.serializeAttachment(state);
      }
    }
    // A paired view may not resize the shared terminal out from under the view
    // that owns the geometry (see attach-fanout.ts).
    if (!this.fanout.fromClient(ws, frame)) return;

    try {
      const session = await this.edgeSession(boot);
      const before = session.checkpoint();
      const frames = session.on_message(frame) as Uint8Array[];
      for (const response of frames) {
        for (const destination of this.fanout.targets(response, ws)) {
          destination.send(response);
        }
      }
      const after = session.checkpoint();
      if (after !== before) await this.ctx.storage.put(CHECKPOINT_KEY, after);
    } catch {
      await this.shut(ws, CLOSE.INTERNAL, "server error");
    }
  }

  async webSocketClose(
    ws: WebSocket,
    _code: number,
    _reason: string,
    _wasClean: boolean,
  ): Promise<void> {
    // Our 2025 compatibility date does not auto-reply to Close frames. Release
    // both transport ends as well as admission; cleanup alone leaves CLOSING.
    await this.shut(ws, 1000, "session closed");
  }

  async webSocketError(ws: WebSocket, _error: unknown): Promise<void> {
    await this.shut(ws, CLOSE.INTERNAL, "connection error");
  }

  async alarm(): Promise<void> {
    this.hydrate();
    const sockets = this.fanout.all();
    if (sockets.length === 0) {
      await this.cleanup();
      return;
    }
    const now = Date.now();
    for (const ws of sockets) {
      const state = attachment(ws);
      if (!state) {
        await this.shut(ws, CLOSE.INTERNAL, "server state unavailable");
        return;
      }
      const expiry = sessionExpiryReason(now, state);
      if (!expiry) continue;
      if ((state.role ?? "primary") === "primary") {
        await this.shut(
          ws,
          expiry === "hard" ? CLOSE.MAX_LIFETIME : CLOSE.IDLE,
          expiry === "hard" ? "closed: session time limit" : "closed: idle",
        );
        return;
      }
      // A paired view timing out only detaches that view.
      try {
        ws.close(CLOSE.IDLE, "closed: idle");
      } catch {
        // Already closed.
      }
      this.fanout.remove(ws);
    }
    await this.refreshAlarm();
  }

  private async activeBoot(): Promise<BootRecord | null> {
    const boot = await this.ctx.storage.get<BootRecord>(BOOT_KEY);
    return boot?.version === 1 && boot.status === "active" ? boot : null;
  }

  /**
   * Rebuild fan-out registration from the sockets the runtime still holds. A
   * wake after hibernation starts with an empty instance but live sockets.
   */
  private hydrate(): void {
    for (const ws of this.ctx.getWebSockets()) {
      if (this.fanout.has(ws)) continue;
      const state = attachment(ws);
      if (!state) continue;
      this.fanout.add(ws, state.seq ?? 0);
      for (const attachId of state.attachIds ?? []) {
        this.fanout.claim(ws, attachId);
      }
    }
  }

  /** Schedule the alarm for the earliest attached view's deadline. */
  private async refreshAlarm(): Promise<void> {
    let earliest: number | null = null;
    for (const ws of this.ctx.getWebSockets()) {
      const state = attachment(ws);
      if (!state) continue;
      const deadline = sessionDeadline(state);
      if (earliest === null || deadline < earliest) earliest = deadline;
    }
    if (earliest !== null) await this.ctx.storage.setAlarm(earliest);
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

  private async shut(
    ws: WebSocket,
    code: number,
    reason: string,
  ): Promise<void> {
    const role = attachment(ws)?.role ?? "primary";
    try {
      ws.close(code, reason);
    } catch {
      // The socket may already be closed; release is still required.
    }
    this.fanout.remove(ws);
    // A paired view leaving never ends the session: the primary owns its life.
    const primaryGone = !this.primarySocket();
    if (role === "attach" && !primaryGone) {
      await this.refreshAlarm();
      return;
    }
    for (const other of this.ctx.getWebSockets()) {
      if (other === ws) continue;
      try {
        other.close(code === CLOSE.IDLE ? CLOSE.IDLE : 1000, reason);
      } catch {
        // Already closed.
      }
      this.fanout.remove(other);
    }
    await this.cleanup();
  }

  private primarySocket(): WebSocket | undefined {
    return this.ctx
      .getWebSockets()
      .find((ws) => (attachment(ws)?.role ?? "primary") === "primary");
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
