// Exercise the actual admission/gate expressions, not a parallel policy model.
import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { runInNewContext } from "node:vm";
import { capture } from "../lib/process.ts";
import { repoPath } from "../lib/repo.ts";
import { str, table, type Table } from "../lib/value.ts";
import {
  action,
  job,
  jobs,
  needs,
  run,
  steps,
  uses,
  workflow,
  workflowNames,
  withInput,
} from "../policy/workflows.ts";
import { Classifier } from "./classify.ts";
import { runeMacosPlan } from "./rune-macos.ts";
import { LANES } from "./plan.ts";

function evaluate(value: unknown, github: Table, extra: Table = {}): unknown {
  if (typeof value !== "string") return value;
  return runInNewContext(value.replace(/^\$\{\{\s*|\s*\}\}$/g, ""), {
    github,
    inputs: { qualify: false },
    vars: { RUNE_AUTO_RELEASE: "true" },
    cancelled: () => false,
    format: (pattern: string, ...args: string[]) =>
      pattern.replace(/\{(\d+)\}/g, (_, index: string) => String(args[Number(index)])),
    ...extra,
  });
}

function group(file: string, github: Table, extra: Table = {}): string {
  const value = str(table(workflow(file)["concurrency"])["group"]) ?? "";
  return value.replace(/\$\{\{([\s\S]*?)\}\}/g, (_, body: string) =>
    String(evaluate(body, github, extra)),
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

test("throwaway qualification CLIs skip interactive dev optimization", () => {
  expect(job(workflow("ci.yml"), "native")["env"]).toMatchObject({
    CARGO_PROFILE_DEV_OPT_LEVEL: "0",
  });
  const rune = workflow("rune-build.yml");
  expect(rune["env"]).toMatchObject({
    CARGO_INCREMENTAL: "0",
    CARGO_PROFILE_DEV_DEBUG: "0",
    CARGO_PROFILE_DEV_OPT_LEVEL: "0",
  });
  for (const name of ["linux-x86_64", "linux-aarch64", "macos", "ios-native"]) {
    expect(job(rune, name)["env"]).toBeUndefined();
  }
});

test("the split preserves real native, trust, transport and signed-byte qualification", () => {
  const build = workflow("rune-build.yml");
  const ios = steps(job(build, "ios-native")).map(run).join("\n");
  for (const command of [
    "aarch64-apple-ios-sim aarch64-apple-ios",
    "test_*.py",
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
  expect(table(inputs["qualify"])["default"]).toBe(true);
  const host = steps(action("rune-host"));
  const packaged = host.filter((step) => {
    const name = str(step["name"]) ?? "";
    return name === "Package" || name.startsWith("Launch the packaged app");
  });
  expect(packaged.map((step) => str(step["if"]))).toEqual([
    "inputs.qualify == 'true'",
    "inputs.qualify == 'true'",
  ]);
  const signing = host.filter((step) => run(step).includes("sign-macos-desktop.sh"));
  expect(signing.length).toBe(2);
  for (const step of signing) expect(str(step["if"])).toContain("inputs.qualify == 'true'");
  const uploads = host.filter((step) => uses(step).startsWith("actions/upload-artifact@"));
  expect(uploads.length).toBe(2);
  for (const step of uploads) expect(str(step["if"])).toContain("inputs.qualify == 'true'");
  const x86 = job(build, "linux-x86_64");
  const arm = job(build, "linux-aarch64");
  expect(str(x86["name"])).toBe("linux-x86_64");
  expect(steps(x86).map(run).join("\n")).not.toContain("test_*.py");
  expect(str(arm["name"])).toBe("linux-aarch64");
  expect(str(arm["runs-on"])).toBe("ubuntu-24.04-arm");
  expect(str(arm["if"])).toContain("inputs.qualify");
  expect(
    withInput(steps(x86).find((step) => uses(step).includes("rune-host")) ?? {}, "qualify"),
  ).toBe("${{ inputs.qualify }}");
  // A tag and the nightly call pass no qualify input, so the default stays on.
  expect(withInput(job(workflow("rune-client.yml"), "build"), "qualify")).toBeUndefined();
  expect(withInput(job(workflow("cache-warm.yml"), "rune"), "qualify")).toBeUndefined();
});

test("macOS tests the final archive's TMD through the ignored library adapter before upload", () => {
  const host = steps(action("rune-host"));
  const launch = host.find((step) => str(step["name"])?.startsWith("Launch the packaged app"));
  expect(launch?.["if"]).toBe("inputs.qualify == 'true'");
  const script = run(launch ?? {});
  expect(script.startsWith("set -euo pipefail\n")).toBe(true);
  expect(script.indexOf("clients/rune/scripts/qualify.sh")).toBeLessThan(
    script.indexOf('if [[ "$(uname -s)" == Darwin ]]'),
  );
  for (const command of [
    'evidence="$RUNNER_TEMP/qualification/tmd-adapter"',
    'unpack="$RUNNER_TEMP/tmd-adapter-unpacked"',
    'ditto -x -k "$archive" "$unpack"',
    'app="$unpack/Phux Rune.app"',
    'export TMD_TEST_BIN="$app/Contents/MacOS/tmd"',
    '--package phux-rune --lib --all-features "$filter"',
    '-- --ignored --exact --nocapture 2>&1 | tee "$evidence/adapter.log"',
    'grep -Fq "test $filter ... ok"',
    "test result: ok\\. 1 passed; 0 failed;",
    '"$evidence/context.log"',
    '"$evidence/tmd-VERSION.txt"',
    '"$evidence/sha256.txt"',
  ]) {
    expect(script).toContain(command);
  }
  expect(script).not.toContain("--release");
  expect(script).not.toContain("python3 clients/rune/scripts/bundle_tmd.py");
  expect(host.indexOf(launch ?? {})).toBeLessThan(
    host.findIndex((step) => uses(step).startsWith("actions/upload-artifact@")),
  );
});

test("the adapter output guards reject zero tests and preserve Cargo failures in the log", () => {
  const script = run(
    steps(action("rune-host")).find((step) =>
      str(step["name"])?.startsWith("Launch the packaged app"),
    ) ?? {},
  );
  const guard = [
    script.split("\n")[0],
    script.slice(script.indexOf("filter=document_viewer"), script.lastIndexOf("\nfi")),
  ].join("\n");
  const directory = mkdtempSync(repoPath("tools/.tmp-tmd-adapter-"));
  const bin = join(directory, "bin");
  const filter = "document_viewer::tests::pinned_tmd_renders_markdown_through_the_actual_adapter";
  const passed = `test ${filter} ... ok\ntest result: ok. 1 passed; 0 failed;`;
  try {
    mkdirSync(bin);
    // Synthetic Cargo output only: never build Rust or launch a packaged app.
    writeFileSync(
      join(bin, "cargo"),
      '#!/bin/sh\nprintf "%s\\n" "$TMD_FAKE_OUTPUT"\nprintf "fixture stderr\\n" >&2\nexit "$TMD_FAKE_STATUS"\n',
      { mode: 0o755 },
    );
    for (const [output, status, expected] of [
      [passed, "0", 0],
      ["test result: ok. 0 passed; 0 failed;", "0", 1],
      [`test ${filter} ... ok\ntest result: ok. 0 passed; 0 failed;`, "0", 1],
      [passed.replace("1 passed", "2 passed"), "0", 1],
      [passed.replace("0 failed", "1 failed"), "0", 1],
      ["test another_test ... ok\ntest result: ok. 1 passed; 0 failed;", "0", 1],
      [passed, "7", 7],
      ["test result: FAILED. 0 passed; 1 failed;", "101", 101],
    ] as const) {
      const result = Bun.spawnSync(["bash", "-c", guard], {
        cwd: repoPath("."),
        env: {
          ...process.env,
          PATH: `${bin}:${process.env["PATH"] ?? ""}`,
          evidence: directory,
          TMD_FAKE_OUTPUT: output,
          TMD_FAKE_STATUS: status,
        },
        stdout: "pipe",
        stderr: "pipe",
      });
      expect([output, result.exitCode]).toEqual([output, expected]);
      const log = readFileSync(join(directory, "adapter.log"), "utf8");
      expect(log).toContain(output);
      expect(log).toContain("fixture stderr");
    }
  } finally {
    rmSync(directory, { recursive: true, force: true });
  }
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

test("only rune-macos.yml publishes a check named rune-macos", () => {
  // GitHub uses a job's name, or its id when name is absent. A skipped caller
  // with either set to rune-macos posts that exact check and can mask the
  // real one. An unevaluated ${{ }} name is not a substitute for a real name.
  const found: string[] = [];
  for (const file of workflowNames()) {
    for (const [id, lane] of Object.entries(jobs(workflow(file)))) {
      const display = str(lane["name"]);
      const published = display === undefined || display.includes("${{") ? id : display;
      const exact = published === "rune-macos" || id === "rune-macos" || display === "rune-macos";
      if (!exact) continue;
      if (file === "rune-macos.yml" && id === "rune-macos" && display === "rune-macos") continue;
      found.push(`${file} ${id} → ${display ?? id}`);
    }
  }
  expect(found).toEqual([]);
  for (const id of ["linux-x86_64", "linux-aarch64"] as const) {
    const name = str(job(workflow("rune-build.yml"), id)["name"]);
    expect(name?.includes("matrix.")).toBe(false);
    expect(name).toBe(id);
  }
});

test("the existing required ci check depends on real Apple qualification on the queue", () => {
  const ci = workflow("ci.yml");
  const apple = job(ci, "queue-apple");
  expect(str(apple["name"])).toBe("queue-apple");
  expect(uses(apple)).toBe("./.github/workflows/rune-macos.yml");
  // Only a release bot's PR waits on the macOS pool; main pushes qualify Apple.
  expect(str(apple["if"])).toContain("github.event_name == 'merge_group'");
  expect(str(apple["if"])).toContain("needs.plan.outputs.release_qualify == 'true'");
  expect(withInput(apple, "qualify")).toBe(true);
  expect(needs(job(ci, "ci"))).toContain("queue-apple");
  const mac = workflow("rune-macos.yml");
  expect(table(mac["on"])["workflow_call"]).toBeDefined();
  // Only ci calls the queue leg: no second standalone native queue build.
  expect(table(mac["on"])["merge_group"]).toBeUndefined();
  expect(table(mac["on"])["schedule"]).toEqual([{ cron: "47 11 * * *" }]);
  expect(table(mac["on"])["workflow_dispatch"]).toBeDefined();
});

test("only a release PR admits Apple, and the required child's concurrency stays apart from the cheap check", () => {
  const pr = {
    ...push,
    event_name: "pull_request",
    head_ref: "topic",
    event: { pull_request: { head: { repo: { full_name: "no-phux/phux" } } } },
  };
  const apple = job(workflow("ci.yml"), "queue-apple");
  for (const [release, rune, want] of [
    ["true", "true", true],
    ["false", "true", false],
    ["", "true", false],
    ["true", "false", false],
  ] as const) {
    expect(
      evaluate(apple["if"], pr, {
        needs: { plan: { outputs: { release_qualify: release, rune } } },
      }),
    ).toBe(want);
  }
  const mac = workflow("rune-macos.yml");
  const selected = { needs: { plan: { outputs: { macos: "true" } } } };
  expect(evaluate(job(mac, "build")["if"], pr, selected)).toBe(false);
  const required = { ...selected, inputs: { qualify: true } };
  expect(evaluate(job(mac, "build")["if"], pr, required)).toBe(true);
  expect(group("rune-macos.yml", pr, required)).not.toBe(group("rune-macos.yml", pr));
  expect(group("rune-macos.yml", pr, required)).not.toBe(group("ci.yml", pr));
  const prefixedFork = {
    ...pr,
    event: { pull_request: { head: { repo: { full_name: "required-no-phux/phux" } } } },
  };
  expect(group("rune-macos.yml", pr, required)).not.toBe(group("rune-macos.yml", prefixedFork));
  const requiredGroup = group("rune-macos.yml", pr, required);
  expect(requiredGroup).toContain("required:no-phux/phux:refs/heads/topic");
  expect(group("rune-macos.yml", prefixedFork)).toContain(
    "standalone:required-no-phux/phux:refs/heads/topic",
  );
  const forkDispatch = {
    ...pr,
    event_name: "workflow_dispatch",
    repository: "required-no-phux/phux",
    ref: "refs/heads/topic",
  };
  expect(group("rune-macos.yml", forkDispatch)).toBe(group("rune-macos.yml", prefixedFork));
  expect(requiredGroup).not.toBe(group("rune-macos.yml", forkDispatch));
});

test("Apple qualification grants no signing, release or provisioning authority", () => {
  const ci = workflow("ci.yml");
  const mac = workflow("rune-macos.yml");
  expect(ci["permissions"]).toEqual({ actions: "read", contents: "read" });
  expect(mac["permissions"]).toEqual({ actions: "read", contents: "read" });
  for (const caller of [job(ci, "queue-apple"), job(mac, "build")]) {
    expect(caller["secrets"]).toBeUndefined();
    expect(caller["permissions"]).toBeUndefined();
  }
  const fork = {
    ...push,
    event_name: "pull_request",
    ref: "refs/pull/42/merge",
    event: { pull_request: { head: { repo: { full_name: "fork/phux" } } } },
  };
  const signing = steps(action("rune-host")).filter((step) =>
    run(step).includes("sign-macos-desktop.sh"),
  );
  expect(signing).toHaveLength(2);
  for (const step of signing) {
    expect(
      evaluate(step["if"], fork, {
        inputs: { qualify: "true" },
        runner: { os: "macOS" },
        startsWith: (value: string, prefix: string) => value.startsWith(prefix),
      }),
    ).toBe(false);
  }
  const ios = steps(job(workflow("rune-build.yml"), "ios-native"))
    .map(run)
    .join("\n");
  expect(ios).not.toContain("package_ota.py");
  expect(ios).not.toContain("secrets.");
  expect(ios).not.toContain("--provision");
});

test("ci rejects a failed or cancelled native queue dependency", () => {
  const script = run(
    steps(job(workflow("ci.yml"), "ci")).find((step) =>
      run(step).includes("scripts/ci/ci-aggregate.sh"),
    ) ?? {},
  );
  expect(needs(job(workflow("ci.yml"), "ci"))).toContain("queue-apple");
  for (const state of ["success", "failure", "cancelled", "skipped"]) {
    const proc = Bun.spawnSync(["bash", "-euo", "pipefail", "-c", script], {
      cwd: repoPath("."),
      env: {
        ...process.env,
        EVENT_NAME: "merge_group",
        RESULTS: JSON.stringify({
          ...Object.fromEntries(
            needs(job(workflow("ci.yml"), "ci")).map((id) => [id, { result: "skipped" }]),
          ),
          plan: {
            result: "success",
            outputs: Object.fromEntries(
              [...LANES, "rune_qualify", "docs_catalog", "release_qualify"].map((key) => [
                key,
                "false",
              ]),
            ),
          },
          native: { result: "success" },
          icount: { result: "success" },
          "queue-apple": { result: state },
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
