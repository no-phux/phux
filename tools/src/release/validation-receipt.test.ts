// Validation reuse must fail closed for mismatched trees and run provenance.
import { beforeEach, describe, expect, test } from "bun:test";
import { deflateRawSync } from "node:zlib";
import type { Api } from "../lib/github.ts";
import type { Table } from "../lib/value.ts";
import { artifactName, findValidation, readReceipt, type Receipt } from "./validation-receipt.ts";

/** A minimal deflate zip, as GitHub serves an artifact. */
function zip(files: Record<string, string>): Uint8Array {
  const encoder = new TextEncoder();
  const locals: Uint8Array[] = [];
  const centrals: Uint8Array[] = [];
  let offset = 0;
  for (const [name, text] of Object.entries(files)) {
    const [nameBytes, data] = [encoder.encode(name), encoder.encode(text)];
    const packed = deflateRawSync(data);
    const crc = Bun.hash.crc32(data);
    const local = new Uint8Array(30 + nameBytes.length + packed.length);
    const lv = new DataView(local.buffer);
    lv.setUint32(0, 0x04034b50, true);
    lv.setUint16(8, 8, true);
    lv.setUint32(14, crc, true);
    lv.setUint32(18, packed.length, true);
    lv.setUint32(22, data.length, true);
    lv.setUint16(26, nameBytes.length, true);
    local.set(nameBytes, 30);
    local.set(packed, 30 + nameBytes.length);
    const central = new Uint8Array(46 + nameBytes.length);
    const cv = new DataView(central.buffer);
    cv.setUint32(0, 0x02014b50, true);
    cv.setUint16(10, 8, true);
    cv.setUint32(16, crc, true);
    cv.setUint32(20, packed.length, true);
    cv.setUint32(24, data.length, true);
    cv.setUint16(28, nameBytes.length, true);
    cv.setUint32(42, offset, true);
    central.set(nameBytes, 46);
    locals.push(local);
    centrals.push(central);
    offset += local.length;
  }
  const directory = Buffer.concat(centrals);
  const end = new Uint8Array(22);
  const ev = new DataView(end.buffer);
  ev.setUint32(0, 0x06054b50, true);
  ev.setUint16(8, centrals.length, true);
  ev.setUint16(10, centrals.length, true);
  ev.setUint32(12, directory.length, true);
  ev.setUint32(16, offset, true);
  return new Uint8Array(Buffer.concat([...locals, directory, end]));
}

describe("validation receipts", () => {
  const expected: Receipt = {
    schema: 1,
    tree: "a".repeat(40),
    workflow: ".github/workflows/ci.yml",
  };
  let run: Table;
  let artifact: Table;
  let body: Table;
  let commitTree: string;
  const api: Api = {
    json(path) {
      if (path.endsWith("/3")) return run;
      if (path.includes("/git/commits/")) return { tree: { sha: commitTree } };
      return { artifacts: [artifact] };
    },
    bytes: () => zip({ "receipt.json": JSON.stringify(body) }),
  };
  const find = (): unknown => findValidation(expected, "owner/project", api);

  beforeEach(() => {
    run = {
      conclusion: "success",
      event: "pull_request",
      head_sha: "c".repeat(40),
      path: expected.workflow,
      head_repository: { full_name: "owner/project" },
      repository: { full_name: "owner/project" },
    };
    artifact = { id: 2, workflow_run: { id: 3 }, expired: false, size_in_bytes: 400 };
    body = { ...expected };
    commitTree = expected.tree;
  });

  test("the artifact is named for the workflow file and tree", () => {
    expect(artifactName(expected)).toBe(`validation-ci-${"a".repeat(40)}`);
  });

  test("a successful exact tree is reused", () => {
    expect(find()).toBe(3);
  });

  test("a changed tree is not reused", () => {
    body["tree"] = "b".repeat(40);
    expect(find()).toBeUndefined();
  });

  test("a forged receipt from an unrelated source commit is not reused", () => {
    commitTree = "d".repeat(40);
    expect(find()).toBeUndefined();
  });

  test("failed, in-flight, wrong-workflow and fork runs are not reused", () => {
    for (const [field, value] of [
      ["conclusion", "failure"],
      ["conclusion", null],
      ["event", "workflow_dispatch"],
      ["path", "other.yml"],
      ["head_repository", { full_name: "fork/project" }],
    ] as const) {
      const previous = run[field];
      run[field] = value;
      expect(find()).toBeUndefined();
      run[field] = previous;
    }
  });

  test("expired and oversized artifacts are not reused", () => {
    artifact["expired"] = true;
    expect(find()).toBeUndefined();
    artifact["expired"] = false;
    artifact["size_in_bytes"] = 100000;
    expect(find()).toBeUndefined();
  });

  test("an archive cannot supply extra files", () => {
    const archive = zip({ "receipt.json": JSON.stringify(body), "unwanted.sh": "exit 1" });
    expect(readReceipt(archive)).toBeUndefined();
  });

  test("an oversized receipt is not read", () => {
    expect(readReceipt(zip({ "receipt.json": " ".repeat(5000) }))).toBeUndefined();
  });

  test("a corrupt archive throws, which the lookup treats as a miss", () => {
    const archive = zip({ "receipt.json": JSON.stringify(body) });
    archive[45] = (archive[45] ?? 0) ^ 0xff; // inside the deflate stream
    expect(() => readReceipt(archive)).toThrow();
    expect(() => readReceipt(new Uint8Array(10))).toThrow("not a zip archive");
  });
});
