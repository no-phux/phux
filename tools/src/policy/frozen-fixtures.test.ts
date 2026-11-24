import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, renameSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { capture } from "../lib/process.ts";
import { BLOB, frozenVerdict } from "./frozen-fixtures.ts";

const FIXTURE = "crates/phux-config/tests/fixtures/frozen/v1.toml";
const UPGRADE = "crates/phux-server/tests/fixtures/upgrade-blob/v3.bin";

function repository(
  run: (root: string, write: (path: string, text: string) => void) => void,
): void {
  const root = mkdtempSync(join(tmpdir(), "phux-frozen-fixtures-"));
  const write = (path: string, text: string): void => {
    mkdirSync(join(root, dirname(path)), { recursive: true });
    writeFileSync(join(root, path), text);
  };
  try {
    write(
      BLOB,
      "pub struct Pane {\n    pub terminal_id: u64,\n    pub(crate) grid_rows: u16,\n    #[serde(default)]\n    title: String,\n}\n",
    );
    write(FIXTURE, "frozen = 1\n");
    write(UPGRADE, "blob\n");
    commit(root, "base", ["init", "-q", "-b", "main"]);
    capture(["git", "checkout", "-q", "-b", "change"], { cwd: root });
    run(root, write);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function commit(root: string, message: string, before: string[] = []): void {
  const git = (args: string[]): string =>
    capture(
      ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", ...args],
      { cwd: root },
    );
  if (before.length > 0) git(before);
  git(["add", "--all"]);
  git(["commit", "-q", "--allow-empty", "-m", message]);
}

const blob = (fields: string): string => `pub struct Pane {\n${fields}}\n`;

test("persisted field names may move or retype but not silently leave blob.rs", () => {
  repository((root, write) => {
    expect(frozenVerdict(root, "main")).toEqual({ changed: [], dropped: [] });
    write(
      BLOB,
      blob(
        "    pub pane_id: u64,\n    pub(crate) grid_rows: u16,\n    #[serde(default)]\n    title: String,\n",
      ),
    );
    commit(root, "rename");
    expect(frozenVerdict(root, "main").dropped).toEqual(["terminal_id"]);
    const kept = '    #[serde(rename = "terminal_id")]\n    pub pane_id: u64,\n';
    write(
      BLOB,
      blob(`${kept}    pub(crate) grid_rows: u16,\n    #[serde(default)]\n    title: String,\n`),
    );
    commit(root, "keep-name");
    expect(frozenVerdict(root, "main").dropped).toEqual([]);
    write(
      BLOB,
      blob(
        `${kept}    #[serde(default)]\n    title: String,\n    #[serde(default)]\n    pub(crate) grid_rows: u32,\n`,
      ),
    );
    commit(root, "reorder");
    expect(frozenVerdict(root, "main").dropped).toEqual([]);
  });
});

test("frozen fixtures may be added but never edited, moved or deleted", () => {
  repository((root, write) => {
    write("crates/phux-config/tests/fixtures/frozen/v2.toml", "frozen = 2\n");
    commit(root, "add");
    expect(frozenVerdict(root, "main").changed).toEqual([]);
    write(FIXTURE, "frozen = 1.1\n");
    renameSync(join(root, UPGRADE), join(root, `${UPGRADE}.old`));
    commit(root, "edit");
    expect(frozenVerdict(root, "main").changed).toEqual([FIXTURE, UPGRADE]);
  });
});

test("a base with no merge base checks nothing and fails", () => {
  repository((root) => {
    expect(() => frozenVerdict(root, "no-such-ref")).toThrow("no merge base with no-such-ref");
  });
});
