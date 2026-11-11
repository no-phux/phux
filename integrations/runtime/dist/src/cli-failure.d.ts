import { PhuxError } from "./errors.js";
import type { ProcessResult } from "./runner.js";
import { type ScreenState } from "./schemas.js";
export declare function commandFailed(verb: string, executable: string, args: string[], result: ProcessResult): PhuxError;
export declare function diagnosticSuffix(stderr: string): string;
/** `parseScreenState`, refusing the document a non-Terminal target printed instead. */
export declare function screenParser(verb: "snapshot" | "wait", target: string | undefined): (value: unknown) => ScreenState;
