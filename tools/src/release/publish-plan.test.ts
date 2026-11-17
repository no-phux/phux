// Publish decisions stay deterministic: no network, no clock.
import { describe, expect, test } from "bun:test";
import { Classifier } from "../routing/classify.ts";
import { plan, type Plan as PushPlan } from "../routing/plan.ts";
import {
  applyRuneGate,
  ciGreenResult,
  ciPushState,
  ciRefusal,
  decide,
  nativeReleaseProof,
  parseTag,
  releaseRefusal,
  runeMacosRefusal,
  splitLanes,
  validationState,
  type CiState,
  type Plan,
  type Release,
} from "./publish-plan.ts";
import { evaluateQueue, jobsRequiredBy, queueAppleName } from "./queue-ci.ts";

const rel = (tag: string, draft: boolean, sha: string): Release => ({ tag, draft, sha });
const green = (): string => "success";
const greenOnly =
  (sha: string) =>
  (commit: string): string =>
    commit === sha ? "success" : "failure";

test("parseTag accepts stable component versions only", () => {
  expect(
    ["v0.42.0", "pi-v0.28.0", "pi-extension-v0.3.0"].map((t) => parseTag(t)?.component),
  ).toEqual(["phux", "pi", "pi-extension"]);
  for (const tag of ["next", "v0.1.0-alpha.1", "v01.2.3", "pi-v0.1.0-beta.1", "pi-v0.1.0+b"]) {
    expect(parseTag(tag)).toBeUndefined();
  }
});

describe("validationState", () => {
  const run = {
    id: 1,
    head_sha: "abc",
    head_branch: "main",
    event: "push",
    path: ".github/workflows/ci.yml",
    status: "completed",
    conclusion: "success",
  };
  const state = (...runs: unknown[]): string =>
    validationState(runs, "abc", ".github/workflows/ci.yml");

  test("only the newest exact push run on main counts", () => {
    expect(state(run)).toBe("success");
    for (const [field, value] of [
      ["head_sha", "def"],
      ["event", "pull_request"],
      ["event", "merge_group"],
      ["head_branch", "topic"],
      ["head_branch", "gh-readonly-queue/main/pr-1"],
      ["path", "other.yml"],
    ] as const) {
      expect(state({ ...run, [field]: value })).toBe("pending");
    }
    expect(state(run, { ...run, id: 2, conclusion: "failure" })).toBe("failure");
    expect(state(run, { ...run, run_attempt: 2, status: "in_progress" })).toBe("pending");
    // A queue run is not a push, so it cannot change the push gate.
    const queued = {
      ...run,
      id: 3,
      event: "merge_group",
      head_branch: "gh-readonly-queue/main/pr-1",
      conclusion: "failure",
    };
    expect(state(run, queued)).toBe("success");
  });

  test("no push run is missing, and a run still going keeps its status", () => {
    const push = (status: string) => ({ ...run, status });
    expect(ciPushState([], "abc", ".github/workflows/ci.yml")).toBe("missing");
    expect(ciPushState([push("in_progress")], "abc", ".github/workflows/ci.yml")).toBe(
      "in_progress",
    );
    expect(ciPushState([push("queued")], "abc", ".github/workflows/ci.yml")).toBe("queued");
    expect(ciPushState([run], "abc", ".github/workflows/ci.yml")).toBe("success");
    expect(ciPushState([{ ...run, event: "merge_group" }], "abc", ".github/workflows/ci.yml")).toBe(
      "missing",
    );
  });
});

