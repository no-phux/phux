// Exercise the actual admission/gate expressions, not a parallel policy model.
import { expect, test } from "bun:test";
import { runInNewContext } from "node:vm";
import { capture } from "../lib/process.ts";
import { str, table, type Table } from "../lib/value.ts";
import { action, job, needs, run, steps, uses, workflow, withInput } from "../policy/workflows.ts";
import { Classifier } from "./classify.ts";
import { runeMacosPlan } from "./rune-macos.ts";

function evaluate(value: unknown, github: Table): unknown {
  if (typeof value !== "string") return value;
  return runInNewContext(value.replace(/^\$\{\{\s*|\s*\}\}$/g, ""), {
    github,
    vars: { RUNE_AUTO_RELEASE: "true" },
    cancelled: () => false,
    format: (pattern: string, ...args: string[]) =>
      pattern.replace(/\{(\d+)\}/g, (_, index: string) => String(args[Number(index)])),
  });
}

function group(file: string, github: Table): string {
  const value = str(table(workflow(file)["concurrency"])["group"]) ?? "";
  return value.replace(/\$\{\{([\s\S]*?)\}\}/g, (_, body: string) =>
    String(evaluate(body, github)),
  );
}

const push: Table = {
  event_name: "push",
  actor: "maintainer",
  repository: "no-phux/phux",
  ref: "refs/heads/main",
  ref_name: "main",
  sha: "a".repeat(40),
  event: {},
};

test("the split preserves real native, trust, transport and signed-byte qualification", () => {
  const build = workflow("rune-build.yml");
  const ios = steps(job(build, "ios-native")).map(run).join("\n");
  for (const command of [
    "aarch64-apple-ios-sim aarch64-apple-ios",
    "test_ios*.py",
    "--target simulator",
    "--target device",
    "swift test",
    "--force-resolved-versions",
    "--transport wss",
    '"$PWD/target/debug/phux"',
  ])
    expect(ios).toContain(command);
  expect(ios.match(/python3 clients\/rune\/ios\/qualify.py/g)?.length).toBe(2);
  const desktop = steps(action("rune-host")).map(run).join("\n");
  for (const command of [
    "clients/rune/scripts/check.sh",
    "clients/rune/scripts/package.sh",
    "sign-macos-desktop.sh --check-config",
    "xcrun stapler validate",
    "spctl --assess",
    "clients/rune/scripts/qualify.sh",
  ])
    expect(desktop).toContain(command);
  // Tags still select all hosts and iOS by default, not the PR Linux subset.
  expect(withInput(job(workflow("rune-client.yml"), "build"), "hosts")).toBeUndefined();
  const inputs = table(table(table(build["on"])["workflow_call"])["inputs"]);
  expect(table(inputs["hosts"])["default"]).toBe("all");
  expect(table(inputs["ios"])["default"]).toBe(true);
});

test.each(["ci.yml", "rune-macos.yml"])(
  "%s cannot replace pending native A with docs-only B",
  (file) => {
    const classifier = new Classifier();
    const a = runeMacosPlan(classifier.classify(["clients/rune/ios/tailnet.py"]), "push");
    const b = runeMacosPlan(classifier.classify(["docs/install.md"]), "push");
    const newer = { ...push, sha: "b".repeat(40) };
    const admitted = new Map([
      [group(file, push), a],
      [group(file, newer), b],
    ]);
    expect(admitted.size).toBe(2);
    expect(admitted.get(group(file, push))).toMatchObject({ macos: true, ios: true });
    expect(admitted.get(group(file, newer))).toMatchObject({ macos: false, ios: false });
    expect(evaluate(table(workflow(file)["concurrency"])["cancel-in-progress"], push)).toBe(false);
  },
);

test.each(["ci.yml", "rune-macos.yml"])(
  "%s keys preserve repository and full-ref boundaries",
  (file) => {
    const dispatch = {
      ...push,
      event_name: "workflow_dispatch",
      ref: "refs/heads/foo-bar",
      ref_name: "foo-bar",
    };
    const pr = {
      ...push,
      event_name: "pull_request",
      head_ref: "foo-bar",
      event: { pull_request: { head: { repo: { full_name: "no-phux/phux" } } } },
    };
    expect(group(file, pr)).toBe(group(file, dispatch));
    expect(group(file, pr)).not.toBe(group(file, { ...dispatch, ref: "refs/tags/foo-bar" }));
    const fork = {
      ...pr,
      head_ref: "bar",
      event: { pull_request: { head: { repo: { full_name: "no-phux/phux-foo" } } } },
    };
    expect(group(file, fork)).not.toBe(group(file, dispatch));
  },
);

