import assert from "node:assert/strict";
import test from "node:test";

import type { ExtensionAPI } from "@earendil-works/pi-coding-agent";
import type { MailDelivery } from "@phux/integration-runtime/mail";

import { PHUX_MAIL_CUSTOM_TYPE, registerPhuxMail } from "../src/mail.js";

type Handler = () => void;

function fakePi() {
  const handlers = new Map<string, Handler[]>();
  const sent: Array<{ message: unknown; options: unknown }> = [];
  const api = {
    on: (name: string, handler: Handler) => handlers.set(name, [...(handlers.get(name) ?? []), handler]),
    sendMessage: (message: unknown, options: unknown) => { sent.push({ message, options }); },
  } as unknown as ExtensionAPI;
  const fire = (name: string) => { for (const handler of handlers.get(name) ?? []) handler(); };
  return { api, sent, fire };
}

/** Answers `script` in order, then parks until aborted. */
function scripted(script: readonly (MailDelivery | null)[]) {
  let calls = 0;
  const wait = (signal: AbortSignal): Promise<MailDelivery | null> => {
    const next = script[calls];
    calls += 1;
    if (next !== undefined) return Promise.resolve(next);
    return new Promise((_, reject) => signal.addEventListener("abort", () => reject(new Error("aborted"))));
  };
  return { wait, calls: () => calls };
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 20; turn += 1) await Promise.resolve();
}

test("mail reaches the conversation as a follow-up that starts a turn when idle", async () => {
  const pi = fakePi();
  const source = scripted([{ text: "[phux mail] A message", ids: ["a"] }]);
  const inbox = registerPhuxMail(pi.api, { env: { PHUX_TERMINAL_ID: "749" }, wait: source.wait });
  assert.notEqual(inbox, null);
  pi.fire("session_start");
  await settle();
  assert.deepEqual(pi.sent, [{
    message: { customType: PHUX_MAIL_CUSTOM_TYPE, content: "[phux mail] A message", display: true, details: { ids: ["a"] } },
    options: { triggerTurn: true, deliverAs: "followUp" },
  }]);
  pi.fire("session_shutdown");
  await settle();
  assert.equal(inbox?.running, false);
});

test("outside a pane nothing waits for mail", async () => {
  const pi = fakePi();
  const source = scripted([]);
  assert.equal(registerPhuxMail(pi.api, { env: {}, wait: source.wait }), null);
  pi.fire("session_start");
  await settle();
  assert.equal(source.calls(), 0);
});
