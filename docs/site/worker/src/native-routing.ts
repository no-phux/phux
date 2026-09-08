import type { DemoMode } from "./portfolio";

const DEFAULT_CLEANUP_MARGIN_MS = 30_000;
const MAX_CLEANUP_MARGIN_MS = 60_000;

export function parseDemoMode(value: string | null): DemoMode | null {
  const mode = value ?? "demo";
  return mode === "demo" || mode === "portfolio" || mode === "native" ? mode : null;
}

export function isNativeEnabled(value: string | undefined): boolean {
  return !["0", "false", "off", "no"].includes((value ?? "true").toLowerCase());
}

export function nativeReservationTtlMs(
  hardMaxMs: number,
  sessionTtlMs: number,
  launchMarginMs = MAX_CLEANUP_MARGIN_MS,
): number {
  return Math.max(sessionTtlMs, sessionReservationTtlMs(hardMaxMs, launchMarginMs));
}

export function sessionReservationTtlMs(
  hardMaxMs: number,
  cleanupMarginMs = DEFAULT_CLEANUP_MARGIN_MS,
): number {
  const hard = Number.isFinite(hardMaxMs) ? Math.max(1, hardMaxMs) : 1;
  const margin = Number.isFinite(cleanupMarginMs)
    ? Math.min(MAX_CLEANUP_MARGIN_MS, Math.max(0, cleanupMarginMs))
    : DEFAULT_CLEANUP_MARGIN_MS;
  return hard + margin;
}

export interface SessionDeadlines {
  idleDeadline: number;
  expiresAt: number;
}

export function sessionDeadline(deadlines: SessionDeadlines): number {
  return Math.min(deadlines.idleDeadline, deadlines.expiresAt);
}

export function sessionExpiryReason(
  now: number,
  deadlines: SessionDeadlines,
): "idle" | "hard" | null {
  if (now >= deadlines.expiresAt) return "hard";
  if (now >= deadlines.idleDeadline) return "idle";
  return null;
}

export function nativeUpstreamRequest(request: Request): Request {
  const url = new URL(request.url);
  url.pathname = "/";
  url.search = "";
  url.hash = "";

  const headers = new Headers(request.headers);
  for (const name of [
    "CF-Connecting-IP",
    "X-Forwarded-For",
    "X-Phux-Session",
    "X-Phux-Token",
  ]) {
    headers.delete(name);
  }

  return new Request(url, { method: request.method, headers });
}
