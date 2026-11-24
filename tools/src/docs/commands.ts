import { existsSync } from "node:fs";
import { parseArgs } from "node:util";
import { Failure, UsageError, type Command } from "../lib/command.ts";
import { status } from "../lib/output.ts";
import { ROOT } from "../lib/repo.ts";
import { GATES, checkDocs, format, type Gate } from "./check.ts";
import { checkLlms } from "./llms.ts";

const ALL = [...GATES, "llms"] as const;
const DRIFT = "llms.txt: docs/llms.txt drifted from docs/building.md";

function isGate(name: string): name is (typeof ALL)[number] {
  return (ALL as readonly string[]).includes(name);
}

/** The builder index drift check: prints each problem, then fails. */
function llms(root: string): void {
  const problems = checkLlms(root);
  for (const problem of problems) status("error", `llms.txt: ${problem}`);
  if (problems.length > 0) throw new Failure(DRIFT);
}

function gate(only: string | undefined): Gate | "llms" | undefined {
  if (only === undefined || isGate(only)) return only;
  throw new UsageError(`--only=${only} is not a gate (${ALL.join(" ")})`);
}

/** The Markdown gates: prints each violation, then fails; returns the file count. */
function markdown(root: string, only: Gate | undefined): number {
  const report = checkDocs(root, only);
  for (const violation of report.violations) status("error", format(root, violation));
  if (report.violations.length > 0) {
    throw new Failure(`docs: ${report.violations.length} problem(s) in ${report.files} files`);
  }
  return report.files;
}

export function docsCheck(args: string[], root = ROOT): number {
  const { values } = parseArgs({
    args,
    options: { list: { type: "boolean", default: false }, only: { type: "string" } },
  });
  if (values.list) {
    console.log(ALL.join("\n"));
    return 0;
  }
  const only = gate(values.only);
  if (only === "llms") {
    llms(root);
    status("ok", "llms.txt: docs/llms.txt matches the Build on phux entry points");
    return 0;
  }
  const files = markdown(root, only);
  // The builder index is part of the full gate; a tree without the protocol
  // page (a fixture) has no builder index to check.
  if (only === undefined && existsSync(`${root}/docs/protocol.md`)) llms(root);
  status("ok", `docs: ${files} files, no dead links or stale commands`);
  return 0;
}

export const commands: Record<string, Command> = {
  "docs-check": {
    summary: "Dead links and anchors, stale commands, the crate map, doc citations, nav, llms.txt.",
    usage: "[--list] [--only GATE]",
    help: [
      "--list        print the gates and exit",
      `--only GATE   run one gate (${ALL.join(", ")})`,
    ].join("\n"),
    run: (args) => docsCheck(args),
  },
};
