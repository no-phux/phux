// The milestone label gate, against a fake `bd` on PATH.
import { describe, expect, test } from "bun:test";
import { chmodSync, mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { repoPath } from "../lib/repo.ts";

function fakeBd(issues: unknown[]): string {
  const bin = mkdtempSync(join(tmpdir(), "milestone-labels-"));
  writeFileSync(join(bin, "issues.json"), JSON.stringify(issues));
  writeFileSync(
    join(bin, "bd"),
    `#!/bin/sh
case "$1" in
  version) echo "bd version 9.9.9" ;;
  info) printf 'Database: /store\\nMode: embedded\\nIssue Count: ${issues.length}\\n' ;;
  list) cat "${join(bin, "issues.json")}" ;;
  *) exit 1 ;;
esac
`,
  );
  chmodSync(join(bin, "bd"), 0o755);
  return bin;
}

function check(path: string): { code: number; stdout: string; stderr: string } {
  const result = Bun.spawnSync([process.execPath, repoPath("tools/main.ts"), "milestone-labels"], {
    stdout: "pipe",
    stderr: "pipe",
    env: { ...process.env, PATH: path },
  });
  return {
    code: result.exitCode ?? -1,
    stdout: result.stdout.toString(),
    stderr: result.stderr.toString(),
  };
}

const issue = (id: string, labels: string[], status = "open"): unknown => ({
  id,
  status,
  issue_type: "task",
  title: `title ${id}`,
  labels,
});

describe("milestone labels", () => {
  test("no bd is a skip, never a verdict", () => {
    const result = check(mkdtempSync(join(tmpdir(), "empty-")));
    expect(result.code).toBe(0);
    expect(result.stdout).toContain("SKIPPED (no `bd` on PATH)");
    expect(result.stdout).not.toContain("OK --");
  });

  test("every non-closed bead with one label passes", () => {
    const bin = fakeBd([
      issue("a", ["rc-1.0"]),
      issue("b", ["post-1.0"], "deferred"),
      issue("c", [], "closed"),
    ]);
    const result = check(`${bin}:/usr/bin:/bin`);
    expect(result.code).toBe(0);
    expect(result.stdout).toContain("store:         /store (mode: embedded)");
    expect(result.stdout).toContain("statuses:      deferred 1, open 1");
    expect(result.stdout).toContain(
      "OK -- all 2 non-closed beads carry exactly one milestone label (1 rc-1.0, 1 post-1.0)",
    );
  });

  test("unlabelled and double-labelled beads fail", () => {
    const bin = fakeBd([issue("a", []), issue("b", ["rc-1.0", "post-1.0"], "routed")]);
    const result = check(`${bin}:/usr/bin:/bin`);
    expect(result.code).toBe(1);
    expect(result.stderr).toContain("1 non-closed bead(s) carry neither rc-1.0 nor post-1.0");
    expect(result.stderr).toContain("milestone-labels:   a  [open/task]  title a");
    expect(result.stderr).toContain("1 non-closed bead(s) carry BOTH milestone labels");
  });
});
