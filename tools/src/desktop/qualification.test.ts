import { expect, test } from "bun:test";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { capture } from "../lib/process.ts";
import { repoPath, ROOT } from "../lib/repo.ts";
import { action, jobs, run, steps, uses, withInput, workflow } from "../policy/workflows.ts";

function dryRun(...args: string[]): string {
  const result = Bun.spawnSync(
    ["just", "--dry-run", "--color", "never", "desktop-framework-check", ...args],
    { cwd: ROOT },
  );
  expect(result.exitCode).toBe(0);
  return result.stdout.toString() + result.stderr.toString();
}

test("PR qualification builds and packages debug, while the public default stays release", () => {
  const debug = dryRun("debug");
  expect(debug).toContain("--debug");
  expect(debug).toContain("native/target/debug/phux-desktop");
  expect(debug).not.toContain("--release");
  const release = dryRun();
  expect(release).toContain("--release");
  expect(release).toContain("native/target/release/phux-desktop");
});

test("shared desktop test orchestration uses historical source recipes, not the harness sources", () => {
  const root = mkdtempSync(join(tmpdir(), "phux-desktop-qualification-"));
  try {
    const bin = join(root, "bin");
    mkdirSync(bin);
    const source = join(root, "source");
    mkdirSync(source);
    const observed = join(root, "observed");
    // This historical source has no desktop-tests wrapper at all.
    writeFileSync(
      join(source, "justfile"),
      "desktop-test:\n    @printf 'tests\\n' >> \"$QUALIFICATION_LOG\"\ndesktop-renderer-test:\n    @printf 'renderer\\n' >> \"$QUALIFICATION_LOG\"\ndesktop-perf-budget:\n    @printf 'perf\\n' >> \"$QUALIFICATION_LOG\"\n",
    );
    for (const [platform, expected] of [
      ["Linux", "tests\nrenderer\n"],
      ["Darwin", "tests\nperf\n"],
    ] as const) {
      writeFileSync(join(bin, "uname"), `#!/usr/bin/env bash\nprintf '%s\\n' '${platform}'\n`, {
        mode: 0o755,
      });
      writeFileSync(observed, "");
      capture(
        [
          "just",
          "--justfile",
          repoPath("justfile"),
          "--working-directory",
          source,
          "desktop-tests",
        ],
        {
          cwd: root,
          env: {
            ...process.env,
            PATH: `${bin}:${process.env["PATH"] ?? ""}`,
            DISPLAY: ":fixture",
            QUALIFICATION_LOG: observed,
          },
        },
      );
      expect(readFileSync(observed, "utf8")).toBe(expected);
    }
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test("macOS provisioning is shared but still reads toolchain pins from the selected source", () => {
  const setup = action("setup-macos-desktop");
  expect(steps(setup)[0]?.["working-directory"]).toBe("${{ inputs.source }}");
  const ciSetup = Object.values(jobs(workflow("ci.yml")))
    .flatMap(steps)
    .find((step) => uses(step) === "./.github/actions/setup-macos-desktop");
  expect(ciSetup).toBeDefined();
  const releaseSteps = Object.values(jobs(workflow("desktop-release.yml"))).flatMap(steps);
  const releaseSetup = releaseSteps.find(
    (step) => uses(step) === "./harness/.github/actions/setup-macos-desktop",
  );
  expect(withInput(releaseSetup ?? {}, "source")).toBe("source");
  expect(
    releaseSteps.some((step) =>
      run(step).includes('--working-directory "$GITHUB_WORKSPACE/source" desktop-tests'),
    ),
  ).toBe(true);
  expect(releaseSteps.filter((step) => run(step) === "just desktop-package").length).toBe(2);
});
