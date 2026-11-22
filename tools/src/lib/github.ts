// The GitHub CLI and REST API through an authenticated `gh`. Every call is a
// child process, so a test stands in a fake `gh` on PATH, or injects an Api.
import { capture, captureBytes, inherit } from "./process.ts";

export function gh(args: readonly string[], timeoutMs?: number): string {
  return capture(["gh", ...args], { timeoutMs });
}

/** Run `gh` with the caller's stdio, for progress a person watches; returns the exit code. */
export function ghInherit(args: readonly string[]): number {
  return inherit(["gh", ...args]);
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
  bytes: (path) => captureBytes(["gh", "api", path], { timeoutMs: API_TIMEOUT_MS }),
};
