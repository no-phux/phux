/* tslint:disable */
/* eslint-disable */

/**
 * One live hosted wire session with up to four independently addressed shells.
 */
export class EdgeSession {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Persist all resources, subscriptions, replica generations and the ID allocator.
     * Mode and the portfolio snapshot remain in the caller's boot record.
     */
    checkpoint(): string;
    constructor(cols: number, rows: number, mode: string, snapshot_json: string);
    /**
     * One encoded frame in, one JS Uint8Array per response frame out.
     */
    on_message(data: Uint8Array): Array<any>;
    static restore(checkpoint_json: string, mode: string, snapshot_json: string): EdgeSession;
}

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_edgesession_free: (a: number, b: number) => void;
    readonly edgesession_checkpoint: (a: number) => [number, number];
    readonly edgesession_new: (a: number, b: number, c: number, d: number, e: number, f: number) => number;
    readonly edgesession_on_message: (a: number, b: number, c: number) => any;
    readonly edgesession_restore: (a: number, b: number, c: number, d: number, e: number, f: number) => [number, number, number];
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
