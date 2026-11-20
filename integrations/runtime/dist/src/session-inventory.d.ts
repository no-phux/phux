import { type ResourceEntry } from "./resources.js";
export interface SessionSummary {
    readonly name: string;
    readonly windows: number;
    readonly attached: boolean;
    /** Persistent session key for typed operations (v4+), or null from a legacy server. */
    readonly key?: string | null;
}
export interface SessionList {
    readonly schema_version: 1 | 2 | 3 | 4;
    readonly sessions: readonly SessionSummary[];
    /** Canonical selectors for every addressable terminal (v2+; empty for v1). */
    readonly terminals: readonly string[];
    /** Every addressable resource of every kind (v3+; empty when absent). */
    readonly resources: readonly ResourceEntry[];
    /** One diagnostic per unreachable federation satellite; empty when absent. */
    readonly unreachable: readonly string[];
}
export declare function parseSessionList(value: unknown): SessionList;
