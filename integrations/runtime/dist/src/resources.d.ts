/** Kinds `phux ls --json` names today (crates/phux-core/src/session_list.rs). */
export declare const RESOURCE_KINDS: readonly ["terminal", "agent_session", "accessibility_tree", "pixel_surface", "document", "unknown"];
export type KnownResourceKind = typeof RESOURCE_KINDS[number];
/** The kinds MCP `phux_documents` lists: trees, surfaces and typed documents. */
export declare const DOCUMENT_KINDS: readonly ["accessibility_tree", "pixel_surface", "document"];
export interface ResourceExit {
    readonly status: number | null;
    readonly signal: number | null;
    readonly reason: string | null;
    readonly exited_at_ms: number;
    readonly retained_until_ms: number;
}
/** One row of `phux ls --json` `resources`, for a kind this runtime knows. */
export interface KnownResource {
    readonly id: string;
    readonly kind: KnownResourceKind;
    readonly name: string | null;
    readonly parent: string | null;
    readonly lifecycle: string;
    readonly exit: ResourceExit | null;
    readonly producer?: string;
    readonly control?: string;
    readonly key: string | null;
}
/** A newer kind: only its identity is interpreted; nothing else is guessed. */
export interface OpaqueResource {
    readonly id: string;
    readonly kind: string;
    readonly name: string | null;
    readonly parent: string | null;
    readonly opaque: true;
}
export type ResourceEntry = KnownResource | OpaqueResource;
/** The MCP `phux_documents` entry shape, projected from the inventory. */
export interface DocumentEntry {
    readonly id: string;
    readonly kind: typeof DOCUMENT_KINDS[number];
    readonly parent: string | null;
    readonly name: string | null;
    readonly producer: string | null;
    readonly control: string | null;
}
/** `phux agent tree snapshot|follow --json`: the newest document, null before the first publish. */
export interface TreeDocument {
    readonly schema_version: number;
    readonly seq: number;
    readonly document: unknown;
}
/** MCP `phux_tree_wait`'s shape: the matching document, or why none matched. */
export type TreeWaitResult = (TreeDocument & {
    readonly outcome: "matched";
}) | {
    readonly schema_version: number;
    readonly outcome: "closed" | "timed_out";
};
/** `phux agent surface snapshot --out PATH --json`: facts about the frame written to `out`. */
export interface SurfaceFacts {
    readonly schema_version: number;
    readonly seq: number;
    readonly bytes: number;
    readonly format: "png" | "jpeg" | null;
    readonly width: number | null;
    readonly height: number | null;
    readonly content_type: string | null;
    readonly out: string | null;
}
/** Parse `phux ls --json` `resources`; unknown kinds stay as opaque identity rows. */
export declare function parseResources(value: unknown, path?: string): ResourceEntry[];
export declare function isDocumentKind(kind: string): kind is DocumentEntry["kind"];
export declare function documentEntries(resources: readonly ResourceEntry[]): DocumentEntry[];
export declare function parseTreeDocument(value: unknown): TreeDocument;
export declare function parseSurfaceFacts(value: unknown): SurfaceFacts;
