import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { parseArgs } from "node:util";
import { UsageError, type Command } from "../lib/command.ts";
import { report } from "../lib/expect.ts";
import { status } from "../lib/output.ts";
import { repoPath } from "../lib/repo.ts";
import { readGraphs, violations } from "./build-features.ts";
import { compare, repairHostLock, reportFindings, requiredDependencies } from "./host-lock.ts";
import { configViolations, locksInStep, readConfig } from "./release-orchestration.ts";
import { syncLocks } from "./sync.ts";

function hostLock(args: string[]): number {
  const { values, positionals } = parseArgs({
    args,
    options: { fix: { type: "boolean", default: false } },
    allowPositionals: true,
  });
  if (positionals.length > 2) throw new UsageError("at most two locks");
  const [rootLock = repoPath("Cargo.lock"), host = repoPath("clients/desktop/native/Cargo.lock")] =
    positionals;
  const rootText = readFileSync(rootLock, "utf8");
  let hostText = readFileSync(host, "utf8");
  const required = requiredDependencies(dirname(resolve(rootLock)));
  if (values.fix) hostText = repairHostLock(rootText, hostText, required, host);
  return reportFindings(compare(rootText, hostText, required), host, rootLock);
}

export const commands: Record<string, Command> = {
  "sync-path-lock-versions": {
    summary:
      "Align standalone Cargo.locks' path crate versions with the root lock (--check: report).",
    usage: "[--check] ROOT_LOCK TARGET_LOCK...",
    run(args) {
      const { values, positionals } = parseArgs({
        args,
        options: { check: { type: "boolean", default: false } },
        allowPositionals: true,
      });
      const [rootLock, ...targets] = positionals;
      if (rootLock === undefined || targets.length === 0) {
        throw new UsageError("a root lock and at least one target lock are required");
      }
      return syncLocks(rootLock, targets, values.check) ? 0 : 1;
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
    run() {
      const graphs = readGraphs();
      const failures = violations(graphs);
      if (failures.length > 0) return report(failures, "");
      for (const [name, graph] of [
        ["full", graphs.full],
        ["lean", graphs.lean],
        ["mcp", graphs.mcp],
      ] as const) {
        status("ok", `${name}: ${graph.size} distinct package names; feature boundaries verified`);
      }
      return 0;
    },
  },
};
