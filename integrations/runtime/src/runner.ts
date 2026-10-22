import { spawn, type ChildProcess } from "node:child_process";

export const DEFAULT_MAX_OUTPUT_BYTES = 4 * 1024 * 1024;

export interface RunRequest {
  readonly executable: string;
  readonly args: readonly string[];
  readonly cwd?: string;
  readonly env?: NodeJS.ProcessEnv;
  readonly signal?: AbortSignal;
  readonly timeoutMs?: number;
  readonly maxStdoutBytes?: number;
  readonly maxStderrBytes?: number;
  /** Bytes written to the child's stdin, which is then closed; stdin is ignored when absent. */
  readonly stdin?: string;
  /**
   * Bind the child's life to this process: its stdin is a pipe this process
   * never writes or closes, and it stays in this process's group. Whenever
   * this process ends, even by SIGKILL, the pipe closes and the child sees
   * end of input; a pane closing signals both. Excludes `stdin`.
   */
  readonly lifeline?: boolean;
}

export type ProcessTermination = "completed" | "aborted" | "timed_out" | "output_limit";
export type OutputLimitStream = "stdout" | "stderr";

interface ProcessResultBase {
  readonly exitCode: number | null;
  readonly stdout: string;
  readonly stderr: string;
}

export type ProcessResult =
  | (ProcessResultBase & {
      readonly termination: "completed" | "aborted" | "timed_out";
      readonly outputLimit?: never;
    })
  | (ProcessResultBase & {
      readonly termination: "output_limit";
      readonly outputLimit: OutputLimitStream;
    });

export type ProcessRunner = (request: RunRequest) => Promise<ProcessResult>;

/**
 * Execute an argv vector directly, never through a shell: in a new POSIX
 * process group, or in this one under {@link RunRequest.lifeline}.
 */
export const nodeProcessRunner: ProcessRunner = (request) =>
  new Promise((resolve, reject) => {
    if (request.signal?.aborted) {
      resolve({ termination: "aborted", exitCode: null, stdout: "", stderr: "" });
      return;
    }
    validateNonNegativeFinite(request.timeoutMs, "timeoutMs");
    if (request.lifeline === true && request.stdin !== undefined) {
      throw new TypeError("lifeline and stdin are exclusive");
    }
    const ownGroup = request.lifeline !== true;
    const maxStdoutBytes = outputLimit(request.maxStdoutBytes, "maxStdoutBytes");
    const maxStderrBytes = outputLimit(request.maxStderrBytes, "maxStderrBytes");

    const child = spawn(request.executable, [...request.args], {
      cwd: request.cwd,
      env: request.env,
      shell: false,
      detached: ownGroup,
      stdio: [request.stdin === undefined && ownGroup ? "ignore" : "pipe", "pipe", "pipe"],
    });
    // The lifeline end stays open until the child exits; this process's own
    // exit is what closes it otherwise.
    child.stdin?.on("error", () => {});
    if (request.stdin !== undefined && child.stdin !== null) {
      child.stdin.end(request.stdin);
    }
    const { stdout, stderr } = child;
    if (stdout === null || stderr === null) {
      reject(new Error("phux subprocess started without output pipes"));
      return;
    }
    const stdoutChunks: Buffer[] = [];
    const stderrChunks: Buffer[] = [];
    let stdoutBytes = 0;
    let stderrBytes = 0;
    let termination: ProcessTermination = "completed";
    let limitedStream: OutputLimitStream | undefined;
    let timeout: NodeJS.Timeout | undefined;
    let forceKill: NodeJS.Timeout | undefined;

    const stop = (reason: Exclude<ProcessTermination, "completed">, stream?: OutputLimitStream): void => {
      if (termination !== "completed") return;
      termination = reason;
      limitedStream = stream;
      terminate(child, ownGroup, "SIGTERM");
      forceKill = setTimeout(() => terminate(child, ownGroup, "SIGKILL"), 1_000);
      forceKill.unref();
    };

    stdout.on("data", (chunk: Buffer) => {
      if (termination !== "completed") return;
      const remaining = maxStdoutBytes - stdoutBytes;
      if (chunk.length > remaining) {
        if (remaining > 0) stdoutChunks.push(chunk.subarray(0, remaining));
        stdoutBytes = maxStdoutBytes;
        stop("output_limit", "stdout");
        return;
      }
      stdoutChunks.push(chunk);
      stdoutBytes += chunk.length;
    });
    stderr.on("data", (chunk: Buffer) => {
      if (termination !== "completed") return;
      const remaining = maxStderrBytes - stderrBytes;
      if (chunk.length > remaining) {
        if (remaining > 0) stderrChunks.push(chunk.subarray(0, remaining));
        stderrBytes = maxStderrBytes;
        stop("output_limit", "stderr");
        return;
      }
      stderrChunks.push(chunk);
      stderrBytes += chunk.length;
    });

    const onAbort = (): void => stop("aborted");
    request.signal?.addEventListener("abort", onAbort, { once: true });
    if (request.timeoutMs !== undefined) {
      timeout = setTimeout(() => stop("timed_out"), request.timeoutMs);
      timeout.unref();
    }

    child.once("error", (error) => {
      cleanup();
      if (forceKill !== undefined) clearTimeout(forceKill);
      reject(error);
    });
    child.once("close", (exitCode) => {
      cleanup();
      child.stdin?.destroy();
      if (forceKill !== undefined && !(ownGroup && processGroupExists(child.pid))) clearTimeout(forceKill);
      const base = {
        exitCode,
        stdout: Buffer.concat(stdoutChunks, stdoutBytes).toString("utf8"),
        stderr: Buffer.concat(stderrChunks, stderrBytes).toString("utf8"),
      };
      if (termination === "output_limit") {
        resolve({ ...base, termination, outputLimit: limitedStream ?? "stdout" });
      } else {
        resolve({ ...base, termination });
      }
    });

    function cleanup(): void {
      if (timeout !== undefined) clearTimeout(timeout);
      request.signal?.removeEventListener("abort", onAbort);
    }
  });

function outputLimit(value: number | undefined, name: string): number {
  const resolved = value ?? DEFAULT_MAX_OUTPUT_BYTES;
  if (!Number.isSafeInteger(resolved) || resolved < 0) {
    throw new RangeError(`${name} must be a non-negative safe integer`);
  }
  return resolved;
}

function validateNonNegativeFinite(value: number | undefined, name: string): void {
  if (value !== undefined && (!Number.isFinite(value) || value < 0)) {
    throw new RangeError(`${name} must be a non-negative finite number`);
  }
}

/** Signal the child's whole group when it has one, else the child alone. */
function terminate(child: ChildProcess, ownGroup: boolean, signal: NodeJS.Signals): void {
  if (ownGroup && child.pid !== undefined) {
    try {
      process.kill(-child.pid, signal);
      return;
    } catch (error) {
      if (isNoSuchProcess(error)) return;
    }
  }
  child.kill(signal);
}

function processGroupExists(pid: number | undefined): boolean {
  if (pid === undefined) return false;
  try {
    process.kill(-pid, 0);
    return true;
  } catch (error) {
    return !isNoSuchProcess(error);
  }
}

function isNoSuchProcess(error: unknown): boolean {
  return error instanceof Error && "code" in error && error.code === "ESRCH";
}
