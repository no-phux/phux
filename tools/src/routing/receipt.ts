// A main push skips ci when a pull request already validated the identical
// tree with the same lane coverage. A pull request's aggregate uploads the
// receipt; the push's plan looks it up. Receipts are metadata, never
// executable artifacts, and any API failure is a cache miss.
import { lstatSync, mkdtempSync, readdirSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { join, parse } from "node:path";
import { describe } from "../lib/command.ts";
import { api, gh } from "../lib/github.ts";
import { deepEqual, list, table, type Table } from "../lib/value.ts";

export interface Receipt extends Table {
  readonly schema: number;
  readonly tree: string;
  readonly workflow: string;
  /** The pull-request lane set that ran; a push reuses only an equal one. */
  readonly coverage: string;
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
    run["event"] === "pull_request" &&
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

/** Whether a pull request already validated `receipt`; failures are a miss. */
export function alreadyValidated(receipt: Receipt, repository: string): boolean {
  try {
    const run = findValidation(receipt, repository);
    if (run !== undefined) console.error(`Identical tree already validated by workflow run ${run}`);
    return run !== undefined;
  } catch (error) {
    console.error(`Validation receipt unavailable; running checks: ${describe(error)}`);
    return false;
  }
}
