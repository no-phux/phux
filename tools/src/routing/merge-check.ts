// Check committed integration without checking out a branch or touching the index.
import { existsSync } from "node:fs";
import { resolve } from "node:path";
import { Failure } from "../lib/command.ts";
import { git } from "../lib/git.ts";
import { ProcessError } from "../lib/process.ts";
import { ROOT } from "../lib/repo.ts";

/** Refuse conflicts; defer committed mergeability while a resolved merge is staged. */
export function checkMerge(base: string, cwd = ROOT): { target: string; resolvedMerge: boolean } {
  if (git(["ls-files", "--unmerged", "-z"], cwd) !== "") {
    throw new Failure("Resolve and stage the current conflicts before running just check.");
  }
  const target = git(["rev-parse", "--verify", "--end-of-options", `${base}^{commit}`], cwd).trim();
  const mergeHead = resolve(cwd, git(["rev-parse", "--git-path", "MERGE_HEAD"], cwd).trim());
  if (existsSync(mergeHead)) return { target, resolvedMerge: true };
  const result = Bun.spawnSync(
    ["git", "merge-tree", "--write-tree", "--name-only", "-z", target, "HEAD"],
    { cwd, stdout: "pipe", stderr: "pipe" },
  );
  if (result.exitCode === 0) return { target, resolvedMerge: false };
  if (result.exitCode !== 1) {
    throw new ProcessError(["git", "merge-tree"], result.stderr.toString());
  }
  // The tree OID comes first, then NUL-separated conflict paths, a blank,
  // and informational messages. Do not parse the human-readable messages.
  const paths = result.stdout.toString().split("\0").slice(1);
  const conflicts = paths.slice(0, paths.indexOf(""));
  throw new Failure(
    [
      `Committed changes conflict with ${base} (${target.slice(0, 12)}):`,
      ...conflicts.map((path) => `  ${JSON.stringify(path)}`),
      `Merge ${base} into your feature worktree, resolve the conflicts, then rerun just check.`,
    ].join("\n"),
  );
}
