import { parseArgs } from "node:util";
import { UsageError, type Command } from "../lib/command.ts";
import { report } from "../lib/expect.ts";
import { ROOT } from "../lib/repo.ts";
import { fileTokensCheck, mergeBase } from "./file-tokens.ts";
import { frozenFixturesCheck } from "./frozen-fixtures.ts";
import { mainRed } from "./main-red.ts";
import { npmAuditGate } from "./npm-audit-gate.ts";
import { toolchainSync } from "./toolchain-sync.ts";

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
  "frozen-fixtures-check": {
    summary: "Fail when frozen fixtures or persisted upgrade-blob field names changed.",
    usage: "[BASE]",
    help: "BASE defaults to origin/main; the diff is taken from its merge base with HEAD.",
    run(args) {
      const [base, ...extra] = parseArgs({ args, allowPositionals: true }).positionals;
      if (extra.length > 0) throw new UsageError("expected at most one BASE");
      return frozenFixturesCheck(ROOT, base ?? "origin/main");
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
  "toolchain-sync-check": {
    summary: "Check toolchain pins agree across manifests, Mise, Nix and CI.",
    run() {
      const { failures, summary } = toolchainSync(ROOT);
      return report(failures, summary);
    },
  },
};
