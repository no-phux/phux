import { resolve } from "node:path";

/** The repository root, independent of the caller's working directory. */
export const ROOT = resolve(import.meta.dir, "../../..");

export function repoPath(relative: string): string {
  return resolve(ROOT, relative);
}
