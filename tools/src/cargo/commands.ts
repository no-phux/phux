import { parseArgs } from "node:util";
import { type Command } from "../lib/command.ts";
import { report } from "../lib/expect.ts";
import { status } from "../lib/output.ts";
import { ROOT } from "../lib/repo.ts";
import { readGraphs, violations } from "./build-features.ts";
import { checkGhosttyPin } from "./ghostty-pin.ts";
import {
  configViolations,
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

export const commands: Record<string, Command> = {
  "release-orchestration-check": {
    summary: "Check the Release Please settings and lock bumps (--fix: regenerate the lock bumps).",
    usage: "[--fix]",
    run(args) {
      if (flagAndPaths(args, "fix").set) writeLockFiles();
      const config = readConfig();
      const failures = [...configViolations(config), ...lockFileViolations(config)];
      if (!locksInStep()) failures.push("standalone Cargo.lock path crate versions drifted");
      return report(failures, "release orchestration passed");
    },
  },
  "build-features-check": {
    summary: "Check production dependency feature boundaries from `cargo tree` (compile-free).",
    run: () =>
      report(violations(readGraphs()), "production feature and host/engine boundaries hold"),
  },
  "ghostty-pin": {
    summary: "Verify the libghostty-vt engine pin and report its prebuilt coverage (network).",
    async run() {
      await checkGhosttyPin({ root: ROOT, fetch, log: status, token: process.env["GH_TOKEN"] });
      return 0;
    },
  },
};
