import { PhuxCli } from "./adapter.js";
type JsonSchema = Readonly<Record<string, unknown>>;
export interface ToolContext {
    readonly sessionID: string;
    readonly agent: string;
    readonly messageID: string;
    readonly id: string;
    readonly signal?: AbortSignal;
}
export interface PhuxToolMetadata {
    readonly operation: string;
    readonly target?: string;
    readonly modelOutputTruncated?: boolean;
    readonly [key: string]: unknown;
}
export interface ToolResult {
    readonly content: string;
    readonly metadata: PhuxToolMetadata;
}
export interface PhuxToolDefinition<Input = never> {
    readonly name: string;
    readonly description: string;
    readonly input: JsonSchema;
    readonly execute: (input: Input, context: ToolContext) => Promise<ToolResult>;
}
export interface PhuxToolRuntime {
    readonly cli: PhuxCli;
    readonly environmentTarget?: string;
    readonly parentTarget?: string;
    getSelectedTarget(context?: ToolContext): string | undefined;
    selectTarget(target: string, context?: ToolContext): void;
    targetSelected?(context: ToolContext): void;
}
export declare const MAX_MODEL_BYTES: number;
export declare const MAX_MODEL_LINES = 200;
export declare const DEFAULT_SHORT_TIMEOUT_MS = 10000;
/** One host-independent model contract; native adapters own registration and session persistence. */
export declare function createPhuxTools(runtime: PhuxToolRuntime): Record<string, PhuxToolDefinition<any>>;
export declare function resolveTarget(explicit: string | undefined, runtime: Pick<PhuxToolRuntime, "getSelectedTarget" | "environmentTarget">, context?: ToolContext): string;
/** Bound terminal text by UTF-8 bytes and lines; preserve the result header and truncation notices. */
export declare function boundedResult(header: string, body: string, phuxTruncated?: boolean): {
    readonly text: string;
    readonly truncated: boolean;
};
export {};
