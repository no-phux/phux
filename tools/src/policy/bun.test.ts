// Every CI step that runs `bun tools/main.ts` has the pinned Bun first: from
// the setup-bun composite, or from the Nix devshell that pins the same
// release (flake.nix reads mise.toml).
import { expect, test } from "bun:test";
import {
  action,
  actionText,
  job,
  jobs,
  run,
  steps,
  uses,
  withInput,
  workflow,
  workflowNames,
} from "./workflows.ts";

const SETUP_BUN = "./.github/actions/setup-bun";
const SETUP_RUST_LANE = "./.github/actions/setup-rust-lane";

function firstIndex(
  list: Record<string, unknown>[],
  predicate: (step: Record<string, unknown>) => boolean,
): number {
  return list.findIndex(predicate);
}

test("setup-bun installs the mise pin", () => {
  const text = actionText("setup-bun");
  expect(text).toContain(`sed -n 's/^bun = "\\([^"]*\\)"/\\1/p' mise.toml`);
  const install = steps(action("setup-bun")).find((step) =>
    uses(step).startsWith("oven-sh/setup-bun@"),
  );
  expect(withInput(install ?? {}, "bun-version")).toBe("${{ steps.pin.outputs.version }}");
});

test("the classifier gets Bun before it routes", () => {
  const all = steps(action("classify-changes"));
  const setup = firstIndex(all, (step) => uses(step) === SETUP_BUN);
  const detect = firstIndex(all, (step) => run(step).includes("bun tools/main.ts detect-changes"));
  expect(setup).toBeGreaterThanOrEqual(0);
  expect(detect).toBeGreaterThan(setup);
});

test("workflow jobs that call the tools package install Bun first", () => {
  let callers = 0;
  for (const name of workflowNames()) {
    for (const [jobName, lane] of Object.entries(jobs(workflow(name)))) {
      const all = steps(lane);
      // A step under `nix develop` gets the devshell's pinned Bun instead.
      const call = firstIndex(
        all,
        (step) =>
          /(^|[\s;(])bun tools\/main\.ts/m.test(run(step)) && !run(step).includes("nix develop"),
      );
      if (call < 0) continue;
      callers++;
      const setup = firstIndex(all, (step) => uses(step) === SETUP_BUN);
      expect([name, jobName, setup >= 0 && setup < call]).toEqual([name, jobName, true]);
    }
  }
  expect(callers).toBeGreaterThan(0);
});

test("the workflow gate installs Bun for tools-check", () => {
  // `just workflow-check` runs `just tools-check` inside a `nix shell` that
  // brings no Bun of its own; the runner's Bun has to be on PATH already.
  const all = steps(job(workflow("ci.yml"), "workflow-gate"));
  const setup = firstIndex(all, (step) => uses(step) === SETUP_BUN);
  const guards = firstIndex(all, (step) => run(step).includes("just workflow-check"));
  expect(setup).toBeGreaterThanOrEqual(0);
  expect(guards).toBeGreaterThan(setup);
  // Unconditional: the docs-only path's install-surface check needs it too.
  expect(all[setup]?.["if"]).toBeUndefined();
});

test("mbx export headroom runs where setup-rust-lane exposed the devshell's Bun", () => {
  // scripts/ci/mbx-export-headroom.sh runs outside `nix develop` and calls
  // `bun tools/main.ts mbx-collapse-target-copies`. The mbx variant of
  // setup-rust-lane appends the devshell's mbx, rustc and bun to PATH.
  const lane = actionText("setup-rust-lane");
  expect(lane).toMatch(/for tool in mbx rustc bun; do dirname "\$\(command -v "\$tool"\)"; done/);
  expect(lane).toContain('cat "$RUNNER_TEMP/mbx-path" >> "$GITHUB_PATH"');
  let callers = 0;
  for (const name of workflowNames()) {
    for (const [jobName, lane] of Object.entries(jobs(workflow(name)))) {
      const all = steps(lane);
      const call = firstIndex(all, (step) =>
        run(step).includes("scripts/ci/mbx-export-headroom.sh"),
      );
      if (call < 0) continue;
      callers++;
      const setup = all.findIndex((step) => uses(step) === SETUP_RUST_LANE);
      const cache = withInput(all[setup] ?? {}, "build-cache") ?? "mbx";
      expect([name, jobName, setup >= 0 && setup < call, cache]).toEqual([
        name,
        jobName,
        true,
        "mbx",
      ]);
    }
  }
  expect(callers).toBe(3);
});

test("perf-icount runs inside the devshell, which pins Bun", () => {
  const all = steps(job(workflow("ci.yml"), "perf-icount"));
  const counts = all.find((step) => run(step).includes("just perf-icount"));
  expect(run(counts ?? {})).toStartWith("nix develop -c bash");
});
