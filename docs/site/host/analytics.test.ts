import { describe, expect, test } from "bun:test";
import {
  CLAIM_MAX_AGE_MS,
  claimProof,
  handleClaim,
  memberIdForEmail,
  timingSafeEqualHex,
  type AnalyticsEnv,
} from "./analytics";

const MEMBER_KEY = "test-member-key";

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
