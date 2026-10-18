import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { parseArgs } from "node:util";
import { UsageError, type Command } from "../lib/command.ts";
import { report } from "../lib/expect.ts";
import { hint, status } from "../lib/output.ts";
import { repoPath } from "../lib/repo.ts";
import { readGraphs, violations } from "./build-features.ts";
import { applyFixes, compare, requiredDependencies } from "./host-lock.ts";
import { syncLocks } from "./lock.ts";
import { configViolations, locksInStep, readConfig } from "./release-orchestration.ts";

function flagAndPaths(args: string[], flag: string) {
  const { values, positionals } = parseArgs({
    args,
    options: { [flag]: { type: "boolean", default: false } },
    allowPositionals: true,
  });
  return { set: values[flag] === true, paths: positionals };
}

function hostLock(args: string[]): number {
  const { set: fix, paths } = flagAndPaths(args, "fix");
  if (paths.length > 2) throw new UsageError("at most two locks");
  const [rootLock = repoPath("Cargo.lock"), host = repoPath("clients/desktop/native/Cargo.lock")] =
    paths;
  const rootText = readFileSync(rootLock, "utf8");
  let hostText = readFileSync(host, "utf8");
  const required = requiredDependencies(dirname(resolve(rootLock)));
  let findings = compare(rootText, hostText, required);
  const fixable = findings.filter((finding) => finding.fix !== undefined).length;
  if (fix && fixable > 0) {
    hostText = applyFixes(hostText, findings);
    writeFileSync(host, hostText);
    status("ok", `fixed ${fixable} entr${fixable === 1 ? "y" : "ies"} in ${host}`);
    findings = compare(rootText, hostText, required);
  }
  if (findings.length === 0) {
    status("ok", `${host} is in step with ${rootLock}`);
    return 0;
  }
  for (const finding of findings) {
    status("error", `${host}: ${finding.message}`);
    hint(
      finding.fix === undefined
        ? "refresh the host lock: just desktop-lock-refresh"
        : "repair with: just desktop-lock-fix",
    );
  }
  return 1;
}

export const commands: Record<string, Command> = {
  "sync-path-lock-versions": {
    summary:
      "Align standalone Cargo.locks' path crate versions with the root lock (--check: report).",
    usage: "[--check] ROOT_LOCK TARGET_LOCK...",
    run(args) {
      const { set: check, paths } = flagAndPaths(args, "check");
      const [rootLock, ...targets] = paths;
      if (rootLock === undefined || targets.length === 0) {
        throw new UsageError("a root lock and at least one target lock are required");
      }
      return syncLocks(rootLock, targets, check) ? 0 : 1;
    },
  },
  "desktop-host-lock": {
    summary: "Check (--fix: repair) that the desktop host lock satisfies a --locked build.",
    usage: "[--fix] [ROOT_LOCK [HOST_LOCK]]",
    run: hostLock,
  },
  "release-orchestration-check": {
    summary: "Check the Release Please settings and the standalone locks' path crate versions.",
    run() {
      const failures = configViolations(readConfig());
      if (!locksInStep()) failures.push("standalone Cargo.lock path crate versions drifted");
      return report(failures, "release orchestration passed");
    },
  },
  "build-features-check": {
    summary: "Check production dependency feature boundaries from `cargo tree` (compile-free).",
    run: () =>
      report(violations(readGraphs()), "production feature boundaries hold (full, lean, mcp)"),
  },
};
