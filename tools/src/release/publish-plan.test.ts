// Publish decisions stay deterministic: no network, no clock.
import { describe, expect, test } from "bun:test";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { ROOT } from "../lib/repo.ts";
import { decide, parseTag, splitLanes, validationState, type Release } from "./publish-plan.ts";

const rel = (tag: string, draft: boolean, sha: string): Release => ({ tag, draft, sha });
const green = (): string => "success";

describe("parseTag", () => {
  test("root, desktop and integration tags", () => {
    expect(parseTag("v0.42.0")?.component).toBe("phux");
    expect(parseTag("desktop-v0.28.0")?.component).toBe("desktop");
    expect(parseTag("pi-extension-v0.3.0")?.component).toBe("pi-extension");
  });

  test("the moving channel is not a version", () => {
    expect(parseTag("next")).toBeUndefined();
  });

  test("stable desktop is a version and prereleases are not", () => {
    expect(parseTag("desktop-v0.1.0")?.component).toBe("desktop");
    for (const tag of [
      "v0.1.0-alpha.1",
      "v01.2.3",
      "pi-extension-v0.1.0-alpha.1",
      "desktop-v0.1.0-alpha.1",
      "desktop-v0.1.0-alpha.0",
      "desktop-v0.1.0-alpha.01",
      "desktop-v0.1.0-beta.1",
      "desktop-v0.1.0-alpha.1+build",
    ]) {
      expect(parseTag(tag)).toBeUndefined();
    }
  });
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
  const state = (runs: unknown[]): string =>
    validationState(runs, "abc", ".github/workflows/ci.yml");

  test("exact success", () => {
    expect(state([run])).toBe("success");
  });

  test("an unrelated run cannot release", () => {
    for (const [field, value] of [
      ["head_sha", "def"],
      ["event", "pull_request"],
      ["head_branch", "topic"],
      ["path", "other.yml"],
    ] as const) {
      expect(state([{ ...run, [field]: value }])).toBe("pending");
    }
  });

  test("a newer failed or running attempt wins", () => {
    const failed = { ...run, id: 2, conclusion: "failure" };
    expect(state([run, failed])).toBe("failure");
    expect(state([{ ...run, status: "in_progress" }])).toBe("pending");
  });
});

