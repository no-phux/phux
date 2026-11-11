export type PhuxErrorCode = "unavailable" | "command_failed" | "aborted" | "timeout" | "output_limit" | "malformed_json" | "invalid_response" | "wrong_resource_kind";
export interface PhuxErrorDetails {
    readonly argv?: readonly string[];
    readonly exitCode?: number | null;
    readonly stderr?: string;
    readonly cause?: unknown;
    readonly cliError?: Readonly<Record<string, unknown>>;
    readonly resourceKind?: string;
}
/** A stable, actionable error surface independent of the process host. */
export declare class PhuxError extends Error {
    readonly code: PhuxErrorCode;
    readonly argv: readonly string[] | undefined;
    readonly exitCode: number | null | undefined;
    readonly stderr: string | undefined;
    readonly cliError: Readonly<Record<string, unknown>> | undefined;
    /** For `wrong_resource_kind`: the `phux ls --json` kind named, when the CLI said which. */
    readonly resourceKind: string | undefined;
    constructor(code: PhuxErrorCode, message: string, details?: PhuxErrorDetails);
}
