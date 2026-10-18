import { parseArgs } from "node:util";
import type { Command } from "../lib/command.ts";
import { status } from "../lib/output.ts";
import { CheckFailed, mutationCheck } from "./check.ts";
import {
  DESCRIPTION,
  HELP,
  Interrupts,
  mutationRun,
  parseOptions,
  USAGE,
  UsageError,
} from "./runner.ts";

export const commands: Record<string, Command> = {
  "mutation-run": {
    summary: "Bounded cargo-mutants scan of a scoped Rust slice (see --help).",
    async run(args) {
      let options;
      try {
        options = parseOptions(args);
      } catch (error) {
        if (!(error instanceof UsageError)) throw error;
        console.error(`${USAGE}\nmutation-run: error: ${error.message}`);
        return 2;
      }
      if (options === undefined) {
        console.log(`${USAGE}\n\n${DESCRIPTION}\n${HELP}`);
        return 0;
      }
      const interrupts = new Interrupts();
      interrupts.install();
      return mutationRun(options, interrupts);
    },
  },
  "mutation-check": {
    summary: "Real-tool acceptance of mutation-run in disposable Rust fixtures [--process-only].",
    async run(args) {
      const { values } = parseArgs({ args, options: { "process-only": { type: "boolean" } } });
      try {
        const reports = await mutationCheck(values["process-only"] === true);
        console.log(`Rust runner acceptance checks passed; reports: ${reports}`);
        return 0;
      } catch (error) {
        if (!(error instanceof CheckFailed)) throw error;
        status("error", error.message);
        return 1;
      }
    },
  },
};