describe("decide", () => {
  test("an event publishes every component on a green commit", () => {
    const releases = [
      rel("v0.42.0", true, "abc"),
      rel("desktop-v0.28.0", true, "abc"),
      rel("v0.41.0", false, "old"),
    ];
    const plan = decide(releases, green, { eventSha: "abc" });
    expect(plan.publish).toEqual(["v0.42.0", "desktop-v0.28.0"]);
    expect(plan.wait).toBe(false);
    expect(plan.fail).toBeUndefined();
  });

  test("an event waits while ci is still running", () => {
    const plan = decide([rel("v0.42.0", true, "abc")], () => "pending", { eventSha: "abc" });
    expect(plan.publish).toEqual([]);
    expect(plan.wait).toBe(true);
    expect(plan.fail).toBeUndefined();
  });

  test("an event does not publish when ci failed", () => {
    const plan = decide([rel("desktop-v0.28.0", true, "abc")], () => "cancelled", {
      eventSha: "abc",
    });
    expect(plan.publish).toEqual([]);
    expect(plan.fail).toContain("cancelled");
  });

  test("an event sweeps a draft left by a cancelled run", () => {
    // GitHub keeps one pending run per concurrency group and cancels the
    // older one, so the publish run a draft waits on can vanish before it
    // starts. The next event must ship that draft instead of stranding it
    // until the daily reconcile.
    const plan = decide([rel("v0.42.0", true, "other")], green, { eventSha: "abc" });
    expect(plan.publish).toEqual(["v0.42.0"]);
    expect(plan.wait).toBe(false);
    expect(plan.fail).toBeUndefined();
  });

  test("an event does not ship a straggler whose ci is not green", () => {
    const ciState = (sha: string): string => (sha === "abc" ? "success" : "failure");
    const plan = decide([rel("v0.42.0", true, "other")], ciState, { eventSha: "abc" });
    expect(plan.publish).toEqual([]);
    expect(plan.blocked).toContain("v0.42.0");
    expect(plan.fail).toBeUndefined();
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

  test("reconcile ships the newest green draft and reports the blocked one", () => {
    const releases = [
      rel("desktop-v0.27.1", false, "pub"),
      rel("desktop-v0.28.0", true, "green"),
      rel("pi-extension-v0.3.0", true, "red"),
    ];
    const ciState = (sha: string): string => (sha === "green" ? "success" : "failure");
    const plan = decide(releases, ciState);
    expect(plan.publish).toEqual(["desktop-v0.28.0"]);
    expect(plan.blocked).toContain("pi-extension-v0.3.0");
    expect(plan.fail).toBeUndefined();
  });

  test("reconcile skips a draft whose ci has not finished", () => {
    const plan = decide([rel("v0.42.0", true, "abc")], () => "pending");
    expect(plan.publish).toEqual([]);
    expect(plan.blocked).toBe("");
  });

  test("only the newest unpublished draft is a candidate", () => {
    const releases = [
      rel("v0.41.0", false, "pub"),
      rel("v0.42.0", true, "mid"),
      rel("v0.43.0", true, "tip"),
    ];
    expect(decide(releases, green).publish).toEqual(["v0.43.0"]);
  });

  test("dispatch of a published tag is a no-op", () => {
    const plan = decide([rel("v0.42.0", false, "abc")], green, { dispatchTag: "v0.42.0" });
    expect(plan.publish).toEqual([]);
    expect(plan.fail).toBeUndefined();
  });

  test("dispatch refuses a tag ci did not pass", () => {
    const plan = decide([rel("desktop-v0.28.0", true, "abc")], () => "failure", {
      dispatchTag: "desktop-v0.28.0",
    });
    expect(plan.publish).toEqual([]);
    expect(plan.fail).toContain("failure");
  });

  test("dispatch refuses a superseded draft", () => {
    const releases = [rel("v0.42.0", false, "new"), rel("v0.39.0", true, "old")];
    expect(decide(releases, green, { dispatchTag: "v0.39.0" }).fail).toContain("older");
  });

  test("dispatch of an unknown tag fails", () => {
    expect(decide([], green, { dispatchTag: "v9.9.9" }).fail).toBe("no release named v9.9.9");
  });

  test("lanes split root, desktop and integrations", () => {
    const lanes = splitLanes(["v0.42.0", "desktop-v0.28.0", "pi-extension-v0.3.0"]);
    expect(lanes).toEqual({
      phux: "v0.42.0",
      desktop: "desktop-v0.28.0",
      integration: ["pi-extension-v0.3.0"],
    });
  });

  test("the desktop lane takes the newest stable draft", () => {
    const releases = [
      rel("desktop-v0.1.0", false, "old"),
      rel("desktop-v0.2.0", true, "minor"),
      rel("desktop-v0.1.1", true, "patch"),
    ];
    const plan = decide(releases, green);
    expect(plan.publish).toEqual(["desktop-v0.2.0"]);
    expect(splitLanes(plan.publish)).toEqual({
      phux: "",
      desktop: "desktop-v0.2.0",
      integration: [],
    });
  });

  test("stable desktop supersedes an older stable draft", () => {
    const plan = decide(
      [rel("desktop-v0.2.0", false, "stable"), rel("desktop-v0.1.0", true, "old")],
      green,
    );
    expect(plan.publish).toEqual([]);
    expect(plan.delete).toEqual(["desktop-v0.1.0"]);
  });

  test("desktop dispatch requires its exact commit's ci", () => {
    const tag = "desktop-v0.1.0";
    const rows = [rel(tag, true, "desktop")];
    expect(decide(rows, green, { dispatchTag: tag }).publish).toEqual([tag]);
    expect(decide(rows, () => "failure", { dispatchTag: tag }).fail).toContain("failure");
    // An event for any other commit sweeps it like every other green draft.
    expect(decide(rows, green, { eventSha: "other" }).publish).toEqual([tag]);
  });
});

// publish.yml holds one concurrency slot; nothing slow may sit inside it.
describe("publish workflow", () => {
  const read = (name: string) => readFileSync(join(ROOT, ".github/workflows", name), "utf8");
  const workflow = read("publish.yml");
  const job = (name: string) => workflow.split(`\n  ${name}:\n`)[1]?.split("\n\n  ")[0] ?? "";

  test("mobile FFI is dispatched after the CLI is public", () => {
    for (const name of ["ffi-xcframework.yml", "ffi-android.yml"]) {
      expect(workflow).not.toContain(`uses: ./.github/workflows/${name}`);
    }
    const mobile = job("mobile-ffi");
    expect(mobile).toContain("needs: [plan, phux]");
    expect(mobile).toContain("if: needs.phux.result == 'success'");
    expect(mobile).toContain("for workflow in ffi-xcframework.yml ffi-android.yml; do");
    expect(mobile).toContain('gh workflow run "$workflow" --ref main -f tag="$TAG"');
  });

  test("a publish rebuilds the site", () => {
    const website = job("website");
    expect(website).toContain("needs: [phux, desktop, channel-manifest]");
    expect(website).toContain(
      "needs.phux.result == 'success' || needs.desktop.result == 'success'",
    );
    expect(website).toContain("gh workflow run website-dispatch.yml --ref main");
    const dispatch = read("website-dispatch.yml").split("\npermissions:")[0] ?? "";
    expect(dispatch).toContain("  workflow_dispatch:\n");
  });
});
