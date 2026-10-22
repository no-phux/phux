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

test("mail taken while no session is shown is reported, not dropped silently", async () => {
  const errors: unknown[] = [];
  const inbox = openCodeMail({
    sessions: { synthetic: async () => { throw new Error("unreachable"); } },
    visibleSession: () => undefined,
    env: {},
    wait: parkedAfter([{ text: "x", ids: ["a"] }]),
    onError: (error) => errors.push(error),
  });
  inbox.start();
  await settle();
  expect(String(errors[0])).toContain("stays unread");
  inbox.stop();
});
