import type { OpenCodeLifecycleEvent } from "../../opencode/src/lifecycle.js";

const LIFECYCLE_TYPES = new Set(["session.status", "session.idle", "session.deleted", "permission.asked"]);

/**
 * V2 public events carry `data`. The shared lifecycle handler still reads the
 * V1 `properties` shape, including `session.deleted`'s `properties.info.id`.
 */
export function toLifecycleEvent(event: { readonly type?: unknown; readonly data?: unknown }): OpenCodeLifecycleEvent | undefined {
  if (typeof event.type !== "string" || !LIFECYCLE_TYPES.has(event.type)) return undefined;
  if (event.data === null || typeof event.data !== "object" || Array.isArray(event.data)) return undefined;
  const data = event.data as Record<string, unknown>;
  if (event.type === "session.deleted") {
    const sessionID = data.sessionID;
    if (typeof sessionID !== "string") return undefined;
    return { type: event.type, properties: { ...data, info: { id: sessionID } } };
  }
  if (event.type === "permission.asked") {
    return {
      type: event.type,
      properties: {
        ...data,
        ...(typeof data.message === "string" && typeof data.title !== "string" ? { title: data.message } : {}),
        ...(typeof data.action === "string" && typeof data.permission !== "string" ? { permission: data.action } : {}),
      },
    };
  }
  return { type: event.type, properties: data };
}

export function deletedSessionId(event: { readonly type?: unknown; readonly data?: unknown }): string | undefined {
  if (event.type !== "session.deleted") return undefined;
  if (event.data === null || typeof event.data !== "object" || Array.isArray(event.data)) return undefined;
  const sessionID = (event.data as { readonly sessionID?: unknown }).sessionID;
  return typeof sessionID === "string" ? sessionID : undefined;
}
