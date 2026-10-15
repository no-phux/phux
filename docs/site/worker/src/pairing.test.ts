import { describe, expect, test } from "bun:test";
import { pairRejection, type PairRecord } from "./pairing";

const now = 1_800_000_000_000;

const fresh: PairRecord = { code: "minted.code", expiresAt: now + 60_000 };

describe("pairing admission", () => {
  test("spends a fresh, unspent, matching code", () => {
    expect(pairRejection(fresh, fresh.code, now)).toBeNull();
  });

  test("refuses a code that was already spent", () => {
    const spent: PairRecord = { ...fresh, claimedAt: now - 1 };
    expect(pairRejection(spent, spent.code, now)).toBe(
      "this pairing link was already used",
    );
  });

  test("refuses a code that expired, including exactly at its deadline", () => {
    expect(pairRejection({ ...fresh, expiresAt: now - 1 }, fresh.code, now)).toBe(
      "this pairing link has expired",
    );
    // A deadline is not a grace period.
    expect(pairRejection({ ...fresh, expiresAt: now }, fresh.code, now)).toBe(
      "this pairing link has expired",
    );
  });

  test("refuses a code that is not this session's, and a missing record", () => {
    expect(pairRejection(fresh, "some.other.code", now)).toBe(
      "this pairing link is not valid",
    );
    expect(pairRejection(undefined, fresh.code, now)).toBe(
      "this pairing link was already used",
    );
  });

  test("spending is reported as a state, so a reused link never re-opens", () => {
    // The record the session stores after a successful attach.
    const spent: PairRecord = { ...fresh, claimedAt: now };
    expect(pairRejection(spent, spent.code, now + 1)).not.toBeNull();
  });
});
