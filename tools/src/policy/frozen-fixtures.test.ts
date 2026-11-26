// Keep frozen fixtures frozen.
//
// Each file under crates/*/tests/fixtures/frozen/ and
// crates/phux-server/tests/fixtures/upgrade-blob/ is something a released image
// wrote and a later one must read. They are the only proof that a rename is
// safe, and a sweeping rename would otherwise rewrite the proof along with the
// code. Add a fixture; never edit, move, or delete one (crates/phux-frozen).
//
// The diff is from the merge base with origin/main to the working tree, so it
// needs origin/main fetched with history (the guards job checks out with
// fetch-depth: 0). Without a merge base this fails rather than passing unchecked.
import { expect, test } from "bun:test";
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { git } from "../lib/git.ts";
import { ROOT } from "../lib/repo.ts";

const BLOB = "crates/phux-server/src/upgrade/blob.rs";
const FIELD = /^\s*(?:pub(?:\([a-z]+\))?\s+)?([a-z_][a-z0-9_]*):\s.*,\s*$/;

function mergeBase(base: string, cwd: string): string {
  try {
    return git(["merge-base", base, "HEAD"], cwd).trim();
  } catch {
    throw new Error(`frozen-fixtures: no merge base with ${base}, so nothing was checked`);
  }
}

function frozenFailures(base: string, cwd: string): string[] {
  const from = mergeBase(base, cwd);
  const changed = git(
    [
      "diff",
      "--name-only",
      "--no-renames",
      "--diff-filter=MDT",
      from,
      "--",
      ":(glob)crates/*/tests/fixtures/frozen/**",
      "crates/phux-server/tests/fixtures/upgrade-blob",
    ],
    cwd,
  )
    .split("\n")
    .filter(Boolean);
  const edited = changed.map(
    (path) =>
      `${path}: frozen fixture changed; restore it and freeze the new shape in a new fixture (crates/phux-frozen)`,
  );
  return [...edited, ...droppedFields(from, cwd)];
}

// Field names are the format, so a sweeping rename that reaches the blob types
// breaks a live upgrade. Golden tests catch it after a full build; this catches
// it from the diff. A field may leave blob.rs only if its old name still appears
// there, normally as `#[serde(rename = "old")]` or `alias`. Removing a persisted
// field outright needs the same acknowledgement, or a blob version bump.
function droppedFields(from: string, cwd: string): string[] {
  const path = join(cwd, BLOB);
  const blob = existsSync(path) ? readFileSync(path, "utf8") : "";
  const removed = git(["diff", "-U0", from, "--", BLOB], cwd)
    .split("\n")
    .filter((line) => /^-[^-]/.test(line));
  return removed.flatMap((line) => {
    const name = FIELD.exec(line.slice(1))?.[1];
    if (name === undefined) return [];
    const kept = new RegExp(`^\\s*(pub(\\([a-z]+\\))?\\s+)?${name}:|"${name}"`, "m").test(blob);
    return kept
      ? []
      : [
          `${BLOB}: persisted field ${name} left the blob; keep the old name with #[serde(rename = "${name}")] so a blob from a running server still reads`,
        ];
  });
}

test("frozen fixtures and persisted blob field names are unchanged since origin/main", () => {
  expect(frozenFailures("origin/main", ROOT)).toEqual([]);
});

test("an edited fixture or a bare persisted-field rename is refused", () => {
  const root = mkdtempSync(join(tmpdir(), "phux-frozen-fixtures-"));
  const write = (path: string, text: string) => {
    mkdirSync(join(root, dirname(path)), { recursive: true });
    writeFileSync(join(root, path), text);
  };
  const commit = (message: string) => {
    git(["add", "-A"], root);
    git(["commit", "--quiet", "-m", message], root);
  };
  const blob = (fields: string) => write(BLOB, `pub struct Pane {\n${fields}}\n`);
  try {
    git(["init", "--quiet", "--initial-branch=main"], root);
    git(["config", "user.name", "Frozen Test"], root);
    git(["config", "user.email", "frozen@example.invalid"], root);
    git(["config", "core.hooksPath", "/dev/null"], root);
    git(["config", "commit.gpgsign", "false"], root);
    blob(
      "    pub terminal_id: u64,\n    pub(crate) grid_rows: u16,\n    #[serde(default)]\n    title: String,\n",
    );
    write("crates/phux-config/tests/fixtures/frozen/v1.toml", "a = 1\n");
    write("crates/phux-server/tests/fixtures/upgrade-blob/0001.json", "{}\n");
    commit("base");
    git(["checkout", "--quiet", "-b", "change"], root);
    expect(frozenFailures("main", root)).toEqual([]);

    write("crates/phux-config/tests/fixtures/frozen/v2.toml", "a = 2\n");
    expect(frozenFailures("main", root)).toEqual([]);
    write("crates/phux-config/tests/fixtures/frozen/v1.toml", "a = 3\n");
    rmSync(join(root, "crates/phux-server/tests/fixtures/upgrade-blob/0001.json"));
    expect(frozenFailures("main", root).map((failure) => failure.split(":")[0])).toEqual([
      "crates/phux-config/tests/fixtures/frozen/v1.toml",
      "crates/phux-server/tests/fixtures/upgrade-blob/0001.json",
    ]);
    git(["checkout", "--quiet", "--", "crates"], root);

    blob(
      "    pub pane_id: u64,\n    pub(crate) grid_rows: u16,\n    #[serde(default)]\n    title: String,\n",
    );
    commit("rename");
    expect(frozenFailures("main", root)).toEqual([
      `${BLOB}: persisted field terminal_id left the blob; keep the old name with #[serde(rename = "terminal_id")] so a blob from a running server still reads`,
    ]);

    blob(
      '    #[serde(rename = "terminal_id")]\n    pub pane_id: u64,\n    pub(crate) grid_rows: u16,\n    #[serde(default)]\n    title: String,\n',
    );
    commit("keep-name");
    expect(frozenFailures("main", root)).toEqual([]);

    blob(
      '    #[serde(rename = "terminal_id")]\n    pub pane_id: u64,\n    #[serde(default)]\n    title: String,\n    #[serde(default)]\n    pub(crate) grid_rows: u32,\n',
    );
    commit("reorder");
    expect(frozenFailures("main", root)).toEqual([]);

    expect(() => frozenFailures("no-such-ref", root)).toThrow("no merge base with no-such-ref");
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});
