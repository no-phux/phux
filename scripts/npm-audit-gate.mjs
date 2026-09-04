#!/usr/bin/env node
// npm audit gate with transient-outage tolerance.
//
// The integration packages run `npm audit --audit-level=high` as a gate, and
// the advisory endpoints have returned 503s mid-lane (observed 2026-09-04,
// three times in an hour) — failing gates that had nothing to do with the
// change under test. The audit itself stays ON: this wrapper only retries
// when the ENDPOINT is unavailable, and propagates a real advisory finding
// (or a persistent outage) as a failure immediately.
//
// Usage (from an integration package):
//   node ../../scripts/npm-audit-gate.mjs --audit-level=high

import { spawn } from "node:child_process";

const args = process.argv.slice(2);
if (args.length === 0) {
  console.error("usage: npm-audit-gate.mjs <npm audit args...>");
  process.exit(2);
}

const maxAttempts = 5;
const backoffSeconds = [5, 10, 20, 40];
// npm prints this line (plus variants) when the registry audit API itself
// fails; anything else is a real verdict and must fail the gate loudly.
const transient =
  /audit endpoint returned an error|ECONNRESET|ETIMEDOUT|EAI_AGAIN|ENOTFOUND|socket hang up|fetch failed|HTTP 50[234]/i;

const sleep = (seconds) =>
  new Promise((resolve) => setTimeout(resolve, seconds * 1000));

for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
  const result = await new Promise((resolve) => {
    const child = spawn("npm", ["audit", ...args], {
      stdio: ["ignore", "inherit", "pipe"],
    });
    let stderr = "";
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("close", (code) => resolve({ code, stderr }));
  });

  if (result.code === 0) {
    process.exit(0);
  }

  if (!transient.test(result.stderr)) {
    // A real advisory finding (or an unexpected npm failure): surface it.
    process.stderr.write(result.stderr);
    process.exit(result.code ?? 1);
  }

  if (attempt === maxAttempts) {
    process.stderr.write(result.stderr);
    console.error(
      `npm-audit-gate: registry audit endpoint still unavailable after ${maxAttempts} attempts; failing.`,
    );
    process.exit(result.code ?? 1);
  }

  const wait = backoffSeconds[attempt - 1] ?? 40;
  console.error(
    `npm-audit-gate: audit endpoint unavailable (attempt ${attempt}/${maxAttempts}); retrying in ${wait}s...`,
  );
  await sleep(wait);
}
