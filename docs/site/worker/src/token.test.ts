import { describe, expect, test } from "bun:test";
import { mintToken, verifyToken } from "./token";

const secret = "test-session-token-secret";

describe("session token scopes", () => {
  test("a session token opens a session and nothing else", async () => {
    const token = await mintToken(secret, {
      sid: "session-1",
      exp: Date.now() + 60_000,
      scope: "session",
    });
    expect(await verifyToken(secret, token, "session")).toMatchObject({ sid: "session-1" });
    expect(await verifyToken(secret, token, "attach")).toBeNull();
  });

  test("a pairing code attaches to its session and cannot open one", async () => {
    const code = await mintToken(secret, {
      sid: "session-2",
      exp: Date.now() + 60_000,
      scope: "attach",
    });
    expect(await verifyToken(secret, code, "attach")).toMatchObject({ sid: "session-2" });
    expect(await verifyToken(secret, code, "session")).toBeNull();
  });

  test("a token minted before scopes existed still opens a session", async () => {
    // Older tokens carried no scope; they were all session tokens.
    const legacy = await mintToken(secret, { sid: "session-3", exp: Date.now() + 60_000 });
    expect(await verifyToken(secret, legacy, "session")).toMatchObject({ sid: "session-3" });
    expect(await verifyToken(secret, legacy, "attach")).toBeNull();
  });

  test("rejects unknown scopes, other secrets, tampering and expiry", async () => {
    const forged = await mintToken(secret, {
      sid: "session-4",
      exp: Date.now() + 60_000,
      // A scope the protocol does not define must not be a usable credential.
      scope: "admin" as unknown as "session",
    });
    expect(await verifyToken(secret, forged, "session")).toBeNull();

    const token = await mintToken(secret, {
      sid: "session-5",
      exp: Date.now() + 60_000,
      scope: "attach",
    });
    expect(await verifyToken("some-other-secret", token, "attach")).toBeNull();
    expect(await verifyToken(secret, `${token}x`, "attach")).toBeNull();
    expect(await verifyToken(secret, "not-a-token", "attach")).toBeNull();

    const expired = await mintToken(secret, {
      sid: "session-6",
      exp: Date.now() - 1,
      scope: "attach",
    });
    expect(await verifyToken(secret, expired, "attach")).toBeNull();
  });
});
