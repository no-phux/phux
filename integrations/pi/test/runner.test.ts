import assert from "node:assert/strict";
import { existsSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { nodeProcessRunner } from "../src/runner.js";

test("node runner passes metacharacters as literal argv without a shell", async () => {
  const literal = "$(printf injected); *; echo nope";
  const result = await nodeProcessRunner({
    executable: process.execPath,
    args: ["-e", "process.stdout.write(process.argv[1])", literal],
  });

  assert.equal(result.termination, "completed");
  assert.equal(result.exitCode, 0);
  assert.equal(result.stdout, literal);
});

test("node runner propagates AbortSignal", async () => {
  const controller = new AbortController();
  const pending = nodeProcessRunner({
    executable: process.execPath,
    args: ["-e", "setInterval(() => {}, 1000)"],
    signal: controller.signal,
  });
  setTimeout(() => controller.abort(), 20);
  const result = await pending;
  assert.equal(result.termination, "aborted");
});

test("node runner enforces timeout", async () => {
  const result = await nodeProcessRunner({
    executable: process.execPath,
    args: ["-e", "setInterval(() => {}, 1000)"],
    timeoutMs: 20,
  });
  assert.equal(result.termination, "timed_out");
});

test("timeout terminates descendants in the spawned POSIX process group", async () => {
  const dir = mkdtempSync(join(tmpdir(), "phux-pi-runner-"));
  const heartbeat = join(dir, "heartbeat");
  // Write once on start, then on an interval. The immediate write is what
  // makes "the descendant is alive" observable without waiting out a tick.
  const descendant = [
    "const fs=require('node:fs')",
    `const beat=()=>fs.appendFileSync(${JSON.stringify(heartbeat)},'x')`,
    "beat()",
    "setInterval(beat,10)",
  ].join(";");
  const parent = [
    "const {spawn}=require('node:child_process')",
    `spawn(process.execPath,['-e',${JSON.stringify(descendant)}],{stdio:'ignore'})`,
    "setInterval(()=>{},1000)",
  ].join(";");

  try {
    // Start the run, then WAIT for the descendant to prove it is alive before
    // letting the timeout fire. The previous shape assumed both nested node
    // cold starts finished inside a 150ms timeout; on a loaded machine they
    // do not, the group was killed before the descendant ever wrote, and the
    // test failed as an ENOENT on the heartbeat file — naming a missing file
    // rather than the startup race that caused it. The timeout still does the
    // killing; it just no longer doubles as a startup budget. 1.5s is ~6x the
    // observed two-cold-start cost, and if a machine is ever slower still the
    // guard below fails by naming the startup race instead of an ENOENT.
    const pending = nodeProcessRunner({
      executable: process.execPath,
      args: ["-e", parent],
      timeoutMs: 1_500,
    });
    const started = await waitFor(() => existsSync(heartbeat), 1_400);
    assert.ok(started, "descendant never started, so the kill proves nothing");

    const result = await pending;
    assert.equal(result.termination, "timed_out");
    await delay(80);
    const first = readFileSync(heartbeat).length;
    await delay(80);
    assert.equal(readFileSync(heartbeat).length, first, "descendant heartbeat must stop");
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});

test("node runner bounds oversized stdout", async () => {
  const result = await nodeProcessRunner({
    executable: process.execPath,
    args: ["-e", "process.stdout.write(Buffer.alloc(4096, 97));setInterval(()=>{},1000)"],
    maxStdoutBytes: 100,
  });
  assert.equal(result.termination, "output_limit");
  if (result.termination !== "output_limit") return;
  assert.equal(result.outputLimit, "stdout");
  assert.equal(Buffer.byteLength(result.stdout), 100);
});

test("node runner bounds oversized stderr", async () => {
  const result = await nodeProcessRunner({
    executable: process.execPath,
    args: ["-e", "process.stderr.write(Buffer.alloc(4096, 98));setInterval(()=>{},1000)"],
    maxStderrBytes: 100,
  });
  assert.equal(result.termination, "output_limit");
  if (result.termination !== "output_limit") return;
  assert.equal(result.outputLimit, "stderr");
  assert.equal(Buffer.byteLength(result.stderr), 100);
});

/// Poll `condition` until it holds or `timeoutMs` elapses. Returns whether it
/// held, so the caller can assert with a message that names what was being
/// waited for instead of failing on the symptom.
async function waitFor(condition: () => boolean, timeoutMs: number): Promise<boolean> {
  const deadline = Date.now() + timeoutMs;
  while (Date.now() < deadline) {
    if (condition()) return true;
    await delay(10);
  }
  return condition();
}

function delay(ms: number): Promise<void> {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
