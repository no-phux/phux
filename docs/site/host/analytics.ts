/**
 * Anonymous-event forwarding to the private ops pipeline.
 *
 * The public repo intentionally shows only the envelope shape; the event
 * taxonomy, storage, and dashboards live in the private no-phux/ops repo.
 * Identity model (also documented on /telemetry):
 *
 *   - anonymous visitors: never correlatable — the private pipeline hashes
 *     ip|ua under a daily-rotated salt it owns. We forward `ip`/`ua` only so
 *     that hashing can happen there; neither is stored raw.
 *   - members (voluntary email signups via POST /api/join): identified only
 *     when they click their personal claim link, which sets the first-party
 *     `phux_mid` cookie we attach to envelopes.
 *
 * Everything here is fire-and-forget via waitUntil and fails silent.
 */

export interface AnalyticsEnv {
  ASSETS?: { fetch(input: Request): Promise<Response> };
  ANALYTICS_INGEST_URL?: string;
  ANALYTICS_INGEST_KEY?: string;
  MEMBER_KEY?: string;
}

export const MEMBER_COOKIE = "phux_mid";

export interface DemoInfo {
  mode: string;
  backend: string;
}

interface Envelope {
  ts: number;
  ip: string;
  country: string;
  ua: string;
  referrer: string;
  host: string;
  method: string;
  path: string;
  query: string;
  status: number;
  content_type: string;
  accept: string;
  member_id?: string;
  kind?: string;
  email?: string;
  source?: string;
  demo?: DemoInfo;
}

// ── buffered forwarding ─────────────────────────────────────────────────────

let pending: Envelope[] = [];
let chain: Promise<unknown> | null = null;

export function forwardEnvelope(
  env: AnalyticsEnv,
  ctx: { waitUntil(p: Promise<unknown>): void } | undefined,
  envelope: Envelope,
): void {
  if (!env.ANALYTICS_INGEST_URL || !env.ANALYTICS_INGEST_KEY) return;
  pending.push(envelope);
  const batch = pending;
  pending = [];
  const next = (chain ?? Promise.resolve()).then(() =>
    fetch(env.ANALYTICS_INGEST_URL!, {
      method: "POST",
      headers: {
        "content-type": "application/json",
        "x-analytics-key": env.ANALYTICS_INGEST_KEY!,
      },
      body: JSON.stringify({ events: batch }),
    }),
  );
  chain = next.catch(() => {
    // analytics must never affect a request
  });
  ctx?.waitUntil(chain);
}

/** Classify one HTTP exchange into an envelope. */
export function buildEnvelope(
  request: Request,
  response: Response,
  demo?: DemoInfo,
): Envelope {
  const url = new URL(request.url);
  return {
    ts: Date.now(),
    ip:
      request.headers.get("cf-connecting-ip") ??
      request.headers.get("x-forwarded-for")?.split(",")[0]?.trim() ??
      "",
    country: (request as Request & { cf?: { country?: string } }).cf?.country ?? "",
    ua: request.headers.get("user-agent") ?? "",
    referrer: request.headers.get("referer") ?? "",
    host: url.hostname,
    method: request.method,
    path: url.pathname,
    query: url.search,
    status: response.status,
    content_type: response.headers.get("content-type") ?? "",
    accept: request.headers.get("accept") ?? "",
    member_id: memberIdFromRequest(request) ?? undefined,
    demo,
  };
}

export function memberIdFromRequest(request: Request): string | null {
  const cookie = request.headers.get("cookie") ?? "";
  for (const part of cookie.split(";")) {
    const [name, ...rest] = part.trim().split("=");
    if (name === MEMBER_COOKIE) {
      const value = rest.join("=");
      return /^[a-f0-9]{16,64}$/.test(value) ? value : null;
    }
  }
  return null;
}

// ── member signup + claim (opt-in identity) ─────────────────────────────────

export async function hmacHex(key: string, message: string): Promise<string> {
  const cryptoKey = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(key),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const signature = await crypto.subtle.sign(
    "HMAC",
    cryptoKey,
    new TextEncoder().encode(message),
  );
  return Array.from(new Uint8Array(signature))
    .map((byte) => byte.toString(16).padStart(2, "0"))
    .join("");
}

export function memberIdForEmail(memberKey: string, email: string): Promise<string> {
  return hmacHex(memberKey, `member:${email.trim().toLowerCase()}`);
}

export function claimProof(memberKey: string, memberId: string): Promise<string> {
  return hmacHex(memberKey, `claim:${memberId}`);
}

