import { capture, captureBytes } from "./process.ts";
import { ROOT } from "./repo.ts";

export function git(args: string[], cwd = ROOT): string {
  return capture(["git", ...args], { cwd });
}

/** A file's text at a revision, or undefined when absent or not UTF-8. */
export function show(rev: string, path: string, cwd = ROOT): string | undefined {
  try {
    return new TextDecoder("utf-8", { fatal: true }).decode(
      captureBytes(["git", "show", `${rev}:${path}`], { cwd }),
    );
  } catch {
    return undefined;
  }
}
