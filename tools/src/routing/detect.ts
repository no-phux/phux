// Resolve a GitHub Actions event to its changed paths.
//
// The caller checks out the event commit with fetch-depth 0; no API token or
// fetch is needed. A merge group diffs its queue commit against its base.
// Missing or invalid history, schedule and dispatch request every surface. A release-please version-field diff requests none.
import { readFileSync } from "node:fs";
import { describe, Failure } from "../lib/command.ts";
import { git, show } from "../lib/git.ts";
import { ROOT } from "../lib/repo.ts";
import { isTable, str, table } from "../lib/value.ts";
import { Classifier, type Classification } from "./classify.ts";
import { releaseMetadataOnly } from "./release-metadata.ts";

export type Detection =
  | { kind: "paths"; paths: string[]; docsCatalog: boolean }
  | { kind: "release-metadata" }
  | { kind: "full"; reason: string };

/** A flat page: `docs/<name>.md`, the set the website's `discover()` rejects. */
const FLAT_DOC = /^docs\/[^/]+\.md$/;

/** True when the range adds, renames, or deletes a top-level `docs/*.md`. */
export function docsCatalogChanged(base: string, head: string, cwd = ROOT): boolean {
  // The catalog only cares about path membership. A rename appears as a
  // delete plus an add without making Git compare the contents.
  const raw = git(
    ["diff", "--name-only", "--no-renames", "--diff-filter=AD", "-z", base, head, "--", "docs/"],
    cwd,
  );
  return raw.split("\0").some((path) => FLAT_DOC.test(path));
}

const NO_LANES: Classification = {
  needed: new Set(),
  runeDirect: false,
  docsOnly: false,
  perfIcountNeeded: false,
  mobileFfiNeeded: false,
  uniffiNeeded: false,
  contentNeeded: false,
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

/** The same base and head `ci-plan` diffs. */
export function plannerRange(eventName: string, event: unknown, cwd = ROOT): [string, string] {
  return eventRange(eventName, event, cwd);
}

function eventRange(name: string, event: unknown, cwd: string): [string, string] {
  if (!isTable(event)) throw new Failure("event payload must be an object");
  if (name === "push") return [commit(event["before"], cwd), commit(event["after"], cwd)];
  if (name === "merge_group") {
    // The queue's commit over current main: every change ahead of it lands.
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
  let base: string, head: string, paths: string[], docsCatalog: boolean;
  try {
    if (eventPath === undefined) throw new Failure("GITHUB_EVENT_PATH is unset");
    [base, head] = eventRange(eventName, JSON.parse(readFileSync(eventPath, "utf8")), cwd);
    paths = diffPaths(base, head, cwd);
    docsCatalog = paths.some((path) => FLAT_DOC.test(path)) && docsCatalogChanged(base, head, cwd);
  } catch (error) {
    return { kind: "full", reason: describe(error) };
  }
  const readPair = (path: string) => [show(base, path, cwd), show(head, path, cwd)] as const;
  return releaseMetadataOnly(paths, readPair)
    ? { kind: "release-metadata" }
    : { kind: "paths", paths, docsCatalog };
}

/** Metadata stays compile-free in ci, but releases need full native proof on their own SHA. */
export function classifyDetection(
  detection: Detection,
  metadata: "skip" | "qualify-rune" = "skip",
): Classification {
  if (detection.kind === "paths") return new Classifier().classify(detection.paths);
  if (detection.kind === "full") {
    console.error(`Full validation: ${detection.reason}`);
    return new Classifier().classify([]);
  }
  if (metadata === "qualify-rune") {
    console.error("Release metadata: qualify both Rune Apple legs on this release tree");
    return { ...NO_LANES, needed: new Set(["rune"]), runeDirect: true };
  }
  console.error("Release metadata only; ci compile lanes skipped");
  return NO_LANES;
}

/** Classify the complete event range, not only the head commit's parent diff. */
export function classifyEvent(
  eventName: string,
  eventPath: string | undefined,
  metadata: "skip" | "qualify-rune" = "skip",
): Classification {
  return classifyDetection(detect(eventName, eventPath), metadata);
}
