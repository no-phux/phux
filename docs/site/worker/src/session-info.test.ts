import { describe, expect, test } from "bun:test";
import {
  isSessionInfo,
  parseSessionInfo,
  publicFallbackReason,
  serializeSessionInfo,
  type InternalFallbackReason,
  type SessionInfo,
} from "./session-info";

describe("session info fallback reasons", () => {
  test("maps every internal reason to the closed public vocabulary", () => {
    const cases: [InternalFallbackReason, string][] = [
      ["auth-required", "auth-required"],
      ["account-active", "account-concurrency"],
      ["account-hourly-limit", "hourly-quota"],
      ["account-daily-limit", "daily-quota"],
      ["native-capacity", "native-capacity"],
      ["per-ip-capacity", "ip-capacity"],
      ["disabled", "native-disabled"],
      ["circuit-open", "native-unhealthy"],
      ["circuit-half-open", "native-unhealthy"],
      ["startup-timeout", "startup-timeout"],
      ["startup-error", "startup-failed"],
      ["pre-upgrade-503", "startup-failed"],
    ];
    for (const [internal, expected] of cases)
      expect(publicFallbackReason(internal)).toBe(expected);
  });
});

describe("session info wire schema", () => {
  const edge: SessionInfo = {
    type: "phux.session.v1",
    outcome: "accepted",
    backend: "edge",
    expiresAt: 1_800_000_000_000,
    fallbackReason: "native-capacity",
  };

  test("round trips the exact public data", () => {
    expect(parseSessionInfo(serializeSessionInfo(edge))).toEqual(edge);
    expect(
      parseSessionInfo(
        serializeSessionInfo({ ...edge, backend: "native", fallbackReason: undefined }),
      ),
    ).toEqual({
      type: "phux.session.v1",
      outcome: "accepted",
      backend: "native",
      expiresAt: edge.expiresAt,
    });
  });

  test("rejects malformed, extra, and non-public data", () => {
    for (const value of [
      null,
      [],
      { ...edge, type: "phux.session.v2" },
      { ...edge, expiresAt: Infinity },
      { ...edge, expiresAt: 1.5 },
      { ...edge, fallbackReason: "pre-upgrade-503" },
      { ...edge, backend: "native" },
      { ...edge, error: "upstream exploded" },
    ])
      expect(isSessionInfo(value)).toBe(false);
    expect(parseSessionInfo("not json")).toBeUndefined();
  });
});
