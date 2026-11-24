// The npm audit gate: a waiver covers exactly the advisory it names, only
// inside the package it names, and only until it expires; npm's own output
// and verdict pass through, and only endpoint outages are retried.
import { afterAll, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";
import { allowlist, unwaived } from "./npm-audit-gate.ts";

const advisory = "GHSA-q2hr-2g5m-vwhr";
const inside = "@earendil-works/pi-coding-agent";
const entry = { advisory, inside, expires: "2026-11-15", reason: "test" };
const shrinkwrapped = `node_modules/${inside}/node_modules/brace-expansion`;

function report(node: string, severity = "high", id = advisory): unknown {
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

test("an unexpired entry waives its advisory inside its package", () => {
  expect(unwaived(report(shrinkwrapped), [entry], "high", "2026-09-30")).toEqual([]);
});

test("a top-level install, an expired entry or another advisory is not waived", () => {
  expect(
    unwaived(report("node_modules/brace-expansion"), [entry], "high", "2026-09-30"),
  ).toHaveLength(1);
  expect(unwaived(report(shrinkwrapped), [entry], "high", "2026-11-16")).toHaveLength(1);
  const other = report(shrinkwrapped, "high", "GHSA-xxxx-xxxx-xxxx");
  expect(unwaived(other, [entry], "high", "2026-09-30")).toEqual([
    `brace-expansion GHSA-xxxx-xxxx-xxxx at ${shrinkwrapped}`,
  ]);
});

test("findings below the level, or flagged only through another package, carry no verdict", () => {
  expect(unwaived(report("node_modules/x", "moderate"), [], "high", "2026-09-30")).toEqual([]);
  const indirect = {
    vulnerabilities: {
      minimatch: { severity: "high", nodes: ["node_modules/minimatch"], via: ["brace-expansion"] },
    },
  };
  expect(unwaived(indirect, [], "high", "2026-09-30")).toEqual([]);
});

test("every allow entry names an advisory, a package, an ISO expiry and a reason", () => {
  for (const item of allowlist()) {
    expect(item.advisory).toMatch(/^GHSA-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4}$/);
    expect(item.inside.length).toBeGreaterThan(0);
    expect(item.expires).toMatch(/^\d{4}-\d{2}-\d{2}$/);
    expect(item.reason.length).toBeGreaterThan(20);
  }
});

const scratch = mkdtempSync(join(tmpdir(), "phux-npm-audit-"));
afterAll(() => rmSync(scratch, { recursive: true, force: true }));

/** A fake `npm` on PATH, from a shell body. */
function fakeNpm(name: string, body: string): string {
  const dir = join(scratch, name);
  mkdirSync(dir, { recursive: true });
  writeFileSync(join(dir, "npm"), `#!/bin/sh\n${body}`, { mode: 0o755 });
  return dir;
}

function gate(argv: string[], path: string, env: Record<string, string> = {}) {
  const result = Bun.spawnSync([process.execPath, ...argv], {
    env: { ...process.env, NO_COLOR: "1", PATH: `${path}:${process.env["PATH"] ?? ""}`, ...env },
    stdout: "pipe",
    stderr: "pipe",
    timeout: 10_000,
  });
  return {
    code: result.exitCode,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

const CLI = [repoPath("tools/main.ts"), "npm-audit-gate"];

test("no audit arguments is a usage error that calls no npm", () => {
  const result = gate(CLI, fakeNpm("usage", "exit 99\n"));
  expect(result.code).toBe(2);
  expect(result.stdout).toBe("");
  expect(result.stderr).toContain("npm-audit-gate: error: expected npm audit arguments");
});

test("native audit verdicts keep npm's output and exit status", () => {
  const npm = fakeNpm(
    "verdicts",
    `if [ "$2" = --json ]; then
  printf '%s\\n' "$AUDIT_TEST_REPORT"
  exit 17
fi
printf 'native audit stdout\\n'
[ "$AUDIT_TEST_CODE" = 0 ] || printf 'native audit refusal\\n' >&2
exit "$AUDIT_TEST_CODE"
`,
  );
  const finding = JSON.stringify(report("node_modules/test", "high", "GHSA-xxxx-xxxx-xxxx"));
  const cases: [number, string, number, string | undefined][] = [
    [0, finding, 0, undefined],
    [17, finding, 17, "error"],
    [17, JSON.stringify({ vulnerabilities: {} }), 0, "warn"],
  ];
  for (const [code, document, expected, label] of cases) {
    const env = { AUDIT_TEST_CODE: String(code), AUDIT_TEST_REPORT: document };
    const result = gate([...CLI, "--audit-level=high"], npm, env);
    expect(result.code, result.stderr).toBe(expected);
    expect(result.stdout).toBe("native audit stdout\n");
    if (label === undefined) {
      expect(result.stderr).toBe("");
    } else {
      expect(result.stderr).toStartWith("native audit refusal\n");
      expect(result.stderr).toContain(`phux: ${label}: npm-audit-gate:`);
    }
  }
});

test("endpoint outages are retried until they recover or the attempts run out", () => {
  const npm = fakeNpm(
    "retries",
    `attempt=0
[ ! -f "$AUDIT_TEST_COUNT" ] || read -r attempt < "$AUDIT_TEST_COUNT"
attempt=$((attempt + 1))
printf '%s\\n' "$attempt" > "$AUDIT_TEST_COUNT"
printf 'native audit stdout\\n'
if [ "$attempt" -le "$AUDIT_TEST_FAILS" ]; then
  printf 'HTTP 503 audit endpoint returned an error\\n' >&2
  exit 17
fi
exit 0
`,
  );
  // The real retry loop, without the backoff's wall-clock wait.
  const module = JSON.stringify(repoPath("tools/src/policy/npm-audit-gate.ts"));
  const script = `import { npmAuditGate } from ${module};
process.exit(await npmAuditGate(["--audit-level=high"], async () => {}));`;
  for (const [failures, expectedCode, attempts] of [
    [2, 0, 3],
    [5, 17, 5],
  ] as const) {
    const count = join(scratch, `count-${failures}`);
    const env = { AUDIT_TEST_COUNT: count, AUDIT_TEST_FAILS: String(failures) };
    const result = gate(["-e", script], npm, env);
    expect(result.code, result.stderr).toBe(expectedCode);
    expect(Number(readFileSync(count, "utf8"))).toBe(attempts);
    expect(result.stdout).toBe("native audit stdout\n".repeat(attempts));
    expect(result.stderr.match(/phux: warn:/g) ?? []).toHaveLength(attempts - 1);
    if (expectedCode === 0) {
      expect(result.stderr).not.toContain("phux: error:");
    } else {
      expect(result.stderr).toMatch(
        /HTTP 503 audit endpoint returned an error\nphux: error: npm-audit-gate: registry audit endpoint still unavailable after 5 attempts; failing\./,
      );
    }
  }
});
