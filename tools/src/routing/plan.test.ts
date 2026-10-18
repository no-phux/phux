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
  [["clients/desktop/native/src/main.rs"], "pull_request", ["guards", "desktop"]],
  // Release qualification builds desktop on main; the push is guards only.
  [["clients/desktop/native/src/main.rs"], "push", ["guards"]],
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
  const c = classifier.classify(["clients/desktop/native/src/main.rs"]);
  expect(plan(c, "push").coverage).toBe("guards,desktop");
  expect(plan(c, "pull_request").coverage).toBe("guards,desktop");
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