describe("decide", () => {
  test("an event publishes every component on a green commit", () => {
    const releases = [
      rel("v0.42.0", true, "abc"),
      rel("pi-v0.28.0", true, "abc"),
      rel("v0.41.0", false, "old"),
    ];
    expect(decide(releases, green, { eventSha: "abc" })).toEqual({
      publish: ["v0.42.0", "pi-v0.28.0"],
      delete: [],
      wait: false,
      fail: undefined,
      blocked: "",
    });
  });

  test("an event waits on running ci and fails on red ci for its own commit", () => {
    const rows = [rel("v0.42.0", true, "abc")];
    const waiting = decide(rows, () => "pending", { eventSha: "abc" });
    expect([waiting.publish, waiting.wait, waiting.fail]).toEqual([[], true, undefined]);
    const failed = decide(rows, () => "cancelled", { eventSha: "abc" });
    expect([failed.publish, failed.fail]).toEqual([
      [],
      "ci.yml is cancelled for abc; not publishing v0.42.0",
    ]);
  });

  test("an event sweeps other green drafts, but not red ones", () => {
    // A cancelled publish run can strand a draft; the next event ships it.
    expect(decide([rel("v0.42.0", true, "other")], green, { eventSha: "abc" }).publish).toEqual([
      "v0.42.0",
    ]);
    const red = decide([rel("v0.42.0", true, "other")], greenOnly("abc"), { eventSha: "abc" });
    expect([red.publish, red.fail]).toEqual([[], undefined]);
    expect(red.blocked).toContain("v0.42.0");
  });

  test("superseded drafts are deleted, not shipped", () => {
    const releases = [
      rel("v0.42.0", false, "new"),
      rel("v0.39.0", true, "old"),
      rel("pi-v0.27.1", false, "pub"),
      rel("pi-v0.24.0", true, "oldd"),
      rel("next", true, "moving"),
    ];
    const plan = decide(releases, green, { eventSha: "old" });
    expect(plan.publish).toEqual([]);
    expect(plan.delete.toSorted()).toEqual(["pi-v0.24.0", "v0.39.0"]);
  });

  test("reconcile ships each component's newest green draft", () => {
    const releases = [
      rel("pi-v0.1.0", false, "pub"),
      rel("pi-v0.2.0", true, "green"),
      rel("pi-v0.1.1", true, "patch"),
      rel("pi-extension-v0.3.0", true, "red"),
      rel("v0.42.0", true, "running"),
    ];
    const ci = (sha: string): string =>
      sha === "running" ? "in_progress" : greenOnly("green")(sha);
    const plan = decide(releases, ci);
    expect(plan.publish).toEqual(["pi-v0.2.0"]);
    expect(plan.blocked).toBe(
      "still draft, ci is not green: pi-extension-v0.3.0 (ci.yml failure for red)",
    );
    expect(plan.fail).toBeUndefined();
  });

  type DispatchCase = [string, Release[], string, CiState, string | undefined, string[]];
  const draft = [rel("pi-v0.1.0", true, "d")];
  const superseded = [rel("v0.42.0", false, "new"), rel("v0.39.0", true, "old")];
  test.each<DispatchCase>([
    ["a published tag is a no-op", [rel("v0.42.0", false, "abc")], "v0.42.0", green, undefined, []],
    ["a green draft ships", draft, "pi-v0.1.0", green, undefined, ["pi-v0.1.0"]],
    ["a red draft stays", draft, "pi-v0.1.0", () => "failure", "failure", []],
    ["an unknown tag fails", [], "v9.9.9", green, "no release named v9.9.9", []],
    ["a superseded draft fails", superseded, "v0.39.0", green, "older", []],
  ])("dispatch: %s", (_, releases, tag, ci, fail, publish) => {
    const plan = decide(releases, ci, { dispatchTag: tag });
    expect(plan.publish).toEqual(publish);
    if (fail === undefined) expect(plan.fail).toBeUndefined();
    else expect(plan.fail).toContain(fail);
  });
});

test("lanes split root and integrations", () => {
  expect(splitLanes(["v0.42.0", "pi-extension-v0.3.0", "next"])).toEqual({
    phux: "v0.42.0",
    integration: ["pi-extension-v0.3.0"],
  });
});

test("a tag needs a finished green ci on main", () => {
  expect(ciRefusal("success", "abc")).toBeUndefined();
  expect(ciRefusal("pending", "abc")).toContain("rerun when green");
  expect(ciRefusal("failure", "abc")).toContain("failure");
  expect(ciRefusal("cancelled", "abc")).toContain("cancelled");
});

test("rune-macos is required on every release commit", () => {
  expect(runeMacosRefusal("success", "bump")).toBeUndefined();
  expect(runeMacosRefusal("pending", "bump")).toContain("rune-macos");
  expect(runeMacosRefusal("failure", "bump")).toContain("failure");
  expect(releaseRefusal("success", "bump", "failure")).toContain("rune-macos is failure");
  expect(releaseRefusal("failure", "bump", "success")).toContain("ci.yml is failure");
  expect(releaseRefusal("success", "bump", "pending")).toBeDefined();
});

test.each(["pending", "failure", "cancelled", "missing", "success"])(
  "both tag and publication gates require exact native state %s to be green",
  (native) => {
    const proof = nativeReleaseProof("bump", native, true);
    expect(proof).toEqual({ evidence: "bump", state: native });
    const refusal = releaseRefusal("success", "bump", proof.state);
    expect(refusal === undefined).toBe(native === "success");
    const plan: Plan = {
      publish: ["v1.2.3"],
      delete: [],
      wait: false,
      fail: undefined,
      blocked: "",
    };
    const gated = applyRuneGate(
      plan,
      "bump",
      () => proof,
      () => "bump",
    );
    expect(gated.publish).toEqual(native === "success" ? ["v1.2.3"] : []);
  },
);

