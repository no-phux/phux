/**
 * Effect-native analytics producer for the public site Worker.
 *
 * Pure parsing and normalization stay ordinary functions. Time, crypto, HTTP,
 * request-body decoding, and failure recovery live in Effect. The Cloudflare
 * fetch export is the only Promise boundary (see host/index.ts).
 */
import { Clock, Effect, Schema } from "effect";
import {
  AnalyticsHttp,
  BodyReadFailure,
  CryptoFailure,
  HttpFailure,
  MemberCrypto,
  readLimitedBody,
} from "./analytics-runtime";
import { inviteTester, type TestFlightEnv } from "./testflight";

export {
  AnalyticsHttp,
  CryptoFailure,
  HttpFailure,
  MemberCrypto,
  runAnalytics,
  runAnalyticsBackground,
} from "./analytics-runtime";

export interface AnalyticsEnv {
  ASSETS?: { fetch(input: Request): Promise<Response> };
  /** Preferred production path: same-account Worker service binding. */
  ANALYTICS?: { fetch(input: Request): Promise<Response> };
  MEMBER_KEY?: string;
  MEMBER_CLAIM_KEY?: string;
  MEMBER_CLAIM_KEY_PREVIOUS?: string;
}

export const MEMBER_COOKIE = "phux_mid";
export const CLAIM_TTL_SECONDS = 7 * 24 * 60 * 60;
const MAX_CLAIM_TTL_SECONDS = 30 * 24 * 60 * 60;
const MAX_JOIN_BODY_BYTES = 8 * 1024;
const CLAIM_VERSION = "v1";

const MemberId = Schema.String.check(
  Schema.isPattern(/^(?:[a-f0-9]{32}|[a-f0-9]{64})$/),
);
const Email = Schema.Trim.pipe(
  Schema.check(
    Schema.isMaxLength(254),
    Schema.isPattern(/^[^@\s]+@[^@\s]+\.[^@\s]{2,}$/),
  ),
);
const ClaimToken = Schema.String.check(
  Schema.isPattern(/^v1\.\d{10}\.(?:[a-f0-9]{32}|[a-f0-9]{64})\.[a-f0-9]{64}$/),
);

export interface DemoInfo {
  mode: string;
  backend: string;
}

export interface Envelope {
  ts: number;
  ip: string;
  country: string;
  ua: string;
  referrer_host: string;
  host: string;
  method: string;
  path: string;
  utm_source: string;
  utm_medium: string;
  utm_campaign: string;
  status: number;
  content_type: string;
  accept: string;
  member_id?: string;
  kind?: string;
  email?: string;
  source?: string;
  demo?: DemoInfo;
}

/** Send one envelope. The caller decides explicitly whether failure is fatal. */
export const forwardEnvelope = Effect.fn("analytics.forwardEnvelope")(function*(
  env: AnalyticsEnv,
  envelope: Envelope,
): Effect.fn.Return<void, HttpFailure, AnalyticsHttp> {
  if (!canForward(env)) return;
  const ingestUrl = "https://analytics.internal/ingest";
  const http = yield* AnalyticsHttp;
  const response = yield* http.execute(
    new Request(ingestUrl, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ events: [envelope] }),
    }),
    env.ANALYTICS,
  );
  if (!response.ok) {
    return yield* new HttpFailure({
      cause: `analytics ingest returned ${response.status}`,
      url: ingestUrl,
    });
  }
});

