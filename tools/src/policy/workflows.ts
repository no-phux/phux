// Read-only views of .github for the workflow policy tests.
import { readFileSync, readdirSync } from "node:fs";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";
import { list, str, table, type Table } from "../lib/value.ts";

export const WORKFLOWS = repoPath(".github/workflows");

/** Every workflow file name, e.g. `ci.yml`. */
export function workflowNames(): string[] {
  return readdirSync(WORKFLOWS)
    .filter((name) => name.endsWith(".yml"))
    .sort();
}

export function workflowText(name: string): string {
  return readFileSync(join(WORKFLOWS, name), "utf8");
}

export function workflow(name: string): Table {
  return table(Bun.YAML.parse(workflowText(name)));
}

export function actionText(name: string): string {
  return readFileSync(repoPath(`.github/actions/${name}/action.yml`), "utf8");
}

export function action(name: string): Table {
  return table(Bun.YAML.parse(actionText(name)));
}

export function jobs(document: Table): Record<string, Table> {
  return Object.fromEntries(
    Object.entries(table(document["jobs"])).map(([name, job]) => [name, table(job)]),
  );
}

export function job(document: Table, name: string): Table {
  const found = jobs(document)[name];
  if (found === undefined) throw new Error(`no job ${name}`);
  return found;
}

/** A job's steps, or a composite action's (`runs.steps`). */
export function steps(owner: Table): Table[] {
  const direct = owner["steps"] ?? table(owner["runs"])["steps"];
  return list(direct).map(table);
}

export function uses(step: Table): string {
  return str(step["uses"]) ?? "";
}

export function run(step: Table): string {
  return str(step["run"]) ?? "";
}

export function withInput(step: Table, name: string): unknown {
  return table(step["with"])[name];
}

export function needs(job: Table): string[] {
  const value = job["needs"];
  return typeof value === "string" ? [value] : list(value).map(String);
}
