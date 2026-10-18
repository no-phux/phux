// Read-only views of .github for the workflow policy tests.
import { readFileSync, readdirSync } from "node:fs";
import { repoPath } from "../lib/repo.ts";
import { list, str, table, type Table } from "../lib/value.ts";

/** Every workflow file name, e.g. `ci.yml`. */
export function workflowNames(): string[] {
  return readdirSync(repoPath(".github/workflows"))
    .filter((name) => name.endsWith(".yml"))
    .sort();
}

export function workflowText(name: string): string {
  return readFileSync(repoPath(`.github/workflows/${name}`), "utf8");
}

export function actionText(name: string): string {
  return readFileSync(repoPath(`.github/actions/${name}/action.yml`), "utf8");
}

export const workflow = (name: string): Table => table(Bun.YAML.parse(workflowText(name)));
export const action = (name: string): Table => table(Bun.YAML.parse(actionText(name)));

export function jobs(document: Table): Record<string, Table> {
  return Object.fromEntries(
    Object.entries(table(document["jobs"])).map(([name, job]) => [name, table(job)]),
  );
}

/** Every job of every workflow, labelled `file job`. */
export function everyJob(): [string, Table][] {
  return workflowNames().flatMap((name) =>
    Object.entries(jobs(workflow(name))).map(([id, lane]): [string, Table] => [
      `${name} ${id}`,
      lane,
    ]),
  );
}

export function job(document: Table, name: string): Table {
  const found = jobs(document)[name];
  if (found === undefined) throw new Error(`no job ${name}`);
  return found;
}

/** A job's steps, or a composite action's (`runs.steps`). */
export function steps(owner: Table): Table[] {
  return list(owner["steps"] ?? table(owner["runs"])["steps"]).map(table);
}

export const uses = (step: Table): string => str(step["uses"]) ?? "";
export const run = (step: Table): string => str(step["run"]) ?? "";
export const withInput = (step: Table, name: string): unknown => table(step["with"])[name];

export function needs(job: Table): string[] {
  const value = job["needs"];
  return typeof value === "string" ? [value] : list(value).map(String);
}
