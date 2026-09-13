import type { ExecutionOptions } from "./adapter.js";
import type { AgentStateList } from "./schemas.js";
export declare const PHUX_CONTEXT_CUSTOM_TYPE = "phux-context";
export declare const PHUX_CONTEXT_VERSION: 1;
export declare const DEFAULT_CONTEXT_TIMEOUT_MS = 1000;
export declare const DEFAULT_CONTEXT_MAX_BYTES: number;
export declare const DEFAULT_CONTEXT_MAX_PANES = 64;
export declare const DEFAULT_CONTEXT_CHECKPOINT_INTERVAL = 8;
export interface PhuxAwarenessAdapter {
    agentList(options?: ExecutionOptions): Promise<AgentStateList>;
}
export interface PhuxContextIdentity {
    readonly self?: string;
    readonly selected?: string;
}
export interface PhuxContextAwarenessOptions {
    readonly enabled?: boolean;
    readonly timeoutMs?: number;
    readonly maxBytes?: number;
    readonly maxPanes?: number;
    readonly checkpointInterval?: number;
}
export type PhuxContextKind = "checkpoint" | "delta";
export interface PhuxContextEmission {
    readonly version: typeof PHUX_CONTEXT_VERSION;
    readonly kind: PhuxContextKind;
    readonly seq: number;
    readonly text: string;
}
/**
 * Per-host-session fleet projection. It emits one full checkpoint, then only
 * changed suffix messages so provider prompt prefixes remain reusable.
 */
export declare class PhuxContextAwareness {
    private readonly adapter;
    private readonly enabled;
    private readonly timeoutMs;
    private readonly maxBytes;
    private readonly maxPanes;
    private readonly checkpointInterval;
    private readonly streams;
    private readonly tails;
    constructor(adapter: PhuxAwarenessAdapter, options?: PhuxContextAwarenessOptions);
    next(streamId: string, identity?: PhuxContextIdentity, signal?: AbortSignal): Promise<PhuxContextEmission | null>;
    /**
     * Produce a compactor-only checkpoint. The next normal turn is forced to
     * persist another checkpoint whether compaction succeeds or fails.
     */
    checkpoint(streamId: string, identity?: PhuxContextIdentity, signal?: AbortSignal): Promise<PhuxContextEmission | null>;
    forceCheckpoint(streamId: string): void;
    delete(streamId: string): void;
    private serialized;
    private emit;
    private stream;
    private project;
}
export declare function contextAwarenessEnabled(value: string | undefined, fallback?: boolean): boolean;
export declare function normalizeTerminalIdentity(value: string | undefined): string | null;
