import { describe, expect, test } from "bun:test";
import {
  buildEnvelope,
  claimProof,
  handleClaim,
  handleJoin,
  memberIdForEmail,
  memberIdFromRequest,
} from "./analytics";

const ENV = {
  ASSETS: { fetch: async () => new Response("x") },
  ANALYTICS_INGEST_URL: "https://ops.phux.sh/ingest",
  ANALYTICS_INGEST_KEY: "k",
  MEMBER_KEY: "member-key",
};

describe("member identity", () => {
  test("member id is a stable HMAC of the normalized email", async () => {
    const a = await memberIdForEmail("member-key", "Foo@Example.COM");
    const b = await memberIdForEmail("member-key", " foo@example.com ");
    expect(a).toBe(b);
    expect(a).toMatch(/^[a-f0-9]{64}$/);
    const c = await memberIdForEmail("other-key", "foo@example.com");
    expect(c).not.toBe(a);
  });

  test("member cookie parses and rejects junk", () => {
    const request = new Request("https://phux.sh/", {
      headers: { cookie: "other=1; phux_mid=abcdef0123456789; x=2" },
    });
    expect(memberIdFromRequest(request)).toBe("abcdef0123456789");
    const bad = new Request("https://phux.sh/", {
      headers: { cookie: "phux_mid=../../etc" },
    });
    expect(memberIdFromRequest(bad)).toBeNull();
  });
});

describe("buildEnvelope", () => {
  test("captures the exchange without blocking on the body", () => {
    const request = new Request("https://docs.phux.sh/wire/proto? utm_source=x", {
      headers: {
        "user-agent": "curl/8",
        referer: "https://x.com/somepost",
        "cf-connecting-ip": "1.2.3.4",
      },
    });
    const response = new Response("ok", {
      status: 200,
      headers: { "content-type": "text/html" },
    });
    const envelope = buildEnvelope(request, response, { mode: "demo", backend: "edge" });
    expect(envelope.path).toBe("/wire/proto");
    expect(envelope.query).toContain("utm_source=x");
    expect(envelope.ip).toBe("1.2.3.4");
    expect(envelope.referrer).toContain("x.com");
    expect(envelope.demo).toEqual({ mode: "demo", backend: "edge" });
    expect(envelope.status).toBe(200);
  });
});

describe("handleJoin", () => {
  test("rejects malformed emails and accepts good ones", async () => {
    const bad = await handleJoin(
      new Request("https://phux.sh/api/join", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ email: "not-an-email" }),
      }),
      ENV,
      undefined,
    );
    expect(bad.status).toBe(400);

    const good = await handleJoin(
      new Request("https://phux.sh/api/join", {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ email: "A@B.co", source: "landing" }),
      }),
      ENV,
      undefined,
    );
    expect(good.status).toBe(200);
    expect(await good.json()).toEqual({ ok: true });
  });
});

describe("handleClaim", () => {
  test("sets the opt-in cookie only with a valid proof", async () => {
    const memberId = (await memberIdForEmail("member-key", "a@b.co")).slice(0, 32);
    const proof = (await claimProof("member-key", memberId)).slice(0, 32);
    const good = await handleClaim(
      new Request(`https://phux.sh/api/claim?t=${memberId}.${proof}`),
      ENV,
    );
    expect(good.status).toBe(302);
    expect(good.headers.get("set-cookie")).toContain(`phux_mid=${memberId}`);
    expect(good.headers.get("set-cookie")).toContain("HttpOnly");

    const bad = await handleClaim(
      new Request(`https://phux.sh/api/claim?t=${memberId}.deadbeefdeadbeef`),
      ENV,
    );
    expect(bad.status).toBe(403);
  });
});
