import { type Input, type PhuxToolDefinition, type PhuxToolRuntime } from "./tool-support.js";
/** About 5 MB once base64-encoded, the common per-image provider cap. */
export declare const MAX_INLINE_IMAGE_BYTES = 3750000;
/** `phux ls --json` with every resource kind; terminal-only callers keep `terminals`. */
export declare function createListTool(runtime: Pick<PhuxToolRuntime, "cli">): PhuxToolDefinition<Input>;
/** Host-native parity with MCP phux_documents, phux_tree_snapshot, phux_tree_wait and phux_surface_snapshot. */
export declare function createDocumentTools(runtime: Pick<PhuxToolRuntime, "cli">): PhuxToolDefinition<any>[];
