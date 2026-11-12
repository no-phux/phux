import { describe, expect, test } from "bun:test";
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

describe("the Rune iOS simulator job", () => {
  const closure = classifier.classify(["crates/phux-protocol/src/lib.rs"]);
  const direct = classifier.classify(["clients/rune/src/main.rs"]);
  const shared = classifier.classify(["Cargo.toml"]);

  test("a pull request reached only through Cargo skips it once the merge queue is on", () => {
    const pr = plan(closure, "pull_request", true);
    expect(pr.lanes.has("rune")).toBe(true);
    expect(planOutputs(pr, closure)).toMatchObject({ rune: true, rune_ios: false });
    expect(pr.coverage).not.toContain("rune_ios");
  });

  test("without the queue, pull requests run it exactly as before", () => {
    expect(plan(closure, "pull_request").runeIos).toBe(true);
    expect(planOutputs(plan(closure, "pull_request"), closure)).toMatchObject({ rune_ios: true });
  });

  test("a direct Rune route keeps it on a pull request", () => {
    for (const c of [direct, shared, classifier.classify([])]) {
      expect(plan(c, "pull_request", true).runeIos).toBe(true);
    }
  });

  test("main policy always runs it, and nothing without Rune does", () => {
    expect(plan(closure, "push", true).runeIos).toBe(true);
    const docs = classifier.classify(["docs/SETUP.md"]);
    expect(planOutputs(plan(docs, "push"), docs)).toMatchObject({ rune: false, rune_ios: false });
  });

  test("a main push never reuses a pull request that skipped it", () => {
    const main = plan(closure, "push").coverage;
    expect(main.split(",")).toContain("rune_ios");
    expect(plan(closure, "pull_request").coverage).toBe(main);
    expect(plan(closure, "pull_request", true).coverage).not.toBe(main);
  });

  test("a reused main tree outputs no iOS job", () => {
    expect(planOutputs(reused(plan(direct, "push")), direct)).toMatchObject({ rune_ios: false });
  });
});
