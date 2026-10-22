import { expect, test } from "bun:test";

import type { MailDelivery } from "../../runtime/src/mail.js";
import { openCodeMail, type SyntheticInput } from "../src/mail.js";

function parkedAfter(script: readonly MailDelivery[]) {
  let calls = 0;
  return (signal: AbortSignal): Promise<MailDelivery | null> => {
    const next = script[calls];
    calls += 1;
    if (next !== undefined) return Promise.resolve(next);
    return new Promise((_, reject) => signal.addEventListener("abort", () => reject(new Error("aborted"))));
  };
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 20; turn += 1) await Promise.resolve();
}

test("mail is queued into the shown session as synthetic input that resumes it", async () => {
  const admitted: unknown[] = [];
  const sessions: SyntheticInput = { synthetic: async (input) => { admitted.push(input); } };
  const inbox = openCodeMail({
    sessions,
    visibleSession: () => "ses_1",
    env: {},
    wait: parkedAfter([{ text: "[phux mail] A message", ids: ["a"] }]),
    release: async () => { throw new Error("nothing to release"); },
  });
  inbox.start();
  await settle();
  expect(admitted).toEqual([{
    sessionID: "ses_1",
    text: "[phux mail] A message",
    description: "phux mail",
    metadata: { phux_mail_ids: ["a"] },
    delivery: "queue",
    resume: true,
  }]);
  inbox.stop();
});

test("nothing is taken while no session is shown", async () => {
  let waits = 0;
  const inbox = openCodeMail({
    sessions: { synthetic: async () => {} },
    visibleSession: () => undefined,
    env: {},
    wait: async () => { waits += 1; return null; },
    release: async () => {},
    onError: () => {},
  });
  inbox.start();
  const deadline = Date.now() + 5000;
  while (inbox.running && Date.now() < deadline) await new Promise((resolve) => setTimeout(resolve, 5));
  expect(inbox.running).toBe(false);
  expect(waits).toBe(0);
});

test("mail the service refuses is released, not dropped", async () => {
  const released: string[][] = [];
  const inbox = openCodeMail({
    sessions: { synthetic: async () => { throw new Error("session gone"); } },
    visibleSession: () => "ses_1",
    env: {},
    wait: parkedAfter([{ text: "x", ids: ["a"] }]),
    release: async (delivery) => { released.push([...delivery.ids]); },
    onError: () => {},
  });
  inbox.start();
  await settle();
  expect(released).toEqual([["a"]]);
  inbox.stop();
});
