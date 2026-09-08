import { describe, expect, test } from "bun:test";
import { parseHostedEvent } from "./core";

describe("hosted client events", () => {
  test("accepts authoritative session and normalized close events", () => {
    expect(
      parseHostedEvent({
        type: "phux.session.v1",
        outcome: "accepted",
        backend: "edge",
        expiresAt: 1_800_000_000_000,
        fallbackReason: "auth-required",
      }),
    ).not.toBeNull();
    expect(
      parseHostedEvent({
        type: "close",
        code: 4005,
        category: "expired",
        wasClean: true,
      }),
    ).not.toBeNull();
  });

  test("rejects unknown fields and unsafe server vocabulary", () => {
    for (const value of [
      null,
      { type: "phux.session.v1", backend: "native", expiresAt: 1 },
      {
        type: "phux.session.v1",
        outcome: "accepted",
        backend: "native",
        expiresAt: 1,
        fallbackReason: "startup-failed",
      },
      {
        type: "close",
        code: 1011,
        category: "raw upstream reason",
        wasClean: false,
      },
      { type: "error", category: "provider-secret", detail: "unsafe" },
    ]) {
      expect(parseHostedEvent(value)).toBeNull();
    }
  });
});
