import type { ExecutionOptions, PhuxCli } from "./adapter.js";
/** The host-independent tool contract and the helpers every tool module shares. */
export type JsonSchema = Readonly<Record<string, unknown>>;
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
/** A base64 image for hosts that pass images to the model beside the text. */
export interface ToolImage {
    readonly data: string;
    readonly mimeType: "image/png" | "image/jpeg";
}
export interface ToolResult {
    readonly content: string;
    readonly metadata: PhuxToolMetadata;
    readonly images?: readonly ToolImage[];
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
export declare const TARGET: {
    type: string;
    minLength: number;
    maxLength: number;
    pattern: string;
    description: string;
};
export declare const STRING: {
    type: string;
    minLength: number;
    maxLength: number;
};
export declare const LOCAL_TIMEOUT: {
    type: string;
    minimum: number;
    maximum: number;
    description: string;
};
export declare const TIMEOUT: {
    type: string;
    minimum: number;
    maximum: number;
    description: string;
};
export interface Input {
    readonly target?: string;
    readonly local_timeout_ms?: number;
    readonly timeout_seconds?: number;
}
export declare function resolveTarget(explicit: string | undefined, runtime: Pick<PhuxToolRuntime, "getSelectedTarget" | "environmentTarget">, context?: ToolContext): string;
export declare function exactTarget(target: string): string;
export declare function writeTarget(explicit: string | undefined, runtime: PhuxToolRuntime, context: ToolContext): string;
export declare function execution(input: Input, context: ToolContext): ExecutionOptions;
export declare function operation(input: Input, context: ToolContext): {
    signal?: AbortSignal;
    timeoutMs?: number;
    phuxTimeoutSeconds: number;
};
export declare function schema(properties: Record<string, JsonSchema>, required?: string[]): JsonSchema;
export declare function documentResult(operation: string, document: object, target?: string): ToolResult;
/** Bound terminal text by UTF-8 bytes and lines; preserve the result header and truncation notices. */
export declare function boundedResult(header: string, body: string, phuxTruncated?: boolean): {
    readonly text: string;
    readonly truncated: boolean;
};
