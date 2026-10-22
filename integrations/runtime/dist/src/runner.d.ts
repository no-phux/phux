export declare const DEFAULT_MAX_OUTPUT_BYTES: number;
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
export type ProcessResult = (ProcessResultBase & {
    readonly termination: "completed" | "aborted" | "timed_out";
    readonly outputLimit?: never;
}) | (ProcessResultBase & {
    readonly termination: "output_limit";
    readonly outputLimit: OutputLimitStream;
});
export type ProcessRunner = (request: RunRequest) => Promise<ProcessResult>;
/**
 * Execute an argv vector directly, never through a shell: in a new POSIX
 * process group, or in this one under {@link RunRequest.lifeline}.
 */
export declare const nodeProcessRunner: ProcessRunner;
export {};
