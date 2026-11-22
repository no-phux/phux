import { describe, expect, test } from "bun:test";

import { applyServerEvent, applyV1Event, deletedSessionId } from "../src/events.js";
import type { OpenCodeLifecycle } from "../src/lifecycle.js";

describe("V2 server events", () => {
  test("busy and idle become prompt and stop", async () => {
    const lifecycle = fakeLifecycle();
    await applyServerEvent(lifecycle, { type: "session.status", data: { sessionID: "ses", status: { type: "busy" } } });
    await applyServerEvent(lifecycle, { type: "session.idle", data: { sessionID: "ses" } });
    expect(lifecycle.calls).toEqual([
      ["observeState", "ses", "working"],
      ["observeState", "ses", "idle"],
    ]);
  });

  test("permission and delete use the session id on the event", async () => {
    const lifecycle = fakeLifecycle();
    await applyServerEvent(lifecycle, {
      type: "permission.asked",
      data: { sessionID: "ses", id: "perm", action: "bash", message: "run tests" },
    });
    await applyServerEvent(lifecycle, { type: "session.deleted", data: { sessionID: "ses" } });
    expect(lifecycle.calls[0]).toEqual(["ask", "ses", { kind: "permission", id: "perm", question: "run tests" }]);
    expect(lifecycle.calls[1]).toEqual(["deleteSession", "ses"]);
    expect(deletedSessionId({ type: "session.deleted", data: { sessionID: "ses" } })).toBe("ses");
  });

  test("ignores unrelated events", async () => {
    const lifecycle = fakeLifecycle();
    await applyServerEvent(lifecycle, { type: "session.created", data: { sessionID: "ses" } });
    await applyServerEvent(lifecycle, { type: "session.deleted", data: {} });
    expect(lifecycle.calls).toEqual([]);
    expect(deletedSessionId({ type: "session.status", data: { sessionID: "ses" } })).toBeUndefined();
  });
});

describe("OpenCode 1.x plugin events", () => {
  test("status, asks and delete read the 1.19 event properties", async () => {
    const lifecycle = fakeLifecycle();
    await applyV1Event(lifecycle, { type: "session.status", properties: { sessionID: "ses", status: { type: "busy" } } });
    await applyV1Event(lifecycle, {
      type: "permission.asked",
      properties: { id: "per_1", sessionID: "ses", permission: "bash", patterns: ["ls -la"], metadata: {} },
    });
    await applyV1Event(lifecycle, {
      type: "question.asked",
      properties: { id: "que_1", sessionID: "ses", questions: [{ question: "Which?", options: [{ label: "a" }, { label: "b" }] }] },
    });
    await applyV1Event(lifecycle, { type: "session.idle", properties: { sessionID: "ses" } });
    await applyV1Event(lifecycle, { type: "session.deleted", properties: { info: { id: "ses" } } });
    expect(lifecycle.calls).toEqual([
      ["observeState", "ses", "working"],
      ["ask", "ses", { kind: "permission", id: "per_1", question: "bash: ls -la" }],
      ["ask", "ses", { kind: "question", id: "que_1", question: "Which?", suggestions: ["a", "b"] }],
      ["observeState", "ses", "idle"],
      ["deleteSession", "ses"],
    ]);
  });

  test("ignores events without a session", async () => {
    const lifecycle = fakeLifecycle();
    await applyV1Event(lifecycle, { type: "session.idle", properties: {} });
    await applyV1Event(lifecycle, { type: "permission.asked" });
    expect(lifecycle.calls).toEqual([]);
  });
});

function fakeLifecycle() {
  const calls: unknown[][] = [];
  const lifecycle = {
    calls,
    observeState: (sessionId: string, state: string) => {
      calls.push(["observeState", sessionId, state]);
      return Promise.resolve();
    },
    ask: (sessionId: string, data: Readonly<Record<string, unknown>>) => {
      calls.push(["ask", sessionId, data]);
      return Promise.resolve();
    },
    deleteSession: (sessionId: string) => {
      calls.push(["deleteSession", sessionId]);
      return Promise.resolve();
    },
  };
  return lifecycle as OpenCodeLifecycle & { calls: unknown[][] };
}
