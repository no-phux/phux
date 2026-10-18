// The dispatcher waits for the run it just created, not an older one.
import { expect, test } from "bun:test";
import { dispatch, type WorkflowRun } from "./dispatch-integrations.ts";

test("returns the new workflow_dispatch run", () => {
  let calls = 0;
  const ran: string[][] = [];
  const listRuns = (): WorkflowRun[] => {
    calls += 1;
    const old = { databaseId: 1, event: "workflow_dispatch" };
    return calls === 1 ? [old] : [old, { databaseId: 9, event: "workflow_dispatch" }];
  };
  const runId = dispatch("no-phux/phux", "pi-extension-v0.3.0", {
    listRuns,
    run: (args) => void ran.push(args),
    sleep: () => {},
    clock: () => 0,
  });
  expect(runId).toBe(9);
  expect(ran).toHaveLength(1);
  expect(ran[0]).toContain("agent-integration-release.yml");
  expect(ran[0]).toContain("tag=pi-extension-v0.3.0");
  expect(ran[0]).toContain("dry_run=false");
});

test("gives up when no new run appears before the deadline", () => {
  let now = 0;
  expect(() =>
    dispatch("no-phux/phux", "pi-extension-v0.3.0", {
      listRuns: () => [{ databaseId: 1, event: "workflow_dispatch" }],
      run: () => {},
      sleep: (seconds) => void (now += seconds),
      clock: () => now,
    }),
  ).toThrow("no agent-integration-release.yml run appeared for pi-extension-v0.3.0");
});
