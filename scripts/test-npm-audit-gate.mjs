// Tests for the npm audit gate's allowlist: a waiver covers exactly the
// advisory it names, only inside the package it names, and only until it
// expires.

import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";

import { unwaived } from "./npm-audit-gate.mjs";

const advisory = "GHSA-q2hr-2g5m-vwhr";
const inside = "@earendil-works/pi-coding-agent";
const entry = { advisory, inside, expires: "2026-11-15", reason: "test" };

function report(node, severity = "high", id = advisory) {
  return {
    vulnerabilities: {
      "brace-expansion": {
        severity,
        nodes: [node],
        via: [{ url: `https://github.com/advisories/${id}` }],
      },
    },
  };
}

const shrinkwrapped = `node_modules/${inside}/node_modules/brace-expansion`;

test("an unexpired entry waives its advisory inside its package", () => {
  assert.deepEqual(unwaived(report(shrinkwrapped), [entry], "high", "2026-09-30"), []);
});

test("the same advisory at a top-level install is not waived", () => {
  const problems = unwaived(report("node_modules/brace-expansion"), [entry], "high", "2026-09-30");
  assert.equal(problems.length, 1);
});

test("an expired entry waives nothing", () => {
  const problems = unwaived(report(shrinkwrapped), [entry], "high", "2026-11-16");
  assert.equal(problems.length, 1);
});

test("a different advisory on the same path is not waived", () => {
  const problems = unwaived(report(shrinkwrapped, "high", "GHSA-xxxx-xxxx-xxxx"), [entry], "high", "2026-09-30");
  assert.equal(problems.length, 1);
});

test("findings below the audit level are not the gate's concern", () => {
  assert.deepEqual(unwaived(report("node_modules/x", "moderate"), [], "high", "2026-09-30"), []);
});

test("a package flagged only through another carries no verdict of its own", () => {
  const indirect = {
    vulnerabilities: { minimatch: { severity: "high", nodes: ["node_modules/minimatch"], via: ["brace-expansion"] } },
  };
  assert.deepEqual(unwaived(indirect, [], "high", "2026-09-30"), []);
});

test("every allow entry names an advisory, a package, an ISO expiry and a reason", () => {
  const allow = JSON.parse(readFileSync(new URL("./npm-audit-allow.json", import.meta.url), "utf8"));
  for (const item of allow) {
    assert.match(item.advisory, /^GHSA-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4}$/);
    assert.ok(item.inside.length > 0);
    assert.match(item.expires, /^\d{4}-\d{2}-\d{2}$/);
    assert.ok(item.reason.length > 20);
  }
});
