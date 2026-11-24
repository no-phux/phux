// The e2e lane coverage gate, on the real tree and on throwaway fixtures.
import { afterEach, beforeEach, expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { Failure } from "../lib/command.ts";
import { ROOT } from "../lib/repo.ts";
import { e2eBinaries, laneCoverage, SELECTION, stressRecipe } from "./e2e-lanes.ts";

let root = "";
const IGNORED = "#[test]\n#[ignore]\nfn slow() {}\n";

function put(path: string, content: string): void {
  mkdirSync(dirname(join(root, path)), { recursive: true });
  writeFileSync(join(root, path), content);
}

beforeEach(() => {
  root = mkdtempSync(join(tmpdir(), "phux-e2e-lanes-"));
  put("justfile", "import 'just/test.just'\n");
  put(
    "just/test.just",
    "stress:\n    cargo nextest run -E 'binary_id(phux-server::stress)'\n\nother:\n    binary_id(phux::other_e2e)\n",
  );
  put(SELECTION, "E2E_BINARIES=(\n    phux::listed_e2e\n    phux::harness\n)\n");
  put("crates/phux/tests/plain_e2e.rs", "#[test]\nfn quick() {}\n");
});

afterEach(() => rmSync(root, { recursive: true, force: true }));

const messages = () => laneCoverage(root).uncovered.map((item) => item.message);

test("the repository's e2e suites all run in some lane", () => {
  const coverage = laneCoverage(ROOT);
  expect(coverage.checked).toBeGreaterThan(0);
  expect(coverage.uncovered).toEqual([]);
});

test("suites without #[ignore] need no lane", () => {
  expect(laneCoverage(root)).toEqual({ checked: 1, uncovered: [] });
});

test("an ignored suite passes when the e2e list or the stress recipe names it", () => {
  put("crates/phux/tests/listed_e2e.rs", IGNORED);
  put("crates/phux-server/tests/stress_e2e.rs", IGNORED);
  put("crates/phux-server/tests/stress/main.rs", "");
  expect(messages()).toEqual([
    "crates/phux-server/tests/stress_e2e.rs has #[ignore]d tests that no lane runs",
  ]);
  rmSync(join(root, "crates/phux-server/tests/stress_e2e.rs"));
  put("crates/phux-server/tests/stress/stress_e2e.rs", IGNORED);
  put("crates/phux-server/tests/stress/main.rs", "mod stress_e2e;\n");
  expect(messages()).toEqual([]);
});

test("an ignored suite another recipe names is still unrun", () => {
  put("crates/phux/tests/other_e2e.rs", IGNORED);
  const [item] = laneCoverage(root).uncovered;
  expect(item?.message).toBe(
    "crates/phux/tests/other_e2e.rs has #[ignore]d tests that no lane runs",
  );
  expect(item?.hints).toContain("binary id: phux::other_e2e");
  expect(item?.hints).toContain(`Fix by adding phux::other_e2e to E2E_BINARIES in ${SELECTION},`);
  // The diagnostic names `just stress` as text for a person to run.
  expect(item?.hints.join("\n")).toContain("recipe (run locally with `just stress`).");
});

test("harness modules count under their harness binary only when wired", () => {
  put("crates/phux/tests/harness/upgrade_e2e.rs", IGNORED);
  expect(messages()).toEqual([
    "crates/phux/tests/harness/upgrade_e2e.rs is not wired into a supported subsystem harness",
  ]);
  put("crates/phux/tests/harness/main.rs", "mod other;\nmod upgrade_e2e;\n");
  expect(messages()).toEqual([]);
  put("crates/phux/tests/harness/deep/nested_e2e.rs", "#[test]\nfn quick() {}\n");
  expect(messages()).toEqual([
    "crates/phux/tests/harness/deep/nested_e2e.rs is not wired into a supported subsystem harness",
  ]);
});

test("a missing justfile, stress recipe, or suite set fails closed", () => {
  rmSync(join(root, "crates"), { recursive: true });
  expect(() => laneCoverage(root)).toThrow(/no \*_e2e\.rs suites found/);
  put("just/test.just", "test:\n    true\n");
  expect(() => laneCoverage(root)).toThrow(/found no stress recipe/);
  rmSync(join(root, "justfile"));
  expect(() => laneCoverage(root)).toThrow(Failure);
});

test("the selection list and the stress body parse like the shell reads them", () => {
  expect(e2eBinaries("x=1\nE2E_BINARIES=(\n  a::b c::d\n  e::f\n)\n")).toEqual([
    "a::b",
    "c::d",
    "e::f",
  ]);
  expect(() => e2eBinaries("E2E=()")).toThrow(Failure);
  expect(stressRecipe("a:\n    x\nstress: dep\n    one\n# note\n    two\nnext:\n    three\n")).toBe(
    "stress: dep\n    one\n# note\n    two",
  );
});
