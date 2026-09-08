// phux live-demo Worker — the SOLE public door.
//
// Responsibilities (all BEFORE routing to a session):
//   1. Only accept WebSocket upgrades at GET /session.
//   2. Per-IP rate limit (sliding window) — cheap, in front of everything.
//   3. Reserve a slot against the GLOBAL concurrency cap. If full, reject with a
//      clear "demo at capacity" close — never queue.
//   4. Mint a short-lived HMAC session token.
//   5. Route to a FRESH SessionDO (one per session) and hand off the socket.
//
// The SessionDO runs the phux-edge WASM server and is only reachable through
// this Worker.
//
// File ownership: worker/** (track D). Do not edit anything in src/.

import { SessionDO } from "./session";
import { GlobalCapDO } from "./global-cap";
import { RateLimitDO, rateLimitName } from "./rate-limit";
import { mintToken } from "./token";
import { loadPortfolioSnapshot } from "./portfolio";
import { PhuxSessionContainer } from "./native-session";
import type { CircuitConfig } from "./native-admission";
import {
  nativeReservationTtlMs,
  sessionReservationTtlMs,
  nativeUpstreamRequest,
  parseDemoMode,
  isNativeEnabled,
} from "./native-routing";
import { startNative } from "./native-start";
import {
  handleAuthRequest,
  verifySessionCookie,
  verifySyntheticBearer,
  type AuthEnv,
} from "./auth";
import {
  publicFallbackReason,
  type InternalFallbackReason,
} from "./session-info";

export { SessionDO, GlobalCapDO, PhuxSessionContainer, RateLimitDO };

export interface Env extends AuthEnv {
  SESSION: DurableObjectNamespace<SessionDO>;
  PHUX_SESSION: DurableObjectNamespace<PhuxSessionContainer>;
  GLOBAL_CAP: DurableObjectNamespace<GlobalCapDO>;
  RATE_LIMIT: DurableObjectNamespace<RateLimitDO>;

  SESSION_TOKEN_SECRET: string; // secret (wrangler secret put)
  SYNTHETIC_TOKEN_SECRET?: string; // shared only with the production monitor
  GITHUB_APP_ID?: string;
  GITHUB_APP_PRIVATE_KEY?: string; // secret (wrangler secret put)
  IDLE_KILL_MS: string;
  HARD_MAX_MS: string;
  NATIVE_HARD_MAX_MS: string;
  NATIVE_CONCURRENCY_CAP: string;
  NATIVE_PER_IP_CAP: string;
  NATIVE_PER_ACCOUNT_CAP?: string;
  NATIVE_LAUNCHES_PER_HOUR?: string;
  NATIVE_DAILY_MS?: string;
  NATIVE_RATE_LIMIT_PER_MIN: string;
  NATIVE_ENABLED?: string;
  NATIVE_STARTUP_TIMEOUT_MS: string;
  NATIVE_CIRCUIT_FAILURE_THRESHOLD: string;
  NATIVE_CIRCUIT_WINDOW_MS: string;
  NATIVE_CIRCUIT_OPEN_MS: string;
  GLOBAL_CONCURRENCY_CAP: string;
  RATE_LIMIT_PER_MIN: string;
  SESSION_TTL_MS: string;
  ALLOWED_ORIGINS: string;
}

// WebSocket close codes used at the front door. 4xxx is the app-private range.
const CLOSE = {
  AT_CAPACITY: 4001,
  RATE_LIMITED: 4002,
  BAD_REQUEST: 4003,
  INTERNAL: 4011,
} as const;

const SESSION_PATH = "/session";

function positiveInt(value: string | undefined, fallback: number): number {
  const parsed = Number.parseInt(value ?? "", 10);
  return Number.isFinite(parsed) && parsed > 0 ? parsed : fallback;
}

function circuitConfig(env: Env): CircuitConfig {
  return {
    threshold: positiveInt(env.NATIVE_CIRCUIT_FAILURE_THRESHOLD, 3),
    windowMs: positiveInt(env.NATIVE_CIRCUIT_WINDOW_MS, 60_000),
    openMs: positiveInt(env.NATIVE_CIRCUIT_OPEN_MS, 60_000),
  };
}

