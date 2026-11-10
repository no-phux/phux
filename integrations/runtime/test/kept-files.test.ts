import assert from "node:assert/strict";
import { chmodSync, existsSync, mkdirSync, mkdtempSync, readdirSync, utimesSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import test from "node:test";

import { KEPT_FILE_RETENTION_MS, keptFilePath, keptFilesDirectory, MAX_KEPT_FILES } from "../src/kept-files.js";

function isolated(): string {
  process.env.TMPDIR = mkdtempSync(join(tmpdir(), "phux-kept-test-"));
  return keptFilesDirectory();
}

test("kept files live in one private directory and expire after the retention window", async () => {
  const directory = isolated();
  const first = await keptFilePath("tree", ".json");
  assert.equal(join(first, ".."), directory);
  assert.match(first, /\/tree-\d+-[0-9a-f]{8}\.json$/);
  writeFileSync(first, "{}");
  const old = join(directory, "tree-old.json");
  writeFileSync(old, "{}");
  const stale = (Date.now() - KEPT_FILE_RETENTION_MS - 60_000) / 1000;
  utimesSync(old, stale, stale);
  await keptFilePath("surface", "");
  assert.equal(existsSync(old), false, "a file past retention is pruned on the next call");
  assert.equal(existsSync(first), true, "a recent file stays readable");
});

test("at most MAX_KEPT_FILES exist once the next file is written", async () => {
  const directory = isolated();
  await keptFilePath("tree", ".json");
  for (let index = 0; index < MAX_KEPT_FILES + 10; index++) {
    const path = join(directory, `surface-${index}.png`);
    writeFileSync(path, "x");
    const when = (Date.now() - (MAX_KEPT_FILES + 10 - index) * 1000) / 1000;
    utimesSync(path, when, when);
  }
  await keptFilePath("tree", ".json");
  const left = readdirSync(directory).sort((a, b) => Number(a.split(/[-.]/)[1]) - Number(b.split(/[-.]/)[1]));
  assert.equal(left.length, MAX_KEPT_FILES - 1);
  assert.equal(left.at(-1), `surface-${MAX_KEPT_FILES + 9}.png`, "the newest files are the ones kept");
});

test("a directory another user could write is refused", async () => {
  const directory = isolated();
  mkdirSync(directory);
  chmodSync(directory, 0o777);
  await assert.rejects(keptFilePath("tree", ".json"), /mode 0700/);
});
