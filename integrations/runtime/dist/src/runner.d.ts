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
/** Execute an argv vector directly in a new POSIX process group; never invoke a shell. */
export declare const nodeProcessRunner: ProcessRunner;
export {};
