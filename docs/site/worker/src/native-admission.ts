export interface CircuitConfig {
  threshold: number;
  windowMs: number;
  openMs: number;
}

export interface CircuitState {
  failures: number[];
  openUntil: number;
  probeSid?: string;
}

export interface AccountQuotaConfig {
  activeCap: number;
  launchesPerHour: number;
  dailyMs: number;
}

export type AccountLimitReason =
  | "account-active"
  | "account-hourly-limit"
  | "account-daily-limit";

export type CircuitDecision =
  | { allow: true; state: CircuitState; probe: boolean }
  | {
      allow: false;
      state: CircuitState;
      reason: "circuit-open" | "circuit-half-open";
    };

export const EMPTY_CIRCUIT: CircuitState = { failures: [], openUntil: 0 };

export function nativeCapacityReason(
  nativeLive: number,
  nativeCap: number,
  ipLive: number,
  perIpCap: number,
): "native-capacity" | "per-ip-capacity" | undefined {
  if (nativeLive >= nativeCap) return "native-capacity";
  if (ipLive >= perIpCap) return "per-ip-capacity";
  return undefined;
}

export function accountLimitReason(
  active: number,
  launchesInHour: number,
  usedTodayMs: number,
  config: AccountQuotaConfig,
): AccountLimitReason | undefined {
  if (active >= config.activeCap) return "account-active";
  if (launchesInHour >= config.launchesPerHour)
    return "account-hourly-limit";
  if (usedTodayMs >= config.dailyMs) return "account-daily-limit";
  return undefined;
}

export interface DailyUsageSlice {
  day: string;
  durationMs: number;
}

export function utcDay(timestamp: number): string {
  return new Date(timestamp).toISOString().slice(0, 10);
}

export function splitUtcUsage(start: number, end: number): DailyUsageSlice[] {
  if (end <= start) return [];
  const slices: DailyUsageSlice[] = [];
  let cursor = start;
  while (cursor < end) {
    const date = new Date(cursor);
    const nextDay = Date.UTC(
      date.getUTCFullYear(),
      date.getUTCMonth(),
      date.getUTCDate() + 1,
    );
    const sliceEnd = Math.min(end, nextDay);
    slices.push({ day: utcDay(cursor), durationMs: sliceEnd - cursor });
    cursor = sliceEnd;
  }
  return slices;
}

// Returns the first instant at which continuous use would exhaust a UTC day's
// allowance. A session may cross midnight when both days have budget.
export function accountNativeDeadline(
  now: number,
  hardMaxMs: number,
  dailyMs: number,
  usageByDay: ReadonlyMap<string, number>,
): number {
  const hardDeadline = now + hardMaxMs;
  let cursor = now;
  while (cursor < hardDeadline) {
    const date = new Date(cursor);
    const nextDay = Date.UTC(
      date.getUTCFullYear(),
      date.getUTCMonth(),
      date.getUTCDate() + 1,
    );
    const remaining = Math.max(0, dailyMs - (usageByDay.get(utcDay(cursor)) ?? 0));
    const dayDeadline = cursor + remaining;
    if (dayDeadline < nextDay) return Math.min(hardDeadline, dayDeadline);
    cursor = Math.min(hardDeadline, nextDay);
  }
  return hardDeadline;
}

function recentFailures(
  state: CircuitState,
  now: number,
  config: CircuitConfig,
): number[] {
  return state.failures.filter((timestamp) => timestamp > now - config.windowMs);
}

export function decideCircuit(
  current: CircuitState,
  sid: string,
  now: number,
  config: CircuitConfig,
): CircuitDecision {
  const state = { ...current, failures: recentFailures(current, now, config) };
  if (state.openUntil > now) {
    return { allow: false, state, reason: "circuit-open" };
  }

  if (state.openUntil > 0) {
    if (state.probeSid) {
      return { allow: false, state, reason: "circuit-half-open" };
    }
    return {
      allow: true,
      probe: true,
      state: { ...state, probeSid: sid },
    };
  }

  return { allow: true, probe: false, state };
}

export function recordCircuitFailure(
  current: CircuitState,
  sid: string,
  now: number,
  config: CircuitConfig,
): CircuitState {
  const failures = [...recentFailures(current, now, config), now];
  const probeFailed = current.probeSid === sid;
  if (probeFailed || failures.length >= config.threshold) {
    return { failures, openUntil: now + config.openMs };
  }
  return { ...current, failures, probeSid: undefined };
}

export function recordCircuitSuccess(): CircuitState {
  return { ...EMPTY_CIRCUIT };
}
