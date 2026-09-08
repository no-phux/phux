import { describe, expect, test } from "bun:test";
import {
  accountLimitReason,
  accountNativeDeadline,
  decideCircuit,
  EMPTY_CIRCUIT,
  nativeCapacityReason,
  recordCircuitFailure,
  recordCircuitSuccess,
  splitUtcUsage,
  type CircuitConfig,
} from "./native-admission";

const config: CircuitConfig = { threshold: 3, windowMs: 1_000, openMs: 5_000 };

describe("native circuit breaker", () => {
  test("opens at threshold and admits exactly one cooldown probe", () => {
    let state = EMPTY_CIRCUIT;
    state = recordCircuitFailure(state, "a", 100, config);
    state = recordCircuitFailure(state, "b", 200, config);
    state = recordCircuitFailure(state, "c", 300, config);
    expect(decideCircuit(state, "blocked", 301, config)).toMatchObject({
      allow: false,
      reason: "circuit-open",
    });

    const probe = decideCircuit(state, "probe", 5_301, config);
    expect(probe).toMatchObject({ allow: true, probe: true });
    expect(decideCircuit(probe.state, "burst", 5_301, config)).toMatchObject({
      allow: false,
      reason: "circuit-half-open",
    });
  });

  test("failed probe reopens and successful start resets", () => {
    const open = { failures: [100, 200, 300], openUntil: 5_300 };
    const probe = decideCircuit(open, "probe", 5_300, config);
    const reopened = recordCircuitFailure(probe.state, "probe", 5_301, config);
    expect(reopened.openUntil).toBe(10_301);
    expect(recordCircuitSuccess()).toEqual(EMPTY_CIRCUIT);
  });

  test("failures outside the window do not accumulate", () => {
    const state = recordCircuitFailure(
      recordCircuitFailure(EMPTY_CIRCUIT, "a", 1, config),
      "b",
      2_000,
      config,
    );
    expect(state.failures).toEqual([2_000]);
    expect(state.openUntil).toBe(0);
  });
});

describe("native burst admission simulation", () => {
  test("admits at most 25 native sessions and sends every overflow to edge", () => {
    const nativeCap = 25;
    let native = 0;
    const outcomes = Array.from({ length: 50 }, () => {
      if (nativeCapacityReason(native, nativeCap, 0, 1)) return "edge";
      native += 1;
      return "native";
    });
    expect(outcomes.filter((outcome) => outcome === "native")).toHaveLength(25);
    expect(outcomes.filter((outcome) => outcome === "edge")).toHaveLength(25);
  });

  test("one active native per IP falls back without consuming another native slot", () => {
    expect(nativeCapacityReason(1, 25, 1, 1)).toBe("per-ip-capacity");
    expect(nativeCapacityReason(25, 25, 0, 1)).toBe("native-capacity");
  });
});

describe("native account quotas", () => {
  const quota = { activeCap: 1, launchesPerHour: 6, dailyMs: 1_800_000 };

  test("enforces each limit at its exact boundary", () => {
    expect(accountLimitReason(1, 0, 0, quota)).toBe("account-active");
    expect(accountLimitReason(0, 6, 0, quota)).toBe("account-hourly-limit");
    expect(accountLimitReason(0, 5, 1_800_000, quota)).toBe(
      "account-daily-limit",
    );
    expect(accountLimitReason(0, 5, 1_799_999, quota)).toBeUndefined();
  });

  test("sequential decisions model atomic one-shell admission", () => {
    let active = 0;
    const outcomes = Array.from({ length: 20 }, () => {
      const reason = accountLimitReason(active, 0, 0, quota);
      if (reason) return reason;
      active += 1;
      return "native";
    });
    expect(outcomes.filter((outcome) => outcome === "native")).toHaveLength(1);
    expect(outcomes.slice(1).every((outcome) => outcome === "account-active")).toBe(
      true,
    );
  });

  test("splits actual duration at UTC midnight and ignores duplicate zero duration", () => {
    const start = Date.UTC(2026, 0, 1, 23, 59, 59, 500);
    expect(splitUtcUsage(start, start + 1_500)).toEqual([
      { day: "2026-01-01", durationMs: 500 },
      { day: "2026-01-02", durationMs: 1_000 },
    ]);
    expect(splitUtcUsage(start, start)).toEqual([]);
    expect(splitUtcUsage(start, start - 1)).toEqual([]);
  });

  test("deadline consumes only actual remaining daily budget", () => {
    const now = Date.UTC(2026, 0, 1, 12);
    expect(
      accountNativeDeadline(
        now,
        300_000,
        1_800_000,
        new Map([["2026-01-01", 1_799_000]]),
      ),
    ).toBe(now + 1_000);
  });

  test("deadline can cross UTC midnight into a fresh daily budget", () => {
    const now = Date.UTC(2026, 0, 1, 23, 59, 59);
    expect(accountNativeDeadline(now, 5_000, 2_000, new Map())).toBe(now + 3_000);
  });
});
