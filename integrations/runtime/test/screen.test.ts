import assert from "node:assert/strict";
import test from "node:test";

import { PhuxError } from "../src/errors.js";
import type { ProcessResult, ProcessRunner, RunRequest } from "../src/runner.js";
import { PhuxScreen } from "../src/screen.js";

const completed = (stdout: string, exitCode = 0, stderr = ""): ProcessResult => ({
  termination: "completed",
  exitCode,
  stdout,
  stderr,
});

function fakeRunner(result: ProcessResult): { runner: ProcessRunner; requests: RunRequest[] } {
  const requests: RunRequest[] = [];
  return {
    requests,
    runner: async (request) => {
      requests.push(request);
      return result;
    },
  };
}

const code = (expected: PhuxError["code"]) => (error: unknown) => error instanceof PhuxError && error.code === expected;

test("rendered asks the server Formatter for html and keeps the viewport geometry", async () => {
  const fake = fakeRunner(completed(JSON.stringify({
    schema_version: 3,
    pane: 4,
    cols: 80,
    rows: 2,
    cursor: { x: 3, y: 1, visible: true },
    lines: [],
    scrollback: [],
    title: "build",
    rendered: { format: "html", data: "<div>ok</div>" },
  })));
  const screen = new PhuxScreen({ runner: fake.runner, socket: "/tmp/p.sock" });

  const rendered = await screen.rendered("@4", { history: 200 });

  assert.deepEqual(fake.requests[0]?.args, ["snapshot", "--json", "--format", "html", "--tail", "200", "--socket", "/tmp/p.sock", "@4"]);
  assert.deepEqual(rendered, { pane: 4, cols: 80, rows: 2, cursor: { x: 3, y: 1, visible: true }, title: "build", html: "<div>ok</div>" });
});

test("rendered rejects a document without the html rendering, and keeps the CLI's error", async () => {
  const plain = JSON.stringify({ schema_version: 3, pane: 1, cols: 80, rows: 1, cursor: null, lines: ["ready"], scrollback: [] });
  await assert.rejects(new PhuxScreen({ runner: fakeRunner(completed(plain)).runner }).rendered("@1"), code("invalid_response"));

  const stderr = JSON.stringify({ error: { code: "no_such_target", message: "no such target: @9" } });
  await assert.rejects(new PhuxScreen({ runner: fakeRunner(completed("", 1, stderr)).runner }).rendered("@9"), (error: unknown) =>
    error instanceof PhuxError && error.code === "command_failed" && (error.cliError?.["error"] as { code: string }).code === "no_such_target");
});

test("watchUntil resumes after a cursor and reports the next cursor for both outcomes", async () => {
  const cursorLine = JSON.stringify({ cursor: "abc:9", cursor_void: false });
  const matched = fakeRunner(completed('{"event":"cwd_changed","seq":8}\n{"event":"dirty","seq":9,"terminal":"@2"}\n', 0, `${cursorLine}\n`));
  const screen = new PhuxScreen({ runner: matched.runner });

  const outcome = await screen.watchUntil("@2", { until: ["dirty", "pane_closed"], after: "abc:3", timeoutSeconds: 20 });

  assert.deepEqual(matched.requests[0]?.args, [
    "watch", "--json", "--until", "dirty", "--until", "pane_closed", "--timeout", "20", "--after", "abc:3", "@2",
  ]);
  assert.equal(matched.requests[0]?.timeoutMs, 25_000);
  assert.deepEqual(outcome, { outcome: "matched", event: "dirty", cursor: "abc:9" });

  const quiet = new PhuxScreen({ runner: fakeRunner(completed("", 124, `phux: error: watch timed out\n${cursorLine}\n`)).runner });
  assert.deepEqual(await quiet.watchUntil("@2", { until: ["dirty"], timeoutSeconds: 1 }), { outcome: "timed_out", cursor: "abc:9" });
});

test("watchUntil keeps other exits as failures and validates its bounds", async () => {
  const failing = new PhuxScreen({ runner: fakeRunner(completed("", 1, '{"error":{"code":"no_such_target"}}')).runner });
  await assert.rejects(failing.watchUntil("@99", { until: ["dirty"], timeoutSeconds: 5 }), code("command_failed"));
  const idle = new PhuxScreen({ runner: fakeRunner(completed("")).runner });
  await assert.rejects(idle.watchUntil("@1", { until: [], timeoutSeconds: 5 }), TypeError);
  await assert.rejects(idle.watchUntil("@1", { until: ["dirty"], timeoutSeconds: 0 }), RangeError);
});

test("local terminations and a missing executable become typed errors", async () => {
  const aborted = new PhuxScreen({ runner: async () => ({ termination: "aborted", exitCode: null, stdout: "", stderr: "" }) });
  await assert.rejects(aborted.rendered("@1"), code("aborted"));
  const missing = new PhuxScreen({
    runner: async () => {
      throw Object.assign(new Error("spawn phux ENOENT"), { code: "ENOENT" });
    },
  });
  await assert.rejects(missing.watchUntil("@1", { until: ["dirty"], timeoutSeconds: 5 }), code("unavailable"));
});
