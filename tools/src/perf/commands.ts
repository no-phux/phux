import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { required, UsageError, type Command } from "../lib/command.ts";
import { repoPath } from "../lib/repo.ts";
import {
  BASELINE_PATH,
  authorize,
  compare,
  loadBaseline,
  loadMeasurements,
  runnerVersion,
  writeBaseline,
} from "./icount.ts";

const USAGE = `compare|update --measured FILE [--baseline FILE]
       perf-icount authorize --base SHA [--event NAME] [--label-update 0|1]
       perf-icount runner-version`;

function measuredArgs(args: string[]): { measured: string; baseline: string } {
  const { values } = parseArgs({
    args,
    options: {
      measured: { type: "string" },
      baseline: { type: "string", default: BASELINE_PATH },
    },
  });
  return { measured: required(values.measured, "--measured"), baseline: values.baseline };
}

function run(sub: string | undefined, args: string[]): number {
  switch (sub) {
    case "compare": {
      const { measured, baseline } = measuredArgs(args);
      const counts = loadMeasurements(readFileSync(measured, "utf8"));
      const { lines, failures } = compare(counts, loadBaseline(baseline));
      console.log(lines.join("\n"));
      if (failures.length === 0) return 0;
      console.error(
        "perf-icount: instruction count rose more than the limit, " +
          "or the scenario set does not match. See docs/performance-ci.md.",
      );
      return 1;
    }
    case "update": {
      const { measured, baseline } = measuredArgs(args);
      console.log(writeBaseline(baseline, loadMeasurements(readFileSync(measured, "utf8"))));
      return 0;
    }
    case "authorize": {
      const { values } = parseArgs({
        args,
        options: {
          base: { type: "string" },
          event: { type: "string", default: "" },
          "label-update": { type: "string", default: "0" },
        },
      });
      const notice = authorize(
        required(values.base, "--base"),
        values.event,
        values["label-update"],
      );
      if (notice !== undefined) console.log(notice);
      return 0;
    }
    case "runner-version":
      parseArgs({ args, options: {} });
      console.log(runnerVersion(readFileSync(repoPath("Cargo.lock"), "utf8")));
      return 0;
    default:
      throw new UsageError(sub === undefined ? "missing subcommand" : `unknown subcommand ${sub}`);
  }
}

export const commands: Record<string, Command> = {
  "perf-icount": {
    summary: "Compare, update or authorize the callgrind instruction-count baseline.",
    usage: USAGE,
    run: ([sub, ...args]) => run(sub, args),
  },
};
