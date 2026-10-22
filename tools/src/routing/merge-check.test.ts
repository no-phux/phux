import { expect, test } from "bun:test";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { git } from "../lib/git.ts";
import { checkMerge } from "./merge-check.ts";

function repository(run: (root: string) => void): void {
  const root = mkdtempSync(join(tmpdir(), "phux-merge-check-"));
  try {
    git(["init", "--quiet", "--initial-branch=main"], root);
    git(["config", "user.name", "Merge Check Test"], root);
    git(["config", "user.email", "merge-check@example.invalid"], root);
    git(["config", "core.hooksPath", "/dev/null"], root);
    git(["config", "commit.gpgsign", "false"], root);
    writeFileSync(join(root, "shared.txt"), "original\n");
    commit(root);
    git(["branch", "feature"], root);
    run(root);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
}

function commit(root: string): void {
  git(["add", "--all"], root);
  git(["commit", "--quiet", "-m", "fix: fixture"], root);
}

function snapshot(root: string): string[] {
  return [
    git(["rev-parse", "HEAD"], root),
    git(["status", "--porcelain=v1", "-z"], root),
    git(["diff"], root),
    git(["diff", "--cached"], root),
  ];
}

test("a stale but clean branch passes without changing HEAD, staged edits or working files", () => {
  repository((root) => {
    writeFileSync(join(root, "main.txt"), "base advanced\n");
    commit(root);
    const base = git(["rev-parse", "main"], root).trim();
    git(["checkout", "--quiet", "feature"], root);
    writeFileSync(join(root, "feature.txt"), "feature\n");
    commit(root);
    writeFileSync(join(root, "shared.txt"), "staged\n");
    git(["add", "shared.txt"], root);
    writeFileSync(join(root, "shared.txt"), "unstaged\n");
    writeFileSync(join(root, "untracked.txt"), "untracked\n");
    const before = snapshot(root);
    expect(checkMerge("main", root)).toEqual({ target: base, resolvedMerge: false });
    expect(snapshot(root)).toEqual(before);
    expect(readFileSync(join(root, "shared.txt"), "utf8")).toBe("unstaged\n");
    expect(readFileSync(join(root, "untracked.txt"), "utf8")).toBe("untracked\n");
  });
});

test("content conflicts name exact paths without starting a merge", () => {
  repository((root) => {
    const path = "space and\nnewline.txt";
    git(["mv", "shared.txt", path], root);
    commit(root);
    git(["branch", "-f", "feature"], root);
    writeFileSync(join(root, path), "main\n");
    commit(root);
    git(["checkout", "--quiet", "feature"], root);
    writeFileSync(join(root, path), "feature\n");
    commit(root);
    const before = snapshot(root);
    expect(() => checkMerge("main", root)).toThrow(JSON.stringify(path));
    expect(snapshot(root)).toEqual(before);
    expect(readFileSync(join(root, path), "utf8")).toBe("feature\n");
    // A real merge can still start afterward: preflight left no merge state.
    const merge = Bun.spawnSync(["git", "merge", "main", "--no-commit"], { cwd: root });
    expect(merge.exitCode).toBe(1);
    expect(() => checkMerge("main", root)).toThrow("Resolve and stage the current conflicts");
    writeFileSync(join(root, path), "resolved\n");
    git(["add", path], root);
    const resolved = snapshot(root);
    expect(checkMerge("main", root)).toEqual({
      target: git(["rev-parse", "main"], root).trim(),
      resolvedMerge: true,
    });
    expect(snapshot(root)).toEqual(resolved);
    git(["commit", "--quiet", "-m", "fix: resolve merge"], root);
    expect(checkMerge("main", root).resolvedMerge).toBe(false);
  });
});

test("modify/delete conflicts and missing base refs fail instead of reporting success", () => {
  repository((root) => {
    git(["rm", "--quiet", "shared.txt"], root);
    commit(root);
    git(["checkout", "--quiet", "feature"], root);
    writeFileSync(join(root, "shared.txt"), "feature\n");
    commit(root);
    expect(() => checkMerge("main", root)).toThrow('"shared.txt"');
    expect(() => checkMerge("missing-base", root)).toThrow();
    expect(() => checkMerge("--help", root)).toThrow();
  });
});
