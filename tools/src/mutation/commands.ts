import type { Command } from "../lib/command.ts";
import { inherit } from "../lib/process.ts";
import { HELP, mutationRun, parseOptions, TOOL_ROOT, USAGE, VERSION } from "./runner.ts";

export const commands: Record<string, Command> = {
  "mutation-run": {
    summary: "Bounded cargo-mutants scan of a scoped Rust slice.",
    usage: USAGE,
    help: HELP,
    run: (args) => mutationRun(parseOptions(args)),
  },
  "mutation-install": {
    summary: `Install the pinned cargo-mutants ${VERSION} under target/mutation-tools.`,
    run: () =>
      inherit([
        "cargo",
        "install",
        "--locked",
        "--jobs=2",
        `--root=${TOOL_ROOT}`,
        `cargo-mutants@${VERSION}`,
      ]),
  },
};
