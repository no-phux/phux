// Every CI step that runs `bun tools/main.ts` has the pinned Bun first: from
// the setup-bun composite, or from the Nix devshell that pins the same
// release (flake.nix reads mise.toml).
import { expect, test } from "bun:test";
import { readdirSync } from "node:fs";
import { repoPath } from "../lib/repo.ts";
import { type Table } from "../lib/value.ts";
import {
  action,
  actionText,
  everyJob,
  job,
  run,
  steps,
  uses,
  withInput,
  workflow,
  workflowNames,
  workflowText,
} from "./workflows.ts";

const ACTIONS = readdirSync(repoPath(".github/actions")).filter((name) => name !== "setup-bun");
// scripts/ci/mbx-export-headroom.sh runs `bun tools/main.ts` itself.
const CALLS_TOOLS = /(^|[\s;(])bun tools\/main\.ts|scripts\/ci\/mbx-export-headroom\.sh/m;

/** setup-bun, or setup-rust-lane's mbx variant, which puts the devshell's Bun on PATH. */
function providesBun(step: Table): boolean {
  if (uses(step) === "./.github/actions/setup-bun") return true;
  const mbx = (withInput(step, "build-cache") ?? "mbx") === "mbx";
  return uses(step) === "./.github/actions/setup-rust-lane" && mbx;
}

test("setup-bun installs the mise pin, and is the only reader of it", () => {
  expect(actionText("setup-bun")).toContain("mise.toml");
  const install = steps(action("setup-bun")).find((step) =>
    uses(step).startsWith("oven-sh/setup-bun@"),
  );
  expect(withInput(install ?? {}, "bun-version")).toBe("${{ steps.pin.outputs.version }}");
  const pin = /oven-sh\/setup-bun@|\^bun = |engines\.bun/;
  const readers = [
    ...workflowNames().filter((name) => pin.test(workflowText(name))),
    ...ACTIONS.filter((name) => pin.test(actionText(name))),
  ];
  expect(readers).toEqual([]);
});

test("every job and composite action that calls the tools package installs Bun first", () => {
  expect(actionText("setup-rust-lane")).toMatch(/for tool in [^;]*\bbun\b[^;]*; do dirname/);
  const owners = [...everyJob(), ...ACTIONS.map((name): [string, Table] => [name, action(name)])];
  let callers = 0;
  for (const [name, owner] of owners) {
    // A step under `nix develop` gets the devshell's pinned Bun instead.
    const all = steps(owner).filter((step) => !run(step).includes("nix develop"));
    const call = all.findIndex((step) => CALLS_TOOLS.test(run(step)));
    if (call < 0) continue;
    callers++;
    expect([name, all.slice(0, call).some(providesBun)]).toEqual([name, true]);
  }
  expect(callers).toBeGreaterThan(2);
});

test("the workflow gate installs Bun unconditionally for tools-check", () => {
  // `just workflow-check` runs `just tools-check` in a `nix shell` without
  // Bun; the docs-only path's install-surface check needs it too.
  const all = steps(job(workflow("ci.yml"), "workflow-gate"));
  const setup = all.findIndex(providesBun);
  expect(setup).toBeGreaterThanOrEqual(0);
  expect(all.findIndex((step) => run(step).includes("just workflow-check"))).toBeGreaterThan(setup);
  expect(all[setup]?.["if"]).toBeUndefined();
});

test("perf-icount runs inside the devshell, which pins Bun", () => {
  const counts = steps(job(workflow("ci.yml"), "perf-icount")).find((step) =>
    run(step).includes("just perf-icount"),
  );
  expect(run(counts ?? {})).toStartWith("nix develop -c");
});
