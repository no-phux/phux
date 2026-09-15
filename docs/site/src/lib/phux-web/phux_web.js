/* @ts-self-types="./phux_web.d.ts" */

/**
 * A hosted session controller returned to JavaScript.
 */
export class HostedClient {
    static __wrap(ptr) {
        const obj = Object.create(HostedClient.prototype);
        obj.__wbg_ptr = ptr;
        HostedClientFinalization.register(obj, obj.__wbg_ptr, obj);
        return obj;
    }
    __destroy_into_raw() {
        const ptr = this.__wbg_ptr;
        this.__wbg_ptr = 0;
        HostedClientFinalization.unregister(this);
        return ptr;
    }
    free() {
        const ptr = this.__destroy_into_raw();
        wasm.__wbg_hostedclient_free(ptr, 0);
    }
    /**
     * Close the socket and synchronously remove all browser handlers and timers.
     */
    close() {
        wasm.hostedclient_close(this.__wbg_ptr);
    }
}
if (Symbol.dispose) HostedClient.prototype[Symbol.dispose] = HostedClient.prototype.free;

/**
 * JS entry point: connect to `ws_url` and render the attached terminal into the
 * canvas element with id `canvas_id`, sized `cols`×`rows`.
 *
 * # Errors
 * Fails if the canvas element is missing or the connection can't be set up.
 * @param {string} ws_url
 * @param {string} canvas_id
 * @param {number} cols
 * @param {number} rows
 * @returns {Promise<void>}
 */
