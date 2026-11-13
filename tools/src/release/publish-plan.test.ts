// Publish decisions stay deterministic: no network, no clock.
import { describe, expect, test } from "bun:test";
import {
  applyRuneGate,
  ciRefusal,
  decide,
  isReleaseBot,
  parseTag,
  releaseRefusal,
  runeMacosRefusal,
  runeMacosTarget,
  splitLanes,
  validationState,
  type CiState,
  type Plan,
  type Release,
  type RuneEvidence,
} from "./publish-plan.ts";

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
      ["head_branch", "topic"],
      ["path", "other.yml"],
    ] as const) {
      expect(state({ ...run, [field]: value })).toBe("pending");
    }
    expect(state(run, { ...run, id: 2, conclusion: "failure" })).toBe("failure");
    expect(state(run, { ...run, run_attempt: 2, status: "in_progress" })).toBe("pending");
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

const evidence = (sha: string, rune: boolean, bot = false, parent?: string): RuneEvidence =>
  parent === undefined ? { sha, bot, rune } : { sha, bot, rune, parent };

test("release-bot identity is the app, not a person who mentions it", () => {
  expect(
    isReleaseBot("phux-release-bot[bot]\n305320402+phux-release-bot[bot]@users.noreply.github.com"),
  ).toBe(true);
  expect(isReleaseBot("phux-release[bot]\nphux-release[bot]@users.noreply.github.com")).toBe(true);
  expect(isReleaseBot("Release Test\nrelease@example.com")).toBe(false);
  expect(isReleaseBot("note phux-release-bot[bot] in the message")).toBe(false);
});

test("rune-macos is required on the release commit when Rune is in its closure", () => {
  const commit = evidence("bump", true, true, "parent");
  expect(runeMacosTarget(commit, evidence("parent", true), "success")).toBe("bump");
  expect(runeMacosRefusal("success", "bump")).toBeUndefined();
  expect(runeMacosRefusal("pending", "bump")).toContain("rune-macos");
  expect(runeMacosRefusal("failure", "bump")).toContain("failure");
  expect(releaseRefusal("success", "bump", "failure", "bump")).toContain("rune-macos is failure");
  expect(releaseRefusal("failure", "bump", "success", "bump")).toContain("ci.yml is failure");
  expect(releaseRefusal("success", "bump", "pending", undefined)).toBeUndefined();
});

test("a release-bot commit without Rune uses its validated parent", () => {
  const commit = evidence("bump", false, true, "parent");
  const parent = evidence("parent", true);
  expect(runeMacosTarget(commit, parent, "success")).toBe("parent");
  expect(runeMacosTarget(commit, parent, "pending")).toBeUndefined();
  expect(runeMacosTarget(commit, evidence("parent", false), "success")).toBeUndefined();
  expect(
    runeMacosTarget(evidence("human", false, false, "parent"), parent, "success"),
  ).toBeUndefined();
  expect(runeMacosTarget(evidence("bump", false, true), parent, "success")).toBeUndefined();
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
  const lookup = (state: string) => (sha: string) =>
    sha === "quiet" ? undefined : { evidence: sha, state };

  test("a green proof publishes, and a commit outside the closure needs nothing", () => {
    expect(applyRuneGate(ready(["v0.1.0"]), "event", lookup("success"), shaOf).publish).toEqual([
      "v0.1.0",
    ]);
    const quiet = applyRuneGate(
      ready(["v0.1.0"]),
      undefined,
      () => undefined,
      () => "quiet",
    );
    expect(quiet.publish).toEqual(["v0.1.0"]);
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
