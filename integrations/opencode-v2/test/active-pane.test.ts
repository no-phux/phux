import { expect, test } from "bun:test";
import { ActivePane, type PaneLifecycle } from "../src/active-pane.js";

test("a pane belongs only to the visible session, including idle sessions", async () => {
  const calls: string[] = [];
  const lifecycle: PaneLifecycle = {
    observeState: async (id, state) => { calls.push(`show ${id} ${state}`); },
    ask: async (id) => { calls.push(`ask ${id}`); },
    deleteSession: async (id) => { calls.push(`clear ${id}`); },
    dispose: async () => { calls.push("dispose"); },
  };
  const pane = new ActivePane(lifecycle);
  pane.show("first");
  pane.status("second", "working");
  pane.ask("second");
  pane.status("first", "working");
  pane.show("second", "idle");
  pane.ask("first");
  pane.ask("second");
  pane.show(undefined);
  await pane.dispose();
  expect(calls).toEqual([
    "show first idle", "show first working", "clear first", "show second idle",
    "ask second", "clear second", "dispose",
  ]);
});
