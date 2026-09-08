import type {
  SessionBackend,
  SessionFallbackReason,
} from "./core";

export type EmbedStatus = "loading" | "unlock" | "live" | "error" | "closed";

interface ParentWindow {
  postMessage(message: unknown, targetOrigin: string): void;
}

/**
 * Resolve the one origin we are willing to talk to, or null when the
 * referrer is missing, malformed, or simply not the parent we expect.
 */
function allowedOrigin(referrer: string, allowedParentOrigin: string): string | null {
  let allowed: URL;
  let source: URL;
  try {
    allowed = new URL(allowedParentOrigin);
    source = new URL(referrer);
  } catch {
    return null;
  }
  return source.origin === allowed.origin ? allowed.origin : null;
}

export function postEmbedStatus(
  parent: ParentWindow,
  referrer: string,
  allowedParentOrigin: string,
  status: EmbedStatus,
): boolean {
  const origin = allowedOrigin(referrer, allowedParentOrigin);
  if (!origin) return false;
  parent.postMessage({ source: "phux-embed", type: "phux:status", status }, origin);
  return true;
}

/**
 * Tell the parent which backend served this session. phall.io shows it on
 * the deck nameplate; without it that readout has nothing to display.
 */
export function postEmbedSession(
  parent: ParentWindow,
  referrer: string,
  allowedParentOrigin: string,
  session: {
    backend: SessionBackend;
    expiresAt: number;
    fallbackReason?: SessionFallbackReason;
  },
): boolean {
  const origin = allowedOrigin(referrer, allowedParentOrigin);
  if (!origin) return false;
  parent.postMessage(
    { source: "phux-embed", type: "phux:session", ...session },
    origin,
  );
  return true;
}

export function postEmbedClose(
  parent: ParentWindow,
  referrer: string,
  allowedParentOrigin: string,
  close: { code: number; category: string; wasClean: boolean },
): boolean {
  const origin = allowedOrigin(referrer, allowedParentOrigin);
  if (!origin) return false;
  parent.postMessage(
    { source: "phux-embed", type: "phux:close", ...close },
    origin,
  );
  return true;
}
