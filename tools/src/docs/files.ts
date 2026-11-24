// Which files the docs gates read: Git's view of the tree when there is one.
import { capture } from "../lib/process.ts";

/**
 * Tracked and untracked-but-not-ignored paths under `root` matching
 * `pathspecs`, relative to `root`; undefined outside a Git work tree.
 */
export function find(root: string, pathspecs: readonly string[]): string[] | undefined {
  try {
    if (capture(["git", "-C", root, "rev-parse", "--is-inside-work-tree"]).trim() !== "true") {
      return undefined;
    }
  } catch {
    return undefined;
  }
  const listed = capture([
    "git",
    "-C",
    root,
    "ls-files",
    "-z",
    "--cached",
    "--others",
    "--exclude-standard",
    "--",
    ...pathspecs,
  ]);
  return listed.split("\0").filter((path) => path !== "");
}
