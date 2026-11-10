import { type PhuxToolDefinition, type PhuxToolRuntime } from "./tool-support.js";
export { createDocumentTools, createListTool, MAX_INLINE_IMAGE_BYTES } from "./resource-tools.js";
export { boundedResult, DEFAULT_SHORT_TIMEOUT_MS, MAX_MODEL_BYTES, MAX_MODEL_LINES, resolveTarget, type PhuxToolDefinition, type PhuxToolMetadata, type PhuxToolRuntime, type ToolContext, type ToolImage, type ToolResult, } from "./tool-support.js";
/** One host-independent model contract; native adapters own registration and session persistence. */
export declare function createPhuxTools(runtime: PhuxToolRuntime): Record<string, PhuxToolDefinition<any>>;
