import { describe, expect, test } from "bun:test";
import { parseHostedEvent, terminalGeometry } from "./core";

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

describe("initial terminal geometry", () => {
  test("fits readable cells into the mobile container instead of scaling 100 columns", () => {
    const grid = terminalGeometry(358, 330, 100, 24);
    expect(grid).toEqual({ cols: 44, rows: 20 });
    expect(grid.cols * 8).toBeLessThanOrEqual(358);
    expect(grid.rows * 16).toBeLessThanOrEqual(330);
  });

  test("keeps the requested desktop grid as an upper bound", () => {
    expect(terminalGeometry(1400, 900, 100, 24)).toEqual({
      cols: 100,
      rows: 24,
    });
  });
});