test("the existing required ci check depends on real Apple qualification on the queue", () => {
  const ci = workflow("ci.yml");
  const apple = job(ci, "rune-macos");
  expect(uses(apple)).toBe("./.github/workflows/rune-macos.yml");
  expect(str(apple["if"])).toBe("github.event_name == 'merge_group'");
  expect(needs(job(ci, "ci"))).toContain("rune-macos");
  const mac = workflow("rune-macos.yml");
  expect(table(mac["on"])["workflow_call"]).toBeDefined();
  // Only ci calls the queue leg: no second standalone native queue build.
  expect(table(mac["on"])["merge_group"]).toBeUndefined();
  expect(table(mac["on"])["schedule"]).toEqual([{ cron: "47 11 * * *" }]);
  expect(table(mac["on"])["workflow_dispatch"]).toBeDefined();
});

test("ci rejects a failed or cancelled native queue dependency", () => {
  const script = run(steps(job(workflow("ci.yml"), "ci"))[0] ?? {});
  expect(needs(job(workflow("ci.yml"), "ci"))).toContain("rune-macos");
  for (const state of ["success", "failure", "cancelled", "skipped"]) {
    const proc = Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
      env: {
        ...process.env,
        REQUIRE_APPLE: "true",
        RESULTS: JSON.stringify({
          plan: { result: "success" },
          "rune-macos": { result: state },
        }),
      },
      stdout: "ignore",
      stderr: "ignore",
    });
    expect([state, proc.exitCode]).toEqual([state, state === "success" ? 0 : 1]);
  }
});

test("the native aggregate cannot waive a selected build as skipped", () => {
  const script = run(steps(job(workflow("rune-macos.yml"), "rune-macos"))[0] ?? {});
  for (const [selected, state, exit] of [
    ["true", "success", 0],
    ["true", "skipped", 1],
    ["true", "failure", 1],
    ["true", "cancelled", 1],
    ["false", "skipped", 0],
  ] as const) {
    const proc = Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
      env: {
        ...process.env,
        RESULTS: JSON.stringify({
          plan: { result: "success", outputs: { macos: selected } },
          build: { result: state },
        }),
      },
      stdout: "ignore",
      stderr: "ignore",
    });
    expect([selected, state, proc.exitCode]).toEqual([selected, state, exit]);
  }
});

test("native completion reconciles drafts without running release-please a second time", () => {
  const release = workflow("release.yml");
  expect(table(table(release["on"])["workflow_run"])["workflows"]).toEqual(["ci", "rune-macos"]);
  const completed = {
    ...push,
    event_name: "workflow_run",
    event: {
      workflow_run: {
        name: "rune-macos",
        event: "push",
        head_branch: "main",
        conclusion: "success",
        head_repository: { full_name: "no-phux/phux" },
      },
    },
  };
  expect(evaluate(job(release, "plan")["if"], completed)).toBe(true);
  expect(evaluate(job(release, "release-please")["if"], completed)).toBe(false);
  const auto = workflow("rune-auto-release.yml");
  expect(table(table(auto["on"])["workflow_run"])["workflows"]).toEqual(["ci", "rune-macos"]);
  expect(evaluate(job(auto, "step")["if"], completed)).toBe(true);
  const ciCompleted = {
    ...completed,
    event: {
      workflow_run: {
        ...table(table(completed.event).workflow_run),
        name: "ci",
      },
    },
  };
  expect(evaluate(job(release, "release-please")["if"], ciCompleted)).toBe(true);
  for (const changed of [
    { event: "pull_request" },
    { head_branch: "topic" },
    { name: "unrelated" },
    { head_repository: { full_name: "fork/phux" } },
  ]) {
    const event = table(completed.event);
    const untrusted = {
      ...completed,
      event: {
        workflow_run: { ...table(event.workflow_run), ...changed },
      },
    };
    expect(evaluate(job(release, "plan")["if"], untrusted)).toBe(false);
    expect(evaluate(job(release, "release-please")["if"], untrusted)).toBe(false);
    expect(evaluate(job(auto, "step")["if"], untrusted)).toBe(false);
  }
  // Both tag consumers use the same fail-closed gate.
  expect(
    steps(job(workflow("rune-client.yml"), "preflight"))
      .map(run)
      .join("\n"),
  ).toContain("ci-green");
  expect(capture(["git", "show", "HEAD:clients/rune/scripts/release.py"])).toContain('"ci-green"');
});
