import { describe, expect, test } from "bun:test";

import { deletedSessionId, toLifecycleEvent } from "../src/events.js";

describe("V2 event mapping", () => {
  test("passes session status through", () => {
    expect(toLifecycleEvent({ type: "session.status", data: { sessionID: "ses", status: { type: "busy" } } })).toEqual({
      type: "session.status",
      properties: { sessionID: "ses", status: { type: "busy" } },
    });
  });

  test("maps a deleted session onto the V1 info.id shape", () => {
    expect(toLifecycleEvent({ type: "session.deleted", data: { sessionID: "ses" } })).toEqual({
      type: "session.deleted",
      properties: { sessionID: "ses", info: { id: "ses" } },
    });
    expect(deletedSessionId({ type: "session.deleted", data: { sessionID: "ses" } })).toBe("ses");
  });

  test("copies permission message and action into the fields the lifecycle handler reads", () => {
    expect(toLifecycleEvent({
      type: "permission.asked",
      data: { sessionID: "ses", id: "perm", action: "bash", message: "run tests" },
    })).toEqual({
      type: "permission.asked",
      properties: {
        sessionID: "ses",
        id: "perm",
        action: "bash",
        message: "run tests",
        title: "run tests",
        permission: "bash",
      },
    });
  });

  test("ignores unrelated events and a deleted event with no session id", () => {
    expect(toLifecycleEvent({ type: "session.created", data: { sessionID: "ses" } })).toBeUndefined();
    expect(toLifecycleEvent({ type: "session.deleted", data: {} })).toBeUndefined();
    expect(deletedSessionId({ type: "session.status", data: { sessionID: "ses" } })).toBeUndefined();
  });
});