function clientIp(request: Request): string {
  // Cloudflare sets CF-Connecting-IP on every edge request; it cannot be
  // spoofed by the client (the edge overwrites it). Fall back defensively.
  return (
    request.headers.get("CF-Connecting-IP") ??
    request.headers.get("X-Forwarded-For")?.split(",")[0].trim() ??
    "unknown"
  );
}

// Reject an upgrade attempt with a real 101 + immediate close carrying a code +
// reason, so the browser terminal can surface "demo at capacity" rather than a
// bare network error. (A plain HTTP 4xx on a WS upgrade is opaque to the client.)
function rejectUpgrade(code: number, reason: string): Response {
  const pair = new WebSocketPair();
  const [client, server] = Object.values(pair);
  server.accept();
  try {
    server.close(code, reason);
  } catch {
    // ignore
  }
  return new Response(null, { status: 101, webSocket: client });
}

export default {
  async fetch(
    request: Request,
    env: Env,
    ctx: ExecutionContext,
  ): Promise<Response> {
    const url = new URL(request.url);

    const authResponse = await handleAuthRequest(request, env);
    if (authResponse) return authResponse;

    // Lightweight health probe and sanitized native diagnostics (no IP data).
    if (url.pathname === "/healthz") {
      const status = await env.GLOBAL_CAP.getByName("global").status(
        circuitConfig(env),
      );
      return Response.json({
        ok: true,
        nativeEnabled: isNativeEnabled(env.NATIVE_ENABLED),
        ...status,
      });
    }

    if (url.pathname !== SESSION_PATH) {
      return new Response("not found", { status: 404 });
    }

    // Must be a WebSocket upgrade. Anything else gets a plain 426.
    if (
      request.method !== "GET" ||
      request.headers.get("Upgrade")?.toLowerCase() !== "websocket"
    ) {
      return new Response("expected websocket upgrade at /session", {
        status: 426,
      });
    }

    const mode = parseDemoMode(url.searchParams.get("mode"));
    if (!mode) {
      return rejectUpgrade(CLOSE.BAD_REQUEST, "unknown demo mode");
    }

    const origin = request.headers.get("Origin") ?? "";
    const allowedOrigins = (env.ALLOWED_ORIGINS || "https://phux.sh")
      .split(",")
      .map((value) => value.trim())
      .filter(Boolean);
    if (!allowedOrigins.includes(origin)) {
      return rejectUpgrade(CLOSE.BAD_REQUEST, "origin not allowed");
    }

    const ip = clientIp(request);

    // ── (2) per-IP rate limit ────────────────────────────────────────────────
    // A secret-keyed object per source address keeps each sliding window
    // strongly consistent without turning GlobalCapDO into a global IP table.
    const cap = env.GLOBAL_CAP.getByName("global");
    const ratePerMin =
      mode === "native"
        ? parseInt(env.NATIVE_RATE_LIMIT_PER_MIN, 10) || 2
        : parseInt(env.RATE_LIMIT_PER_MIN, 10) || 3;
    const rateKey = await rateLimitName(ip, env.AUTH_COOKIE_SECRET);
    const rateOk = await env.RATE_LIMIT.getByName(rateKey).check(ratePerMin);
    if (!rateOk) {
      return rejectUpgrade(
        CLOSE.RATE_LIMITED,
        "rate limited: too many demo sessions from your ip, wait a minute",
      );
    }

    // ── (3) reserve a global concurrency slot ────────────────────────────────
    const sid = crypto.randomUUID();
    const sessionTtlMs = parseInt(env.SESSION_TTL_MS, 10) || 30000;
    const edgeHardMaxMs = positiveInt(env.HARD_MAX_MS, 600_000);
    const edgeTtlMs = sessionReservationTtlMs(edgeHardMaxMs);
    const nativeHardMaxMs = positiveInt(env.NATIVE_HARD_MAX_MS, 300_000);
    const nativeTtlMs = nativeReservationTtlMs(nativeHardMaxMs, sessionTtlMs);
    const ttlMs = mode === "native" ? nativeTtlMs : edgeTtlMs;
    const capLimit = parseInt(env.GLOBAL_CONCURRENCY_CAP, 10) || 5;
    // Reservation auto-expires if the session never actually opens (self-heal),
    // so a crash between reserve and connect cannot leak a slot forever.
    let fallbackReason: InternalFallbackReason | undefined;
    let nativeAdmitted = false;
    let nativeUntil = 0;
    let reserved: boolean;
    if (mode === "native") {
      const identity =
        verifySyntheticBearer(request, env.SYNTHETIC_TOKEN_SECRET) ??
        (await verifySessionCookie(request, env.AUTH_COOKIE_SECRET));
      if (!identity) {
        reserved = await cap.reserve(sid, capLimit, edgeTtlMs);
        fallbackReason = "auth-required";
      } else if (!isNativeEnabled(env.NATIVE_ENABLED)) {
        reserved = await cap.reserve(sid, capLimit, edgeTtlMs);
        fallbackReason = "disabled";
      } else {
        const admission = await cap.admitNative(
          sid,
          ip,
          identity.principal,
          capLimit,
          positiveInt(env.NATIVE_CONCURRENCY_CAP, 25),
          positiveInt(env.NATIVE_PER_IP_CAP, 1),
          {
            activeCap: positiveInt(env.NATIVE_PER_ACCOUNT_CAP, 1),
            launchesPerHour: positiveInt(env.NATIVE_LAUNCHES_PER_HOUR, 6),
            dailyMs: positiveInt(env.NATIVE_DAILY_MS, 1_800_000),
          },
          nativeHardMaxMs,
          ttlMs,
          edgeTtlMs,
          circuitConfig(env),
        );
        reserved = admission.backend !== "reject";
        nativeAdmitted = admission.backend === "native";
        nativeUntil =
          admission.backend === "native" ? admission.nativeUntil : 0;
        fallbackReason =
          admission.backend === "edge" ? admission.reason : undefined;
      }
    } else {
      reserved = await cap.reserve(sid, capLimit, ttlMs);
    }
    if (!reserved) {
      return rejectUpgrade(
        CLOSE.AT_CAPACITY,
        "demo at capacity, try again in a minute",
      );
    }

    if (mode === "native" && nativeAdmitted) {
      const session = env.PHUX_SESSION.getByName(sid);
      const result = await startNative({
        sid,
        expiresAt: nativeUntil,
        startupTimeoutMs: positiveInt(env.NATIVE_STARTUP_TIMEOUT_MS, 8_000),
        edgeTtlMs,
        circuitConfig: circuitConfig(env),
        request: nativeUpstreamRequest(request),
        session,
        cap,
      });
      if ("response" in result) return result.response;
      fallbackReason = result.fallbackReason;
    }

    if (mode === "native") {
      console.log(`native_fallback reason=${fallbackReason ?? "unavailable"}`);
    }

    // ── (4) mint a short-lived session token ─────────────────────────────────
    let token: string;
    try {
      token = await mintToken(env.SESSION_TOKEN_SECRET, {
        sid,
        exp: Date.now() + ttlMs,
      });
    } catch (err) {
      // Roll back the reservation so a minting failure doesn't leak a slot.
      ctx.waitUntil(cap.release(sid));
      return rejectUpgrade(CLOSE.INTERNAL, "internal error minting session");
    }

    // ── (5) route to a FRESH SessionDO and hand off the upgrade ──────────────
    // Fresh because the DO name is the unique session id — never reused, so no
    // state bleeds between visitors. The SessionDO verifies the token, spawns
    // the edge WASM server, proxies bytes, and releases the slot on teardown.
    const session = env.SESSION.getByName(sid);
    const fwd = new Request(request.url, request);
    fwd.headers.set("X-Phux-Session", sid);
    fwd.headers.set("X-Phux-Token", token);

    try {
      const snapshot =
        mode === "portfolio" || mode === "native"
          ? await loadPortfolioSnapshot(ctx, env)
          : undefined;
      await session.prepare(
        sid,
        mode === "native" ? "native-fallback" : mode,
        snapshot,
        fallbackReason ? publicFallbackReason(fallbackReason) : undefined,
      );
      return await session.fetch(fwd);
    } catch (err) {
      ctx.waitUntil(cap.release(sid));
      return rejectUpgrade(CLOSE.INTERNAL, "internal error starting session");
    }
  },
} satisfies ExportedHandler<Env>;
