// Validation reuse must fail closed for mismatched trees and run provenance.
import { beforeEach, expect, test } from "bun:test";
import { writeFileSync } from "node:fs";
import { join } from "node:path";
import type { Table } from "../lib/value.ts";
import { artifactName, findValidation, type Receipt, type Source } from "./receipt.ts";

const expected: Receipt = {
  schema: 2,
  tree: "a".repeat(40),
  workflow: ".github/workflows/ci.yml",
  coverage: "guards,lint",
};
let run: Table;
let artifact: Table;
let files: Record<string, string>;
let commitTree: string;
let requested: Receipt;
const source: Source = {
  json(path) {
    if (path.endsWith("/runs/3")) return run;
    if (path.includes("/git/commits/")) return { tree: { sha: commitTree } };
    return { artifacts: [artifact] };
  },
  download(runId, name, dir) {
    expect([runId, name]).toEqual(["3", artifactName(requested)]);
    for (const [file, text] of Object.entries(files)) writeFileSync(join(dir, file), text);
  },
};
const find = (receipt = expected): unknown => {
  requested = receipt;
  return findValidation(receipt, "owner/project", source);
};

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
  files = { "receipt.json": JSON.stringify(expected) };
  commitTree = expected.tree;
});

test("the artifact is named for the workflow file and tree", () => {
  expect(artifactName(expected)).toBe(`validation-ci-${"a".repeat(40)}`);
});

test("a successful exact tree is reused", () => {
  expect(find()).toBe(3);
});

test("a merge group that validated the exact tree is reused", () => {
  run["event"] = "merge_group";
  expect(find()).toBe(3);
});

test("the required ci merge-group caller attests only the exact native child receipt", () => {
  const apple = {
    ...expected,
    workflow: ".github/workflows/rune-macos.yml",
    coverage: "macos,rune_ios",
  };
  files["receipt.json"] = JSON.stringify(apple);
  run["event"] = "merge_group";
  expect(find(apple)).toBeUndefined();
  const child = {
    path: `owner/project/${apple.workflow}@refs/heads/gh-readonly-queue/main/pr-1`,
    sha: run["head_sha"],
  };
  run["referenced_workflows"] = [{ ...child, sha: "d".repeat(40) }];
  expect(find(apple)).toBeUndefined();
  run["referenced_workflows"] = [child];
  expect(find(apple)).toBe(3);
  run["event"] = "pull_request";
  expect(find(apple)).toBeUndefined();
  run["event"] = "merge_group";
  files["receipt.json"] = JSON.stringify({ ...apple, coverage: "macos" });
  expect(find(apple)).toBeUndefined();
  files["receipt.json"] = JSON.stringify(apple);
  run["conclusion"] = "failure";
  expect(find(apple)).toBeUndefined();
});

test.each<[string, () => void]>([
  ["a changed tree", () => (files["receipt.json"] = JSON.stringify({ ...expected, tree: "b" }))],
  ["a forged receipt from another commit", () => (commitTree = "d".repeat(40))],
  ["a failed run", () => (run["conclusion"] = "failure")],
  ["an in-flight run", () => (run["conclusion"] = null)],
  ["a dispatched run", () => (run["event"] = "workflow_dispatch")],
  ["a main push run", () => (run["event"] = "push")],
  [
    "different lane coverage",
    () => (files["receipt.json"] = JSON.stringify({ ...expected, coverage: "guards" })),
  ],
  ["another workflow", () => (run["path"] = "other.yml")],
  ["a fork run", () => (run["head_repository"] = { full_name: "fork/project" })],
  ["an expired artifact", () => (artifact["expired"] = true)],
  ["an oversized artifact", () => (artifact["size_in_bytes"] = 100000)],
  ["an artifact with extra files", () => (files["unwanted.sh"] = "exit 1")],
  ["an oversized receipt", () => (files["receipt.json"] = " ".repeat(5000))],
])("%s is not reused", (_, change) => {
  change();
  expect(find()).toBeUndefined();
});
