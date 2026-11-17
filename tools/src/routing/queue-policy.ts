// PR fast paths are safe only when the target branch actually enforces a queue.
// This read is deliberately not a variable, queue-entry or source-branch claim.
import { readFileSync } from "node:fs";
import { describe } from "../lib/command.ts";
import { api, type Api } from "../lib/github.ts";
import { isTable, table, type Table } from "../lib/value.ts";

function integerBetween(value: unknown, min: number, max: number): value is number {
  return typeof value === "number" && Number.isInteger(value) && value >= min && value <= max;
}

// GitHub's repository-rule-merge-queue schema supplies these bounds:
// https://github.com/github/rest-api-description/blob/main/descriptions/api.github.com/api.github.com.json
function validQueueLimits(parameters: Table): boolean {
  const min = parameters["min_entries_to_merge"];
  const max = parameters["max_entries_to_merge"];
  return (
    integerBetween(parameters["check_response_timeout_minutes"], 1, 360) &&
    integerBetween(parameters["max_entries_to_build"], 0, 100) &&
    integerBetween(min, 0, 100) &&
    integerBetween(max, 0, 100) &&
    min <= max &&
    integerBetween(parameters["min_entries_to_merge_wait_minutes"], 0, 360)
  );
}

function validQueueParameters(value: unknown): boolean {
  if (!isTable(value)) return false;
  const grouping = value["grouping_strategy"];
  if (grouping !== "ALLGREEN" && grouping !== "HEADGREEN") return false;
  const method = value["merge_method"];
  if (method !== "MERGE" && method !== "SQUASH" && method !== "REBASE") return false;
  return validQueueLimits(value);
}

function validBranchRule(rule: unknown): boolean {
  if (!isTable(rule) || typeof rule["type"] !== "string") return false;
  return rule["type"] !== "merge_queue" || validQueueParameters(rule["parameters"]);
}

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
    if (!rules.every(validBranchRule)) return false;
    return rules.some((rule: unknown) => table(rule)["type"] === "merge_queue");
  } catch (error) {
    console.error(`Merge queue enforcement unverified; qualifying PR: ${describe(error)}`);
    return false;
  }
}
