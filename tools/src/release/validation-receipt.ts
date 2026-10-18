// Reuse successful validation only for an identical Git tree and workflow.
//
// Receipts contain metadata, never executable artifacts. API/network failure
// is a cache miss. A main push without matching PR/main evidence runs normal
// validation.
import { appendFileSync, mkdirSync, writeFileSync } from "node:fs";
import { join, parse } from "node:path";
import { inflateRawSync } from "node:zlib";
import { git } from "../lib/git.ts";
import { api as ghApi, type Api } from "../lib/github.ts";
import { deepEqual, isTable, table, type Table } from "../lib/value.ts";

export interface Receipt extends Table {
  readonly schema: number;
  readonly tree: string;
  readonly workflow: string;
}

export function identity(workflow: string, cwd = process.cwd()): Receipt {
  const tree = git(["rev-parse", "HEAD^{tree}"], cwd).trim();
  return { schema: 1, tree, workflow, coverage: process.env["VALIDATION_COVERAGE"] ?? "" };
}

export function artifactName(receipt: Receipt): string {
  return `validation-${parse(receipt.workflow).name}-${receipt.tree}`;
}

function trustedRun(run: Table, repository: string, workflow: string): boolean {
  return (
    run["conclusion"] === "success" &&
    (run["event"] === "pull_request" || run["event"] === "push") &&
    run["path"] === workflow &&
    table(run["head_repository"])["full_name"] === repository &&
    table(run["repository"])["full_name"] === repository
  );
}

class ZipError extends Error {}

interface ZipEntry {
  readonly name: string;
  readonly method: number;
  readonly crc: number;
  readonly compressedSize: number;
  readonly size: number;
  readonly offset: number;
}

const EOCD = 0x06054b50;
const CENTRAL = 0x02014b50;
const LOCAL = 0x04034b50;

/** The central directory: the archive's authoritative member list. */
function zipEntries(archive: Uint8Array): ZipEntry[] {
  const view = new DataView(archive.buffer, archive.byteOffset, archive.byteLength);
  let end = -1;
  for (let at = archive.length - 22; at >= Math.max(0, archive.length - 22 - 0xffff); at--) {
    if (view.getUint32(at, true) === EOCD) {
      end = at;
      break;
    }
  }
  if (end < 0) throw new ZipError("not a zip archive");
  const count = view.getUint16(end + 10, true);
  let at = view.getUint32(end + 16, true);
  const entries: ZipEntry[] = [];
  for (let index = 0; index < count; index++) {
    if (at + 46 > archive.length || view.getUint32(at, true) !== CENTRAL) {
      throw new ZipError("bad central directory");
    }
    const nameLength = view.getUint16(at + 28, true);
    const skip = view.getUint16(at + 30, true) + view.getUint16(at + 32, true);
    entries.push({
      name: new TextDecoder().decode(archive.subarray(at + 46, at + 46 + nameLength)),
      method: view.getUint16(at + 10, true),
      crc: view.getUint32(at + 16, true),
      compressedSize: view.getUint32(at + 20, true),
      size: view.getUint32(at + 24, true),
      offset: view.getUint32(at + 42, true),
    });
    at += 46 + nameLength + skip;
  }
  return entries;
}

function zipRead(archive: Uint8Array, entry: ZipEntry): Uint8Array {
  const view = new DataView(archive.buffer, archive.byteOffset, archive.byteLength);
  if (entry.offset + 30 > archive.length || view.getUint32(entry.offset, true) !== LOCAL) {
    throw new ZipError(`bad local header for ${entry.name}`);
  }
  const start =
    entry.offset +
    30 +
    view.getUint16(entry.offset + 26, true) +
    view.getUint16(entry.offset + 28, true);
  const raw = archive.subarray(start, start + entry.compressedSize);
  let data: Uint8Array;
  if (entry.method === 0) data = raw;
  else if (entry.method === 8) data = inflateRawSync(raw);
  else throw new ZipError(`unsupported compression ${entry.method} for ${entry.name}`);
  if (data.length !== entry.size || Bun.hash.crc32(data) !== entry.crc) {
    throw new ZipError(`bad CRC or size for ${entry.name}`);
  }
  return data;
}

