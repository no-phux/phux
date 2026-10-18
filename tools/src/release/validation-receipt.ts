// Reuse successful validation only for an identical Git tree and workflow.
//
// Receipts contain metadata, never executable artifacts. API/network failure
// is a cache miss. A main push without matching PR/main evidence runs normal
// validation.
import { lstatSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, parse } from "node:path";
import { writeOutputs } from "../lib/actions.ts";
import { describe, Failure } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { api, gh } from "../lib/github.ts";
import { deepEqual, list, table, type Table } from "../lib/value.ts";

export interface Receipt extends Table {
  readonly schema: number;
  readonly tree: string;
  readonly workflow: string;
}

/** GitHub as the lookup reads it; tests stand in a fake. */
export interface Source {
  json(path: string): unknown;
  /** Extract run `runId`'s artifact `name` into the empty directory `dir`. */
  download(runId: string, name: string, dir: string): void;
}

function ghSource(repository: string): Source {
  return {
    json: (path) => api.json(path),
    download(runId, name, dir) {
      gh(["run", "download", runId, "--repo", repository, "--name", name, "--dir", dir], 30_000);
    },
  };
}

export function artifactName(receipt: Receipt): string {
  return `validation-${parse(receipt.workflow).name}-${receipt.tree}`;
}

function trustedRun(run: Table, repository: string, workflow: string): boolean {
  return (
    run["conclusion"] === "success" &&
    (run["event"] === "pull_request" || run["event"] === "push") &&
    run["path"] === workflow &&
    typeof run["head_sha"] === "string" &&
    table(run["head_repository"])["full_name"] === repository &&
    table(run["repository"])["full_name"] === repository
  );
}

/** The receipt in the run's artifact, when that holds exactly a small `receipt.json`. */
function downloadReceipt(source: Source, runId: string, name: string): unknown {
  const dir = mkdtempSync(join(tmpdir(), "validation-receipt-"));
  try {
    source.download(runId, name, dir);
    const path = join(dir, "receipt.json");
    if (readdirSync(dir).join("/") !== "receipt.json") return undefined;
    const stat = lstatSync(path);
    return stat.isFile() && stat.size <= 4096 ? JSON.parse(readFileSync(path, "utf8")) : undefined;
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
}

function matches(artifact: Table, expected: Receipt, repository: string, source: Source): boolean {
  const size = artifact["size_in_bytes"] ?? 0;
  const runId = table(artifact["workflow_run"])["id"];
  if (artifact["expired"] || typeof size !== "number" || size > 65536) return false;
  if (typeof runId !== "number") return false;
  const run = table(source.json(`repos/${repository}/actions/runs/${runId}`));
  if (!trustedRun(run, repository, expected.workflow)) return false;
  // Artifact JSON is not provenance. Bind it to the source run's immutable
  // commit through GitHub's Git database, including the workflow implementation
  // in that tree. For PRs this deliberately requires the head tree to equal
  // the tested merge tree; a stale PR base is a safe cache miss.
  const commit = source.json(`repos/${repository}/git/commits/${String(run["head_sha"])}`);
  if (table(table(commit)["tree"])["sha"] !== expected.tree) return false;
  return deepEqual(downloadReceipt(source, String(runId), artifactName(expected)), expected);
}

/** The id of the run that already validated `expected`, or undefined. */
export function findValidation(
  expected: Receipt,
  repository: string,
  source = ghSource(repository),
): number | undefined {
  const path = `repos/${repository}/actions/artifacts?name=${artifactName(expected)}&per_page=100`;
  const artifacts = list(table(source.json(path))["artifacts"]).map(table);
  const found = artifacts.find((artifact) => matches(artifact, expected, repository, source));
  return found === undefined ? undefined : Number(table(found["workflow_run"])["id"]);
}

function check(receipt: Receipt): boolean {
  if (process.env["GITHUB_EVENT_NAME"] !== "push") return false;
  const repository = process.env["GITHUB_REPOSITORY"];
  if (!repository) throw new Failure("GITHUB_REPOSITORY is unset");
  try {
    const run = findValidation(receipt, repository);
    if (run !== undefined) console.log(`Identical tree already validated by workflow run ${run}`);
    return run !== undefined;
  } catch (error) {
    console.log(`Validation receipt unavailable; running checks: ${describe(error)}`);
    return false;
  }
}

export async function run(mode: "check" | "write", workflow: string): Promise<void> {
  const tree = git(["rev-parse", "HEAD^{tree}"], process.cwd()).trim();
  const coverage = process.env["VALIDATION_COVERAGE"] ?? "";
  const receipt: Receipt = { schema: 1, tree, workflow, coverage };
  if (mode === "write") {
    await Bun.write("target/validation-receipt/receipt.json", `${JSON.stringify(receipt)}\n`);
  }
  writeOutputs({ name: artifactName(receipt), validated: mode === "check" && check(receipt) });
}
