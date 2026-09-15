import { describe, expect, test } from "bun:test";
import { Clock, Effect } from "effect";
import {
  buildEnvelope,
  createClaimToken,
  forwardEnvelope,
  handleClaim,
  handleJoin,
  memberIdForEmail,
  memberIdFromRequest,
  runAnalytics,
} from "./analytics";
import { handleAnalyticsHttp } from "./analytics-http";

const ENV = {
  ASSETS: { fetch: async () => new Response("x") },
  ANALYTICS: {
    fetch: async () => new Response(null, { status: 204 }),
  },
  MEMBER_KEY: "member-key",
  MEMBER_CLAIM_KEY: "claim-key",
};

const TEST_NOW = 1_700_000_000_000;
const testClock: Clock.Clock = {
  currentTimeMillisUnsafe: () => TEST_NOW,
  currentTimeMillis: Effect.succeed(TEST_NOW),
  currentTimeNanosUnsafe: () => BigInt(TEST_NOW) * 1_000_000n,
  currentTimeNanos: Effect.succeed(BigInt(TEST_NOW) * 1_000_000n),
  monotonicTimeNanosUnsafe: () => 0n,
  monotonicTimeNanos: Effect.succeed(0n),
  sleep: () => Effect.void,
};

describe("member identity", () => {
  test("member id is a stable HMAC of the normalized email", async () => {
    const a = await runAnalytics(memberIdForEmail("member-key", "Foo@Example.COM"));
    const b = await runAnalytics(memberIdForEmail("member-key", " foo@example.com "));
    expect(a).toBe(b);
    expect(a).toMatch(/^[a-f0-9]{64}$/);
    const c = await runAnalytics(memberIdForEmail("other-key", "foo@example.com"));
    expect(c).not.toBe(a);
  });

  test("member cookie parses and rejects junk", () => {
    const memberId = "abcdef0123456789abcdef0123456789";
    const request = new Request("https://phux.sh/", {
      headers: { cookie: `other=1; phux_mid=${memberId}; x=2` },
    });
    expect(memberIdFromRequest(request)).toBe(memberId);
    const bad = new Request("https://phux.sh/", {
      headers: { cookie: "phux_mid=../../etc" },
    });
    expect(memberIdFromRequest(bad)).toBeNull();
  });
});

describe("buildEnvelope", () => {
  test("keeps campaign fields and drops arbitrary query data", async () => {
    const request = new Request(
      "https://docs.phux.sh/wire/proto?utm_source=x&utm_campaign=launch&token=oauth-secret",
      {
        headers: {
          "user-agent": "curl/8",
          referer: "https://x.com/account?code=provider-code",
          "cf-connecting-ip": "1.2.3.4",
        },
      },
    );
    const response = new Response("ok", {
      status: 200,
      headers: { "content-type": "text/html" },
    });
    const envelope = await runAnalytics(
      buildEnvelope(request, response, { mode: "demo", backend: "edge" }).pipe(
        Effect.provideService(Clock.Clock, testClock),
      ),
    );
    expect(envelope.ts).toBe(TEST_NOW);
    expect(envelope.path).toBe("/wire/proto");
    expect(envelope.utm_source).toBe("x");
    expect(envelope.utm_campaign).toBe("launch");
    expect(envelope.ip).toBe("1.2.3.4");
    expect(envelope.referrer_host).toBe("x.com");
    expect(envelope.demo).toEqual({ mode: "demo", backend: "edge" });
    expect(envelope.status).toBe(200);
    const serialized = JSON.stringify(envelope);
    expect(serialized).not.toContain("oauth-secret");
    expect(serialized).not.toContain("provider-code");
  });

  test("bounds and strips control characters from attribution values", async () => {
    const source = `  launch\u0000${"x".repeat(100)}  `;
    const envelope = await runAnalytics(
      buildEnvelope(
        new Request(
          `https://phux.sh/?utm_source=${encodeURIComponent(source)}`,
        ),
        new Response(),
      ),
    );
    expect(envelope.utm_source).not.toContain("\u0000");
    expect(envelope.utm_source.length).toBe(80);
  });
});

