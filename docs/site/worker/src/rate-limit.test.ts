import { describe, expect, test } from "bun:test";
import { rateLimitName } from "./rate-limit-key";

describe("per-IP rate-limit identity", () => {
  test("is stable, secret-keyed, and does not retain the address", async () => {
    const ip = "203.0.113.42";
    const first = await rateLimitName(ip, "secret-one");
    expect(first).toHaveLength(64);
    expect(first).toBe(await rateLimitName(ip, "secret-one"));
    expect(first).not.toBe(await rateLimitName(ip, "secret-two"));
    expect(first).not.toContain(ip);
  });
});
