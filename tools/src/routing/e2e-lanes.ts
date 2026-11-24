// Coverage gate for the `just e2e` nextest filterset.
//
// THE HOLE THIS CLOSES. `#[ignore]`d tests do not run in the default pool
// (`cargo nextest run --workspace`, which is CI's unit step). They run only
// where a lane passes `--run-ignored` and names their binary. `just e2e`
// reads a hand-maintained list of binary IDs. A new `*_e2e.rs` full of
// `#[ignore]`d tests can therefore compile, pass review, and never execute
// anywhere -- green forever, testing nothing. That is not hypothetical:
// phux/tests/upgrade_e2e.rs shipped with a module doc claiming "run via
// `just e2e`" while absent from the filterset, and plugin_agent_bench_e2e and
// workspace_archive_e2e were in the same state.
//
// WHY NOT DERIVE THE LIST. The PR lane is a curated subset. The heavy
// `stress_*` storms run locally via `just stress`, and new expensive e2e
// binaries can make the same choice. So the contract enforced here is the
// weaker, checkable one: every e2e binary must run SOMEWHERE. A binary with no
// `#[ignore]` already runs in the default pool and needs no entry; a binary
// that has at least one `#[ignore]` must be named by a lane recipe, or those
// tests are dead weight.
//
// SCOPE is the `*_e2e.rs` naming convention, including subsystem harness
// modules under `tests/<harness>/`, not every test target, and that is
// deliberate. Outside that convention an `#[ignore]` can legitimately mean
// "this needs a host tool or a machine property CI does not have". Those are
// correct as unrun; forcing them into a lane would mean weakening them or
// maintaining an exemption list, which is the very failure mode this gate
// exists to remove. `*_e2e.rs` carries the opposite intent by convention:
// spawn a real server, run in a lane.
//
// Reading the shared list and the stress recipe (rather than asking nextest)
// keeps this compile-free: a seconds-long gate that can run on the docs-only
// fast path and in a job that never builds the workspace.
import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { Failure } from "../lib/command.ts";

/** The shell library `just test-archive` and `just e2e` source for their selection. */
export const SELECTION = "dev/lib/e2e-selection.sh";

export interface Uncovered {
  readonly message: string;
  readonly hints: readonly string[];
}

export interface LaneCoverage {
  readonly checked: number;
  readonly uncovered: readonly Uncovered[];
}

/** The nextest binary IDs in the selection library's `E2E_BINARIES=( ... )`. */
export function e2eBinaries(selection: string): string[] {
  const body = /^E2E_BINARIES=\(([^)]*)\)/m.exec(selection)?.[1];
  if (body === undefined) throw new Failure(`found no E2E_BINARIES array in ${SELECTION}`);
  return body.split(/\s+/).filter(Boolean);
}

/** The `stress` recipe, from its header to the next unindented line. */
export function stressRecipe(recipes: string): string {
  const lines: string[] = [];
  let inside = false;
  for (const line of recipes.split("\n")) {
    if (line.startsWith("stress:")) inside = true;
    else if (inside && /^[^ \t#]/.test(line)) inside = false;
    if (inside) lines.push(line);
  }
  return lines.join("\n");
}

/** Every `*_e2e.rs` under a crate's tests, repository-relative and sorted. */
function e2eSuites(root: string): string[] {
  const glob = new Bun.Glob("crates/**/tests/**/*_e2e.rs");
  return [...glob.scanSync({ cwd: root, dot: true })].toSorted();
}

function unwired(path: string, entry: string, stem: string): Uncovered {
  return {
    message: `${path} is not wired into a supported subsystem harness`,
    hints: [`expected ${entry} to declare: mod ${stem};`],
  };
}

function unrun(path: string, binary: string): Uncovered {
  return {
    message: `${path} has #[ignore]d tests that no lane runs`,
    hints: [
      `binary id: ${binary}`,
      "#[ignore]d tests only run where a recipe passes --run-ignored AND",
      "names the binary. This one is named nowhere, so it never executes",
      "in CI and will stay green no matter what it asserts.",
      "",
      `Fix by adding ${binary} to E2E_BINARIES in ${SELECTION},`,
      "or, if it is too slow for the PR critical path, to the `stress`",
      "recipe (run locally with `just stress`).",
    ],
  };
}

/**
 * The nextest binary ID of one suite: `crate::stem` for a top-level target,
 * `crate::harness` for `tests/<harness>/<stem>.rs` declared by that harness's
 * main.rs. Deeper or unwired modules fail closed rather than lose coverage.
 */
function binaryId(root: string, path: string): string | Uncovered {
  const crate = path.slice("crates/".length).split("/")[0] ?? "";
  const stem = (path.split("/").at(-1) ?? "").replace(/\.rs$/, "");
  const prefix = `crates/${crate}/tests/`;
  const relative = path.startsWith(prefix) ? path.slice(prefix.length) : path;
  if (relative === `${stem}.rs`) return `${crate}::${stem}`;
  const harness = relative.split("/")[0] ?? "";
  const entry = `crates/${crate}/tests/${harness}/main.rs`;
  const wired =
    relative === `${harness}/${stem}.rs` &&
    existsSync(join(root, entry)) &&
    readFileSync(join(root, entry), "utf8").split("\n").includes(`mod ${stem};`);
  return wired ? `${crate}::${harness}` : unwired(path, entry, stem);
}

/** Which `*_e2e.rs` suites carry `#[ignore]`d tests that no lane selects. */
export function laneCoverage(root: string): LaneCoverage {
  if (!existsSync(join(root, "justfile"))) throw new Failure("justfile not found");
  // Recipes live in just/*.just; the root justfile is imports + default.
  const recipeFiles = [
    "justfile",
    ...[...new Bun.Glob("just/*.just").scanSync({ cwd: root, dot: true })].toSorted(),
  ];
  const recipes = recipeFiles.map((file) => readFileSync(join(root, file), "utf8")).join("");
  // Only the stress recipe names candidates independently.
  const stress = stressRecipe(recipes);
  if (stress === "") {
    throw new Failure(
      "found no stress recipe body in just/test.just (if it was renamed, update this gate)",
    );
  }
  const listed = new Set(e2eBinaries(readFileSync(join(root, SELECTION), "utf8")));
  const suites = e2eSuites(root);
  if (suites.length === 0) {
    throw new Failure(
      "no *_e2e.rs suites found under crates/*/tests/ (the glob or the naming convention changed; update this gate)",
    );
  }
  const uncovered: Uncovered[] = [];
  for (const path of suites) {
    const id = binaryId(root, path);
    if (typeof id !== "string") {
      uncovered.push(id);
      continue;
    }
    // No `#[ignore]` means the default pool, CI's unit step, runs every test.
    if (!readFileSync(join(root, path), "utf8").includes("#[ignore")) continue;
    if (listed.has(id) || stress.includes(`binary_id(${id})`)) continue;
    uncovered.push(unrun(path, id));
  }
  return { checked: suites.length, uncovered };
}
