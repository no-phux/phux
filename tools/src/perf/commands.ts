import { readFileSync } from "node:fs";
import { required, UsageError, type Command } from "../lib/command.ts";
import { repoPath } from "../lib/repo.ts";
import {
  BASELINE,
  authorize,
  compare,
  loadBaseline,
  loadMeasurements,
  runnerVersion,
  writeBaseline,
  type Measurements,
} from "./icount.ts";

function measured(args: string[]): Measurements {
  const [file, ...rest] = args;
  if (rest.length > 0) throw new UsageError(`unexpected ${rest.join(" ")}`);
  return loadMeasurements(readFileSync(required(file, "FILE"), "utf8"));
}

function noArguments(args: string[]): void {
  if (args.length > 0) throw new UsageError(`unexpected ${args.join(" ")}`);
}

function run(sub: string | undefined, args: string[]): number {
  switch (sub) {
    case "compare": {
      const { lines, failures } = compare(measured(args), loadBaseline());
      console.log(lines.join("\n"));
      if (failures.length === 0) return 0;
      console.error(`perf-icount: see docs/development.md#performance\n${failures.join("\n")}`);
      return 1;
    }
    case "update":
      writeBaseline(measured(args));
      console.log(`wrote ${BASELINE}`);
      return 0;
    case "authorize": {
      noArguments(args);
      const env = process.env;
      const notice = authorize(
        env["PHUX_BASE_SHA"] ?? "",
        env["GITHUB_EVENT_NAME"] ?? "",
        env["PHUX_PERF_BASELINE_UPDATE"] === "1",
      );
      if (notice !== undefined) console.log(notice);
      return 0;
    }
    case "runner-version":
      noArguments(args);
      console.log(runnerVersion(readFileSync(repoPath("Cargo.lock"), "utf8")));
      return 0;
    default:
      throw new UsageError(sub === undefined ? "missing subcommand" : `unknown subcommand ${sub}`);
  }
}

export const commands: Record<string, Command> = {
  "perf-icount": {
    summary: "Compare, update or authorize the callgrind instruction-count baseline.",
    usage: "compare FILE | update FILE | authorize | runner-version",
    help:
      "FILE holds iai-callgrind JSON lines. authorize reads PHUX_BASE_SHA, GITHUB_EVENT_NAME\n" +
      "and PHUX_PERF_BASELINE_UPDATE=1 (the perf-baseline label).",
    run: ([sub, ...args]) => run(sub, args),
  },
};
