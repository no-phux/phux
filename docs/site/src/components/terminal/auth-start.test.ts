import { describe, expect, test } from "bun:test";
import { nativeAuthStartUrl } from "./auth-start";

describe("nativeAuthStartUrl", () => {
  test("sends the worker's return_to allowlist, not a camelCase alias", () => {
    const url = new URL(nativeAuthStartUrl("https://phux.sh", "github", "/embed"));
    expect(url.origin).toBe("https://phux.sh");
    expect(url.pathname).toBe("/auth/github");
    expect(url.searchParams.get("return_to")).toBe("/embed");
    expect(url.searchParams.has("returnTo")).toBe(false);
  });

  test("keeps the homepage completion path exact", () => {
    expect(nativeAuthStartUrl("https://phux.sh", "google", "/")).toBe(
      "https://phux.sh/auth/google?return_to=%2F",
    );
  });
});
