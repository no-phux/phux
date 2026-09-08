import { describe, expect, test } from "bun:test";
import { postEmbedClose, postEmbedSession, postEmbedStatus } from "./embed-status";

describe("embed status messages", () => {
  test("posts only to the exact validated parent origin", () => {
    const messages: unknown[] = [];
    const parent = {
      postMessage(message: unknown, targetOrigin: string) {
        messages.push({ message, targetOrigin });
      },
    };
    expect(
      postEmbedStatus(parent, "https://phall.io/work", "https://phall.io", "live"),
    ).toBe(true);
    expect(messages).toEqual([
      {
        message: {
          source: "phux-embed",
          type: "phux:status",
          status: "live",
        },
        targetOrigin: "https://phall.io",
      },
    ]);
  });

  test("rejects missing, malformed, and different referrer origins", () => {
    const parent = { postMessage() {} };
    expect(postEmbedStatus(parent, "", "https://phall.io", "loading")).toBe(false);
    expect(
      postEmbedStatus(parent, "https://evil.example", "https://phall.io", "error"),
    ).toBe(false);
    expect(postEmbedStatus(parent, "https://phall.io", "*", "live")).toBe(false);
    expect(postEmbedStatus(parent, "https://phall.io", "https://phall.io", "unlock")).toBe(
      true,
    );
  });
});

describe("embed session messages", () => {
  test("posts only to the exact validated parent origin", () => {
    const messages: unknown[] = [];
    const parent = {
      postMessage(message: unknown, targetOrigin: string) {
        messages.push({ message, targetOrigin });
      },
    };
    expect(
      postEmbedSession(parent, "https://phall.io/work", "https://phall.io", {
        backend: "edge",
        expiresAt: 1_800_000_000_000,
        fallbackReason: "auth-required",
      }),
    ).toBe(true);
    expect(messages).toEqual([
      {
        message: {
          source: "phux-embed",
          type: "phux:session",
          backend: "edge",
          expiresAt: 1_800_000_000_000,
          fallbackReason: "auth-required",
        },
        targetOrigin: "https://phall.io",
      },
    ]);
  });

  test("rejects missing, malformed, and different referrer origins", () => {
    const parent = { postMessage() {} };
    expect(
      postEmbedSession(parent, "", "https://phall.io", {
        backend: "edge",
        expiresAt: 1,
      }),
    ).toBe(false);
    expect(
      postEmbedSession(parent, "https://evil.example", "https://phall.io", {
        backend: "edge",
        expiresAt: 1,
      }),
    ).toBe(false);
  });

  test("posts normalized close data only to the validated parent", () => {
    const messages: unknown[] = [];
    const parent = {
      postMessage(message: unknown, targetOrigin: string) {
        messages.push({ message, targetOrigin });
      },
    };
    expect(
      postEmbedClose(parent, "https://phall.io", "https://phall.io", {
        code: 4005,
        category: "expired",
        wasClean: true,
      }),
    ).toBe(true);
    expect(JSON.stringify(messages)).not.toContain("reason");
  });
});