/** Classify one HTTP exchange into an envelope using Effect's Clock. */
export const buildEnvelope = Effect.fnUntraced(function*(
  request: Request,
  response: Response,
  demo?: DemoInfo,
): Effect.fn.Return<Envelope> {
  const url = new URL(request.url);
  const ts = yield* Clock.currentTimeMillis;
  return {
    ts,
    ip:
      request.headers.get("cf-connecting-ip") ??
      request.headers.get("x-forwarded-for")?.split(",")[0]?.trim() ??
      "",
    country:
      (request as Request & { cf?: { country?: string } }).cf?.country ?? "",
    ua: request.headers.get("user-agent") ?? "",
    referrer_host: referrerHost(request.headers.get("referer")),
    host: url.hostname,
    method: request.method,
    path: url.pathname,
    utm_source: attribution(url.searchParams.get("utm_source")),
    utm_medium: attribution(url.searchParams.get("utm_medium")),
    utm_campaign: attribution(url.searchParams.get("utm_campaign")),
    status: response.status,
    content_type: response.headers.get("content-type") ?? "",
    accept: request.headers.get("accept") ?? "",
    member_id: memberIdFromRequest(request) ?? undefined,
    demo,
  };
});

export function memberIdFromRequest(request: Request): string | null {
  const cookie = request.headers.get("cookie") ?? "";
  for (const part of cookie.split(";")) {
    const [name, ...rest] = part.trim().split("=");
    if (name === MEMBER_COOKIE) {
      const value = rest.join("=");
      return Schema.is(MemberId)(value) ? value : null;
    }
  }
  return null;
}

export const hmacHex = Effect.fnUntraced(function*(
  key: string,
  message: string,
): Effect.fn.Return<string, CryptoFailure, MemberCrypto> {
  const service = yield* MemberCrypto;
  return yield* service.hmacHex(key, message);
});

export const memberIdForEmail = Effect.fnUntraced(function*(
  memberKey: string,
  email: string,
): Effect.fn.Return<string, CryptoFailure, MemberCrypto> {
  return yield* hmacHex(memberKey, `member:${email.trim().toLowerCase()}`);
});

export const claimProof = Effect.fnUntraced(function*(
  claimKey: string,
  memberId: string,
  expiresAt: number,
): Effect.fn.Return<string, CryptoFailure, MemberCrypto> {
  return yield* hmacHex(claimKey, claimMessage(memberId, expiresAt));
});

export const createClaimToken = Effect.fn("analytics.createClaimToken")(
  function*(
  claimKey: string,
  memberId: string,
  ttlSeconds = CLAIM_TTL_SECONDS,
  ): Effect.fn.Return<string, CryptoFailure, MemberCrypto> {
    const now = yield* Clock.currentTimeMillis;
    const expiresAt = Math.floor(now / 1_000) + ttlSeconds;
    const proof = yield* claimProof(claimKey, memberId, expiresAt);
    return `${CLAIM_VERSION}.${expiresAt}.${memberId}.${proof}`;
  },
);

export interface AnalyticsHandlerResult {
  readonly response: Response;
  readonly background?: Effect.Effect<void, HttpFailure, AnalyticsHttp>;
}

/** POST /api/join — voluntary email capture. */
export function handleJoin(
  request: Request,
  env: AnalyticsEnv,
): Effect.Effect<AnalyticsHandlerResult, never, MemberCrypto> {
  if (!env.MEMBER_KEY || !canForward(env)) {
    return Effect.succeed({
      response: Response.json(
        { ok: false, error: "join is not configured" },
        { status: 503 },
      ),
    });
  }
  const memberKey = env.MEMBER_KEY;
  return Effect.gen(function* () {
    const input = yield* decodeJoinRequest(request);
    const email = input.email.trim().toLowerCase();
    if (!Schema.is(Email)(email)) return invalidEmail();
    return {
      response: Response.json(
        { ok: true },
        { headers: { "cache-control": "no-store" } },
      ),
      background: yield* signupBackground(
        request,
        env,
        memberKey,
        email,
        input.source.slice(0, 120),
      ),
    };
  }).pipe(recoverRequestFailures);
}

/**
 * POST /api/beta — TestFlight access request for phux-mobile. Apple sends the
 * invite when App Store Connect is configured (host/testflight.ts); the
 * request is also recorded as a member signup whose source carries the
 * outcome, so `testflight:failed` and `testflight:unconfigured` rows are the
 * manual-invite queue.
 */
