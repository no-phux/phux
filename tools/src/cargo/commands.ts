import { readFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import { parseArgs } from "node:util";
import type { Command } from "../lib/command.ts";
import { report } from "../lib/expect.ts";
import { status } from "../lib/output.ts";
import { repoPath } from "../lib/repo.ts";
import { readGraphs, violations } from "./build-features.ts";
import { compare, repairHostLock, reportFindings, requiredDependencies } from "./host-lock.ts";
import { LockError } from "./lock.ts";
import { configViolations, locksInStep, readConfig } from "./release-orchestration.ts";
import { syncLocks } from "./sync.ts";

const SYNC_USAGE =
  "usage: bun tools/main.ts sync-path-lock-versions [--check] ROOT_LOCK TARGET_LOCK...";
const HOST_LOCK_USAGE =
  "usage: bun tools/main.ts desktop-host-lock [--fix] [ROOT_LOCK [HOST_LOCK]]";

/** Parse flags and positionals; undefined (after printing `usage`) on a bad command line. */
function parse(
  args: string[],
  flag: string,
  usage: string,
): { set: boolean; positionals: string[] } | undefined {
  try {
    const { values, positionals } = parseArgs({
      args,
      options: { [flag]: { type: "boolean", default: false } },
      allowPositionals: true,
    });
    return { set: values[flag] === true, positionals };
  } catch (error) {
    console.error(`${String(error instanceof Error ? error.message : error)}\n${usage}`);
    return undefined;
  }
}

function hostLock(args: string[]): number {
  const parsed = parse(args, "fix", HOST_LOCK_USAGE);
  if (parsed === undefined || parsed.positionals.length > 2) {
    if (parsed !== undefined) console.error(HOST_LOCK_USAGE);
    return 2;
  }
  const [rootLock = repoPath("Cargo.lock"), host = repoPath("clients/desktop/native/Cargo.lock")] =
    parsed.positionals;
  const rootText = readFileSync(rootLock, "utf8");
  let hostText = readFileSync(host, "utf8");
  const required = requiredDependencies(dirname(resolve(rootLock)));
  try {
    if (parsed.set) hostText = repairHostLock(rootText, hostText, required, host);
  } catch (error) {
    if (!(error instanceof LockError)) throw error;
    status("error", error.message);
    return 1;
  }
  return reportFindings(compare(rootText, hostText, required), host, rootLock);
}

export const commands: Record<string, Command> = {
  "sync-path-lock-versions": {
    summary:
      "Align standalone Cargo.locks' path crate versions with the root lock (--check: report).",
    run(args) {
      const parsed = parse(args, "check", SYNC_USAGE);
      if (parsed === undefined || parsed.positionals.length < 2) {
        if (parsed !== undefined) console.error(SYNC_USAGE);
        return 2;
      }
      const [rootLock = "", ...targets] = parsed.positionals;
      return syncLocks(rootLock, targets, parsed.set) ? 0 : 1;
    },
  },
  "desktop-host-lock": {
    summary: "Check (--fix: repair) that the desktop host lock satisfies a --locked build.",
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
