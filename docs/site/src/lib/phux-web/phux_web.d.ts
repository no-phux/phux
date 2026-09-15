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

/**
 * JS entry point for the WebTransport-first path: try HTTP/3-over-QUIC at
 * `wt_url` (an `https://` session URL; append `?token=<hex>` for a
 * token-authenticated listener) and fall back to the WebSocket at `ws_url`
 * when the API or the endpoint is unavailable. After initial readiness the
 * entry point supervises transport loss and repeats the bounded fallback.
 *
 * # Errors
 * Fails if the canvas element is missing or both transports fail to
 * connect.
 */
export function start_webtransport(wt_url: string, ws_url: string, canvas_id: string, cols: number, rows: number): Promise<void>;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_hostedclient_free: (a: number, b: number) => void;
    readonly hostedclient_close: (a: number) => void;
    readonly start: (a: number, b: number, c: number, d: number, e: number, f: number) => any;
    readonly start_hosted: (a: number, b: number, c: number, d: number, e: number, f: number, g: any) => any;
    readonly start_webtransport: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => any;
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___u32__u32__i32__true_: (a: number, b: number, c: number, d: number) => number;
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___js_sys_9f665ce99963efb1___Function_fn_wasm_bindgen_2a67c6f173b08fad___JsValue_____wasm_bindgen_2a67c6f173b08fad___sys__Undefined___js_sys_9f665ce99963efb1___Function_fn_wasm_bindgen_2a67c6f173b08fad___JsValue_____wasm_bindgen_2a67c6f173b08fad___sys__Undefined_______true_: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___JsValue__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true_: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true_: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true__10: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true_: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true__7: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true__8: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke_______true_: (a: number, b: number) => void;
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
