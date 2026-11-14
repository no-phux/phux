// PR fast paths are safe only when the target branch actually enforces a queue.
// This read is deliberately not a variable, queue-entry or source-branch claim.
import { readFileSync } from "node:fs";
import { describe } from "../lib/command.ts";
import { api, type Api } from "../lib/github.ts";
import { isTable, table } from "../lib/value.ts";

export function mergeQueueVerified(
  repository: string,
  eventPath: string | undefined,
  source: Pick<Api, "json"> = api,
): boolean {
  try {
    const event: unknown = JSON.parse(readFileSync(eventPath ?? "", "utf8"));
    const base = table(table(table(event)["pull_request"])["base"]);
    if (table(base["repo"])["full_name"] !== repository) return false;
    const branch = base["ref"];
    if (!/^[^/]+\/[^/]+$/.test(repository) || typeof branch !== "string" || branch === "")
      return false;
    const repo = repository.split("/").map(encodeURIComponent).join("/");
    const rules = source.json(`repos/${repo}/rules/branches/${encodeURIComponent(branch)}`);
    if (!Array.isArray(rules)) return false;
    if (!rules.every((rule: unknown) => isTable(rule) && typeof rule["type"] === "string"))
      return false;
    return rules.some((rule: unknown) => table(rule)["type"] === "merge_queue");
  } catch (error) {
    console.error(`Merge queue enforcement unverified; qualifying PR: ${describe(error)}`);
    return false;
  }
}
