import { parseArgs } from "node:util";
import { UsageError, type Command } from "../lib/command.ts";
import { fileTokensCheck, mergeBase } from "./file-tokens.ts";
import { mainRed } from "./main-red.ts";
import { npmAuditGate } from "./npm-audit-gate.ts";

const text = { type: "string" } as const;

export const commands: Record<string, Command> = {
  "file-tokens-check": {
    summary: "Reject staged files that are new or grew past the agent token budget.",
    usage: "[--base COMMIT | --merge-base REF]",
    help: "Checks the index of the current checkout (the pre-commit hook), or HEAD against COMMIT or its merge base with REF.",
    run(args) {
      const { values } = parseArgs({ args, options: { base: text, "merge-base": text } });
      if (values.base !== undefined && values["merge-base"] !== undefined) {
        throw new UsageError("--base and --merge-base are exclusive");
      }
      const cwd = process.cwd();
      const ref = values["merge-base"];
      return fileTokensCheck(cwd, ref === undefined ? values.base : mergeBase(cwd, ref));
    },
  },
  "main-red": {
    summary: "Open, update or close the ci/main-red issue for a completed main push run (env in).",
    run: () => mainRed(),
  },
  "npm-audit-gate": {
    summary: "Run npm audit here, retrying registry outages and honoring the reviewed allowlist.",
    usage: "NPM_AUDIT_ARGS...",
    help: "Run from an integration package, e.g. bun ../../tools/main.ts npm-audit-gate --audit-level=high",
    run(args) {
      if (args.length === 0) throw new UsageError("expected npm audit arguments");
      return npmAuditGate(args);
    },
  },
};
