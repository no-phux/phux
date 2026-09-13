export type PhuxErrorCode = "unavailable" | "command_failed" | "aborted" | "timeout" | "output_limit" | "malformed_json" | "invalid_response";
export interface PhuxErrorDetails {
    readonly argv?: readonly string[];
    readonly exitCode?: number | null;
    readonly stderr?: string;
    readonly cause?: unknown;
}
/** A stable, actionable error surface independent of the process host. */
export declare class PhuxError extends Error {
    readonly code: PhuxErrorCode;
    readonly argv: readonly string[] | undefined;
    readonly exitCode: number | null | undefined;
    readonly stderr: string | undefined;
    constructor(code: PhuxErrorCode, message: string, details?: PhuxErrorDetails);
}
