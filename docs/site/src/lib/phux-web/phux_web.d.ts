/* tslint:disable */
/* eslint-disable */

/**
 * A hosted session controller returned to JavaScript.
 */
export class HostedClient {
    private constructor();
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Close the socket and synchronously remove all browser handlers and timers.
     */
    close(): void;
}

/**
 * JS entry point: connect to `ws_url` and render the attached terminal into the
 * canvas element with id `canvas_id`, sized `cols`×`rows`.
 *
 * # Errors
 * Fails if the canvas element is missing or the connection can't be set up.
 */
export function start(ws_url: string, canvas_id: string, cols: number, rows: number): Promise<void>;

/**
 * Hosted JS entry point. Unlike [`start`], this requires the hosted session
 * control preamble before accepting binary phux wire frames and reports safe,
 * structured lifecycle events through `callback`.
 *
 * # Errors
 * Fails if the canvas element is missing or the connection can't be set up.
 */
export function start_hosted(ws_url: string, canvas_id: string, cols: number, rows: number, callback: Function): Promise<HostedClient>;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly start: (a: number, b: number, c: number, d: number, e: number, f: number) => any;
    readonly __wbg_hostedclient_free: (a: number, b: number) => void;
    readonly hostedclient_close: (a: number) => void;
    readonly start_hosted: (a: number, b: number, c: number, d: number, e: number, f: number, g: any) => any;
    readonly wasm_bindgen__convert__closures_____invoke__hcfc5522f822be379: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen__convert__closures_____invoke__h3aea8bc5d570c941: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h00ed1e8b06293b39: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h00ed1e8b06293b39_2: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h00ed1e8b06293b39_3: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen__convert__closures_____invoke__h24594d4a37771efb: (a: number, b: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_destroy_closure: (a: number, b: number) => void;
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