export function handleBeta(
  request: Request,
  env: AnalyticsEnv & TestFlightEnv,
  invite: typeof inviteTester = inviteTester,
): Effect.Effect<AnalyticsHandlerResult, never, MemberCrypto> {
  return Effect.gen(function* () {
    const input = yield* decodeJoinRequest(request);
    const email = input.email.trim().toLowerCase();
    if (!Schema.is(Email)(email)) return invalidEmail();
    // The hidden field is invisible to people; only form-filling bots set it.
    if (input.website) {
      return { response: Response.json({ ok: true, status: "requested" }) };
    }
    const outcome = yield* Effect.promise(() => invite(env, email));
    const recorded = Boolean(env.MEMBER_KEY) && canForward(env);
    if (outcome !== "invited" && !recorded) {
      const unconfigured = outcome === "unconfigured";
      return {
        response: Response.json(
          {
            ok: false,
            error: unconfigured
              ? "beta requests are not configured"
              : "could not reach TestFlight, try again",
          },
          { status: unconfigured ? 503 : 502 },
        ),
      };
    }
    return {
      response: Response.json(
        { ok: true, status: outcome === "invited" ? "invited" : "requested" },
        { headers: { "cache-control": "no-store" } },
      ),
      background: recorded
        ? yield* signupBackground(
            request,
            env,
            env.MEMBER_KEY!,
            email,
            `testflight:${outcome}`,
          )
        : undefined,
    };
  }).pipe(recoverRequestFailures);
}

const signupBackground = Effect.fnUntraced(function*(
  request: Request,
  env: AnalyticsEnv,
  memberKey: string,
  email: string,
  source: string,
): Effect.fn.Return<
  Effect.Effect<void, HttpFailure, AnalyticsHttp>,
  CryptoFailure,
  MemberCrypto
> {
  const memberId = yield* memberIdForEmail(memberKey, email);
  const envelope = yield* buildEnvelope(
    request,
    new Response(null, { status: 200 }),
  );
  return forwardEnvelope(env, {
    ...envelope,
    kind: "signup",
    email,
    source,
    member_id: memberId,
    path: new URL(request.url).pathname,
  });
});

function invalidEmail(): AnalyticsHandlerResult {
  return {
    response: Response.json(
      { ok: false, error: "invalid email" },
      { status: 400 },
    ),
  };
}

function recoverRequestFailures<R>(
  program: Effect.Effect<
    AnalyticsHandlerResult,
    BodyReadFailure | CryptoFailure,
    R
  >,
): Effect.Effect<AnalyticsHandlerResult, never, R> {
  return program.pipe(
    Effect.catchTag("BodyReadFailure", (error) =>
      Effect.succeed({
        response: Response.json(
          {
            ok: false,
            error: error.reason === "too-large" ? "request too large" : "bad request",
          },
          { status: error.reason === "too-large" ? 413 : 400 },
        ),
      }),
    ),
    Effect.catchTag("CryptoFailure", () =>
      Effect.succeed({
        response: Response.json(
          { ok: false, error: "identity service unavailable" },
          { status: 503 },
        ),
      }),
    ),
  );
}