test("a cheap green without exact full-coverage proof is still pending", () => {
  expect(nativeReleaseProof("docs", "success", false)).toEqual({
    evidence: "docs",
    state: "pending",
  });
});

describe("applyRuneGate", () => {
  const ready = (publish: string[], extra: Partial<Plan> = {}): Plan => ({
    publish,
    delete: [],
    wait: false,
    fail: undefined,
    blocked: "",
    ...extra,
  });
  const shaOf = (tag: string): string => (tag.startsWith("pi-") ? "other" : "event");
  const lookup = (state: string) => (sha: string) => ({ evidence: sha, state });

  test("a green proof publishes, but unrelated last-commit changes waive nothing", () => {
    expect(applyRuneGate(ready(["v0.1.0"]), "event", lookup("success"), shaOf).publish).toEqual([
      "v0.1.0",
    ]);
    const quiet = applyRuneGate(
      ready(["v0.1.0"]),
      undefined,
      () => nativeReleaseProof("quiet", "success", false),
      () => "quiet",
    );
    expect(quiet.publish).toEqual([]);
    expect(quiet.blocked).toContain("pending");
    expect(quiet.fail).toBeUndefined();
  });

  test("the event's own commit waits on rune-macos and fails when it is red", () => {
    const waiting = applyRuneGate(
      ready(["v0.1.0", "pi-v0.2.0"]),
      "event",
      lookup("pending"),
      shaOf,
    );
    expect([waiting.publish, waiting.wait, waiting.fail]).toEqual([[], true, undefined]);
    const failed = applyRuneGate(ready(["v0.1.0"]), "event", lookup("failure"), shaOf);
    expect(failed.publish).toEqual([]);
    expect(failed.fail).toContain("rune-macos is failure for event");
    expect(failed.fail).toContain("v0.1.0");
  });

  test("another draft's red rune-macos stays a draft and does not fail the run", () => {
    const plan = applyRuneGate(
      ready(["v0.1.0", "pi-v0.2.0"]),
      "event",
      (sha) => ({ evidence: sha, state: sha === "event" ? "success" : "failure" }),
      shaOf,
    );
    expect(plan.publish).toEqual(["v0.1.0"]);
    expect(plan.fail).toBeUndefined();
    expect(plan.blocked).toContain("pi-v0.2.0");
    expect(plan.blocked).toContain("rune-macos failure for other");
  });

  test("a run already waiting on ci is left to that wait", () => {
    const waiting = ready([], { wait: true });
    expect(applyRuneGate(waiting, "event", lookup("failure"), shaOf)).toEqual(waiting);
  });
});

