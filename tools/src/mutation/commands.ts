import { parseArgs } from "node:util";
import type { Command } from "../lib/command.ts";
import { mutationCheck } from "./check.ts";
import {
  HELP,
  Interrupts,
  mutationRun,
  parseOptions,
  TOOL_ROOT,
  USAGE,
  VERSION,
} from "./runner.ts";

export const commands: Record<string, Command> = {
  "mutation-run": {
    summary: "Bounded cargo-mutants scan of a scoped Rust slice.",
    usage: USAGE,
    help: HELP,
    async run(args) {
      const options = parseOptions(args);
      const interrupts = new Interrupts();
      interrupts.install();
      return mutationRun(options, interrupts);
    },
  },
  "mutation-install": {
    summary: `Install the pinned cargo-mutants ${VERSION} under target/mutation-tools.`,
    run() {
      const argv = ["cargo", "install", "cargo-mutants", "--version", VERSION, "--locked"];
      const install = Bun.spawnSync([...argv, "--jobs", "2", "--root", TOOL_ROOT], {
        stdio: ["inherit", "inherit", "inherit"],
      });
      return install.exitCode ?? 1;
    },
  },
  "mutation-check": {
    summary: "Real-tool acceptance of mutation-run in disposable Rust fixtures.",
    usage: "[--process-only]",
    async run(args) {
      const { values } = parseArgs({ args, options: { "process-only": { type: "boolean" } } });
      const reports = await mutationCheck(values["process-only"] === true);
      console.log(`Rust runner acceptance checks passed; reports: ${reports}`);
      return 0;
    },
  },
};
