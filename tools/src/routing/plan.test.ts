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
  const options: PlanOptions = { pullRequestFast: true, scope: extra.scope ?? scopeOf(paths) };
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
  const options = { pullRequestFast: true, scope: scopeOf(paths) };
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

test("the ci receipt separates full Linux qualification and never claims Apple coverage", () => {
  // The Linux rune lane still runs. macOS coverage would let a main push
  // reuse a pull request that never started a macOS runner.
  const closure = classifier.classify(["crates/phux-protocol/src/lib.rs"]);
  const pr = plan(closure, "pull_request", { pullRequestFast: true });
  const main = plan(closure, "push");
  expect(pr.lanes.has("rune")).toBe(true);
  expect(main.lanes.has("rune")).toBe(true);
  expect(main.coverage).toBe(`${pr.coverage},linux-aarch64,packaging`);
  expect(pr.coverage.split(",")).not.toContain("rune_ios");
  expect(pr.coverage.split(",")).not.toContain("macos");
  expect(planOutputs(pr, closure)).not.toHaveProperty("rune_ios");
  expect(planOutputs(reused(main), closure)).not.toHaveProperty("rune_ios");
  expect([...reused(main).lanes]).not.toContain("rune");
});

test("legacy queue booleans cannot be mistaken for native scope or Apple qualification", () => {
  const paths = ["crates/phux-client-runtime/src/lib.rs"];
  const c = classifier.classify(paths);
  const options: PlanOptions = { pullRequestFast: true, scope: { kind: "diff", paths } };
  expect(planOutputs(plan(c, "pull_request", options), c)).toMatchObject({
    native: false,
    icount: false,
    rune: true,
  });
  for (const legacy of [false, true]) {
    // A legacy boolean supplies no path scope, so those lanes fail closed.
    const old = plan(c, "pull_request", legacy);
    expect(planOutputs(old, c)).toMatchObject({ native: true, icount: true, rune: true });
    expect(old.coverage).not.toContain("rune_ios");
    expect(old.coverage).not.toContain("macos");
  }
  expect(plan(c, "pull_request", { ...options, mergeQueue: true }).coverage).toBe(
    plan(c, "pull_request", options).coverage,
  );
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
      "scripts/zig-mirrors/build.zig",
      "scripts/zig-mirrors/build.zig.zon",
      "scripts/prepare-vt-wasm.mjs",
      "scripts/smoke-vt-wasm.mjs",
      "scripts/check-ghostty-pin.sh",
      "scripts/check-ghostty-pin_test.sh",
      "scripts/lib/dev-toolchain.sh",
      "scripts/lib/output.sh",
      "scripts/lib/apple-toolchain-env.sh",
      "scripts/lib/mbx-target.sh",
    ]);
    expect([...ICOUNT_PR_PATHS]).toEqual([
      "benchmarks/icount-baseline.json",
      "crates/phux-server/benches/icount.rs",
      "crates/phux-terminal/benches/terminal_icount.rs",
      "scripts/ci/perf-icount.sh",
      "just/perf.just",
      "tools/src/perf/**",
      ".github/actions/setup-rust-lane/**",
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
    expect(lanes(["scripts/lib/mbx-target.sh"], "pull_request")).toContain("native");
  });

  test.each(["scripts/zig-mirrors/build.zig", "scripts/zig-mirrors/build.zig.zon"])(
    "a fast-path pull request changing only %s runs native setup and the web engine",
    (path) => {
      const c = classifier.classify([path]);
      const selected = plan(c, "pull_request", {
        pullRequestFast: true,
        scope: { kind: "diff", paths: [path] },
      });
      expect(planOutputs(selected, c)).toMatchObject({ native: true, web_engine: true });
    },
  );

  test("a pull request that changes a baseline, bench, or icount harness runs icount", () => {
    for (const path of [
      "benchmarks/icount-baseline.json",
      "crates/phux-server/benches/icount.rs",
      "crates/phux-terminal/benches/terminal_icount.rs",
      "scripts/ci/perf-icount.sh",
      "just/perf.just",
      "tools/src/perf/icount.ts",
      "tools/src/perf/nested/extra.ts",
      ".github/actions/setup-rust-lane/action.yml",
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