/** The receipt in an artifact archive that holds exactly `receipt.json`, small. */
export function readReceipt(archive: Uint8Array): unknown {
  const entries = zipEntries(archive);
  const [entry] = entries;
  if (entries.length !== 1 || entry?.name !== "receipt.json") return undefined;
  if (entry.size > 4096) return undefined;
  return JSON.parse(new TextDecoder().decode(zipRead(archive, entry)));
}

function matches(artifact: Table, expected: Receipt, repository: string, api: Api): boolean {
  const size = artifact["size_in_bytes"] ?? 0;
  if (artifact["expired"] || typeof size !== "number" || size > 65536) return false;
  const runId = required(table(artifact["workflow_run"]), "id");
  const run = table(api.json(`repos/${repository}/actions/runs/${String(runId)}`));
  if (!trustedRun(run, repository, expected.workflow)) return false;
  // Artifact JSON is not provenance. Bind it to the source run's immutable
  // commit through GitHub's Git database, including the workflow implementation
  // in that tree. For PRs this deliberately requires the head tree to equal
  // the tested merge tree; a stale PR base is a safe cache miss.
  const commit = api.json(`repos/${repository}/git/commits/${String(required(run, "head_sha"))}`);
  if (table(table(commit)["tree"])["sha"] !== expected.tree) return false;
  const archive = api.bytes(
    `repos/${repository}/actions/artifacts/${String(required(artifact, "id"))}/zip`,
  );
  return deepEqual(readReceipt(archive), expected);
}

/** A missing field aborts the lookup, which the caller treats as a cache miss. */
function required(document: Table, key: string): unknown {
  if (!(key in document)) throw new Error(`missing ${key}`);
  return document[key];
}

/** The run that already validated `expected`, or undefined. */
export function findValidation(expected: Receipt, repository: string, api: Api = ghApi): unknown {
  const name = artifactName(expected);
  const result = table(api.json(`repos/${repository}/actions/artifacts?name=${name}&per_page=100`));
  const artifacts = required(result, "artifacts");
  if (!Array.isArray(artifacts)) throw new Error("artifacts is not a list");
  for (const artifact of artifacts) {
    if (!isTable(artifact)) throw new Error("artifact is not an object");
    if (matches(artifact, expected, repository, api)) {
      return required(table(artifact["workflow_run"]), "id");
    }
  }
  return undefined;
}

function lookup(expected: Receipt, repository: string): unknown {
  try {
    return findValidation(expected, repository);
  } catch (error) {
    console.log(`Validation receipt unavailable; running checks: ${String(error)}`);
    return undefined;
  }
}

export function run(mode: "check" | "write", workflow: string): void {
  const receipt = identity(workflow);
  const output = { name: artifactName(receipt), validated: "false" };
  if (mode === "write") {
    const destination = "target/validation-receipt";
    mkdirSync(destination, { recursive: true });
    writeFileSync(join(destination, "receipt.json"), `${JSON.stringify(receipt)}\n`);
  } else if (process.env["GITHUB_EVENT_NAME"] === "push") {
    const repository = process.env["GITHUB_REPOSITORY"];
    if (repository === undefined) throw new Error("GITHUB_REPOSITORY is unset");
    const run = lookup(receipt, repository);
    if (run !== undefined && run !== null) {
      console.log(`Identical tree already validated by workflow run ${JSON.stringify(run)}`);
      output.validated = "true";
    }
  }
  const path = process.env["GITHUB_OUTPUT"];
  if (path === undefined) throw new Error("GITHUB_OUTPUT is unset");
  appendFileSync(
    path,
    Object.entries(output)
      .map(([key, value]) => `${key}=${value}\n`)
      .join(""),
  );
}
