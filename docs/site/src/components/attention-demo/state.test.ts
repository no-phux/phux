import { describe, expect, test } from "bun:test";
import {
  ATTENTION_DEMO_SEQUENCE,
  attentionDemoDuration,
  nextAttentionDemoPhase,
} from "./state";

describe("attention demo sequence", () => {
  test("shows the blocked handoff long enough to understand before resolving", () => {
    expect(ATTENTION_DEMO_SEQUENCE.map((step) => step.phase)).toEqual([
      "working",
      "blocked",
      "resolved",
    ]);
    expect(attentionDemoDuration("blocked")).toBe(7_000);
    expect(attentionDemoDuration("blocked")).toBeGreaterThan(
      attentionDemoDuration("working"),
    );
  });

  test("loops back to active work after the resolution", () => {
    expect(nextAttentionDemoPhase("working")).toBe("blocked");
    expect(nextAttentionDemoPhase("blocked")).toBe("resolved");
    expect(nextAttentionDemoPhase("resolved")).toBe("working");
  });
});