describe("analytics forwarding", () => {
  test("forwards only over the service binding without an ingest credential", async () => {
    let forwarded: Request | undefined;
    await runAnalytics(
      forwardEnvelope(
        {
          ANALYTICS: {
            fetch: async (request) => {
              forwarded = request;
              return new Response(null, { status: 204 });
            },
          },
        },
        {
          ts: 1,
          ip: "",
          country: "",
          ua: "",
          referrer_host: "",
          host: "phux.sh",
          method: "GET",
          path: "/",
          utm_source: "",
          utm_medium: "",
          utm_campaign: "",
          status: 200,
          content_type: "text/html",
          accept: "",
        },
      ),
    );
    expect(forwarded).toBeDefined();
    expect(new URL(forwarded!.url).hostname).toBe("analytics.internal");
    expect(forwarded!.headers.has("x-analytics-key")).toBe(false);
  });
});

describe("handleJoin", () => {
  test("rejects malformed emails and returns an Effect-native background write", async () => {
    const bad = await runAnalytics(
      handleJoin(
        new Request("https://phux.sh/api/join", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ email: "not-an-email" }),
        }),
        ENV,
      ),
    );
    expect(bad.response.status).toBe(400);

    const good = await runAnalytics(
      handleJoin(
        new Request("https://phux.sh/api/join", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ email: "A@B.co", source: "landing" }),
        }),
        ENV,
      ),
    );
    expect(good.response.status).toBe(200);
    expect(await good.response.json()).toEqual({ ok: true });
    expect(good.background).toBeDefined();
  });

  test("bounds the request body before decoding", async () => {
    const result = await runAnalytics(
      handleJoin(
        new Request("https://phux.sh/api/join", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ email: "a@b.co", source: "x".repeat(9_000) }),
        }),
        ENV,
      ),
    );
    expect(result.response.status).toBe(413);
  });
});

describe("handleClaim", () => {
  test("accepts full-length expiring proofs and supports one rotation key", async () => {
    const memberId = (
      await runAnalytics(memberIdForEmail("member-key", "a@b.co"))
    ).slice(0, 32);
    const token = await runAnalytics(createClaimToken("old-claim-key", memberId));
    const response = await runAnalytics(
      handleClaim(
        new Request(`https://phux.sh/api/claim?t=${token}`),
        {
          ...ENV,
          MEMBER_CLAIM_KEY: "new-claim-key",
          MEMBER_CLAIM_KEY_PREVIOUS: "old-claim-key",
        },
      ),
    );
    expect(response.status).toBe(302);
    expect(response.headers.get("set-cookie")).toContain(`phux_mid=${memberId}`);
    expect(response.headers.get("set-cookie")).toContain("HttpOnly");
  });

  test("claim creation uses the provided Effect Clock", async () => {
    const token = await runAnalytics(
      createClaimToken("claim-key", "abcdef0123456789").pipe(
        Effect.provideService(Clock.Clock, testClock),
      ),
    );
    expect(token).toStartWith("v1.1700604800.abcdef0123456789.");
  });

  test("rejects expired and legacy prefix proofs", async () => {
    const memberId = "abcdef0123456789abcdef0123456789";
    const expired = await runAnalytics(
      createClaimToken(ENV.MEMBER_CLAIM_KEY, memberId, -1),
    );
    const expiredResponse = await runAnalytics(
      handleClaim(
        new Request(`https://phux.sh/api/claim?t=${expired}`),
        ENV,
      ),
    );
    expect(expiredResponse.status).toBe(403);

    const legacy = await runAnalytics(
      handleClaim(
        new Request(`https://phux.sh/api/claim?t=${memberId}.deadbeefdeadbeef`),
        ENV,
      ),
    );
    expect(legacy.status).toBe(400);
  });
});

describe("analytics HttpRouter", () => {
  test("enforces endpoint-specific methods before entering the domain", async () => {
    const join = await handleAnalyticsHttp(
      new Request("https://phux.sh/api/join"),
      ENV,
    );
    const claim = await handleAnalyticsHttp(
      new Request("https://phux.sh/api/claim", { method: "POST" }),
      ENV,
    );
    expect(join.status).toBe(405);
    expect(claim.status).toBe(405);
  });
});