/** POST /api/join — voluntary email capture. Never throws to the client. */
export async function handleJoin(
  request: Request,
  env: AnalyticsEnv,
  ctx: { waitUntil(p: Promise<unknown>): void } | undefined,
): Promise<Response> {
  if (!env.MEMBER_KEY || !env.ANALYTICS_INGEST_URL || !env.ANALYTICS_INGEST_KEY) {
    return Response.json({ ok: false, error: "join is not configured" }, { status: 503 });
  }
  let email = "";
  let source = "";
  try {
    const type = request.headers.get("content-type") ?? "";
    if (type.includes("application/json")) {
      const body = (await request.json()) as {
        email?: unknown;
        source?: unknown;
      };
      email = String(body.email ?? "");
      source = String(body.source ?? "");
    } else {
      const form = await request.formData();
      email = String(form.get("email") ?? "");
      source = String(form.get("source") ?? "");
    }
  } catch {
    return Response.json({ ok: false, error: "bad request" }, { status: 400 });
  }
  email = email.trim().toLowerCase();
  if (!/^[^@\s]+@[^@\s]+\.[^@\s]{2,}$/.test(email) || email.length > 254) {
    return Response.json({ ok: false, error: "invalid email" }, { status: 400 });
  }
  const memberId = await memberIdForEmail(env.MEMBER_KEY, email);
  forwardEnvelope(env, ctx, {
    ...buildEnvelope(request, new Response(null, { status: 200 })),
    kind: "signup",
    email,
    source: source.slice(0, 120),
    member_id: memberId,
    path: "/api/join",
  });
  return Response.json({ ok: true }, { headers: { "cache-control": "no-store" } });
}

/**
 * GET /api/claim?t=<memberId>.<proof>[.<issuedAtSec>] — sets the opt-in device
 * cookie.
 *
 * Security shape (PHA-425): the proof must be the FULL 64-hex HMAC compared in
 * constant time — older links carried a 32-hex prefix and any ≥16-char prefix
 * of it was accepted. Links carrying an issuedAtSec segment expire after
 * CLAIM_MAX_AGE_MS (ops repo); segment-less legacy links stay valid because
 * rotating MEMBER_KEY invalidates everything at once and the member set stays
 * small enough for per-link revocation to be unnecessary.
 */
export async function handleClaim(
  request: Request,
  env: AnalyticsEnv,
): Promise<Response> {
  if (!env.MEMBER_KEY) return new Response("not configured", { status: 503 });
  const token = new URL(request.url).searchParams.get("t") ?? "";
  const parts = token.split(".");
  if (parts.length < 2 || parts.length > 3) return new Response("bad token", { status: 400 });
  const [memberId, proof, issuedAt] = parts;
  if (!/^[a-f0-9]{64}$/.test(memberId ?? "")) return new Response("bad token", { status: 400 });
  if (!/^[a-f0-9]{64}$/.test(proof ?? "")) return new Response("bad proof", { status: 403 });
  const expected = await claimProof(env.MEMBER_KEY, memberId!);
  if (!timingSafeEqualHex(proof!, expected)) {
    return new Response("bad proof", { status: 403 });
  }
  if (issuedAt !== undefined) {
    if (!/^\d{10,13}$/.test(issuedAt)) return new Response("bad token", { status: 400 });
    const issuedMs = issuedAt.length === 13 ? Number(issuedAt) : Number(issuedAt) * 1000;
    const skewMs = 5 * 60 * 1000;
    if (
      !Number.isFinite(issuedMs) ||
      issuedMs > Date.now() + skewMs ||
      Date.now() - issuedMs > CLAIM_MAX_AGE_MS
    ) {
      return new Response("claim link expired", { status: 403 });
    }
  }
  const headers = new Headers({ location: "/?joined=1" });
  headers.append(
    "set-cookie",
    `${MEMBER_COOKIE}=${memberId}; Path=/; Max-Age=31536000; HttpOnly; Secure; SameSite=Lax`,
  );
  return new Response(null, { status: 302, headers });
}

// Mirrors CLAIM_MAX_AGE_MS in the ops repo (analytics/src/do.ts).
export const CLAIM_MAX_AGE_MS = 90 * 86_400_000;

/** Constant-time comparison for equal-length lowercase hex strings. */
export function timingSafeEqualHex(a: string, b: string): boolean {
  if (a.length !== b.length) return false;
  let difference = 0;
  for (let index = 0; index < a.length; index += 1) {
    difference |= a.charCodeAt(index) ^ b.charCodeAt(index);
  }
  return difference === 0;
}
