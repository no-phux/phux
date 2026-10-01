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
    /**
     * Close the focused terminal without closing its siblings.
     *
     * # Errors
     * Refuses the last pane; use the host's Release Session control instead.
     */
    close_pane(): void;
    /**
     * Move keyboard focus to the next published pane.
     *
     * # Errors
     * Fails when the connection has no usable terminal.
     */
    focus_next_pane(): void;
    /**
     * Resize the terminal to `cols`x`rows` cells (for example after the
     * host element changes size). The canvas follows the server's new
     * geometry.
     */
    resize(cols: number, rows: number): void;
    /**
     * Split the focused terminal using one new resource on this connection.
     *
     * # Errors
     * Refuses invalid axes, a fifth pane, or a split while another is pending.
     */
    split_pane(axis: string): void;
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
 * structured lifecycle events through `callback`. An optional `signal` cancels
 * establishment, releasing the socket even before the attach completes.
 *
 * # Errors
 * Fails if the canvas element is missing or the connection can't be set up.
 */
export function start_hosted(ws_url: string, canvas_id: string, cols: number, rows: number, callback: Function, signal?: AbortSignal | null): Promise<HostedClient>;

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
    readonly hostedclient_close_pane: (a: number) => [number, number];
    readonly hostedclient_focus_next_pane: (a: number) => [number, number];
    readonly hostedclient_resize: (a: number, b: number, c: number) => void;
    readonly hostedclient_split_pane: (a: number, b: number, c: number) => [number, number];
    readonly start: (a: number, b: number, c: number, d: number, e: number, f: number) => any;
    readonly start_hosted: (a: number, b: number, c: number, d: number, e: number, f: number, g: any, h: number) => any;
    readonly start_webtransport: (a: number, b: number, c: number, d: number, e: number, f: number, g: number, h: number) => any;
    readonly wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___u32__u32__i32__true_: (a: number, b: number, c: number, d: number) => number;
    readonly wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___js_sys_19e6b106fee1164f___Function_fn_wasm_bindgen_4319d9ad64dc54c6___JsValue_____wasm_bindgen_4319d9ad64dc54c6___sys__Undefined___js_sys_19e6b106fee1164f___Function_fn_wasm_bindgen_4319d9ad64dc54c6___JsValue_____wasm_bindgen_4319d9ad64dc54c6___sys__Undefined_______true_: (a: number, b: number, c: any, d: any) => void;
    readonly wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___u32__u32______true_: (a: number, b: number, c: number, d: number) => void;
    readonly wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___JsValue__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true_: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true_: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true__13: (a: number, b: number, c: any) => [number, number];
    readonly wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___web_sys_bb7631f4d8aa1b67___features__gen_MessageEvent__MessageEvent______true_: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___web_sys_bb7631f4d8aa1b67___features__gen_MessageEvent__MessageEvent______true__11: (a: number, b: number, c: any) => void;
    readonly wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke_______true_: (a: number, b: number) => void;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_exn_store: (a: number) => void;
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