describe("merge_group ci-green", () => {
  const sha = "a".repeat(40);
  const classifier = new Classifier();
  const rust = ["crates/phux-mcp/src/lib.rs"];
  const pushPlan = (paths: string[]): PushPlan =>
    plan(classifier.classify(paths), "push", {
      scope: { kind: "diff", paths },
      runeQualify: true,
    });
  const planned = pushPlan(rust);
  const queueRun = {
    id: 9,
    head_sha: sha,
    head_branch: "gh-readonly-queue/main/pr-1-abc",
    event: "merge_group",
    path: ".github/workflows/ci.yml",
    status: "completed",
    conclusion: "success",
  };
  const jobs = (conclusion: (id: string) => string = () => "success", apple = "success") => [
    ...jobsRequiredBy(planned).map((required) => ({
      name: required.name,
      conclusion: conclusion(required.id),
      status: "completed",
    })),
    { name: queueAppleName(), conclusion: apple, status: "completed" },
  ];
  const decide = (
    run: typeof queueRun | undefined,
    rows: readonly unknown[],
    compare = "behind",
    lanesProved = false,
    nativeQualified = true,
    plan: PushPlan = planned,
  ) =>
    evaluateQueue(sha, run, rows, compare, plan, lanesProved, nativeQualified, nativeReleaseProof);

  test("a skipped Rust lane is not a pass", () => {
    expect(planned.lanes.has("lint")).toBe(true);
    const refusal = decide(
      queueRun,
      jobs((id) => (id === "lint" ? "skipped" : "success")),
    );
    expect(refusal).toContain("rust lint");
    expect(refusal).toContain("fallback (C): get a push-equivalent ci run on that exact SHA");
  });

  test("the full push-plan job set passes, and a missing native receipt does not", () => {
    expect(decide(queueRun, jobs())).toBeUndefined();
    expect(decide(queueRun, jobs(), "identical")).toBeUndefined();
    expect(decide(queueRun, jobs(), "behind", false, false)).toContain("native receipt");
  });

  test("a called rune-macos aggregate in the same run passes, including skipped other hosts", () => {
    const apple = queueAppleName();
    const rows = jobs().filter((row) => row.name !== apple);
    rows.push(
      { name: `${apple} / plan`, conclusion: "success", status: "completed" },
      { name: `${apple} / build / macos-arm64`, conclusion: "success", status: "completed" },
      { name: `${apple} / build / linux-x86_64`, conclusion: "skipped", status: "completed" },
      { name: `${apple} / rune-macos`, conclusion: "success", status: "completed" },
    );
    expect(decide(queueRun, rows)).toBeUndefined();
  });

  test("a rune push plan accepts skipped macOS legs inside the Linux rune call", () => {
    const paths = ["clients/rune/src/main.rs"];
    const runePlan = pushPlan(paths);
    expect(runePlan.lanes.has("rune")).toBe(true);
    const rows = jobsRequiredBy(runePlan).flatMap((required) =>
      required.id === "rune"
        ? [
            { name: "rune / linux-x86_64", conclusion: "success", status: "completed" },
            { name: "rune / linux-aarch64", conclusion: "success", status: "completed" },
            { name: "rune / macos-arm64", conclusion: "skipped", status: "completed" },
          ]
        : [{ name: required.name, conclusion: "success", status: "completed" }],
    );
    rows.push({ name: queueAppleName(), conclusion: "success", status: "completed" });
    expect(decide(queueRun, rows, "behind", false, true, runePlan)).toBeUndefined();
    const skipped = rows.map((row) =>
      row.name.startsWith("rune /") ? { ...row, conclusion: "skipped" } : row,
    );
    expect(decide(queueRun, skipped, "behind", false, true, runePlan)).toContain("rune");
  });

  test("a run on a different SHA is refused", () => {
    const refusal = decide({ ...queueRun, head_sha: "b".repeat(40) }, jobs());
    expect(refusal).toContain("b".repeat(40));
    expect(refusal).toContain("fallback (C)");
  });

  test("a ref outside gh-readonly-queue is refused", () => {
    expect(decide({ ...queueRun, head_branch: "main" }, jobs())).toContain("gh-readonly-queue/");
    expect(decide({ ...queueRun, head_branch: "topic" }, jobs())).toContain("fallback (C)");
  });

  test("a SHA that main does not contain is refused", () => {
    expect(decide(queueRun, jobs(), "ahead")).toContain("not reachable from main");
    expect(decide(queueRun, jobs(), "diverged")).toContain("fallback (C)");
  });

  test("a missing or failed rune-macos child is refused", () => {
    const rows = jobs().filter((row) => row.name !== queueAppleName());
    expect(decide(queueRun, rows)).toContain("no successful rune-macos child");
    expect(
      decide(
        queueRun,
        jobs(() => "success", "failure"),
      ),
    ).toContain("rune-macos child is failure");
    expect(
      decide(
        queueRun,
        jobs(() => "success", "skipped"),
      ),
    ).toContain("no successful rune-macos child");
  });

  test("push missing plus a qualifying merge_group run is accepted", () => {
    expect(
      ciGreenResult(sha, "missing", "pending", () => decide(queueRun, jobs())),
    ).toBeUndefined();
  });

  test("push in_progress plus a qualifying merge_group run refuses", () => {
    let consulted = false;
    const refusal = ciGreenResult(sha, "in_progress", "success", () => {
      consulted = true;
      return undefined;
    });
    expect(consulted).toBe(false);
    expect(refusal).toContain("rerun when green");
  });

  test("push queued plus a qualifying merge_group run refuses", () => {
    let consulted = false;
    const refusal = ciGreenResult(sha, "queued", "success", () => {
      consulted = true;
      return undefined;
    });
    expect(consulted).toBe(false);
    expect(refusal).toContain("rerun when green");
  });

  test.each(["waiting", "pending", "requested"])(
    "push %s plus a qualifying merge_group run refuses",
    (push) => {
      expect(ciGreenResult(sha, push, "success", () => undefined)).toContain("rerun when green");
    },
  );

  test("an identical-tree receipt covers a lane reuse would skip, and not one it keeps", () => {
    const skippedLint = jobs((id) => (id === "lint" ? "skipped" : "success"));
    expect(decide(queueRun, skippedLint, "behind", true)).toBeUndefined();
    const skippedFfi = jobs((id) => (id === "ffi" ? "skipped" : "success"));
    expect(decide(queueRun, skippedFfi, "behind", true)).toContain("rust ffi");
    expect(decide(queueRun, skippedLint, "behind", false)).toContain("rust lint");
  });
});
