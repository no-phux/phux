import type { OpenCodeLifecycle } from "./lifecycle.js";

/** Apply one OpenCode V2 server event to the phux producer. Unrelated events are ignored. */
export function applyServerEvent(
  lifecycle: OpenCodeLifecycle,
  event: { readonly type?: unknown; readonly data?: unknown },
): Promise<void> {
  if (event.data === null || typeof event.data !== "object" || Array.isArray(event.data)) return Promise.resolve();
  const data = event.data as Record<string, unknown>;
  const sessionID = data.sessionID;
  if (typeof sessionID !== "string") return Promise.resolve();
  switch (event.type) {
    case "session.status": {
      const status = data.status;
      const statusType = status !== null && typeof status === "object" ? (status as { readonly type?: unknown }).type : undefined;
      if (statusType === "busy") return lifecycle.observeState(sessionID, "working");
      if (statusType === "idle") return lifecycle.observeState(sessionID, "idle");
      return Promise.resolve();
    }
    case "session.idle":
      return lifecycle.observeState(sessionID, "idle");
    case "session.deleted":
      return lifecycle.deleteSession(sessionID);
    case "permission.asked":
      return lifecycle.ask(sessionID, permissionData(data));
    default:
      return Promise.resolve();
  }
}

export function deletedSessionId(event: { readonly type?: unknown; readonly data?: unknown }): string | undefined {
  if (event.type !== "session.deleted") return undefined;
  if (event.data === null || typeof event.data !== "object" || Array.isArray(event.data)) return undefined;
  const sessionID = (event.data as { readonly sessionID?: unknown }).sessionID;
  return typeof sessionID === "string" ? sessionID : undefined;
}

function permissionData(data: Record<string, unknown>): Readonly<Record<string, unknown>> {
  const id = typeof data.id === "string" ? data.id : undefined;
  const question = typeof data.message === "string" ? data.message
    : typeof data.action === "string" ? data.action
    : undefined;
  return {
    kind: "permission",
    ...(id === undefined ? {} : { id }),
    ...(question === undefined ? {} : { question }),
  };
}
