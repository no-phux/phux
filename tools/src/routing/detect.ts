// Resolve a GitHub Actions event to its changed paths.
//
// The caller checks out the event commit with fetch-depth 0; no API token or
// fetch is needed. Missing or invalid history, schedule and dispatch request
// every surface. A release-please version-field diff requests none.
import { readFileSync } from "node:fs";
import { describe, Failure } from "../lib/command.ts";
import { git, show } from "../lib/git.ts";
import { ROOT } from "../lib/repo.ts";
import { isTable, str, table } from "../lib/value.ts";
import { Classifier, type Classification } from "./classify.ts";
import { releaseMetadataOnly } from "./release-metadata.ts";

export type Detection =
  | { kind: "paths"; paths: string[] }
  | { kind: "release-metadata" }
  | { kind: "full"; reason: string };

const NO_LANES: Classification = {
  needed: new Set(),
  docsOnly: false,
  perfIcountNeeded: false,
  mobileFfiNeeded: false,
  uniffiNeeded: false,
  testFilterset: "",
  unitMode: "skip",
  unitTargets: "",
  e2eNeeded: false,
};

function commit(value: unknown, cwd: string): string {
  const sha = str(value) ?? "";
  if (!/^[0-9a-fA-F]{40,64}$/.test(sha)) throw new Failure("missing or malformed event commit");
  git(["cat-file", "-e", `${sha}^{commit}`], cwd);
  return sha;
}

function eventRange(name: string, event: unknown, cwd: string): [string, string] {
  if (!isTable(event)) throw new Failure("event payload must be an object");
  if (name === "push") return [commit(event["before"], cwd), commit(event["after"], cwd)];
  if (name === "merge_group") {
    const group = table(event["merge_group"]);
    return [commit(group["base_sha"], cwd), commit(group["head_sha"], cwd)];
  }
  if (name !== "pull_request") throw new Failure("event requests full validation");
  const pr = table(event["pull_request"]);
  const base = commit(table(pr["base"])["sha"], cwd);
  const head = commit(table(pr["head"])["sha"], cwd);
  return [git(["merge-base", base, head], cwd).trim(), head];
}

/** Paths changed between two revisions, both sides of a rename included. */
export function diffPaths(base: string, head: string, cwd = ROOT): string[] {
  const names = git(["diff", "--no-renames", "--name-only", "-z", base, head, "--"], cwd);
  const paths = names.split("\0").filter((path) => path !== "");
  if (paths.some((path) => /[\r\n]/.test(path))) {
    throw new Failure("path cannot be represented as an Actions output");
  }
  return paths;
}

export function detect(eventName: string, eventPath: string | undefined, cwd = ROOT): Detection {
  let base: string, head: string, paths: string[];
  try {
    if (eventPath === undefined) throw new Failure("GITHUB_EVENT_PATH is unset");
    [base, head] = eventRange(eventName, JSON.parse(readFileSync(eventPath, "utf8")), cwd);
    paths = diffPaths(base, head, cwd);
  } catch (error) {
    return { kind: "full", reason: describe(error) };
  }
  const readPair = (path: string) => [show(base, path, cwd), show(head, path, cwd)] as const;
  return releaseMetadataOnly(paths, readPair)
    ? { kind: "release-metadata" }
    : { kind: "paths", paths };
}

/** Classify the current event, logging why it skips or widens routing. */
export function classifyEvent(eventName: string, eventPath: string | undefined): Classification {
  const detection = detect(eventName, eventPath);
  if (detection.kind === "paths") return new Classifier().classify(detection.paths);
  if (detection.kind === "full") {
    console.error(`Full validation: ${detection.reason}`);
    return new Classifier().classify([]);
  }
  console.error("Release metadata only; compile lanes skipped");
  return NO_LANES;
}
