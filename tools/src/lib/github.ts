// The GitHub CLI and REST API through an authenticated `gh`. Every call is a
// child process, so a test stands in a fake `gh` on PATH, or injects an Api.
import { Failure } from "./command.ts";

/** A `gh` invocation that failed, with what it printed on stderr. */
export class GhError extends Failure {
  constructor(
    readonly args: readonly string[],
    readonly stderr: string,
  ) {
    super(`gh ${args.join(" ")} failed${stderr.trim() ? `: ${stderr.trim()}` : ""}`);
  }
}

/** Run `gh` and return stdout bytes; a non-zero exit or a timeout throws GhError. */
export function ghBytes(args: readonly string[], timeoutMs?: number): Uint8Array {
  const result = Bun.spawnSync(["gh", ...args], {
    stdout: "pipe",
    stderr: "pipe",
    ...(timeoutMs === undefined ? {} : { timeout: timeoutMs }),
  });
  if (result.exitCode !== 0) {
    const reason = result.exitedDueToTimeout ? `timed out after ${timeoutMs}ms` : "";
    throw new GhError(args, result.stderr.toString() || reason);
  }
  return result.stdout;
}

export function gh(args: readonly string[], timeoutMs?: number): string {
  return new TextDecoder().decode(ghBytes(args, timeoutMs));
}

/** Run `gh` with the caller's stdio, for progress a person watches; returns the exit code. */
export function ghInherit(args: readonly string[]): number {
  return Bun.spawnSync(["gh", ...args], { stdio: ["inherit", "inherit", "inherit"] }).exitCode ?? 1;
}

/** Run `gh` and parse one JSON document per non-blank stdout line (`--jq` output). */
export function ghLines(args: readonly string[]): unknown[] {
  return gh(args)
    .split("\n")
    .filter((line) => line.trim() !== "")
    .map((line): unknown => JSON.parse(line));
}

/** The REST API by path, e.g. `repos/OWNER/REPO/actions/runs/1`. */
export interface Api {
  json(path: string): unknown;
  bytes(path: string): Uint8Array;
}

const API_TIMEOUT_MS = 30_000;

export const api: Api = {
  json: (path): unknown => JSON.parse(gh(["api", path], API_TIMEOUT_MS)),
  bytes: (path) => ghBytes(["api", path], API_TIMEOUT_MS),
};
