import { describe, expect, test } from "bun:test";
import type { ExtensionAPI } from "@oh-my-pi/pi-coding-agent";

import type { MailDelivery } from "../../runtime/src/mail.js";
import { PHUX_MAIL_CUSTOM_TYPE, registerOmpMail } from "../src/mail.js";

function fakeOmp() {
  const handlers = new Map<string, Array<() => void>>();
  const sent: Array<{ message: unknown; options: unknown }> = [];
  const api = {
    on: (name: string, handler: () => void) => handlers.set(name, [...(handlers.get(name) ?? []), handler]),
    sendMessage: (message: unknown, options: unknown) => { sent.push({ message, options }); },
  } as unknown as ExtensionAPI;
  const fire = (name: string) => { for (const handler of handlers.get(name) ?? []) handler(); };
  return { api, sent, fire };
}

function parkedAfter(script: readonly MailDelivery[]) {
  let calls = 0;
  return {
    calls: () => calls,
    wait: (signal: AbortSignal): Promise<MailDelivery | null> => {
      const next = script[calls];
      calls += 1;
      if (next !== undefined) return Promise.resolve(next);
      return new Promise((_, reject) => signal.addEventListener("abort", () => reject(new Error("aborted"))));
    },
  };
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 20; turn += 1) await Promise.resolve();
}

describe("OMP mail", () => {
  test("taken mail is a follow-up message that starts a turn when idle", async () => {
    const omp = fakeOmp();
    const source = parkedAfter([{ text: "[phux mail] A message", ids: ["a"] }]);
    const inbox = registerOmpMail(omp.api, {
      env: { PHUX_TERMINAL_ID: "17" },
      executable: "phux",
      wait: source.wait,
      release: async () => { throw new Error("nothing to release"); },
    });
    omp.fire("session_start");
    await settle();
    expect(omp.sent).toEqual([{
      message: { customType: PHUX_MAIL_CUSTOM_TYPE, content: "[phux mail] A message", display: true, details: { ids: ["a"] } },
      options: { triggerTurn: true, deliverAs: "followUp" },
    }]);
    omp.fire("session_shutdown");
    await settle();
    expect(inbox?.running).toBe(false);
  });

  test("outside a pane nothing waits", async () => {
    const omp = fakeOmp();
    const source = parkedAfter([]);
    expect(registerOmpMail(omp.api, { env: {}, executable: "phux", wait: source.wait })).toBeNull();
    omp.fire("session_start");
    await settle();
    expect(source.calls()).toBe(0);
  });
});
