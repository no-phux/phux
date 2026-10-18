// Publish decisions stay deterministic: no network, no clock.
import { describe, expect, test } from "bun:test";
import {
  decide,
  parseTag,
  splitLanes,
  validationState,
  type CiState,
  type Release,
} from "./publish-plan.ts";

const rel = (tag: string, draft: boolean, sha: string): Release => ({ tag, draft, sha });
const green = (): string => "success";
const greenOnly =
  (sha: string) =>
  (commit: string): string =>
    commit === sha ? "success" : "failure";

test("parseTag accepts stable component versions only", () => {
  expect(
    ["v0.42.0", "desktop-v0.28.0", "pi-extension-v0.3.0"].map((t) => parseTag(t)?.component),
  ).toEqual(["phux", "desktop", "pi-extension"]);
  for (const tag of ["next", "v0.1.0-alpha.1", "v01.2.3", "desktop-v0.1.0-beta.1", "pi-v0.1.0+b"]) {
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
      rel("desktop-v0.28.0", true, "abc"),
      rel("v0.41.0", false, "old"),
    ];
    expect(decide(releases, green, { eventSha: "abc" })).toEqual({
      publish: ["v0.42.0", "desktop-v0.28.0"],
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
      rel("desktop-v0.27.1", false, "pub"),
      rel("desktop-v0.24.0", true, "oldd"),
      rel("next", true, "moving"),
    ];
    const plan = decide(releases, green, { eventSha: "old" });
    expect(plan.publish).toEqual([]);
    expect(plan.delete.toSorted()).toEqual(["desktop-v0.24.0", "v0.39.0"]);
  });

  test("reconcile ships each component's newest green draft", () => {
    const releases = [
      rel("desktop-v0.1.0", false, "pub"),
      rel("desktop-v0.2.0", true, "green"),
      rel("desktop-v0.1.1", true, "patch"),
      rel("pi-extension-v0.3.0", true, "red"),
      rel("v0.42.0", true, "running"),
    ];
    const ci = (sha: string): string =>
      sha === "running" ? "in_progress" : greenOnly("green")(sha);
    const plan = decide(releases, ci);
    expect(plan.publish).toEqual(["desktop-v0.2.0"]);
    expect(plan.blocked).toBe(
      "still draft, ci is not green: pi-extension-v0.3.0 (ci.yml failure for red)",
    );
    expect(plan.fail).toBeUndefined();
  });

  type DispatchCase = [string, Release[], string, CiState, string | undefined, string[]];
  const draft = [rel("desktop-v0.1.0", true, "d")];
  const superseded = [rel("v0.42.0", false, "new"), rel("v0.39.0", true, "old")];
  test.each<DispatchCase>([
    ["a published tag is a no-op", [rel("v0.42.0", false, "abc")], "v0.42.0", green, undefined, []],
    ["a green draft ships", draft, "desktop-v0.1.0", green, undefined, ["desktop-v0.1.0"]],
    ["a red draft stays", draft, "desktop-v0.1.0", () => "failure", "failure", []],
    ["an unknown tag fails", [], "v9.9.9", green, "no release named v9.9.9", []],
    ["a superseded draft fails", superseded, "v0.39.0", green, "older", []],
  ])("dispatch: %s", (_, releases, tag, ci, fail, publish) => {
    const plan = decide(releases, ci, { dispatchTag: tag });
    expect(plan.publish).toEqual(publish);
    if (fail === undefined) expect(plan.fail).toBeUndefined();
    else expect(plan.fail).toContain(fail);
  });
});

test("lanes split root, desktop and integrations", () => {
  expect(splitLanes(["v0.42.0", "desktop-v0.28.0", "pi-extension-v0.3.0", "next"])).toEqual({
    phux: "v0.42.0",
    desktop: "desktop-v0.28.0",
    integration: ["pi-extension-v0.3.0"],
  });
});
