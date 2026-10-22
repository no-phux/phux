import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { formatOutputs, writeOutputs } from "../lib/actions.ts";
import { Failure, UsageError, type Command } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { inherit } from "../lib/process.ts";
import { ROOT } from "../lib/repo.ts";
import { Classifier, toOutputs, type Classification } from "./classify.ts";
import { classifyEvent, diffPaths } from "./detect.ts";
import { checkMerge } from "./merge-check.ts";
import { CI_ONLY, LANE_RECIPES, LANES, plan, planOutputs, reused, type Plan } from "./plan.ts";
import { alreadyValidated, artifactName, type Receipt } from "./receipt.ts";

const print = (result: Classification) => console.log(formatOutputs(toOutputs(result)));
const WORKFLOW = ".github/workflows/ci.yml";

/** Files that differ between `base` and the working tree, untracked included. */
function workingChanges(base: string): string[] {
  const tracked = git(["diff", "--no-renames", "--name-only", base, "--"]);
  const untracked = git(["ls-files", "--others", "--exclude-standard"]);
  return [...new Set(`${tracked}\n${untracked}`.split("\n").filter(Boolean))];
}

/** The env `just test` and `just e2e` read for a plan's test scope. */
function testEnv(p: Plan): Record<string, string> {
  return {
    PHUX_UNIT_MODE: p.unitMode,
    PHUX_UNIT_TARGETS: p.unitTargets,
    PHUX_NEXTEST_FILTERSET: p.testFilterset,
  };
}

function preflight(base: string, only: boolean): void {
  const result = checkMerge(base);
  if (result.resolvedMerge) {
    if (only) {
      throw new Failure(
        "The current merge is resolved and staged. Run just check to validate it before committing, then rerun --preflight after the merge commit.",
      );
    }
    console.log("resolved merge in progress; validating the working tree before the merge commit");
    return;
  }
  console.log(`committed branch merges cleanly with ${base} (${result.target.slice(0, 12)})`);
}

export const commands: Record<string, Command> = {
  classify: {
    summary: "Route changed paths (one per line on stdin, or a git range) to CI surfaces.",
    usage: "[--since REV [--until REV]]",
    run(args) {
      const { values } = parseArgs({
        args,
        options: { since: { type: "string" }, until: { type: "string", default: "HEAD" } },
      });
      const paths =
        values.since === undefined
          ? readFileSync(0, "utf8").split("\n")
          : diffPaths(values.since, values.until);
      print(new Classifier().classify(paths));
      return 0;
    },
  },
  "detect-changes": {
    summary: "Route the current GitHub Actions event (GITHUB_EVENT_NAME/PATH) to CI surfaces.",
    run() {
      print(
        classifyEvent(process.env["GITHUB_EVENT_NAME"] ?? "", process.env["GITHUB_EVENT_PATH"]),
      );
      return 0;
    },
  },
  "ci-plan": {
    summary: "Plan ci.yml for the current event: its lanes, test scope, and receipt.",
    run() {
      const event = process.env["GITHUB_EVENT_NAME"] ?? "";
      const repository = process.env["GITHUB_REPOSITORY"] ?? "";
      const c = classifyEvent(event, process.env["GITHUB_EVENT_PATH"]);
      const p = plan(c, event === "push" ? "push" : "pull_request");
      const tree = git(["rev-parse", "HEAD^{tree}"]).trim();
      const receipt: Receipt = { schema: 2, tree, workflow: WORKFLOW, coverage: p.coverage };
      const validated =
        event === "push" && repository !== "" && alreadyValidated(receipt, repository);
      const run = validated ? reused(p) : p;
      const outputs = planOutputs(run, c);
      console.error(
        `${validated ? "Tree already validated; lanes: " : "Lanes: "}${[...run.lanes].join(" ")}`,
      );
      writeOutputs({
        ...outputs,
        validated,
        receipt: JSON.stringify(receipt),
        receipt_name: artifactName(receipt),
      });
      return 0;
    },
  },
  check: {
    summary: "Run the ci.yml lanes this checkout's changes need, through the same recipes.",
    usage: "[--base REV] [--dry-run | --preflight]",
    help: "REV defaults to origin/main (its merge base with HEAD for routing). Checks committed mergeability before running lanes; uncommitted changes count for routing. Fetch origin first for a current integration check. --preflight checks mergeability only; --dry-run prints the plan only.",
    run(args) {
      const { values } = parseArgs({
        args,
        options: {
          base: { type: "string" },
          "dry-run": { type: "boolean", default: false },
          preflight: { type: "boolean", default: false },
        },
      });
      if (values["dry-run"] && values.preflight) {
        throw new UsageError("choose --dry-run or --preflight");
      }
      if (!values["dry-run"]) {
        preflight(values.base ?? "origin/main", values.preflight);
        if (values.preflight) return 0;
      }
      const base = values.base ?? git(["merge-base", "origin/main", "HEAD"]).trim();
      const p = plan(new Classifier().classify(workingChanges(base)), "pull_request");
      const lanes = LANES.filter((lane) => p.lanes.has(lane));
      const local = lanes.filter((lane) => !CI_ONLY.has(lane));
      const skipped = lanes.filter((lane) => CI_ONLY.has(lane));
      console.log(`changes since ${base.slice(0, 12)} need: ${lanes.join(", ")}`);
      if (skipped.length > 0) console.log(`CI only, not run here: ${skipped.join(", ")}`);
      if (values["dry-run"]) return 0;
      const env = { ...process.env, ...testEnv(p) };
      for (const lane of local) {
        for (const recipe of LANE_RECIPES[lane]) {
          console.log(`\n== ${lane}: just ${recipe.join(" ")}`);
          if (inherit(["just", ...recipe], { cwd: ROOT, env }) !== 0) {
            throw new Failure(`lane ${lane} failed at just ${recipe.join(" ")}`);
          }
        }
      }
      console.log(`\nall ${local.length} local lanes passed`);
      return 0;
    },
  },
};
