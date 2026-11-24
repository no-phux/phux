import { mock } from "bun:test";
import * as solid from "solid-js";

// Isolate module mocks from the rest of the package tests. Render each route
// effect synchronously; the real host runs it in a mounted Solid slot.
mock.module("solid-js", () => ({ ...solid, createEffect: (run: () => void) => run() }));
const lifecycle: string[] = [];
mock.module("../../src/lifecycle.js", () => ({
  OpenCodeLifecycle: class {
    constructor() { lifecycle.push("construct"); }
    async observeState(id: string, state: string) { lifecycle.push(`show ${id} ${state}`); }
    async ask(id: string) { lifecycle.push(`ask ${id}`); }
    async deleteSession(id: string) { lifecycle.push(`clear ${id}`); }
    async dispose() { lifecycle.push("dispose"); }
  },
}));

const { default: plugin } = await import("../../src/tui.js");
const slots: { append: string; render: () => unknown }[] = [];
const handlers = new Map<string, (event: unknown) => void>();
const admitted: unknown[] = [];
const errors: unknown[] = [];
console.error = (...args) => { errors.push(args.map(String)); };
let route: { type: string; sessionID?: string } = { type: "session", sessionID: "visible" };
const cleanup = await plugin.setup({
  client: { session: { synthetic: async (input: unknown) => {
    admitted.push(input);
    if (process.env.TEST_REFUSE === "1") throw new Error("service refused synthetic input");
  } } },
  ui: {
    slot: (slot: typeof slots[number]) => { slots.push(slot); return () => {}; },
    router: { current: () => route },
  },
  data: {
    session: { status: () => "idle" },
    on: (name: string, handler: (event: unknown) => void) => {
      handlers.set(name, handler);
      return () => { handlers.delete(name); };
    },
  },
} as any);
const app = slots.find((slot) => slot.append === "app");
app?.render();
if (app !== undefined) {
  const deadline = Date.now() + 5000;
  while ((admitted.length === 0 || (process.env.TEST_REFUSE === "1" && errors.length === 0))
    && Date.now() < deadline) {
    await new Promise((resolve) => setTimeout(resolve, 5));
  }
}
handlers.get("session.status")?.({ data: { sessionID: "visible", status: { type: "busy" } } });
handlers.get("permission.asked")?.({ data: { sessionID: "visible" } });
handlers.get("session.idle")?.({ data: { sessionID: "visible" } });
route = { type: "home" };
app?.render();
const subscriptions = [...handlers.keys()];
await cleanup?.();
console.log(JSON.stringify({
  slots: slots.map((slot) => slot.append), lifecycle, subscriptions,
  remainingSubscriptions: handlers.size, admitted, errors,
}));
