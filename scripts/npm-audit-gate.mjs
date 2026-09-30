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
// A finding is waived only when every vulnerable node matches an unexpired
// entry in npm-audit-allow.json: the same advisory, installed at a path
// inside the named package. That is for fixes that cannot reach us, such as
// a dev dependency whose own npm-shrinkwrap pins the vulnerable copy; an
// expired or non-matching entry fails the gate like any other finding.
//
// Usage (from an integration package):
//   node ../../scripts/npm-audit-gate.mjs --audit-level=high

import { spawn } from "node:child_process";
import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { fileURLToPath } from "node:url";

const severities = ["info", "low", "moderate", "high", "critical"];

// npm prints this line (plus variants) when the registry audit API itself
// fails; anything else is a real verdict and must fail the gate loudly.
const transient =
  /audit endpoint returned an error|ECONNRESET|ETIMEDOUT|EAI_AGAIN|ENOTFOUND|socket hang up|fetch failed|HTTP 50[234]/i;
const maxAttempts = 5;
const backoffSeconds = [5, 10, 20, 40];

const sleep = (seconds) =>
  new Promise((resolve) => setTimeout(resolve, seconds * 1000));

/** Run a command, capturing stderr and (when `stdout` is "pipe") stdout. */
function run(command, commandArgs, stdout) {
  return new Promise((resolve) => {
    const child = spawn(command, commandArgs, {
      stdio: ["ignore", stdout, "pipe"],
    });
    let out = "";
    let stderr = "";
    child.stdout?.on("data", (chunk) => {
      out += chunk;
    });
    child.stderr.on("data", (chunk) => {
      stderr += chunk;
    });
    child.on("close", (code) => resolve({ code, out, stderr }));
  });
}

/** Advisory ids ("GHSA-...") a vulnerability entry reports directly. */
function advisoryIds(vulnerability) {
  return vulnerability.via
    .filter((via) => typeof via === "object" && typeof via.url === "string")
    .map((via) => basename(via.url));
}

/** Whether `entry` waives advisory `id` at node path `node` on `today`. */
function waives(entry, id, node, today) {
  return (
    entry.advisory === id &&
    node.startsWith(`node_modules/${entry.inside}/node_modules/`) &&
    entry.expires >= today
  );
}

/**
 * The findings at or above `level` that no allow entry waives, one line
 * each; empty when the allowlist covers every one of them.
 */
export function unwaived(report, allow, level, today) {
  const floor = severities.indexOf(level);
  const problems = [];
  for (const [name, vulnerability] of Object.entries(report.vulnerabilities ?? {})) {
    if (severities.indexOf(vulnerability.severity) < floor) continue;
    // A package flagged only through another vulnerable package has no
    // advisory of its own; that package's own entry carries the verdict.
    for (const id of advisoryIds(vulnerability)) {
      for (const node of vulnerability.nodes) {
        if (!allow.some((entry) => waives(entry, id, node, today))) {
          problems.push(`${name} ${id} at ${node}`);
        }
      }
    }
  }
  return problems;
}

/** Whether the allowlist waives every finding `npm audit` reports. */
async function allowlisted(level) {
  const allow = JSON.parse(
    readFileSync(new URL("./npm-audit-allow.json", import.meta.url), "utf8"),
  );
  const { out } = await run("npm", ["audit", "--json"], "pipe");
  let report;
  try {
    report = JSON.parse(out);
  } catch {
    return false;
  }
  const today = new Date().toISOString().slice(0, 10);
  const problems = unwaived(report, allow, level, today);
  if (problems.length > 0) {
    console.error(`npm-audit-gate: not waived: ${problems.join("; ")}`);
    return false;
  }
  console.error(
    "npm-audit-gate: every finding is waived by scripts/npm-audit-allow.json",
  );
  return true;
}

async function main(args) {
  if (args.length === 0) {
    console.error("usage: npm-audit-gate.mjs <npm audit args...>");
    process.exit(2);
  }
  const level =
    args.find((arg) => arg.startsWith("--audit-level="))?.split("=")[1] ?? "low";

  for (let attempt = 1; attempt <= maxAttempts; attempt += 1) {
    const result = await run("npm", ["audit", ...args], "inherit");
    if (result.code === 0) {
      process.exit(0);
    }

    if (!transient.test(result.stderr)) {
      // A real advisory finding (or an unexpected npm failure): surface it,
      // unless the allowlist waives every finding.
      process.stderr.write(result.stderr);
      process.exit((await allowlisted(level)) ? 0 : (result.code ?? 1));
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
}

// Imported by its test for `unwaived`; only a direct run audits.
if (process.argv[1] === fileURLToPath(import.meta.url)) {
  await main(process.argv.slice(2));
}
