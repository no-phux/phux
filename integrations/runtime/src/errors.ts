export type PhuxErrorCode =
  | "unavailable"
  | "command_failed"
  | "aborted"
  | "timeout"
  | "output_limit"
  | "malformed_json"
  | "invalid_response";

export interface PhuxErrorDetails {
  readonly argv?: readonly string[];
  readonly exitCode?: number | null;
  readonly stderr?: string;
  readonly cause?: unknown;
  readonly cliError?: Readonly<Record<string, unknown>>;
}

/** A stable, actionable error surface independent of the process host. */
export class PhuxError extends Error {
  readonly code: PhuxErrorCode;
  readonly argv: readonly string[] | undefined;
  readonly exitCode: number | null | undefined;
  readonly stderr: string | undefined;
  readonly cliError: Readonly<Record<string, unknown>> | undefined;

  constructor(code: PhuxErrorCode, message: string, details: PhuxErrorDetails = {}) {
    super(message, details.cause === undefined ? undefined : { cause: details.cause });
    this.name = "PhuxError";
    this.code = code;
    this.argv = details.argv;
    this.exitCode = details.exitCode;
    this.stderr = details.stderr;
    this.cliError = details.cliError;
  }
}
