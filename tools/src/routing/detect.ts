// Resolve a GitHub Actions event to its changed paths.
//
// The caller checks out the event commit with fetch-depth 0; no API token or
// fetch is needed. Missing or invalid history, schedule and dispatch request
// every surface. A release-please version-field diff requests none.
import { readFileSync } from "node:fs";
import { git, show } from "../lib/git.ts";
import { ROOT } from "../lib/repo.ts";
import { isTable, str, table } from "../lib/value.ts";
import { releaseMetadataOnly } from "./release-metadata.ts";

export type Detection =
  | { kind: "paths"; paths: string[] }
  | { kind: "release-metadata" }
  | { kind: "full"; reason: string };

function commit(value: unknown, cwd: string): string {
  const sha = str(value);
  if (sha === undefined || !/^[0-9a-fA-F]{40,64}$/.test(sha)) {
    throw new Error("missing or malformed event commit");
  }
  git(["cat-file", "-e", `${sha}^{commit}`], cwd);
  return sha;
}

function eventRange(name: string, event: unknown, cwd: string): [string, string] {
  if (!isTable(event)) throw new Error("event payload must be an object");
  switch (name) {
    case "push":
      return [commit(event["before"], cwd), commit(event["after"], cwd)];
    case "pull_request": {
      const pr = table(event["pull_request"]);
      const base = commit(table(pr["base"])["sha"], cwd);
      const head = commit(table(pr["head"])["sha"], cwd);
      return [git(["merge-base", base, head], cwd).trim(), head];
    }
    case "merge_group": {
      const group = table(event["merge_group"]);
      return [commit(group["base_sha"], cwd), commit(group["head_sha"], cwd)];
    }
    default:
      throw new Error("event requests full validation");
  }
}

/** Paths changed between two revisions, both sides of a rename included. */
export function diffPaths(base: string, head: string, cwd = ROOT): string[] {
  const names = git(["diff", "--no-renames", "--name-only", "-z", base, head, "--"], cwd);
  const paths = names.split("\0").filter((path) => path !== "");
  if (paths.some((path) => /[\r\n]/.test(path))) {
    throw new Error("path cannot be represented as an Actions output");
  }
  return paths;
}

export function detect(eventName: string, eventPath: string | undefined, cwd = ROOT): Detection {
  let range: [string, string];
  let paths: string[];
  try {
    if (eventPath === undefined) throw new Error("GITHUB_EVENT_PATH is unset");
    const event: unknown = JSON.parse(readFileSync(eventPath, "utf8"));
    range = eventRange(eventName, event, cwd);
    paths = diffPaths(...range, cwd);
  } catch (error) {
    return { kind: "full", reason: error instanceof Error ? error.message : String(error) };
  }
  const [base, head] = range;
  const readPair = (path: string) => [show(base, path, cwd), show(head, path, cwd)] as const;
  if (releaseMetadataOnly(paths, readPair)) return { kind: "release-metadata" };
  return { kind: "paths", paths };
}
