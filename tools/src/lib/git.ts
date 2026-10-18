import { ROOT } from "./repo.ts";

export class GitError extends Error {}

/** Run git and return stdout bytes; a non-zero exit throws GitError. */
export function gitBytes(args: string[], cwd = ROOT): Uint8Array {
  const result = Bun.spawnSync(["git", ...args], { cwd, stdout: "pipe", stderr: "pipe" });
  if (result.exitCode !== 0) {
    throw new GitError(`git ${args.join(" ")}: ${result.stderr.toString().trim()}`);
  }
  return result.stdout;
}

export function git(args: string[], cwd = ROOT): string {
  return new TextDecoder().decode(gitBytes(args, cwd));
}

/** A file's text at a revision, or undefined when absent or not UTF-8. */
export function show(rev: string, path: string, cwd = ROOT): string | undefined {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(
      gitBytes(["show", `${rev}:${path}`], cwd),
    );
  } catch {
    return undefined;
  }
}
