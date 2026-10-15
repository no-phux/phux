export const PUBLIC_FALLBACK_REASONS = [
  "auth-required",
  "account-concurrency",
  "hourly-quota",
  "daily-quota",
  "native-capacity",
  "ip-capacity",
  "native-disabled",
  "native-unhealthy",
  "startup-timeout",
  "startup-failed",
] as const;

export type PublicFallbackReason = (typeof PUBLIC_FALLBACK_REASONS)[number];
export type SessionBackend = "native" | "edge";

/**
 * A single-use attach code for a second screen, minted by the session itself and
 * delivered only to the view that opened it. The pairing link carries it; the
 * session's own durable object decides whether it is still spendable, so a
 * leaked code buys one attachment to one disposable shell and nothing else.
 */
export type SessionPair = {
  code: string;
  /** Never later than the session's own expiry. */
  expiresAt: number;
};

export type SessionInfo = {
  type: "phux.session.v1";
  outcome: "accepted";
  backend: SessionBackend;
  expiresAt: number;
  fallbackReason?: PublicFallbackReason;
  pair?: SessionPair;
};

export type InternalFallbackReason =
  | "auth-required"
  | "account-active"
  | "account-hourly-limit"
  | "account-daily-limit"
  | "native-capacity"
  | "per-ip-capacity"
  | "disabled"
  | "circuit-open"
  | "circuit-half-open"
  | "startup-timeout"
  | "startup-error"
  | `pre-upgrade-${number}`;

const publicReasons = new Set<string>(PUBLIC_FALLBACK_REASONS);

export function publicFallbackReason(
  reason: InternalFallbackReason,
): PublicFallbackReason {
  switch (reason) {
    case "auth-required":
      return "auth-required";
    case "account-active":
      return "account-concurrency";
    case "account-hourly-limit":
      return "hourly-quota";
    case "account-daily-limit":
      return "daily-quota";
    case "native-capacity":
      return "native-capacity";
    case "per-ip-capacity":
      return "ip-capacity";
    case "disabled":
      return "native-disabled";
    case "circuit-open":
    case "circuit-half-open":
      return "native-unhealthy";
    case "startup-timeout":
      return "startup-timeout";
    case "startup-error":
      return "startup-failed";
    default:
      if (reason.startsWith("pre-upgrade-")) return "startup-failed";
      throw new TypeError("unknown internal fallback reason");
  }
}

/** Longest attach code accepted on the wire (the minted code is far shorter). */
export const MAX_PAIR_CODE_LENGTH = 512;

function isSessionPair(value: unknown, sessionExpiresAt: number): boolean {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const pair = value as Record<string, unknown>;
  if (Object.keys(pair).some((key) => key !== "code" && key !== "expiresAt"))
    return false;
  return (
    typeof pair.code === "string" &&
    pair.code.length > 0 &&
    pair.code.length <= MAX_PAIR_CODE_LENGTH &&
    typeof pair.expiresAt === "number" &&
    Number.isSafeInteger(pair.expiresAt) &&
    pair.expiresAt > 0 &&
    pair.expiresAt <= sessionExpiresAt
  );
}

export function isSessionInfo(value: unknown): value is SessionInfo {
  if (!value || typeof value !== "object" || Array.isArray(value)) return false;
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record);
  if (keys.some((key) => !["type", "outcome", "backend", "expiresAt", "fallbackReason", "pair"].includes(key)))
    return false;
  if (
    record.type !== "phux.session.v1" ||
    record.outcome !== "accepted" ||
    (record.backend !== "native" && record.backend !== "edge") ||
    typeof record.expiresAt !== "number" ||
    !Number.isSafeInteger(record.expiresAt) ||
    record.expiresAt <= 0
  )
    return false;
  if (record.fallbackReason !== undefined) {
    if (
      record.backend !== "edge" ||
      typeof record.fallbackReason !== "string" ||
      !publicReasons.has(record.fallbackReason)
    )
      return false;
  }
  if (
    record.pair !== undefined &&
    !isSessionPair(record.pair, record.expiresAt as number)
  )
    return false;
  return true;
}

export function serializeSessionInfo(info: SessionInfo): string {
  if (!isSessionInfo(info)) throw new TypeError("invalid session info");
  return JSON.stringify(info);
}

export function parseSessionInfo(value: string): SessionInfo | undefined {
  try {
    const parsed: unknown = JSON.parse(value);
    return isSessionInfo(parsed) ? parsed : undefined;
  } catch {
    return undefined;
  }
}