/** GET /api/claim?t=<version>.<expiry>.<memberId>.<proof>. */
export function handleClaim(
  request: Request,
  env: AnalyticsEnv,
): Effect.Effect<Response, never, MemberCrypto> {
  if (!env.MEMBER_CLAIM_KEY) {
    return Effect.succeed(new Response("not configured", { status: 503 }));
  }
  return Effect.gen(function* () {
    const parsed = parseClaimToken(new URL(request.url).searchParams.get("t") ?? "");
    if (!parsed) return new Response("bad token", { status: 400 });

    const now = Math.floor((yield* Clock.currentTimeMillis) / 1_000);
    if (
      parsed.expiresAt <= now ||
      parsed.expiresAt > now + MAX_CLAIM_TTL_SECONDS
    ) {
      return new Response("expired proof", { status: 403 });
    }

    const cryptoService = yield* MemberCrypto;
    const keys = [env.MEMBER_CLAIM_KEY, env.MEMBER_CLAIM_KEY_PREVIOUS].filter(
      (key): key is string => Boolean(key),
    );
    const matches = yield* Effect.forEach(keys, (key) =>
      claimProof(key, parsed.memberId, parsed.expiresAt).pipe(
        Effect.flatMap((expected) =>
          cryptoService.timingSafeEqual(parsed.proof, expected),
        ),
      ),
    );
    if (!matches.some(Boolean)) {
      return new Response("bad proof", { status: 403 });
    }

    const headers = new Headers({ location: "/?joined=1" });
    headers.append(
      "set-cookie",
      `${MEMBER_COOKIE}=${parsed.memberId}; Path=/; Max-Age=31536000; HttpOnly; Secure; SameSite=Lax`,
    );
    return new Response(null, { status: 302, headers });
  }).pipe(
    Effect.catchTag("CryptoFailure", () =>
      Effect.succeed(new Response("identity service unavailable", { status: 503 })),
    ),
  );
}

const JoinInput = Schema.Struct({
  email: Schema.String,
  source: Schema.optionalKey(Schema.String),
  website: Schema.optionalKey(Schema.String),
});

interface JoinRequest {
  readonly email: string;
  readonly source: string;
  /** Honeypot: a field hidden from people, so only bots fill it. */
  readonly website: string;
}

function decodeJoinRequest(
  request: Request,
): Effect.Effect<JoinRequest, BodyReadFailure> {
  return Effect.gen(function* () {
    const bytes = yield* readLimitedBody(request, MAX_JOIN_BODY_BYTES);
    const contentType = request.headers.get("content-type") ?? "";
    const value = yield* parseBody(bytes, contentType);
    const decoded = yield* Schema.decodeUnknownEffect(JoinInput)(value).pipe(
      Effect.mapError(() => new BodyReadFailure({ reason: "malformed" })),
    );
    return {
      email: decoded.email,
      source: decoded.source ?? "",
      website: decoded.website ?? "",
    };
  });
}

function parseBody(
  bytes: Uint8Array,
  contentType: string,
): Effect.Effect<unknown, BodyReadFailure> {
  const text = new TextDecoder().decode(bytes);
  if (contentType.includes("application/json")) {
    return Effect.try({
      try: () => JSON.parse(text) as unknown,
      catch: () => new BodyReadFailure({ reason: "malformed" }),
    });
  }
  if (contentType.includes("application/x-www-form-urlencoded")) {
    const params = new URLSearchParams(text);
    return Effect.succeed({
      email: params.get("email"),
      source: params.get("source") ?? "",
      website: params.get("website") ?? "",
    });
  }
  return Effect.fail(new BodyReadFailure({ reason: "malformed" }));
}

function parseClaimToken(token: string): {
  readonly expiresAt: number;
  readonly memberId: string;
  readonly proof: string;
} | null {
  if (!Schema.is(ClaimToken)(token)) return null;
  const [, expires, memberId, proof] = token.split(".") as [
    typeof CLAIM_VERSION,
    string,
    string,
    string,
  ];
  return { expiresAt: Number(expires), memberId, proof };
}

function claimMessage(memberId: string, expiresAt: number): string {
  return `claim:${CLAIM_VERSION}:${memberId}:${expiresAt}`;
}

function canForward(env: AnalyticsEnv): boolean {
  return Boolean(env.ANALYTICS);
}

function attribution(value: string | null): string {
  return (value ?? "")
    .replace(/[\u0000-\u001f\u007f]/g, "")
    .trim()
    .slice(0, 80);
}

function referrerHost(value: string | null): string {
  if (!value) return "";
  try {
    return new URL(value).hostname.toLowerCase().slice(0, 253);
  } catch {
    return "";
  }
}
