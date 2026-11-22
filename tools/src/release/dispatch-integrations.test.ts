// The dispatcher waits for the run it just created, not an older one.
import { expect, test } from "bun:test";
import { Failure } from "../lib/command.ts";
import { dispatch } from "./dispatch-integrations.ts";

const TAG = "pi-extension-v0.3.0";

test("returns the new workflow_dispatch run", async () => {
  const ran: string[][] = [];
  const listings = [[1], [1], [1, 9]];
  const runId = await dispatch("no-phux/phux", TAG, {
    dispatchRuns: () => listings.shift() ?? [],
    run: (args) => void ran.push(args),
    sleep: async () => {},
  });
  expect(runId).toBe(9);
  expect(ran).toHaveLength(1);
  expect(ran[0]).toContain("agent-integration-release.yml");
  expect(ran[0]).toContain(`tag=${TAG}`);
  expect(ran[0]).toContain("dry_run=false");
});

test("gives up when no new run appears before the deadline", async () => {
  const deps = { dispatchRuns: () => [1], run: () => {}, sleep: async () => {} };
  const error = await dispatch("no-phux/phux", TAG, deps).catch((caught: unknown) => caught);
  expect(error).toBeInstanceOf(Failure);
  expect(String(error)).toContain(`no agent-integration-release.yml run appeared for ${TAG}`);
});
