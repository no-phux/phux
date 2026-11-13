import { describe, expect, test } from "bun:test";
import { Classifier } from "./classify.ts";
import {
  eventHasLabel,
  ICOUNT_PR_PATHS,
  LANES,
  NATIVE_SETUP_PATHS,
  PERF_BASELINE_LABEL,
  plan,
  planOutputs,
  reused,
  triggerFor,
  type Lane,
  type PathScope,
  type PlanOptions,
  type Trigger,
} from "./plan.ts";

const classifier = new Classifier();

function scopeOf(paths: readonly string[]): PathScope {
  return paths.length === 0 ? { kind: "full" } : { kind: "diff", paths };
}

function lanes(
  paths: string[],
  trigger: Trigger,
  extra: { perfBaseline?: boolean; scope?: PathScope } = {},
): Lane[] {
  const options: PlanOptions = { scope: extra.scope ?? scopeOf(paths) };
  const chosen = plan(classifier.classify(paths), trigger, {
    ...options,
    ...(extra.perfBaseline === undefined ? {} : { perfBaseline: extra.perfBaseline }),
  }).lanes;
  return LANES.filter((lane) => chosen.has(lane));
}

test.each<[string[], Trigger, Lane[]]>([
  [["docs/RELEASING.md"], "pull_request", ["guards"]],
  [["docs/RELEASING.md"], "push", ["guards", "icount", "native"]],
  // ci.yml defines both jobs, so a pull request that edits it runs every lane.
  [[".github/workflows/ci.yml"], "pull_request", [...LANES]],
  [[".github/workflows/release.yml"], "pull_request", ["guards"]],
  [[".github/ISSUE_TEMPLATE/bug.yml"], "pull_request", ["guards"]],
  [["clients/rune/src/main.rs"], "pull_request", ["guards", "rune"]],
  [["clients/rune/src/main.rs"], "push", ["guards", "icount", "native", "rune"]],
  [["integrations/pi/src/index.ts"], "pull_request", ["guards", "integrations"]],
  [["crates/phux-mcp/src/lib.rs"], "pull_request", ["guards", "lint", "tests", "e2e"]],
  [
    ["crates/phux-mcp/src/lib.rs"],
    "push",
    ["guards", "lint", "tests", "e2e", "ffi", "icount", "native"],
  ],
  [["crates/phux-mcp/benches/b.rs"], "pull_request", ["guards", "lint"]],
  [
    ["crates/phux-server/src/lib.rs"],
    "pull_request",
    ["guards", "lint", "tests", "e2e", "ffi", "web"],
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
  const paths = ["crates/phux-mcp/src/lib.rs"];
  const c = classifier.classify(paths);
  const options = { scope: scopeOf(paths) };
  // The receipt does not claim native setup or icount: this pull request ran neither.
  expect(plan(c, "push", options).coverage).toBe("guards,lint,tests,e2e");
  expect(plan(c, "pull_request", options).coverage).toBe("guards,lint,tests,e2e");
});

test("outputs name every lane and the web engine only inside the web lane", () => {
  const c = classifier.classify(["clients/phux-vt-web/vendor/ghostty-vt.wasm"]);
  const outputs = planOutputs(plan(c, "pull_request"), c);
  for (const lane of LANES) expect(typeof outputs[lane]).toBe("boolean");
  expect(outputs).toMatchObject({ web: true, web_engine: true });
});

test("a reused main tree still qualifies the binding, native setup, and icount", () => {
  const rustPaths = ["crates/phux-mcp/src/lib.rs"];
  const rust = plan(classifier.classify(rustPaths), "push", { scope: scopeOf(rustPaths) });
  expect([...reused(rust).lanes]).toEqual(["ffi", "icount", "native"]);
  const docsPaths = ["docs/SETUP.md"];
  const docs = plan(classifier.classify(docsPaths), "push", { scope: scopeOf(docsPaths) });
  expect([...reused(docs).lanes]).toEqual(["icount", "native"]);
  // The receipt lists the pull-request lanes, not the two jobs main re-runs.
  expect(docs.coverage).toBe("guards");
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

describe("native setup and rust icount", () => {
  test("the native setup list is the job's scripts, the files they read, and ci.yml", () => {
    expect([...NATIVE_SETUP_PATHS]).toEqual([
      ".github/workflows/ci.yml",
      "Cargo.toml",
      "Cargo.lock",
      ".cargo/**",
      "rust-toolchain.toml",
      "mise.toml",
      ".config/zig-toolchain.json",
      "clients/phux-web/Cargo.toml",
      "crates/**/build.rs",
      "crates/**/*.c",
      "crates/**/*.h",
      "crates/**/*.ld",
      "crates/**/*.lds",
      "scripts/test-dev-setup.sh",
      "scripts/setup-rust.sh",
      "scripts/install-zig.sh",
      "scripts/native-smoke.sh",
      "scripts/test-env-runner.sh",
      "scripts/doctor.sh",
      "scripts/check-docs.sh",
      "scripts/build-vt-wasm.sh",
      "scripts/prepare-vt-wasm.mjs",
      "scripts/smoke-vt-wasm.mjs",
      "scripts/check-ghostty-pin.sh",
      "scripts/check-ghostty-pin_test.sh",
      "scripts/lib/dev-toolchain.sh",
      "scripts/lib/output.sh",
      "scripts/lib/apple-toolchain-env.sh",
    ]);
    expect([...ICOUNT_PR_PATHS]).toEqual([
      "benchmarks/icount-baseline.json",
      "crates/phux-server/benches/icount.rs",
      "crates/phux-terminal/benches/terminal_icount.rs",
      "scripts/ci/perf-icount.sh",
      "just/perf.just",
      "tools/src/perf/**",
      ".github/workflows/ci.yml",
    ]);
  });

  test("a pull request without those paths skips both", () => {
    expect(lanes(["docs/RELEASING.md"], "pull_request")).toEqual(["guards"]);
    expect(lanes(["crates/phux-server/src/lib.rs"], "pull_request")).not.toContain("icount");
    expect(lanes(["crates/phux-server/src/lib.rs"], "pull_request")).not.toContain("native");
  });

  test("Cargo.lock, the root Cargo.toml, .cargo, and the test env runner trigger native setup on a pull request", () => {
    for (const path of [
      "Cargo.lock",
      "Cargo.toml",
      ".cargo/config.toml",
      ".cargo/nested/config.toml",
      "scripts/test-env-runner.sh",
    ]) {
      expect(lanes([path], "pull_request")).toContain("native");
    }
  });

  test("a pull request touching only a crate C file runs native setup", () => {
    expect(lanes(["crates/phux/src/term.c"], "pull_request")).toContain("native");
    expect(lanes(["crates/phux/include/phux.h"], "pull_request")).toContain("native");
    expect(lanes(["crates/phux/link.ld"], "pull_request")).toContain("native");
    expect(lanes(["crates/phux/link.lds"], "pull_request")).toContain("native");
  });

  test("a pull request touching only a crate build.rs runs native setup", () => {
    expect(lanes(["crates/phux/build.rs"], "pull_request")).toContain("native");
  });

  test("a pull request touching only a crate Cargo.toml skips native setup", () => {
    expect(lanes(["crates/phux-protocol/Cargo.toml"], "pull_request")).not.toContain("native");
    expect(lanes(["crates/phux/Cargo.toml"], "pull_request")).not.toContain("native");
    expect(lanes(["crates/phux-protocol/Cargo.toml"], "push")).toContain("native");
    expect(lanes(["crates/phux-protocol/Cargo.toml"], triggerFor("merge_group"))).toContain(
      "native",
    );
  });

  test("a pull request that changes a native-setup input runs native setup only", () => {
    const chosen = lanes(["scripts/install-zig.sh"], "pull_request");
    expect(chosen).toContain("native");
    expect(chosen).not.toContain("icount");
    expect(lanes(["scripts/setup-rust.sh"], "pull_request")).toContain("native");
    expect(lanes(["scripts/lib/dev-toolchain.sh"], "pull_request")).toContain("native");
  });

  test("a pull request that changes a baseline, bench, or icount harness runs icount", () => {
    for (const path of [
      "benchmarks/icount-baseline.json",
      "crates/phux-server/benches/icount.rs",
      "crates/phux-terminal/benches/terminal_icount.rs",
      "scripts/ci/perf-icount.sh",
      "just/perf.just",
      "tools/src/perf/icount.ts",
      "tools/src/perf/nested/extra.ts",
      ".github/workflows/ci.yml",
    ]) {
      expect(lanes([path], "pull_request")).toContain("icount");
    }
    // The baseline is not a setup input, so native setup stays off.
    expect(lanes(["benchmarks/icount-baseline.json"], "pull_request")).not.toContain("native");
    // Another bench is not this job's bench.
    expect(lanes(["crates/phux-terminal/benches/capture.rs"], "pull_request")).not.toContain(
      "icount",
    );
  });

  test("merge_group and push always run both", () => {
    for (const event of ["merge_group", "push"] as const) {
      expect(triggerFor(event)).toBe("push");
      const chosen = lanes(["docs/RELEASING.md"], triggerFor(event));
      expect(chosen).toContain("native");
      expect(chosen).toContain("icount");
    }
    expect(triggerFor("pull_request")).toBe("pull_request");
    expect(triggerFor("workflow_dispatch")).toBe("pull_request");
  });

  test("the perf-baseline label runs icount on a pull request", () => {
    const chosen = lanes(["docs/RELEASING.md"], "pull_request", { perfBaseline: true });
    expect(chosen).toContain("icount");
    expect(chosen).not.toContain("native");
    expect(
      eventHasLabel(
        { pull_request: { labels: [{ name: PERF_BASELINE_LABEL }, { name: "bug" }] } },
        PERF_BASELINE_LABEL,
      ),
    ).toBe(true);
    expect(
      eventHasLabel({ pull_request: { labels: [{ name: "bug" }] } }, PERF_BASELINE_LABEL),
    ).toBe(false);
    expect(eventHasLabel({ ref: "refs/heads/main" }, PERF_BASELINE_LABEL)).toBe(false);
    expect(eventHasLabel(null, PERF_BASELINE_LABEL)).toBe(false);
  });

  test("release metadata keeps them off a pull request and on main", () => {
    const none = { scope: { kind: "none" as const } };
    expect(lanes(["docs/RELEASING.md"], "pull_request", none)).toEqual(["guards"]);
    expect(lanes(["docs/RELEASING.md"], "push", none)).toEqual(["guards", "icount", "native"]);
  });
});
