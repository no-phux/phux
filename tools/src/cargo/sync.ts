// Align a standalone Cargo.lock's phux path crates with the root lock.
//
// The desktop host (clients/desktop/native) is its own Cargo workspace whose
// path dependencies include the GPUIX checkout, which is bootstrapped rather
// than committed. `cargo update --workspace` therefore cannot run there on a
// bare release checkout, yet a release bump still changes the versions its
// lock records for the root path crates, and `--locked` builds then refuse to
// start.
//
// A path package carries no `source` line, and its version is exactly what the
// root workspace declares. This rewrites the version of every sourceless
// package in each target lock whose name is also a sourceless package in the
// root lock, leaving every other byte untouched.
import { readFileSync, writeFileSync } from "node:fs";
import { resolve } from "node:path";
import { status } from "../lib/output.ts";
import { pathVersions, syncPathVersions } from "./lock.ts";

/**
 * Sync (or, with `check`, only compare) each target lock against the root
 * lock. Paths are shown as given and read relative to `cwd`. Returns whether
 * every target was already in step (always true when writing).
 */
export function syncLocks(
  rootLock: string,
  targets: readonly string[],
  check: boolean,
  cwd = process.cwd(),
): boolean {
  const versions = pathVersions(readFileSync(resolve(cwd, rootLock), "utf8"));
  let inStep = true;
  for (const target of targets) {
    const path = resolve(cwd, target);
    const before = readFileSync(path, "utf8");
    const after = syncPathVersions(before, versions);
    if (after === before) continue;
    if (check) {
      status("error", `${target}: path crate versions differ from ${rootLock}`);
      inStep = false;
    } else {
      writeFileSync(path, after);
      status("ok", `updated ${target}`);
    }
  }
  return inStep;
}
