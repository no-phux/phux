// Child processes. `capture` is for programs whose output the caller reads;
// `inherit` is for progress a person watches.
import { Failure } from "./command.ts";

export interface RunOptions {
  cwd?: string | undefined;
  env?: Record<string, string | undefined> | undefined;
  timeoutMs?: number | undefined;
}

/** A child that exited non-zero or timed out, with what it printed on stderr. */
export class ProcessError extends Failure {
  constructor(
    readonly argv: readonly string[],
    readonly stderr: string,
  ) {
    super(`${argv.join(" ")} failed${stderr.trim() ? `: ${stderr.trim()}` : ""}`);
  }
}

/** The spawn options `options` sets, without undefined keys. */
function spawnOptions(options: RunOptions): {
  cwd?: string;
  env?: Record<string, string | undefined>;
  timeout?: number;
} {
  return {
    ...(options.cwd === undefined ? {} : { cwd: options.cwd }),
    ...(options.env === undefined ? {} : { env: options.env }),
    ...(options.timeoutMs === undefined ? {} : { timeout: options.timeoutMs }),
  };
}

/** Run `argv` and return its stdout bytes; a non-zero exit or a timeout throws ProcessError. */
export function captureBytes(argv: readonly string[], options: RunOptions = {}): Uint8Array {
  const result = Bun.spawnSync([...argv], {
    ...spawnOptions(options),
    stdout: "pipe",
    stderr: "pipe",
  });
  if (result.exitCode !== 0) {
    const reason = result.exitedDueToTimeout ? `timed out after ${options.timeoutMs}ms` : "";
    throw new ProcessError(argv, result.stderr.toString() || reason);
  }
  return result.stdout;
}

export function capture(argv: readonly string[], options: RunOptions = {}): string {
  return new TextDecoder().decode(captureBytes(argv, options));
}

/** Run `argv` with the caller's stdio; returns the exit code. */
export function inherit(argv: readonly string[], options: RunOptions = {}): number {
  const result = Bun.spawnSync([...argv], {
    ...spawnOptions(options),
    stdio: ["inherit", "inherit", "inherit"],
  });
  return result.exitCode ?? 1;
}