export function start(ws_url, canvas_id, cols, rows) {
    const ptr0 = passStringToWasm0(ws_url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(canvas_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.start(ptr0, len0, ptr1, len1, cols, rows);
    return ret;
}

/**
 * Hosted JS entry point. Unlike [`start`], this requires the hosted session
 * control preamble before accepting binary phux wire frames and reports safe,
 * structured lifecycle events through `callback`.
 *
 * # Errors
 * Fails if the canvas element is missing or the connection can't be set up.
 * @param {string} ws_url
 * @param {string} canvas_id
 * @param {number} cols
 * @param {number} rows
 * @param {Function} callback
 * @returns {Promise<HostedClient>}
 */
export function start_hosted(ws_url, canvas_id, cols, rows, callback) {
    const ptr0 = passStringToWasm0(ws_url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(canvas_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.start_hosted(ptr0, len0, ptr1, len1, cols, rows, callback);
    return ret;
}

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
 * @param {string} wt_url
 * @param {string} ws_url
 * @param {string} canvas_id
 * @param {number} cols
 * @param {number} rows
 * @returns {Promise<void>}
 */
export function start_webtransport(wt_url, ws_url, canvas_id, cols, rows) {
    const ptr0 = passStringToWasm0(wt_url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(ws_url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ptr2 = passStringToWasm0(canvas_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len2 = WASM_VECTOR_LEN;
    const ret = wasm.start_webtransport(ptr0, len0, ptr1, len1, ptr2, len2, cols, rows);
    return ret;
}
function __wbg_get_imports() {
    const import0 = {
        __proto__: null,
        __wbg___wbindgen_boolean_get_7a12af2b3f899c5a: function(arg0) {
            const v = arg0;
            const ret = typeof(v) === 'boolean' ? v : undefined;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg___wbindgen_debug_string_0e68cf47c9cbd9b0: function(arg0, arg1) {
            const ret = debugString(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_function_fcda5e3902d732fe: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_undefined_8c687d0b90d5b524: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_number_get_1dc732b810cb937c: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_string_get_92ab86bb19cbc12f: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_5d9e815e6fdf150f: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_997e73d32238e655: function(arg0) {
            arg0._wbg_cb_unref();
        },
        __wbg_abort_bbf8d9fde084bf78: function(arg0) {
            const ret = arg0.abort();
            return ret;
        },
        __wbg_addEventListener_9534e2dc12044ee4: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.addEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_altKey_fa381deee4b395cd: function(arg0) {
            const ret = arg0.altKey;
            return ret;
        },
        __wbg_appendChild_0a61ee9a9da4556f: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.appendChild(arg1);
            return ret;
        }, arguments); },
        __wbg_apply_5d9aa7604c2490a8: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.apply(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_body_7d83a19bffb260db: function(arg0) {
            const ret = arg0.body;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_buffer_60eec204070b2802: function(arg0) {
            const ret = arg0.buffer;
            return ret;
        },
        __wbg_bufferedAmount_fc5916c107f506df: function(arg0) {
            const ret = arg0.bufferedAmount;
            return ret;
        },
        __wbg_call_6bcf8d3e20937e46: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_clearInterval_d8d44413537029f6: function(arg0, arg1) {
            arg0.clearInterval(arg1);
        },
        __wbg_clearTimeout_3629d6209dfcc46e: function(arg0) {
            const ret = clearTimeout(arg0);
            return ret;
        },
        __wbg_close_1c036ed53353fc0a: function(arg0) {
            arg0.close();
        },
        __wbg_close_a009f540ff811039: function() { return handleError(function (arg0) {
            arg0.close();
        }, arguments); },
        __wbg_code_6bc89190cbffee9e: function(arg0) {
            const ret = arg0.code;
            return ret;
        },
        __wbg_code_de6c620a1a241f59: function(arg0, arg1) {
            const ret = arg1.code;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_createBidirectionalStream_9da6ab86a361386f: function(arg0) {
            const ret = arg0.createBidirectionalStream();
            return ret;
        },
        __wbg_createElement_b9024dc5ba95ac27: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.createElement(getStringFromWasm0(arg1, arg2));
            return ret;
        }, arguments); },
        __wbg_ctrlKey_785e802b8798af13: function(arg0) {
            const ret = arg0.ctrlKey;
            return ret;
        },
        __wbg_data_aa7ba49b34c0d883: function(arg0) {
            const ret = arg0.data;
            return ret;
        },
        __wbg_document_c7f486c52d63d24e: function(arg0) {
            const ret = arg0.document;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_error_756c5934221e6fee: function(arg0) {
            console.error(arg0);
        },
        __wbg_exports_89c3f1bc156ad5da: function(arg0) {
            const ret = arg0.exports;
            return ret;
        },
        __wbg_fillRect_ff9957352a08db2c: function(arg0, arg1, arg2, arg3, arg4) {
            arg0.fillRect(arg1, arg2, arg3, arg4);
        },
        __wbg_fillText_9b463cd65b9c1016: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.fillText(getStringFromWasm0(arg1, arg2), arg3, arg4);
        }, arguments); },
        __wbg_getContext_e0c05ffee530bdcf: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.getContext(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_getElementById_ccc92d66acf76819: function(arg0, arg1, arg2) {
            const ret = arg0.getElementById(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_get_989d0a1309644f2b: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_get_index_a916f0ff97f6772a: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        },
        __wbg_height_c15ee46a1345e9af: function(arg0) {
            const ret = arg0.height;
            return ret;
        },
        __wbg_hostedclient_new: function(arg0) {
            const ret = HostedClient.__wrap(arg0);
            return ret;
        },
        __wbg_instanceof_CanvasRenderingContext2d_bde5245b14027f3f: function(arg0) {
            let result;
            try {
                result = arg0 instanceof CanvasRenderingContext2D;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_CloseEvent_10e1b11b5f106135: function(arg0) {
            let result;
            try {
                result = arg0 instanceof CloseEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlCanvasElement_4d7e131643d814c1: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLCanvasElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Instance_b6357685b8ef5a6e: function(arg0) {
            let result;
            try {
                result = arg0 instanceof WebAssembly.Instance;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Memory_0e6851ed67934513: function(arg0) {
            let result;
            try {
                result = arg0 instanceof WebAssembly.Memory;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_a3b8566f0a9c5d1a: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instantiate_64c7f8b825d3068a: function(arg0, arg1, arg2) {
            const ret = WebAssembly.instantiate(getArrayU8FromWasm0(arg0, arg1), arg2);
            return ret;
        },
        __wbg_key_d29d670923b81510: function(arg0, arg1) {
            const ret = arg1.key;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_length_31bdaf014f5fbde2: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_metaKey_3f9183dfd5e1b726: function(arg0) {
            const ret = arg0.metaKey;
            return ret;
        },
        __wbg_new_1da3429bc3c4541c: function(arg0) {
            const ret = new Uint8Array(arg0);
            return ret;
        },
        __wbg_new_1e3b92f38a4e243f: function() { return handleError(function (arg0, arg1) {
            const ret = new WebTransport(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg_new_5db3e955fdac89fc: function() { return handleError(function (arg0) {
            const ret = new WritableStreamDefaultWriter(arg0);
            return ret;
        }, arguments); },
        __wbg_new_5f587e22401e8fe9: function() { return handleError(function (arg0) {
            const ret = new ReadableStreamDefaultReader(arg0);
            return ret;
        }, arguments); },
        __wbg_new_82926b2a4d30e175: function() { return handleError(function (arg0, arg1) {
            const ret = new WebSocket(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg_new_bebc3f4757acf305: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_new_ffa92086ea89f79c: function() {
            const ret = new Array();
            return ret;
        },
        __wbg_new_from_slice_4ee02165f9de919e: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_no_args_61f96c76373d34b7: function(arg0, arg1) {
            const ret = new Function(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_typed_6f8b0d724fe26c07: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___js_sys_9f665ce99963efb1___Function_fn_wasm_bindgen_2a67c6f173b08fad___JsValue_____wasm_bindgen_2a67c6f173b08fad___sys__Undefined___js_sys_9f665ce99963efb1___Function_fn_wasm_bindgen_2a67c6f173b08fad___JsValue_____wasm_bindgen_2a67c6f173b08fad___sys__Undefined_______true_(a, state0.b, arg0, arg1);
                    } finally {
                        state0.a = a;
                    }
                };
                const ret = new Promise(cb0);
                return ret;
            } finally {
                state0.a = 0;
            }
        },
        __wbg_new_with_str_sequence_9ab4875494df9d99: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = new WebSocket(getStringFromWasm0(arg0, arg1), arg2);
            return ret;
        }, arguments); },
        __wbg_now_0b7736bc17fd7719: function(arg0) {
            const ret = arg0.now();
            return ret;
        },
        __wbg_ownerDocument_5e91fd7a8aefc7cc: function(arg0) {
            const ret = arg0.ownerDocument;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_parentElement_c0b090e8023d3df2: function(arg0) {
            const ret = arg0.parentElement;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_parse_6937a9050adfb0e1: function() { return handleError(function (arg0, arg1) {
            const ret = JSON.parse(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg_performance_d96ad0b441f4510a: function(arg0) {
            const ret = arg0.performance;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_preventDefault_4235a4ccf8540533: function(arg0) {
            arg0.preventDefault();
        },
        __wbg_prototypesetcall_ae9f5e7459250748: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        },
        __wbg_push_bfdf956ba476f65b: function(arg0, arg1) {
            const ret = arg0.push(arg1);
            return ret;
        },
        __wbg_queueMicrotask_85c90f6987555d65: function(arg0) {
            const ret = arg0.queueMicrotask;
            return ret;
        },
        __wbg_queueMicrotask_f6a1fa10b81d1fc0: function(arg0) {
            queueMicrotask(arg0);
        },
        __wbg_read_31091533ffadf971: function(arg0) {
            const ret = arg0.read();
            return ret;
        },
        __wbg_readable_b1bfe1bf305ca892: function(arg0) {
            const ret = arg0.readable;
            return ret;
        },
        __wbg_readyState_83df235b27cf257f: function(arg0) {
            const ret = arg0.readyState;
            return ret;
        },
        __wbg_ready_196967f79af47b0d: function(arg0) {
            const ret = arg0.ready;
            return ret;
        },
        __wbg_ready_b4854eb24fbf43d3: function(arg0) {
            const ret = arg0.ready;
            return ret;
        },
        __wbg_removeAttribute_8ce0ab2fa42b383d: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.removeAttribute(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_removeEventListener_4c9706814dcfaf79: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.removeEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_resolve_35ec7e0c6af4c82c: function(arg0) {
            const ret = Promise.resolve(arg0);
            return ret;
        },
        __wbg_send_af811d92e7aea5c4: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.send(getArrayU8FromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_setAttribute_8c84e9351986b1f6: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.setAttribute(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_setInterval_11cf709e8a350d32: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.setInterval(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_setTimeout_56bcdccbad22fd44: function() { return handleError(function (arg0, arg1) {
            const ret = setTimeout(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_set_5f2ad37e5e02dc7b: function(arg0, arg1, arg2) {
            arg0.set(getArrayU8FromWasm0(arg1, arg2));
        },
        __wbg_set_a377297433dfea63: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(arg0, arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_set_binaryType_f88804219cea0f9e: function(arg0, arg1) {
            arg0.binaryType = __wbindgen_enum_BinaryType[arg1];
        },
        __wbg_set_className_f682baa8f21f2696: function(arg0, arg1, arg2) {
            arg0.className = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_fillStyle_59940a18480ccdf7: function(arg0, arg1, arg2) {
            arg0.fillStyle = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_font_50854c5bdd1ca607: function(arg0, arg1, arg2) {
            arg0.font = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_height_698fb3b255bc1348: function(arg0, arg1) {
            arg0.height = arg1 >>> 0;
        },
        __wbg_set_id_dc5bd387fca63e7d: function(arg0, arg1, arg2) {
            arg0.id = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_index_bc485a48c4864d05: function(arg0, arg1, arg2) {
            arg0[arg1 >>> 0] = arg2;
        },
        __wbg_set_onclose_97aab74a1c2662f7: function(arg0, arg1) {
            arg0.onclose = arg1;
        },
        __wbg_set_onerror_88cb2134dca3aba2: function(arg0, arg1) {
            arg0.onerror = arg1;
        },
        __wbg_set_onmessage_0f0d391465909a45: function(arg0, arg1) {
            arg0.onmessage = arg1;
        },
        __wbg_set_onopen_e91e64af99b7966d: function(arg0, arg1) {
            arg0.onopen = arg1;
        },
        __wbg_set_textBaseline_41538118f83d0399: function(arg0, arg1, arg2) {
            arg0.textBaseline = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_textContent_729c78ca859aa5a5: function(arg0, arg1, arg2) {
            arg0.textContent = arg1 === 0 ? undefined : getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_width_4c3a2252e0dea033: function(arg0, arg1) {
            arg0.width = arg1 >>> 0;
        },
        __wbg_shiftKey_2b651c3e5a9c07f5: function(arg0) {
            const ret = arg0.shiftKey;
            return ret;
        },
        __wbg_static_accessor_GLOBAL_8eb4cd83130a11a0: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_1e7044f654e934db: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_SELF_d8b50611246a6d92: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_fd0bc376bf0f8b42: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_subarray_1daff70dde20c145: function(arg0, arg1, arg2) {
            const ret = arg0.subarray(arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_then_7a850dae4493f353: function(arg0, arg1, arg2) {
            const ret = arg0.then(arg1, arg2);
            return ret;
        },
        __wbg_then_b830475380919203: function(arg0, arg1) {
            const ret = arg0.then(arg1);
            return ret;
        },
        __wbg_warn_d3544c7814fab534: function(arg0) {
            console.warn(arg0);
        },
        __wbg_wasClean_a32ef5f1fd90161e: function(arg0) {
            const ret = arg0.wasClean;
            return ret;
        },
        __wbg_width_b5e609025d3f7451: function(arg0) {
            const ret = arg0.width;
            return ret;
        },
        __wbg_writable_b32d3608b067eee1: function(arg0) {
            const ret = arg0.writable;
            return ret;
        },
        __wbg_write_d32d19927af3f942: function(arg0, arg1) {
            const ret = arg0.write(arg1);
            return ret;
        },
        __wbindgen_generic_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 222, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___JsValue__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true_);
            return ret;
        },
        __wbindgen_generic_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("Event")], shim_idx: 66, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true_);
            return ret;
        },
        __wbindgen_generic_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("KeyboardEvent")], shim_idx: 66, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true__7);
            return ret;
        },
        __wbindgen_generic_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("MessageEvent")], shim_idx: 66, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true__8);
            return ret;
        },
        __wbindgen_generic_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("WebTransportBidirectionalStream")], shim_idx: 64, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true_);
            return ret;
        },
        __wbindgen_generic_0000000000000006: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("undefined")], shim_idx: 64, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true__10);
            return ret;
        },
        __wbindgen_generic_0000000000000007: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [U32, U32], shim_idx: 71, ret: I32, inner_ret: Some(I32) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___u32__u32__i32__true_);
            return ret;
        },
        __wbindgen_generic_0000000000000008: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [], shim_idx: 204, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke_______true_);
            return ret;
        },
        __wbindgen_generic_0000000000000009: function(arg0) {
            // Cast intrinsic for `F64 -> Externref`.
            const ret = arg0;
            return ret;
        },
        __wbindgen_generic_000000000000000a: function(arg0, arg1) {
            // Cast intrinsic for `Ref(String) -> Externref`.
            const ret = getStringFromWasm0(arg0, arg1);
            return ret;
        },
        __wbindgen_init_externref_table: function() {
            const table = wasm.__wbindgen_externrefs;
            const offset = table.grow(4);
            table.set(0, undefined);
            table.set(offset + 0, undefined);
            table.set(offset + 1, null);
            table.set(offset + 2, true);
            table.set(offset + 3, false);
        },
    };
    return {
        __proto__: null,
        "./phux_web_bg.js": import0,
    };
}

function wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke_______true_(arg0, arg1) {
    wasm.wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke_______true_(arg0, arg1);
}

function wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true_(arg0, arg1, arg2) {
    wasm.wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true_(arg0, arg1, arg2);
}

function wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true__7(arg0, arg1, arg2) {
    wasm.wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true__7(arg0, arg1, arg2);
}

function wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true__8(arg0, arg1, arg2) {
    wasm.wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___web_sys_1bc51f71935ac26a___features__gen_MessageEvent__MessageEvent______true__8(arg0, arg1, arg2);
}

function wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___JsValue__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true_(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___JsValue__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true_(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true_(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true_(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true__10(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___wasm_bindgen_2a67c6f173b08fad___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_2a67c6f173b08fad___JsError___true__10(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___js_sys_9f665ce99963efb1___Function_fn_wasm_bindgen_2a67c6f173b08fad___JsValue_____wasm_bindgen_2a67c6f173b08fad___sys__Undefined___js_sys_9f665ce99963efb1___Function_fn_wasm_bindgen_2a67c6f173b08fad___JsValue_____wasm_bindgen_2a67c6f173b08fad___sys__Undefined_______true_(arg0, arg1, arg2, arg3) {
    wasm.wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___js_sys_9f665ce99963efb1___Function_fn_wasm_bindgen_2a67c6f173b08fad___JsValue_____wasm_bindgen_2a67c6f173b08fad___sys__Undefined___js_sys_9f665ce99963efb1___Function_fn_wasm_bindgen_2a67c6f173b08fad___JsValue_____wasm_bindgen_2a67c6f173b08fad___sys__Undefined_______true_(arg0, arg1, arg2, arg3);
}

function wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___u32__u32__i32__true_(arg0, arg1, arg2, arg3) {
    const ret = wasm.wasm_bindgen_2a67c6f173b08fad___convert__closures_____invoke___u32__u32__i32__true_(arg0, arg1, arg2, arg3);
    return ret;
}


const __wbindgen_enum_BinaryType = ["blob", "arraybuffer"];
const HostedClientFinalization = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(ptr => wasm.__wbg_hostedclient_free(ptr, 1));

function addToExternrefTable0(obj) {
    const idx = wasm.__externref_table_alloc();
    wasm.__wbindgen_externrefs.set(idx, obj);
    return idx;
}

const CLOSURE_DTORS = (typeof FinalizationRegistry === 'undefined')
    ? { register: () => {}, unregister: () => {} }
    : new FinalizationRegistry(state => wasm.__wbindgen_destroy_closure(state.a, state.b));

function debugString(val) {
    // primitive types
    const type = typeof val;
    if (type == 'number' || type == 'boolean' || val == null) {
        return  `${val}`;
    }
    if (type == 'string') {
        return `"${val}"`;
    }
    if (type == 'symbol') {
        const description = val.description;
        if (description == null) {
            return 'Symbol';
        } else {
            return `Symbol(${description})`;
        }
    }
    if (type == 'function') {
        const name = val.name;
        if (typeof name == 'string' && name.length > 0) {
            return `Function(${name})`;
        } else {
            return 'Function';
        }
    }
    // objects
    if (Array.isArray(val)) {
        const length = val.length;
        let debug = '[';
        if (length > 0) {
            debug += debugString(val[0]);
        }
        for(let i = 1; i < length; i++) {
            debug += ', ' + debugString(val[i]);
        }
        debug += ']';
        return debug;
    }
    // Test for built-in
    const builtInMatches = /\[object ([^\]]+)\]/.exec(toString.call(val));
    let className;
    if (builtInMatches && builtInMatches.length > 1) {
        className = builtInMatches[1];
    } else {
        // Failed to match the standard '[object ClassName]'
        return toString.call(val);
    }
    if (className == 'Object') {
        // we're a user defined class or Object
        // JSON.stringify avoids problems with cycles, and is generally much
        // easier than looping through ownProperties of `val`.
        try {
            return 'Object(' + JSON.stringify(val) + ')';
        } catch (_) {
            return 'Object';
        }
    }
    // errors
    if (val instanceof Error) {
        return `${val.name}: ${val.message}\n${val.stack}`;
    }
    // TODO we could test for more things here, like `Set`s and `Map`s.
    return className;
}

function getArrayU8FromWasm0(ptr, len) {
    ptr = ptr >>> 0;
    return getUint8ArrayMemory0().subarray(ptr / 1, ptr / 1 + len);
}

let cachedDataViewMemory0 = null;
function getDataViewMemory0() {
    if (cachedDataViewMemory0 === null || cachedDataViewMemory0.buffer.detached === true || (cachedDataViewMemory0.buffer.detached === undefined && cachedDataViewMemory0.buffer !== wasm.memory.buffer)) {
        cachedDataViewMemory0 = new DataView(wasm.memory.buffer);
    }
    return cachedDataViewMemory0;
}

function getStringFromWasm0(ptr, len) {
    return decodeText(ptr >>> 0, len);
}

let cachedUint8ArrayMemory0 = null;
function getUint8ArrayMemory0() {
    if (cachedUint8ArrayMemory0 === null || cachedUint8ArrayMemory0.byteLength === 0) {
        cachedUint8ArrayMemory0 = new Uint8Array(wasm.memory.buffer);
    }
    return cachedUint8ArrayMemory0;
}

function handleError(f, args) {
    try {
        return f.apply(this, args);
    } catch (e) {
        const idx = addToExternrefTable0(e);
        wasm.__wbindgen_exn_store(idx);
    }
}

function isLikeNone(x) {
    return x === undefined || x === null;
}

function makeMutClosure(arg0, arg1, f) {
    const state = { a: arg0, b: arg1, cnt: 1 };
    const real = (...args) => {

        // First up with a closure we increment the internal reference
        // count. This ensures that the Rust closure environment won't
        // be deallocated while we're invoking it.
        state.cnt++;
        const a = state.a;
        state.a = 0;
        try {
            return f(a, state.b, ...args);
        } finally {
            state.a = a;
            real._wbg_cb_unref();
        }
    };
    real._wbg_cb_unref = () => {
        if (--state.cnt === 0) {
            wasm.__wbindgen_destroy_closure(state.a, state.b);
            state.a = 0;
            CLOSURE_DTORS.unregister(state);
        }
    };
    CLOSURE_DTORS.register(real, state, state);
    return real;
}

function passStringToWasm0(arg, malloc, realloc) {
    if (realloc === undefined) {
        const buf = cachedTextEncoder.encode(arg);
        const ptr = malloc(buf.length, 1) >>> 0;
        getUint8ArrayMemory0().subarray(ptr, ptr + buf.length).set(buf);
        WASM_VECTOR_LEN = buf.length;
        return ptr;
    }

    let len = arg.length;
    let ptr = malloc(len, 1) >>> 0;

    const mem = getUint8ArrayMemory0();

    let offset = 0;

    for (; offset < len; offset++) {
        const code = arg.charCodeAt(offset);
        if (code > 0x7F) break;
        mem[ptr + offset] = code;
    }
    if (offset !== len) {
        if (offset !== 0) {
            arg = arg.slice(offset);
        }
        ptr = realloc(ptr, len, len = offset + arg.length * 3, 1) >>> 0;
        const view = getUint8ArrayMemory0().subarray(ptr + offset, ptr + len);
        const ret = cachedTextEncoder.encodeInto(arg, view);

        offset += ret.written;
        ptr = realloc(ptr, len, offset, 1) >>> 0;
    }

    WASM_VECTOR_LEN = offset;
    return ptr;
}

function takeFromExternrefTable0(idx) {
    const value = wasm.__wbindgen_externrefs.get(idx);
    wasm.__externref_table_dealloc(idx);
    return value;
}

let cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
cachedTextDecoder.decode();
const MAX_SAFARI_DECODE_BYTES = 2146435072;
let numBytesDecoded = 0;
function decodeText(ptr, len) {
    numBytesDecoded += len;
    if (numBytesDecoded >= MAX_SAFARI_DECODE_BYTES) {
        cachedTextDecoder = new TextDecoder('utf-8', { ignoreBOM: true, fatal: true });
        cachedTextDecoder.decode();
        numBytesDecoded = len;
    }
    return cachedTextDecoder.decode(getUint8ArrayMemory0().subarray(ptr, ptr + len));
}

const cachedTextEncoder = new TextEncoder();

if (!('encodeInto' in cachedTextEncoder)) {
    cachedTextEncoder.encodeInto = function (arg, view) {
        const buf = cachedTextEncoder.encode(arg);
        view.set(buf);
        return {
            read: arg.length,
            written: buf.length
        };
    };
}

let WASM_VECTOR_LEN = 0;

let wasmModule, wasmInstance, wasm;
function __wbg_finalize_init(instance, module) {
    wasmInstance = instance;
    wasm = instance.exports;
    wasmModule = module;
    cachedDataViewMemory0 = null;
    cachedUint8ArrayMemory0 = null;
    wasm.__wbindgen_start();
    return wasm;
}

async function __wbg_load(module, imports) {
    if (typeof Response === 'function' && module instanceof Response) {
        if (!module.ok) {
            throw new Error(`failed to fetch Wasm: ${module.status} ${module.statusText} fetching '${module.url}'`);
        }

        if (typeof WebAssembly.instantiateStreaming === 'function') {
            try {
                return await WebAssembly.instantiateStreaming(module, imports);
            } catch (e) {
                const validResponse = expectedResponseType(module.type);

                if (validResponse && module.headers.get('Content-Type') !== 'application/wasm') {
                    console.warn("`WebAssembly.instantiateStreaming` failed because your server does not serve Wasm with `application/wasm` MIME type. Falling back to `WebAssembly.instantiate` which is slower. Original error:\n", e);

                } else { throw e; }
            }
        }

        const bytes = await module.arrayBuffer();
        return await WebAssembly.instantiate(bytes, imports);
    } else {
        const instance = await WebAssembly.instantiate(module, imports);

        if (instance instanceof WebAssembly.Instance) {
            return { instance, module };
        } else {
            return instance;
        }
    }

    function expectedResponseType(type) {
        switch (type) {
            case 'basic': case 'cors': case 'default': return true;
        }
        return false;
    }
}

function initSync(module) {
    if (wasm !== undefined) return wasm;


    if (module !== undefined) {
        if (Object.getPrototypeOf(module) === Object.prototype) {
            ({module} = module)
        } else {
            console.warn('using deprecated parameters for `initSync()`; pass a single object instead')
        }
    }

    const imports = __wbg_get_imports();
    if (!(module instanceof WebAssembly.Module)) {
        module = new WebAssembly.Module(module);
    }
    const instance = new WebAssembly.Instance(module, imports);
    return __wbg_finalize_init(instance, module);
}

async function __wbg_init(module_or_path) {
    if (wasm !== undefined) return wasm;


    if (module_or_path !== undefined) {
        if (Object.getPrototypeOf(module_or_path) === Object.prototype) {
            ({module_or_path} = module_or_path)
        } else {
            console.warn('using deprecated parameters for the initialization function; pass a single object instead')
        }
    }

    if (module_or_path === undefined) {
        module_or_path = new URL('phux_web_bg.wasm', import.meta.url);
    }
    const imports = __wbg_get_imports();

    if (typeof module_or_path === 'string' || (typeof Request === 'function' && module_or_path instanceof Request) || (typeof URL === 'function' && module_or_path instanceof URL)) {
        module_or_path = fetch(module_or_path);
    }

    const { instance, module } = await __wbg_load(await module_or_path, imports);

    return __wbg_finalize_init(instance, module);
}

export { initSync, __wbg_init as default };
