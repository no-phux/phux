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

/**
 * Apply one OpenCode 1.x plugin event (`{ type, properties }`) to the phux
 * producer. 1.x names a permission `permission.asked` (`permission.updated`
 * before 1.2) and also asks `question.asked`.
 */
export function applyV1Event(
  lifecycle: OpenCodeLifecycle,
  event: { readonly type?: unknown; readonly properties?: unknown },
): Promise<void> {
  const props = record(event.properties);
  if (props === undefined) return Promise.resolve();
  if (event.type === "session.deleted") {
    const id = record(props.info)?.id;
    return typeof id === "string" ? lifecycle.deleteSession(id) : Promise.resolve();
  }
  const sessionID = props.sessionID;
  if (typeof sessionID !== "string") return Promise.resolve();
  const id = typeof props.id === "string" ? { id: props.id } : {};
  switch (event.type) {
    case "permission.asked":
    case "permission.updated": {
      const what = typeof props.permission === "string" ? props.permission : props.type;
      const patterns = [props.patterns ?? props.pattern].flat().filter((item) => typeof item === "string");
      const title = typeof props.title === "string" ? props.title : [what, ...patterns].filter(Boolean).join(": ");
      return lifecycle.ask(sessionID, { kind: "permission", ...id, ...(title ? { question: title } : {}) });
    }
    case "question.asked": {
      const first = record([props.questions].flat()[0]);
      const question = typeof first?.question === "string" ? first.question : undefined;
      const options = Array.isArray(first?.options) ? first.options : [];
      const suggestions = options.map((option) => record(option)?.label).filter((label) => typeof label === "string");
      return lifecycle.ask(sessionID, {
        kind: "question",
        ...id,
        ...(question === undefined ? {} : { question }),
        ...(suggestions.length === 0 ? {} : { suggestions }),
      });
    }
    default:
      return applyServerEvent(lifecycle, { type: event.type, data: props });
  }
}

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === "object" && !Array.isArray(value) ? value as Record<string, unknown> : undefined;
}
