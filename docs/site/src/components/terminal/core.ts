// Framework-agnostic controller for the live phux terminal.
//
// Wraps the real phux-web wasm client (src/lib/phux-web, built from phux's
// clients/phux-web — the real wire codec + embedded libghostty-vt engine). No
// React in here, so a web component or any framework can drive it too. The
// React wrapper lives in ../PhuxTerminal.tsx.

export type SessionBackend = "native" | "edge";
export type SessionFallbackReason =
  | "auth-required"
  | "account-concurrency"
  | "hourly-quota"
  | "daily-quota"
  | "native-capacity"
  | "ip-capacity"
  | "native-disabled"
  | "native-unhealthy"
  | "startup-timeout"
  | "startup-failed";

export type HostedEvent =
  | {
      type: "phux.session.v1";
      outcome: "accepted";
      backend: SessionBackend;
      expiresAt: number;
      fallbackReason?: SessionFallbackReason;
    }
  | { type: "error"; category: "protocol" | "client" | "transport" }
  | {
      type: "close";
      code: number;
      category:
        | "normal"
        | "going-away"
        | "protocol"
        | "server"
        | "unavailable"
        | "capacity"
        | "rate-limited"
        | "bad-request"
        | "idle"
        | "expired"
        | "unauthorized"
        | "network";
      wasClean: boolean;
    };

export interface PhuxController {
  close(): void;
}

export interface MountOptions {
  /** WebSocket URL of a phux server (or the demo Worker). */
  wsUrl: string;
  /** Id of the <canvas> the client renders into; must already be in the DOM. */
  canvasId: string;
  cols: number;
  rows: number;
  mode?: "demo" | "portfolio" | "native";
  onEvent?: (event: HostedEvent) => void;
}

/**
 * Load the phux-web wasm (client + embedded engine) and attach a live session
 * to the canvas. The module is a dynamic import so it code-splits and only
 * downloads when a terminal actually mounts.
 *
 * `start()` opens the WebSocket, runs HELLO/ATTACH, and drives the engine→canvas
 * render loop + keyboard + cursor blink for the connection's lifetime.
 */
export async function mountPhuxTerminal(
  opts: MountOptions,
): Promise<PhuxController> {
  const [mod, wasm] = await Promise.all([
    import("../../lib/phux-web/phux_web.js"),
    import("../../lib/phux-web/phux_web_bg.wasm?url"),
  ]);
  await mod.default({ module_or_path: wasm.default });
  const endpoint = new URL(opts.wsUrl);
  if (opts.mode && opts.mode !== "demo")
    endpoint.searchParams.set("mode", opts.mode);
  return mod.start_hosted(
    endpoint.toString(),
    opts.canvasId,
    opts.cols,
    opts.rows,
    (value: unknown) => {
      const event = parseHostedEvent(value);
      if (event) opts.onEvent?.(event);
    },
  );
}

const fallbacks = new Set<SessionFallbackReason>([
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
]);

const closeCategories = new Set([
  "normal",
  "going-away",
  "protocol",
  "server",
  "unavailable",
  "capacity",
  "rate-limited",
  "bad-request",
  "idle",
  "expired",
  "unauthorized",
  "network",
]);

export function parseHostedEvent(value: unknown): HostedEvent | null {
  if (!value || typeof value !== "object" || Array.isArray(value)) return null;
  const event = value as Record<string, unknown>;
  if (event.type === "phux.session.v1") {
    const keys = Object.keys(event);
    if (
      !keys.every((key) =>
        ["type", "outcome", "backend", "expiresAt", "fallbackReason"].includes(
          key,
        ),
      ) ||
      event.outcome !== "accepted" ||
      (event.backend !== "native" && event.backend !== "edge") ||
      !Number.isSafeInteger(event.expiresAt) ||
      (event.expiresAt as number) <= 0
    ) {
      return null;
    }
    if (
      event.fallbackReason !== undefined &&
      (event.backend !== "edge" ||
        typeof event.fallbackReason !== "string" ||
        !fallbacks.has(event.fallbackReason as SessionFallbackReason))
    ) {
      return null;
    }
    return event as HostedEvent;
  }
  if (
    event.type === "error" &&
    Object.keys(event).length === 2 &&
    ["protocol", "client", "transport"].includes(String(event.category))
  ) {
    return event as HostedEvent;
  }
  if (
    event.type === "close" &&
    Object.keys(event).length === 4 &&
    Number.isInteger(event.code) &&
    typeof event.category === "string" &&
    closeCategories.has(event.category) &&
    typeof event.wasClean === "boolean"
  ) {
    return event as HostedEvent;
  }
  return null;
}
