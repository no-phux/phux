import { expect, test } from "bun:test";
import { Classifier } from "./classify.ts";
import { LANES, plan, planOutputs, reused, type Lane, type Trigger } from "./plan.ts";

const classifier = new Classifier();

function lanes(paths: string[], trigger: Trigger): Lane[] {
  const chosen = plan(classifier.classify(paths), trigger).lanes;
  return LANES.filter((lane) => chosen.has(lane));
}

test.each<[string[], Trigger, Lane[]]>([
  [["docs/RELEASING.md"], "pull_request", ["guards"]],
  [["docs/RELEASING.md"], "push", ["guards"]],
  // A change to how lanes run reruns every lane.
  [[".github/workflows/ci.yml"], "pull_request", [...LANES]],
  [[".github/workflows/release.yml"], "pull_request", ["guards"]],
  [[".github/ISSUE_TEMPLATE/bug.yml"], "pull_request", ["guards"]],
  [["clients/rune/src/main.rs"], "pull_request", ["guards", "rune"]],
  [["clients/rune/src/main.rs"], "push", ["guards", "rune"]],
  [["integrations/pi/src/index.ts"], "pull_request", ["guards", "integrations"]],
  [["crates/phux-mcp/src/lib.rs"], "pull_request", ["guards", "lint", "tests", "e2e"]],
  [["crates/phux-mcp/src/lib.rs"], "push", ["guards", "lint", "tests", "e2e", "ffi"]],
  [["crates/phux-mcp/benches/b.rs"], "pull_request", ["guards", "lint"]],
  [
    ["crates/phux-server/src/lib.rs"],
    "pull_request",
    ["guards", "lint", "tests", "e2e", "ffi", "icount", "web"],
  ],
  [[], "pull_request", [...LANES]],
])("%p on %s runs %p", (paths, trigger, want) => {
  expect(lanes(paths, trigger)).toEqual(want);
});

test("a push runs the full unit pool; a pull request keeps its filterset", () => {
  const c = classifier.classify(["crates/phux-mcp/src/lib.rs"]);
  expect(plan(c, "pull_request").testFilterset).toContain("rdeps(=phux-mcp)");
  expect(plan(c, "push")).toMatchObject({ unitMode: "workspace", testFilterset: "" });
});

test("coverage is the pull-request lane set on either event", () => {
  const c = classifier.classify(["crates/phux-mcp/src/lib.rs"]);
  expect(plan(c, "push").coverage).toBe("guards,lint,tests,e2e");
  expect(plan(c, "pull_request").coverage).toBe("guards,lint,tests,e2e");
});

test("outputs name every lane and the web engine only inside the web lane", () => {
  const c = classifier.classify(["clients/phux-vt-web/vendor/ghostty-vt.wasm"]);
  const outputs = planOutputs(plan(c, "pull_request"), c);
  for (const lane of LANES) expect(typeof outputs[lane]).toBe("boolean");
  expect(outputs).toMatchObject({ web: true, web_engine: true });
});

test("a reused main tree still qualifies the binding, and nothing else", () => {
  const rust = plan(classifier.classify(["crates/phux-mcp/src/lib.rs"]), "push");
  expect([...reused(rust).lanes]).toEqual(["ffi"]);
  const docs = plan(classifier.classify(["docs/SETUP.md"]), "push");
  expect([...reused(docs).lanes]).toEqual([]);
});

test("the ci receipt has no macOS coverage on a pull request or a main push", () => {
  // The Linux rune lane still runs. macOS coverage would let a main push
  // reuse a pull request that never started a macOS runner.
  const closure = classifier.classify(["crates/phux-protocol/src/lib.rs"]);
  const pr = plan(closure, "pull_request");
  const main = plan(closure, "push");
  expect(pr.lanes.has("rune")).toBe(true);
  expect(main.lanes.has("rune")).toBe(true);
  expect(pr.coverage).toBe(main.coverage);
  expect(pr.coverage.split(",")).not.toContain("rune_ios");
  expect(pr.coverage.split(",")).not.toContain("macos");
  expect(planOutputs(pr, closure)).not.toHaveProperty("rune_ios");
  expect(planOutputs(reused(main), closure)).not.toHaveProperty("rune_ios");
  expect([...reused(main).lanes]).not.toContain("rune");
});
