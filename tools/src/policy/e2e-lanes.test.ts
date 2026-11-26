// Every e2e binary runs somewhere.
//
// `#[ignore]`d tests do not run in the default pool (`cargo nextest run
// --workspace`, CI's unit step). They run only where a lane passes
// `--run-ignored` and names their binary, and `just e2e` reads a
// hand-maintained list (E2E_BINARIES in dev/lib/e2e-selection.sh). A new
// `*_e2e.rs` full of `#[ignore]`d tests could compile, pass review, and never
// execute anywhere: upgrade_e2e.rs shipped claiming "run via `just e2e`" while
// absent from the list, as did plugin_agent_bench_e2e and workspace_archive_e2e.
//
// The PR lane is a curated subset (the heavy `stress_*` storms run locally via
// `just stress`), so the list is not derived. The contract is the checkable
// one: a binary with no `#[ignore]` already runs in the default pool; a binary
// with at least one must be named by E2E_BINARIES or the `stress` recipe.
//
// Scope is the `*_e2e.rs` convention, including subsystem harness modules
// under `tests/<harness>/`, not every test target. Elsewhere an `#[ignore]`
// can mean "needs a host tool CI does not have"; `*_e2e.rs` carries the
// opposite intent: spawn a real server, run in a lane.
import { expect, test } from "bun:test";
import { existsSync, readFileSync } from "node:fs";
import { basename } from "node:path";
import { Glob } from "bun";
import { recipeBody } from "../lib/just.ts";
import { capture } from "../lib/process.ts";
import { repoPath, ROOT } from "../lib/repo.ts";

type Read = (path: string) => string | undefined;

const readRepo: Read = (path) =>
  existsSync(repoPath(path)) ? readFileSync(repoPath(path), "utf8") : undefined;

function e2eBinaries(): string[] {
  return capture(
    ["bash", "-c", 'source dev/lib/e2e-selection.sh; printf "%s\\n" "${E2E_BINARIES[@]}"'],
    { cwd: ROOT },
  )
    .trim()
    .split("\n");
}

function suites(): string[] {
  return [...new Glob("crates/**/tests/**/*_e2e.rs").scanSync(ROOT)].sort();
}

/** The nextest binary id of a suite, or a failure when it is not wired into one. */
function binaryId(path: string, read: Read): { id: string } | { failure: string } {
  const crate = path.split("/")[1] ?? "";
  const stem = basename(path, ".rs");
  const prefix = `crates/${crate}/tests/`;
  const relative = path.startsWith(prefix) ? path.slice(prefix.length) : path;
  if (relative === `${stem}.rs`) return { id: `${crate}::${stem}` };
  // A harness module: the binary is tests/<harness>/main.rs, which must
  // declare it. Deeper or unwired modules fail closed.
  const harness = relative.split("/")[0] ?? "";
  const entry = `${prefix}${harness}/main.rs`;
  const wired =
    relative === `${harness}/${stem}.rs` &&
    new RegExp(`^mod ${stem};$`, "m").test(read(entry) ?? "");
  if (wired) return { id: `${crate}::${harness}` };
  return {
    failure: `${path} is not wired into a supported subsystem harness; expected ${entry} to declare: mod ${stem};`,
  };
}

function laneFailures(
  paths: readonly string[],
  read: Read,
  binaries: readonly string[],
  stress: string | undefined,
): string[] {
  if (stress === undefined) {
    return ["found no `stress` recipe; if it was renamed, update this check to match"];
  }
  if (paths.length === 0) {
    return ["no *_e2e.rs suites found under crates/*/tests/; the naming convention changed"];
  }
  return paths.flatMap((path) => {
    const binary = binaryId(path, read);
    if ("failure" in binary) return [binary.failure];
    if (!(read(path) ?? "").includes("#[ignore")) return [];
    if (binaries.includes(binary.id) || stress.includes(`binary_id(${binary.id})`)) return [];
    return [
      `${path} has #[ignore]d tests that no lane runs (binary id ${binary.id}). ` +
        "#[ignore]d tests only run where a recipe passes --run-ignored and names the binary, " +
        "so this one never executes in CI and stays green whatever it asserts. Add " +
        `${binary.id} to E2E_BINARIES in dev/lib/e2e-selection.sh, or, if it is too slow ` +
        "for the PR critical path, to the `stress` recipe (run locally with `just stress`).",
    ];
  });
}

test("every e2e suite with #[ignore]d tests is named by a lane", () => {
  const paths = suites();
  expect(paths.length).toBeGreaterThan(0);
  expect(laneFailures(paths, readRepo, e2eBinaries(), recipeBody("stress"))).toEqual([]);
});

test("an unlisted, unwired, or nested e2e suite is refused", () => {
  const files: Record<string, string> = {
    "crates/a/tests/plain_e2e.rs": "#[test] fn ok() {}",
    "crates/a/tests/listed_e2e.rs": "#[ignore] #[test] fn slow() {}",
    "crates/a/tests/stressed_e2e.rs": "#[ignore] #[test] fn storm() {}",
    "crates/a/tests/lost_e2e.rs": '#[ignore = "server"] #[test] fn lost() {}',
    "crates/a/tests/suite/main.rs": "mod wired_e2e;\n",
    "crates/a/tests/suite/wired_e2e.rs": "#[ignore] #[test] fn wired() {}",
    "crates/a/tests/suite/unwired_e2e.rs": "#[test] fn unwired() {}",
    "crates/a/tests/suite/deep/nested_e2e.rs": "#[test] fn nested() {}",
  };
  const read: Read = (path) => files[path];
  const paths = Object.keys(files).filter((path) => path.endsWith("_e2e.rs"));
  const stress = "-E 'binary_id(a::stressed_e2e)'";
  const failures = laneFailures(paths, read, ["a::listed_e2e", "a::suite"], stress);
  expect(failures.map((failure) => failure.split(" ")[0])).toEqual([
    "crates/a/tests/lost_e2e.rs",
    "crates/a/tests/suite/unwired_e2e.rs",
    "crates/a/tests/suite/deep/nested_e2e.rs",
  ]);
  expect(failures[0]).toContain("`just stress`");
  expect(
    laneFailures(paths, read, ["a::listed_e2e"], stress).some((failure) =>
      failure.startsWith("crates/a/tests/suite/wired_e2e.rs has #[ignore]d tests"),
    ),
  ).toBe(true);
  expect(laneFailures(paths, read, [], undefined)).toHaveLength(1);
  expect(laneFailures([], read, [], stress)).toHaveLength(1);
});
