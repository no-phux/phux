import { readFileSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { parseArgs } from "node:util";
import { UsageError, type Command } from "../lib/command.ts";
import { report } from "../lib/expect.ts";
import { hint, status } from "../lib/output.ts";
import { repoPath } from "../lib/repo.ts";
import { readGraphs, violations } from "./build-features.ts";
import { applyFixes, compare, requiredDependencies } from "./host-lock.ts";
import {
  configViolations,
  DESKTOP_TRAIN,
  lockFileViolations,
  locksInStep,
  readConfig,
  writeLockFiles,
} from "./release-orchestration.ts";

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
  "desktop-host-lock": {
    summary: "Check (--fix: repair) that the desktop host lock satisfies a --locked build.",
    usage: "[--fix] [ROOT_LOCK [HOST_LOCK]]",
    run: hostLock,
  },
  "release-orchestration-check": {
    summary: "Check the Release Please settings and lock bumps (--fix: regenerate the lock bumps).",
    usage: "[--fix]",
    run(args) {
      if (flagAndPaths(args, "fix").set) writeLockFiles();
      const config = readConfig();
      const failures = [
        ...configViolations(config, readConfig(undefined, DESKTOP_TRAIN.config)),
        ...lockFileViolations(config),
      ];
      if (!locksInStep()) failures.push("standalone Cargo.lock path crate versions drifted");
      return report(failures, "release orchestration passed");
    },
  },
  "build-features-check": {
    summary: "Check production dependency feature boundaries from `cargo tree` (compile-free).",
    run: () =>
      report(violations(readGraphs()), "production feature and host/engine boundaries hold"),
  },
};
