/* tslint:disable */
/* eslint-disable */

/**
 * One live wire session: the curated shell + the bits of server state the wire
 * needs (a terminal id, an output sequence counter, the viewport size).
 */
export class EdgeSession {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Serialize logical session state only. The caller persists mode and the
     * portfolio snapshot separately and supplies them again to `restore`.
     */
    checkpoint(): string;
    /**
     * Create a session. `cols`/`rows` are defaults until the client's `ATTACH`
     * reports its viewport.
     */
    constructor(cols: number, rows: number, mode: string, snapshot_json: string);
    /**
     * Handle one inbound WebSocket message (one encoded `FrameKind`). Returns a
     * JS array of `Uint8Array`s — each is one frame to send back, one WS
     * message per element.
     */
    on_message(data: Uint8Array): Array<any>;
    static restore(checkpoint_json: string, mode: string, snapshot_json: string): EdgeSession;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_edgesession_free: (a: number, b: number) => void;
    readonly edgesession_new: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly edgesession_checkpoint: (a: number) => [number, number];
    readonly edgesession_restore: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number];
    readonly edgesession_on_message: (a: number, b: number, c: number) => any;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
