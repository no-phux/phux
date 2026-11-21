import { readFileSync } from "node:fs";
import { parseArgs } from "node:util";
import { formatOutputs, writeOutputs } from "../lib/actions.ts";
import { describe, Failure, UsageError, type Command } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { inherit } from "../lib/process.ts";
import { ROOT } from "../lib/repo.ts";
import { Classifier, toOutputs, type Classification } from "./classify.ts";
import { classifyDetection, classifyEvent, detect, diffPaths, type Detection } from "./detect.ts";
import { checkMerge } from "./merge-check.ts";
import {
  CI_ONLY,
  eventHasLabel,
  LANE_RECIPES,
  LANES,
  PERF_BASELINE_LABEL,
  plan,
  planOutputs,
  releasePullRequest,
  reused,
  triggerFor,
  type PathScope,
  type Plan,
} from "./plan.ts";
import { alreadyValidated, artifactName, type Receipt } from "./receipt.ts";
import { reusedMacos, runeMacosPlan } from "./rune-macos.ts";

const print = (result: Classification) => console.log(formatOutputs(toOutputs(result)));
const WORKFLOW = ".github/workflows/ci.yml";
const RUNE_MACOS = ".github/workflows/rune-macos.yml";

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

function perfBaselineLabel(event: string, eventPath: string | undefined): boolean {
  if (event !== "pull_request" || eventPath === undefined) return false;
  try {
    return eventHasLabel(JSON.parse(readFileSync(eventPath, "utf8")), PERF_BASELINE_LABEL);
  } catch (error) {
    console.error(`perf-baseline label unreadable; running icount: ${describe(error)}`);
    return true;
  }
}

function releaseHead(eventPath: string | undefined, repository: string): boolean {
  try {
    return releasePullRequest(JSON.parse(readFileSync(eventPath ?? "", "utf8")), repository);
  } catch (error) {
    console.error(`pull request head unreadable; fast path: ${describe(error)}`);
    return false;
  }
}

function pathScope(detection: Detection): PathScope {
  if (detection.kind === "paths") return { kind: "diff", paths: detection.paths };
  return { kind: detection.kind === "full" ? "full" : "none" };
}

// Full history fails closed and runs the catalog job. Release metadata
// has no flat docs page, so the job stays off.
function docsCatalogSelected(detection: Detection): boolean {
  return detection.kind === "paths" ? detection.docsCatalog : detection.kind !== "release-metadata";
}

function validatedPush(event: string, repository: string, receipt: Receipt): boolean {
  return (
    event === "push" &&
    repository !== "" &&
    receipt.coverage !== "" &&
    alreadyValidated(receipt, repository)
  );
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
      const eventPath = process.env["GITHUB_EVENT_PATH"];
      const repository = process.env["GITHUB_REPOSITORY"] ?? "";
      const detection = detect(event, eventPath);
      // Pull requests gate on their path-scoped Linux lanes only. Apple,
      // Linux packaging and the mandatory lanes qualify on the main push
      // (docs/ci.md#native-apple-qualification). A release bot's PR keeps
      // full qualification. A merge group is main's tree before it lands:
      // main policy.
      const release = event === "pull_request" && releaseHead(eventPath, repository);
      const c = classifyDetection(detection, release ? "qualify-rune" : "skip");
      const scope = pathScope(detection);
      const p = plan(c, triggerFor(event), {
        scope,
        perfBaseline: perfBaselineLabel(event, eventPath),
        pullRequestFast: !release,
        runeQualify: event !== "pull_request",
        docsCatalog: docsCatalogSelected(detection),
      });
      const tree = git(["rev-parse", "HEAD^{tree}"]).trim();
      const receipt: Receipt = { schema: 2, tree, workflow: WORKFLOW, coverage: p.coverage };
      const validated = validatedPush(event, repository, receipt);
      const run = validated ? reused(p) : p;
      const outputs = planOutputs(run, c);
      console.error(
        `${validated ? "Tree already validated; lanes: " : "Lanes: "}${[...run.lanes].join(" ")}`,
      );
      writeOutputs({
        ...outputs,
        release_qualify: release,
        validated,
        receipt: JSON.stringify(receipt),
        receipt_name: artifactName(receipt),
      });
      return 0;
    },
  },
  "rune-macos-plan": {
    summary: "Plan rune-macos.yml: macOS and iOS legs, and the receipt of what ran.",
    run() {
      const event = process.env["GITHUB_EVENT_NAME"] ?? "";
      const repository = process.env["GITHUB_REPOSITORY"] ?? "";
      const c = classifyEvent(event, process.env["GITHUB_EVENT_PATH"], "qualify-rune");
      // Only ci's required release-PR child qualifies a PR; the standalone PR
      // check stays cheap. The caller supplies the one policy decision.
      const pullRequest = event === "pull_request" && process.env["PHUX_RUNE_QUALIFY"] !== "true";
      const planned = runeMacosPlan(c, pullRequest ? "pull_request" : "push");
      const tree = git(["rev-parse", "HEAD^{tree}"]).trim();
      const receipt: Receipt = {
        schema: 2,
        tree,
        workflow: RUNE_MACOS,
        coverage: planned.coverage,
      };
      const validated = validatedPush(event, repository, receipt);
      const run = validated ? reusedMacos(planned) : planned;
      console.error(
        `${validated ? "Tree already validated; " : ""}rune-macos: ${run.macos ? "macos" : "skip"}${run.ios ? " ios" : ""}`,
      );
      writeOutputs({
        macos: run.macos,
        ios: run.ios,
        coverage: planned.coverage,
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
      const paths = workingChanges(base);
      const p = plan(new Classifier().classify(paths), "pull_request", {
        scope: { kind: "diff", paths },
      });
      const lanes = LANES.filter((lane) => p.lanes.has(lane));
      const local = lanes.filter((lane) => !CI_ONLY.has(lane));
      const skipped = [
        ...lanes.filter((lane) => CI_ONLY.has(lane)),
        ...(p.lanes.has("rune") ? ["rune macOS and iOS simulator (rune-macos.yml)"] : []),
      ];
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
