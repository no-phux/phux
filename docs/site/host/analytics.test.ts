import { describe, expect, test } from "bun:test";
import {
  CLAIM_MAX_AGE_MS,
  buildEnvelope,
  claimProof,
  forwardEnvelope,
  handleClaim,
  memberIdForEmail,
  timingSafeEqualHex,
  type AnalyticsEnv,
} from "./analytics";

const MEMBER_KEY = "test-member-key";

describe("private analytics envelope (PHA-425)", () => {
  test("allowlists UTM fields and never forwards query or referrer secrets", () => {
    const envelope = buildEnvelope(
      new Request(
        "https://phux.sh/?utm_source=hn&utm_medium=post&utm_campaign=launch&email=person%40example.com&token=oauth-secret",
        {
          headers: {
            referer:
              "https://example.com/account?code=provider-code&email=other%40example.com",
          },
        },
      ),
      new Response(null, { status: 200 }),
    );

    expect(envelope.utm_source).toBe("hn");
    expect(envelope.utm_medium).toBe("post");
    expect(envelope.utm_campaign).toBe("launch");
    expect(envelope.referrer_host).toBe("example.com");
    const serialized = JSON.stringify(envelope);
    for (const secret of [
      "person@example.com",
      "other@example.com",
      "oauth-secret",
      "provider-code",
      "query",
    ]) {
      expect(serialized).not.toContain(secret);
    }
  });

  test("bounds and strips control characters from attribution values", () => {
    const source = `  launch\u0000${"x".repeat(100)}  `;
    const envelope = buildEnvelope(
      new Request(`https://phux.sh/?utm_source=${encodeURIComponent(source)}`),
      new Response(),
    );
    expect(envelope.utm_source).not.toContain("\u0000");
    expect(envelope.utm_source.length).toBe(80);
  });

  test("uses the service binding without a shared ingest credential", async () => {
    let forwarded: Request | null = null;
    const completions: Promise<unknown>[] = [];
    const env: AnalyticsEnv = {
      ANALYTICS: {
        fetch: async (request) => {
          forwarded = request;
          return new Response(null, { status: 204 });
        },
      },
    };
    forwardEnvelope(
      env,
      { waitUntil: (promise) => completions.push(promise) },
      buildEnvelope(new Request("https://phux.sh/"), new Response()),
    );
    await Promise.all(completions);

    expect(forwarded).not.toBeNull();
    expect(new URL(forwarded!.url).hostname).toBe("analytics.internal");
    expect(forwarded!.headers.has("x-analytics-key")).toBe(false);
  });
});

function envOf(): AnalyticsEnv {
  return { MEMBER_KEY };
}

function claimUrl(memberId: string, proof: string, issuedAt?: number): string {
  const token =
    issuedAt === undefined
      ? `${memberId}.${proof}`
      : `${memberId}.${proof}.${Math.floor(issuedAt / 1000)}`;
  return `https://phux.sh/api/claim?t=${token}`;
}

describe("GET /api/claim (PHA-425)", () => {
  test("accepts the private store's stable 32-hex member IDs", async () => {
    const memberId = (await memberIdForEmail(
      MEMBER_KEY,
      "stored@example.com",
    )).slice(0, 32);
    const proof = await claimProof(MEMBER_KEY, memberId);
    const response = await handleClaim(
      new Request(claimUrl(memberId, proof, Date.now())),
      envOf(),
    );
    expect(response.status).toBe(302);
    expect(response.headers.get("set-cookie")).toContain(`phux_mid=${memberId}`);
  });

  test("full proof sets the opt-in cookie and redirects", async () => {
    const memberId = await memberIdForEmail(MEMBER_KEY, "person@example.com");
    const proof = await claimProof(MEMBER_KEY, memberId);
    const response = await handleClaim(
      new Request(claimUrl(memberId, proof)),
      envOf(),
    );
    expect(response.status).toBe(302);
    expect(response.headers.get("location")).toBe("/?joined=1");
    expect(response.headers.get("set-cookie")).toContain(`phux_mid=${memberId}`);
  });

  test("any prefix of the proof is rejected (legacy 32-hex links included)", async () => {
    const memberId = await memberIdForEmail(MEMBER_KEY, "person@example.com");
    const proof = await claimProof(MEMBER_KEY, memberId);
    for (const cut of [16, 32, 48, 63]) {
      const response = await handleClaim(
        new Request(claimUrl(memberId, proof.slice(0, cut))),
        envOf(),
      );
      expect(response.status).toBe(403);
    }
  });

  test("wrong full-length proof is rejected", async () => {
    const memberId = await memberIdForEmail(MEMBER_KEY, "person@example.com");
    const proof = await claimProof(MEMBER_KEY, memberId);
    const tampered = proof.slice(0, -1) + (proof.endsWith("0") ? "1" : "0");
    const response = await handleClaim(
      new Request(claimUrl(memberId, tampered)),
      envOf(),
    );
    expect(response.status).toBe(403);
  });

  test("fresh issuedAt link is accepted, expired and future links are not", async () => {
    const memberId = await memberIdForEmail(MEMBER_KEY, "person@example.com");
    const proof = await claimProof(MEMBER_KEY, memberId);
    const fresh = await handleClaim(
      new Request(claimUrl(memberId, proof, Date.now() - 1000)),
      envOf(),
    );
    expect(fresh.status).toBe(302);

    const expired = await handleClaim(
      new Request(claimUrl(memberId, proof, Date.now() - CLAIM_MAX_AGE_MS - 1000)),
      envOf(),
    );
    expect(expired.status).toBe(403);

    const future = await handleClaim(
      new Request(claimUrl(memberId, proof, Date.now() + 60 * 60 * 1000)),
      envOf(),
    );
    expect(future.status).toBe(403);
  });

  test("malformed tokens are rejected without touching the proof path", async () => {
    for (const token of ["", "abc", `${"a".repeat(64)}`, `${"a".repeat(64)}.${"b".repeat(64)}.x`]) {
      const response = await handleClaim(
        new Request(`https://phux.sh/api/claim?t=${token}`),
        envOf(),
      );
      expect([400, 403]).toContain(response.status);
    }
  });
});

describe("timingSafeEqualHex", () => {
  test("matches only identical strings", () => {
    expect(timingSafeEqualHex("ab12", "ab12")).toBe(true);
    expect(timingSafeEqualHex("ab12", "ab13")).toBe(false);
    expect(timingSafeEqualHex("ab12", "ab1")).toBe(false);
  });
});
