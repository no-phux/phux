import assert from "node:assert/strict";
import test from "node:test";

import { PhuxError } from "../src/errors.js";
import { MailInbox, parseMailWait, waitForMail, type MailDelivery } from "../src/mail.js";
import type { ProcessRunner, RunRequest } from "../src/runner.js";

const TAKEN = JSON.stringify({
  schema: "phux.coord/v1",
  reader: "@730",
  mail: [{ id: "0123456789abcdef0123456789abcdef", thread: "pr-1", from: "pi@749", body: "rebased" }],
  text: "[phux mail] A message from other agents.",
});

test("a wait runs phux mail wait with argv and a deadline past the keeper's", async () => {
  const requests: RunRequest[] = [];
  const runner: ProcessRunner = async (request) => {
    requests.push(request);
    return { termination: "completed", exitCode: 0, stdout: TAKEN, stderr: "" };
  };
  const delivery = await waitForMail({ runner, timeoutSeconds: 60 }, new AbortController().signal);
  assert.deepEqual(delivery, {
    text: "[phux mail] A message from other agents.",
    ids: ["0123456789abcdef0123456789abcdef"],
  });
  assert.deepEqual(requests[0]?.args, ["mail", "wait", "--json", "--timeout", "60"]);
  assert.equal(requests[0]?.timeoutMs, 75_000);
});

test("a wait that takes nothing is null and a malformed document is an error", () => {
  assert.equal(parseMailWait(JSON.stringify({ schema: "phux.coord/v1", reader: "@1", mail: [], text: null })), null);
  assert.throws(() => parseMailWait("not json"), (error: unknown) =>
    error instanceof PhuxError && error.code === "malformed_json");
  assert.throws(() => parseMailWait(JSON.stringify({ schema: "other", mail: [] })), (error: unknown) =>
    error instanceof PhuxError && error.code === "invalid_response");
  assert.throws(() => parseMailWait(JSON.stringify({ schema: "phux.coord/v1", mail: [{ id: "a" }], text: null })),
    (error: unknown) => error instanceof PhuxError && error.code === "invalid_response");
});

test("a failed wait carries the CLI's stderr", async () => {
  const runner: ProcessRunner = async () =>
    ({ termination: "completed", exitCode: 1, stdout: "", stderr: "keeper unavailable" });
  await assert.rejects(waitForMail({ runner }, new AbortController().signal), (error: unknown) =>
    error instanceof PhuxError && error.code === "command_failed" && error.message.includes("keeper unavailable"));
});

/** A wait that answers from `script` in order, then parks until aborted. */
function scripted(script: readonly (MailDelivery | null | Error)[]) {
  let calls = 0;
  const wait = (signal: AbortSignal): Promise<MailDelivery | null> => {
    const next = script[calls];
    calls += 1;
    if (next === undefined) {
      return new Promise((_, reject) => signal.addEventListener("abort", () => reject(new Error("aborted"))));
    }
    return next instanceof Error ? Promise.reject(next) : Promise.resolve(next);
  };
  return { wait, calls: () => calls };
}

async function settle(): Promise<void> {
  for (let turn = 0; turn < 20; turn += 1) await Promise.resolve();
}

test("the inbox delivers each taken batch and keeps waiting until stopped", async () => {
  const first = { text: "one", ids: ["a"] };
  const second = { text: "two", ids: ["b"] };
  const source = scripted([first, null, second]);
  const delivered: MailDelivery[] = [];
  const inbox = new MailInbox({ wait: source.wait, deliver: (delivery) => { delivered.push(delivery); } });
  inbox.start();
  inbox.start();
  await settle();
  assert.deepEqual(delivered, [first, second]);
  assert.equal(source.calls(), 4, "one loop, parked on its fourth wait");
  assert.equal(inbox.running, true);
  inbox.stop();
  await settle();
  assert.equal(inbox.running, false);
});

test("one failure is retried at once and a second stops the loop until it is started again", async () => {
  const errors: unknown[] = [];
  const source = scripted([new Error("keeper replaced"), { text: "x", ids: ["a"] }, new Error("one"), new Error("two")]);
  const delivered: MailDelivery[] = [];
  const inbox = new MailInbox({
    wait: source.wait,
    deliver: (delivery) => { delivered.push(delivery); },
    onError: (error) => errors.push(error),
  });
  inbox.start();
  await settle();
  assert.equal(delivered.length, 1, "the retry after one failure delivered");
  assert.equal(errors.length, 3);
  assert.equal(inbox.running, false, "two failures in a row stop it");
  inbox.start();
  await settle();
  assert.equal(inbox.running, true, "a lifecycle event re-arms it");
  inbox.stop();
});
