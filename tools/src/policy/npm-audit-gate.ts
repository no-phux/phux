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
// entry in npm-audit-allow.json beside this file: the same advisory,
// installed at a path inside the named package. That is for fixes that cannot
// reach us, such as a dev dependency whose own npm-shrinkwrap pins the
// vulnerable copy; an expired or non-matching entry fails the gate like any
// other finding.
//
// From an integration package: bun ../../tools/main.ts npm-audit-gate --audit-level=high
import { readFileSync } from "node:fs";
import { basename } from "node:path";
import { status } from "../lib/output.ts";
import { list, str, strings, table } from "../lib/value.ts";

export const ALLOWLIST = "tools/src/policy/npm-audit-allow.json";
const SEVERITIES = ["info", "low", "moderate", "high", "critical"];

// npm prints this line (plus variants) when the registry audit API itself
// fails; anything else is a real verdict and must fail the gate loudly.
const TRANSIENT =
  /audit endpoint returned an error|ECONNRESET|ETIMEDOUT|EAI_AGAIN|ENOTFOUND|socket hang up|fetch failed|HTTP 50[234]/i;
const MAX_ATTEMPTS = 5;
const BACKOFF_SECONDS = [5, 10, 20, 40];

export interface AllowEntry {
  readonly advisory: string;
  readonly inside: string;
  readonly expires: string;
  readonly reason: string;
}

/** Advisory ids ("GHSA-...") a vulnerability entry reports directly. */
function advisoryIds(vulnerability: Record<string, unknown>): string[] {
  return list(vulnerability["via"]).flatMap((via) => {
    const url = typeof via === "object" && via !== null ? str(table(via)["url"]) : undefined;
    return url === undefined ? [] : [basename(url)];
  });
}

function waives(entry: AllowEntry, id: string, node: string, today: string): boolean {
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
export function unwaived(
  report: unknown,
  allow: readonly AllowEntry[],
  level: string,
  today: string,
): string[] {
  const floor = SEVERITIES.indexOf(level);
  const problems: string[] = [];
  for (const [name, value] of Object.entries(table(table(report)["vulnerabilities"] ?? {}))) {
    const vulnerability = table(value);
    if (SEVERITIES.indexOf(str(vulnerability["severity"]) ?? "") < floor) continue;
    // A package flagged only through another vulnerable package has no
    // advisory of its own; that package's own entry carries the verdict.
    for (const id of advisoryIds(vulnerability)) {
      for (const node of strings(vulnerability["nodes"])) {
        if (!allow.some((entry) => waives(entry, id, node, today))) {
          problems.push(`${name} ${id} at ${node}`);
        }
      }
    }
  }
  return problems;
}

export function allowlist(): AllowEntry[] {
  const path = new URL("./npm-audit-allow.json", import.meta.url);
  return list(JSON.parse(readFileSync(path, "utf8"))).map((value) => {
    const entry = table(value);
    return {
      advisory: str(entry["advisory"]) ?? "",
      inside: str(entry["inside"]) ?? "",
      expires: str(entry["expires"]) ?? "",
      reason: str(entry["reason"]) ?? "",
    };
  });
}

interface Audit {
  code: number;
  stdout: string;
  stderr: string;
}

/** Run npm, capturing stderr and, with `pipe`, stdout; `inherit` passes stdout through. */
async function npm(args: string[], stdout: "pipe" | "inherit"): Promise<Audit> {
  const child = Bun.spawn(["npm", ...args], { stdin: "ignore", stdout, stderr: "pipe" });
  const [out, err, code] = await Promise.all([
    stdout === "pipe" ? new Response(child.stdout).text() : Promise.resolve(""),
    new Response(child.stderr).text(),
    child.exited,
  ]);
  return { code, stdout: out, stderr: err };
}

/** Whether the allowlist waives every finding `npm audit` reports. */
async function allowlisted(level: string): Promise<boolean> {
  const { stdout } = await npm(["audit", "--json"], "pipe");
  let report: unknown;
  try {
    report = JSON.parse(stdout);
  } catch {
    return false;
  }
  const problems = unwaived(report, allowlist(), level, new Date().toISOString().slice(0, 10));
  if (problems.length > 0) {
    status("error", `npm-audit-gate: not waived: ${problems.join("; ")}`);
    return false;
  }
  status("warn", `npm-audit-gate: every finding is waived by ${ALLOWLIST}`);
  return true;
}

const pause = (seconds: number): Promise<void> => Bun.sleep(seconds * 1000);

/**
 * `npm audit ARGS` in the current directory, retried while the registry
 * endpoint is down. npm's own output passes through; returns the exit code.
 */
export async function npmAuditGate(args: string[], sleep = pause): Promise<number> {
  const level = args.find((arg) => arg.startsWith("--audit-level="))?.split("=")[1] ?? "low";
  for (let attempt = 1; ; attempt += 1) {
    const result = await npm(["audit", ...args], "inherit");
    if (result.code === 0) return 0;
    if (!TRANSIENT.test(result.stderr)) {
      process.stderr.write(result.stderr);
      return (await allowlisted(level)) ? 0 : result.code;
    }
    if (attempt === MAX_ATTEMPTS) {
      process.stderr.write(result.stderr);
      status(
        "error",
        `npm-audit-gate: registry audit endpoint still unavailable after ${MAX_ATTEMPTS} attempts; failing.`,
      );
      return result.code;
    }
    const wait = BACKOFF_SECONDS[attempt - 1] ?? 40;
    status(
      "warn",
      `npm-audit-gate: audit endpoint unavailable (attempt ${attempt}/${MAX_ATTEMPTS}); retrying in ${wait}s...`,
    );
    await sleep(wait);
  }
}
