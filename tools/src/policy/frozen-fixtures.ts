// Keep frozen fixtures frozen.
//
// Each file under crates/*/tests/fixtures/frozen/ and
// crates/phux-server/tests/fixtures/upgrade-blob/ is something a released image
// wrote and a later one must read. They are the only proof that a rename is
// safe, and a sweeping rename would otherwise rewrite the proof along with the
// code. Add a fixture; never edit, move, or delete one (crates/phux-frozen).
import { Failure } from "../lib/command.ts";
import { hint, status } from "../lib/output.ts";
import { capture } from "../lib/process.ts";
import { readFileSync } from "node:fs";
import { join } from "node:path";

export const BLOB = "crates/phux-server/src/upgrade/blob.rs";
const FIELD = /^\s*(pub(\([a-z]+\))?\s+)?([a-z_][a-z0-9_]*):\s.*,\s*$/;

export interface FrozenVerdict {
  /** Frozen fixtures edited, moved or deleted since the merge base. */
  readonly changed: string[];
  /** Persisted field names that left blob.rs without a rename or alias. */
  readonly dropped: string[];
}

export function frozenVerdict(root: string, base: string): FrozenVerdict {
  const git = (args: string[]): string => capture(["git", ...args], { cwd: root });
  let mergeBase: string;
  try {
    mergeBase = git(["merge-base", base, "HEAD"]).trim();
  } catch {
    throw new Failure(`frozen-fixtures: no merge base with ${base}, so nothing was checked.`);
  }
  const changed = git([
    "diff",
    "--name-only",
    "--no-renames",
    "--diff-filter=MDT",
    mergeBase,
    "--",
    ":(glob)crates/*/tests/fixtures/frozen/**",
    "crates/phux-server/tests/fixtures/upgrade-blob",
  ])
    .split("\n")
    .filter((line) => line !== "");

  // Field names are the format, so a sweeping rename that reaches the blob
  // types breaks a live upgrade. Golden tests catch it after a full build;
  // this catches it from the diff. A field may leave blob.rs only if its old
  // name still appears there, normally as `#[serde(rename = "old")]` or
  // `alias`. Removing a persisted field outright needs the same
  // acknowledgement, or a blob version bump.
  const current = readFileSync(join(root, BLOB), "utf8");
  const dropped = git(["diff", "-U0", mergeBase, "--", BLOB])
    .split("\n")
    .filter((line) => /^-[^-]/.test(line))
    .flatMap((line) => {
      const name = FIELD.exec(line.slice(1))?.[3];
      if (name === undefined) return [];
      const kept = new RegExp(`^\\s*(pub(\\([a-z]+\\))?\\s+)?${name}:|"${name}"`, "m");
      return kept.test(current) ? [] : [name];
    });
  return { changed, dropped };
}

export function frozenFixturesCheck(root: string, base: string): number {
  const { changed, dropped } = frozenVerdict(root, base);
  if (changed.length > 0) {
    process.stderr.write(`${changed.join("\n")}\n`);
    status("error", "frozen-fixtures: frozen fixtures changed.");
    hint("Restore them and freeze the new shape in a new fixture; see crates/phux-frozen.");
    return 1;
  }
  if (dropped.length > 0) {
    process.stderr.write(dropped.map((name) => `  ${name}\n`).join(""));
    status("error", `frozen-fixtures: persisted field names left ${BLOB}.`);
    hint(
      'Keep each old name with #[serde(rename = "old")], so a blob from a running server still reads.',
    );
    return 1;
  }
  status("ok", "frozen-fixtures: frozen fixtures are unchanged");
  return 0;
}
