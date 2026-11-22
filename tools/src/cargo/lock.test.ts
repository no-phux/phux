import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { syncLocks, syncPathVersions } from "./lock.ts";

const lock = (version: string) => `version = 4

[[package]]
name = "phux-protocol"
version = "${version}"

[[package]]
name = "registry-package"
version = "0.46.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
`;

test("only path package versions change", () => {
  const versions = new Map([
    ["phux-protocol", "0.48.0"],
    ["registry-package", "0.48.0"],
  ]);
  expect(syncPathVersions(lock("0.46.0"), versions)).toBe(lock("0.48.0"));
});

test("--check reports without writing; a sync is idempotent", () => {
  const dir = mkdtempSync(join(tmpdir(), "sync-lock-"));
  writeFileSync(join(dir, "root.lock"), lock("0.48.0"));
  writeFileSync(join(dir, "edge.lock"), lock("0.46.0"));
  const edge = () => readFileSync(join(dir, "edge.lock"), "utf8");
  expect(syncLocks("root.lock", ["edge.lock"], true, dir)).toBe(false);
  expect(edge()).toBe(lock("0.46.0"));
  expect(syncLocks("root.lock", ["edge.lock"], false, dir)).toBe(true);
  expect(syncLocks("root.lock", ["edge.lock"], true, dir)).toBe(true);
  expect(edge()).toBe(lock("0.48.0"));
});
