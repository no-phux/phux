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
    /**
     * Close the focused terminal without closing its siblings.
     *
     * # Errors
     * Refuses the last pane; use the host's Release Session control instead.
     */
    close_pane() {
        const ret = wasm.hostedclient_close_pane(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Move keyboard focus to the next published pane.
     *
     * # Errors
     * Fails when the connection has no usable terminal.
     */
    focus_next_pane() {
        const ret = wasm.hostedclient_focus_next_pane(this.__wbg_ptr);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
    }
    /**
     * Resize the terminal to `cols`x`rows` cells (for example after the
     * host element changes size). The canvas follows the server's new
     * geometry.
     * @param {number} cols
     * @param {number} rows
     */
    resize(cols, rows) {
        wasm.hostedclient_resize(this.__wbg_ptr, cols, rows);
    }
    /**
     * Split the focused terminal using one new resource on this connection.
     *
     * # Errors
     * Refuses invalid axes, a fifth pane, or a split while another is pending.
     * @param {string} axis
     */
    split_pane(axis) {
        const ptr0 = passStringToWasm0(axis, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
        const len0 = WASM_VECTOR_LEN;
        const ret = wasm.hostedclient_split_pane(this.__wbg_ptr, ptr0, len0);
        if (ret[1]) {
            throw takeFromExternrefTable0(ret[0]);
        }
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
 * structured lifecycle events through `callback`. An optional `signal` cancels
 * establishment, releasing the socket even before the attach completes.
 *
 * # Errors
 * Fails if the canvas element is missing or the connection can't be set up.
 * @param {string} ws_url
 * @param {string} canvas_id
 * @param {number} cols
 * @param {number} rows
 * @param {Function} callback
 * @param {AbortSignal | null} [signal]
 * @returns {Promise<HostedClient>}
 */
export function start_hosted(ws_url, canvas_id, cols, rows, callback, signal) {
    const ptr0 = passStringToWasm0(ws_url, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len0 = WASM_VECTOR_LEN;
    const ptr1 = passStringToWasm0(canvas_id, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
    const len1 = WASM_VECTOR_LEN;
    const ret = wasm.start_hosted(ptr0, len0, ptr1, len1, cols, rows, callback, isLikeNone(signal) ? 0 : addToExternrefTable0(signal));
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
        __wbg___wbindgen_boolean_get_5b446f51afd21013: function(arg0) {
            const v = arg0;
            const ret = typeof(v) === 'boolean' ? v : undefined;
            return isLikeNone(ret) ? 0xFFFFFF : ret ? 1 : 0;
        },
        __wbg___wbindgen_debug_string_4687d8d8c2017d52: function(arg0, arg1) {
            const ret = debugString(arg1);
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_is_function_1f9d30630b8b1d3d: function(arg0) {
            const ret = typeof(arg0) === 'function';
            return ret;
        },
        __wbg___wbindgen_is_undefined_8865fb403f8fe9d8: function(arg0) {
            const ret = arg0 === undefined;
            return ret;
        },
        __wbg___wbindgen_number_get_2e0e7dee9f701a71: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'number' ? obj : undefined;
            getDataViewMemory0().setFloat64(arg0 + 8 * 1, isLikeNone(ret) ? 0 : ret, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, !isLikeNone(ret), true);
        },
        __wbg___wbindgen_string_get_0380ccaa2f57f0d9: function(arg0, arg1) {
            const obj = arg1;
            const ret = typeof(obj) === 'string' ? obj : undefined;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg___wbindgen_throw_41e9ee4f547fc59a: function(arg0, arg1) {
            throw new Error(getStringFromWasm0(arg0, arg1));
        },
        __wbg__wbg_cb_unref_dcc1a90847f04c41: function(arg0) {
            arg0._wbg_cb_unref();
        },
        __wbg_abort_d5fa4a667a578774: function(arg0) {
            const ret = arg0.abort();
            return ret;
        },
        __wbg_aborted_5b2b5f06b5e59fde: function(arg0) {
            const ret = arg0.aborted;
            return ret;
        },
        __wbg_activeElement_69b55e3697f14d9b: function(arg0) {
            const ret = arg0.activeElement;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_addEventListener_4d0db17c671ea324: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.addEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_altKey_1e5fe17a42050103: function(arg0) {
            const ret = arg0.altKey;
            return ret;
        },
        __wbg_altKey_cfb4c4fab9a7d0d0: function(arg0) {
            const ret = arg0.altKey;
            return ret;
        },
        __wbg_appendChild_fb8c52e7dd8484ea: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.appendChild(arg1);
            return ret;
        }, arguments); },
        __wbg_apply_a910804df6e1e433: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.apply(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_beginPath_8598d895c13f1c86: function(arg0) {
            arg0.beginPath();
        },
        __wbg_body_e549239eaff082e1: function(arg0) {
            const ret = arg0.body;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_buffer_7afc7cca4d0cf036: function(arg0) {
            const ret = arg0.buffer;
            return ret;
        },
        __wbg_bufferedAmount_64b8a4229f549d4b: function(arg0) {
            const ret = arg0.bufferedAmount;
            return ret;
        },
        __wbg_button_1aa380c7ca420cd1: function(arg0) {
            const ret = arg0.button;
            return ret;
        },
        __wbg_buttons_de1e6c7f6de70736: function(arg0) {
            const ret = arg0.buttons;
            return ret;
        },
        __wbg_call_187d372bd5fdd4aa: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.call(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_cancelAnimationFrame_6d30ac112c49342a: function() { return handleError(function (arg0, arg1) {
            arg0.cancelAnimationFrame(arg1);
        }, arguments); },
        __wbg_clearInterval_f1a050672da2658c: function(arg0, arg1) {
            arg0.clearInterval(arg1);
        },
        __wbg_clearTimeout_3629d6209dfcc46e: function(arg0) {
            const ret = clearTimeout(arg0);
            return ret;
        },
        __wbg_clientX_667e5633f888238d: function(arg0) {
            const ret = arg0.clientX;
            return ret;
        },
        __wbg_clientY_2cdc00632f185e67: function(arg0) {
            const ret = arg0.clientY;
            return ret;
        },
        __wbg_clip_275e70d2dc182fec: function(arg0) {
            arg0.clip();
        },
        __wbg_clipboardData_0048090643ed57a2: function(arg0) {
            const ret = arg0.clipboardData;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_close_954efe52a1b209ca: function(arg0) {
            arg0.close();
        },
        __wbg_close_d3ed56b5763be5ae: function() { return handleError(function (arg0) {
            arg0.close();
        }, arguments); },
        __wbg_closest_6abd1bf5d7559cda: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.closest(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_code_422d539f5d7bacbb: function(arg0, arg1) {
            const ret = arg1.code;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_code_4cb6dbcfceec1eac: function(arg0) {
            const ret = arg0.code;
            return ret;
        },
        __wbg_createBidirectionalStream_6ce45e7d3d15bafb: function(arg0) {
            const ret = arg0.createBidirectionalStream();
            return ret;
        },
        __wbg_createElement_74049073a11f9c31: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.createElement(getStringFromWasm0(arg1, arg2));
            return ret;
        }, arguments); },
        __wbg_ctrlKey_282349f48e6cb157: function(arg0) {
            const ret = arg0.ctrlKey;
            return ret;
        },
        __wbg_ctrlKey_8b5101745e8782fa: function(arg0) {
            const ret = arg0.ctrlKey;
            return ret;
        },
        __wbg_data_522f7abc70721269: function(arg0) {
            const ret = arg0.data;
            return ret;
        },
        __wbg_data_64004cd467093bab: function(arg0, arg1) {
            const ret = arg1.data;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_data_7fb5381a4f605ae4: function(arg0, arg1) {
            const ret = arg1.data;
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_deltaMode_6280d8266d5520f5: function(arg0) {
            const ret = arg0.deltaMode;
            return ret;
        },
        __wbg_deltaY_7abc8fc9878d0002: function(arg0) {
            const ret = arg0.deltaY;
            return ret;
        },
        __wbg_dispatchEvent_57e1a8af186ff471: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.dispatchEvent(arg1);
            return ret;
        }, arguments); },
        __wbg_document_9854e03c05fc8834: function(arg0) {
            const ret = arg0.document;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_error_c9cf3fc2064683a9: function(arg0) {
            console.error(arg0);
        },
        __wbg_execCommand_b978e4b30307165f: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.execCommand(getStringFromWasm0(arg1, arg2));
            return ret;
        }, arguments); },
        __wbg_exports_01489638e7199ac6: function(arg0) {
            const ret = arg0.exports;
            return ret;
        },
        __wbg_fillRect_0ef59adb9acb7d06: function(arg0, arg1, arg2, arg3, arg4) {
            arg0.fillRect(arg1, arg2, arg3, arg4);
        },
        __wbg_fillText_1bcec8b81ad73bd0: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.fillText(getStringFromWasm0(arg1, arg2), arg3, arg4);
        }, arguments); },
        __wbg_focus_f740d61348f422e7: function() { return handleError(function (arg0) {
            arg0.focus();
        }, arguments); },
        __wbg_getAttribute_061ad00c16e2f622: function(arg0, arg1, arg2, arg3) {
            const ret = arg1.getAttribute(getStringFromWasm0(arg2, arg3));
            var ptr1 = isLikeNone(ret) ? 0 : passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            var len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_getBoundingClientRect_57152b1a20f3de34: function(arg0) {
            const ret = arg0.getBoundingClientRect();
            return ret;
        },
        __wbg_getContext_635e36719cad2623: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.getContext(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_getData_bc58070c107b7d8a: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = arg1.getData(getStringFromWasm0(arg2, arg3));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_getElementById_cc94972b404e4eaa: function(arg0, arg1, arg2) {
            const ret = arg0.getElementById(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_getModifierState_506d6a27dcf99b93: function(arg0, arg1, arg2) {
            const ret = arg0.getModifierState(getStringFromWasm0(arg1, arg2));
            return ret;
        },
        __wbg_get_31af05bd4842a84f: function() { return handleError(function (arg0, arg1) {
            const ret = Reflect.get(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_get_index_c2a95797340d0c18: function(arg0, arg1) {
            const ret = arg0[arg1 >>> 0];
            return ret;
        },
        __wbg_grow_1fb133f54188ea88: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.grow(arg1 >>> 0);
            return ret;
        }, arguments); },
        __wbg_hasAttribute_b009da6c546736e9: function(arg0, arg1, arg2) {
            const ret = arg0.hasAttribute(getStringFromWasm0(arg1, arg2));
            return ret;
        },
        __wbg_height_fb13ed9fe991b5f4: function(arg0) {
            const ret = arg0.height;
            return ret;
        },
        __wbg_height_fc97e1a0c2e7331f: function(arg0) {
            const ret = arg0.height;
            return ret;
        },
        __wbg_hostedclient_new: function(arg0) {
            const ret = HostedClient.__wrap(arg0);
            return ret;
        },
        __wbg_inputType_bcd41e55691a33d9: function(arg0, arg1) {
            const ret = arg1.inputType;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_insertBefore_f1ba67809033f6b1: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.insertBefore(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_instanceof_CanvasRenderingContext2d_769208c72dcbf5e6: function(arg0) {
            let result;
            try {
                result = arg0 instanceof CanvasRenderingContext2D;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_ClipboardEvent_cbfd05ef84dbba4f: function(arg0) {
            let result;
            try {
                result = arg0 instanceof ClipboardEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_CloseEvent_ded167d13facabaf: function(arg0) {
            let result;
            try {
                result = arg0 instanceof CloseEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_CompositionEvent_b172c3ce9b2f99bf: function(arg0) {
            let result;
            try {
                result = arg0 instanceof CompositionEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Element_818e11074cdb63b5: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Element;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlCanvasElement_0a30c11fbbf41841: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLCanvasElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlDocument_75043caeb1f2ded6: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLDocument;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlInputElement_5c33d1de59c09c49: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLInputElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_HtmlTextAreaElement_bbe97f862930488c: function(arg0) {
            let result;
            try {
                result = arg0 instanceof HTMLTextAreaElement;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_InputEvent_020398d31a393488: function(arg0) {
            let result;
            try {
                result = arg0 instanceof InputEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Instance_537755ac3ac35b8d: function(arg0) {
            let result;
            try {
                result = arg0 instanceof WebAssembly.Instance;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_KeyboardEvent_0176f04c0ee63f6d: function(arg0) {
            let result;
            try {
                result = arg0 instanceof KeyboardEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Memory_4748c125037a4cc1: function(arg0) {
            let result;
            try {
                result = arg0 instanceof WebAssembly.Memory;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_MouseEvent_ce70f2c3ec0e40af: function(arg0) {
            let result;
            try {
                result = arg0 instanceof MouseEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_PointerEvent_7da84319295c0048: function(arg0) {
            let result;
            try {
                result = arg0 instanceof PointerEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Table_25a3cf651f1b0d35: function(arg0) {
            let result;
            try {
                result = arg0 instanceof WebAssembly.Table;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_WheelEvent_614405cd78c84e54: function(arg0) {
            let result;
            try {
                result = arg0 instanceof WheelEvent;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instanceof_Window_82d71df4eddf88bc: function(arg0) {
            let result;
            try {
                result = arg0 instanceof Window;
            } catch (_) {
                result = false;
            }
            const ret = result;
            return ret;
        },
        __wbg_instantiate_48e0cb5c8bab6337: function(arg0, arg1, arg2) {
            const ret = WebAssembly.instantiate(getArrayU8FromWasm0(arg0, arg1), arg2);
            return ret;
        },
        __wbg_isComposing_600c9236938f8e5a: function(arg0) {
            const ret = arg0.isComposing;
            return ret;
        },
        __wbg_isComposing_d03a4fd2d6155b14: function(arg0) {
            const ret = arg0.isComposing;
            return ret;
        },
        __wbg_isSameNode_25f28dbf2e5865f1: function(arg0, arg1) {
            const ret = arg0.isSameNode(arg1);
            return ret;
        },
        __wbg_is_4b278c0bd3caba97: function(arg0, arg1) {
            const ret = Object.is(arg0, arg1);
            return ret;
        },
        __wbg_keyCode_fbee6c8fd374ff5e: function(arg0) {
            const ret = arg0.keyCode;
            return ret;
        },
        __wbg_key_1193871533b99ae5: function(arg0, arg1) {
            const ret = arg1.key;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_left_e0a244490fe2f293: function(arg0) {
            const ret = arg0.left;
            return ret;
        },
        __wbg_length_7f3c00c40364105e: function(arg0) {
            const ret = arg0.length;
            return ret;
        },
        __wbg_matchMedia_8a4857f947f11f82: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.matchMedia(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_matches_778d0873b064b1bf: function(arg0) {
            const ret = arg0.matches;
            return ret;
        },
        __wbg_metaKey_9c3be2ab835b0148: function(arg0) {
            const ret = arg0.metaKey;
            return ret;
        },
        __wbg_metaKey_fb78da4d237d5bbb: function(arg0) {
            const ret = arg0.metaKey;
            return ret;
        },
        __wbg_new_16797ab29a2ac227: function() { return handleError(function (arg0) {
            const ret = new ReadableStreamDefaultReader(arg0);
            return ret;
        }, arguments); },
        __wbg_new_1dbf7428bba60a42: function(arg0) {
            const ret = new Uint8Array(arg0);
            return ret;
        },
        __wbg_new_3e09b06cb26fb67d: function() { return handleError(function (arg0) {
            const ret = new WebAssembly.Module(arg0);
            return ret;
        }, arguments); },
        __wbg_new_4059fb0406225e04: function() { return handleError(function (arg0, arg1) {
            const ret = new WebSocket(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg_new_617a8cdb8bb1130e: function() {
            const ret = new Object();
            return ret;
        },
        __wbg_new_a198c2be75b00fe5: function() { return handleError(function (arg0, arg1) {
            const ret = new WebTransport(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg_new_d8e1e47df3827d47: function() { return handleError(function (arg0, arg1) {
            const ret = new WebAssembly.Instance(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_new_ee2291f50781bf1d: function() {
            const ret = new Array();
            return ret;
        },
        __wbg_new_f7e2e50f56fefeb2: function() { return handleError(function (arg0) {
            const ret = new WritableStreamDefaultWriter(arg0);
            return ret;
        }, arguments); },
        __wbg_new_from_slice_9a868026ffa4208a: function(arg0, arg1) {
            const ret = new Uint8Array(getArrayU8FromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_no_args_549db978c37d28cc: function(arg0, arg1) {
            const ret = new Function(getStringFromWasm0(arg0, arg1));
            return ret;
        },
        __wbg_new_typed_b01cb72a8af741a3: function(arg0, arg1) {
            try {
                var state0 = {a: arg0, b: arg1};
                var cb0 = (arg0, arg1) => {
                    const a = state0.a;
                    state0.a = 0;
                    try {
                        return wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___js_sys_19e6b106fee1164f___Function_fn_wasm_bindgen_4319d9ad64dc54c6___JsValue_____wasm_bindgen_4319d9ad64dc54c6___sys__Undefined___js_sys_19e6b106fee1164f___Function_fn_wasm_bindgen_4319d9ad64dc54c6___JsValue_____wasm_bindgen_4319d9ad64dc54c6___sys__Undefined_______true_(a, state0.b, arg0, arg1);
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
        __wbg_new_with_event_init_dict_536cb65c14ffc56b: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = new CustomEvent(getStringFromWasm0(arg0, arg1), arg2);
            return ret;
        }, arguments); },
        __wbg_new_with_str_sequence_f79e461d58fdeacc: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = new WebSocket(getStringFromWasm0(arg0, arg1), arg2);
            return ret;
        }, arguments); },
        __wbg_nextSibling_e5ecc9bf11c2365d: function(arg0) {
            const ret = arg0.nextSibling;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_now_d0b7f4bea9f38490: function(arg0) {
            const ret = arg0.now();
            return ret;
        },
        __wbg_open_63d11545c91841a8: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4, arg5, arg6) {
            const ret = arg0.open(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4), getStringFromWasm0(arg5, arg6));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_ownerDocument_32ced9dbf52cf8d2: function(arg0) {
            const ret = arg0.ownerDocument;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_parentElement_108b58de82cab63f: function(arg0) {
            const ret = arg0.parentElement;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_parentNode_32afd0d7ad7dca98: function(arg0) {
            const ret = arg0.parentNode;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_parse_0fc53dead14b3b42: function() { return handleError(function (arg0, arg1) {
            const ret = JSON.parse(getStringFromWasm0(arg0, arg1));
            return ret;
        }, arguments); },
        __wbg_performance_d0d7b03fc649b694: function(arg0) {
            const ret = arg0.performance;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_pointerId_1b91f6b6c8f92501: function(arg0) {
            const ret = arg0.pointerId;
            return ret;
        },
        __wbg_preventDefault_af59afb0f0a02e20: function(arg0) {
            arg0.preventDefault();
        },
        __wbg_prototypesetcall_bc27214492979395: function(arg0, arg1, arg2) {
            Uint8Array.prototype.set.call(getArrayU8FromWasm0(arg0, arg1), arg2);
        },
        __wbg_push_2baf45db356cf468: function(arg0, arg1) {
            const ret = arg0.push(arg1);
            return ret;
        },
        __wbg_querySelector_49877e2a9e3f670b: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.querySelector(getStringFromWasm0(arg1, arg2));
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        }, arguments); },
        __wbg_queueMicrotask_9833f9a49df95a49: function(arg0) {
            const ret = arg0.queueMicrotask;
            return ret;
        },
        __wbg_queueMicrotask_a72f977e97f23c5f: function(arg0) {
            queueMicrotask(arg0);
        },
        __wbg_read_2a943774344c9728: function(arg0) {
            const ret = arg0.read();
            return ret;
        },
        __wbg_readable_41e8ffa2e279379f: function(arg0) {
            const ret = arg0.readable;
            return ret;
        },
        __wbg_readyState_d8514376867e415b: function(arg0) {
            const ret = arg0.readyState;
            return ret;
        },
        __wbg_ready_6136bde9dfad6e5a: function(arg0) {
            const ret = arg0.ready;
            return ret;
        },
        __wbg_ready_e2644b82dba7a121: function(arg0) {
            const ret = arg0.ready;
            return ret;
        },
        __wbg_rect_c6f60004ffec8f09: function(arg0, arg1, arg2, arg3, arg4) {
            arg0.rect(arg1, arg2, arg3, arg4);
        },
        __wbg_removeAttribute_2f2700a6f933a6be: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.removeAttribute(getStringFromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_removeChild_3745fc2545da50fa: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.removeChild(arg1);
            return ret;
        }, arguments); },
        __wbg_removeEventListener_6e68185345978771: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            arg0.removeEventListener(getStringFromWasm0(arg1, arg2), arg3);
        }, arguments); },
        __wbg_removeProperty_13c9429e04312477: function() { return handleError(function (arg0, arg1, arg2, arg3) {
            const ret = arg1.removeProperty(getStringFromWasm0(arg2, arg3));
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        }, arguments); },
        __wbg_remove_9a5288388316028c: function(arg0) {
            arg0.remove();
        },
        __wbg_repeat_8e70ccf8a9875d04: function(arg0) {
            const ret = arg0.repeat;
            return ret;
        },
        __wbg_requestAnimationFrame_7b526ab7aa550c74: function() { return handleError(function (arg0, arg1) {
            const ret = arg0.requestAnimationFrame(arg1);
            return ret;
        }, arguments); },
        __wbg_resolve_0076e10020304ede: function(arg0) {
            const ret = Promise.resolve(arg0);
            return ret;
        },
        __wbg_restore_c93ba7571816b182: function(arg0) {
            arg0.restore();
        },
        __wbg_save_f32554f1747071d1: function(arg0) {
            arg0.save();
        },
        __wbg_select_2ecff207e9372789: function(arg0) {
            arg0.select();
        },
        __wbg_send_c1dad0ec2e52defb: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.send(getArrayU8FromWasm0(arg1, arg2));
        }, arguments); },
        __wbg_setAttribute_9e7d603908f63705: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.setAttribute(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_setData_5d08810e80ca40e2: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.setData(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_setInterval_aa4e3d3f590ce835: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = arg0.setInterval(arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_setPointerCapture_a53e23b7bccef290: function() { return handleError(function (arg0, arg1) {
            arg0.setPointerCapture(arg1);
        }, arguments); },
        __wbg_setProperty_097bc3d55ce44513: function() { return handleError(function (arg0, arg1, arg2, arg3, arg4) {
            arg0.setProperty(getStringFromWasm0(arg1, arg2), getStringFromWasm0(arg3, arg4));
        }, arguments); },
        __wbg_setTimeout_56bcdccbad22fd44: function() { return handleError(function (arg0, arg1) {
            const ret = setTimeout(arg0, arg1);
            return ret;
        }, arguments); },
        __wbg_set_145a351398b48c65: function() { return handleError(function (arg0, arg1, arg2) {
            const ret = Reflect.set(arg0, arg1, arg2);
            return ret;
        }, arguments); },
        __wbg_set_4b165309ba769a9b: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.set(arg1 >>> 0, arg2);
        }, arguments); },
        __wbg_set_575d3ddb70fe831d: function(arg0, arg1, arg2) {
            arg0.set(getArrayU8FromWasm0(arg1, arg2));
        },
        __wbg_set_binaryType_21835bf0df8f70aa: function(arg0, arg1) {
            arg0.binaryType = __wbindgen_enum_BinaryType[arg1];
        },
        __wbg_set_bubbles_13637ca47f23465d: function(arg0, arg1) {
            arg0.bubbles = arg1 !== 0;
        },
        __wbg_set_className_541fce5cd31918aa: function(arg0, arg1, arg2) {
            arg0.className = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_detail_69a5bfecc90520f4: function(arg0, arg1) {
            arg0.detail = arg1;
        },
        __wbg_set_fillStyle_a2961b4d44e572af: function(arg0, arg1, arg2) {
            arg0.fillStyle = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_font_1f60a05a2544a2ff: function(arg0, arg1, arg2) {
            arg0.font = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_height_c9789c1c77eaedff: function(arg0, arg1) {
            arg0.height = arg1 >>> 0;
        },
        __wbg_set_id_60955e6018d03b26: function(arg0, arg1, arg2) {
            arg0.id = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_index_1e17e9f8455f4ec8: function(arg0, arg1, arg2) {
            arg0[arg1 >>> 0] = arg2;
        },
        __wbg_set_innerHTML_7af59a832a09a074: function(arg0, arg1, arg2) {
            arg0.innerHTML = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_lineWidth_cc15473552c60c9c: function(arg0, arg1) {
            arg0.lineWidth = arg1;
        },
        __wbg_set_onclose_a84370531f3d2948: function(arg0, arg1) {
            arg0.onclose = arg1;
        },
        __wbg_set_onerror_94ee307653399172: function(arg0, arg1) {
            arg0.onerror = arg1;
        },
        __wbg_set_onmessage_cb6f77d2d8e0402a: function(arg0, arg1) {
            arg0.onmessage = arg1;
        },
        __wbg_set_onopen_c914147e8a2db4b0: function(arg0, arg1) {
            arg0.onopen = arg1;
        },
        __wbg_set_strokeStyle_d51608fa918b53d4: function(arg0, arg1, arg2) {
            arg0.strokeStyle = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_textBaseline_d5ba548751584f49: function(arg0, arg1, arg2) {
            arg0.textBaseline = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_textContent_6d6fc559f198055f: function(arg0, arg1, arg2) {
            arg0.textContent = arg1 === 0 ? undefined : getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_value_f5c1ffc19bac3037: function(arg0, arg1, arg2) {
            arg0.value = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_value_fbd659f94bfb9181: function(arg0, arg1, arg2) {
            arg0.value = getStringFromWasm0(arg1, arg2);
        },
        __wbg_set_width_b0e1267db4b196b5: function(arg0, arg1) {
            arg0.width = arg1 >>> 0;
        },
        __wbg_shiftKey_be6d5295ee392b33: function(arg0) {
            const ret = arg0.shiftKey;
            return ret;
        },
        __wbg_shiftKey_ecdc496db72e8a50: function(arg0) {
            const ret = arg0.shiftKey;
            return ret;
        },
        __wbg_static_accessor_GLOBAL_266715b9d96ba635: function() {
            const ret = typeof global === 'undefined' ? null : global;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_GLOBAL_THIS_10fb7dc1ae063179: function() {
            const ret = typeof globalThis === 'undefined' ? null : globalThis;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_SELF_0b583911f537483a: function() {
            const ret = typeof self === 'undefined' ? null : self;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_static_accessor_WINDOW_d7f903d1508cbdc4: function() {
            const ret = typeof window === 'undefined' ? null : window;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_strokeRect_c6e73405ca787ae9: function(arg0, arg1, arg2, arg3, arg4) {
            arg0.strokeRect(arg1, arg2, arg3, arg4);
        },
        __wbg_style_4bce24230e493a7c: function(arg0) {
            const ret = arg0.style;
            return ret;
        },
        __wbg_subarray_002b94d5e13d1411: function(arg0, arg1, arg2) {
            const ret = arg0.subarray(arg1 >>> 0, arg2 >>> 0);
            return ret;
        },
        __wbg_target_38ae9feb025b820c: function(arg0) {
            const ret = arg0.target;
            return isLikeNone(ret) ? 0 : addToExternrefTable0(ret);
        },
        __wbg_then_c949d5a25a4e78f8: function(arg0, arg1, arg2) {
            const ret = arg0.then(arg1, arg2);
            return ret;
        },
        __wbg_then_e71170d78fcf8954: function(arg0, arg1) {
            const ret = arg0.then(arg1);
            return ret;
        },
        __wbg_top_ff4627294d2cdeb8: function(arg0) {
            const ret = arg0.top;
            return ret;
        },
        __wbg_translate_b75b7d842d89a889: function() { return handleError(function (arg0, arg1, arg2) {
            arg0.translate(arg1, arg2);
        }, arguments); },
        __wbg_type_b805b444107983c3: function(arg0, arg1) {
            const ret = arg1.type;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_value_05305a761dfa3e0e: function(arg0, arg1) {
            const ret = arg1.value;
            const ptr1 = passStringToWasm0(ret, wasm.__wbindgen_malloc, wasm.__wbindgen_realloc);
            const len1 = WASM_VECTOR_LEN;
            getDataViewMemory0().setInt32(arg0 + 4 * 1, len1, true);
            getDataViewMemory0().setInt32(arg0 + 4 * 0, ptr1, true);
        },
        __wbg_warn_13abc63e0d4b3527: function(arg0) {
            console.warn(arg0);
        },
        __wbg_wasClean_b246ecade802c6ff: function(arg0) {
            const ret = arg0.wasClean;
            return ret;
        },
        __wbg_width_3d0dce3d9892e35e: function(arg0) {
            const ret = arg0.width;
            return ret;
        },
        __wbg_width_d5e379bde85b6eaa: function(arg0) {
            const ret = arg0.width;
            return ret;
        },
        __wbg_writable_382dc0100ad88bb0: function(arg0) {
            const ret = arg0.writable;
            return ret;
        },
        __wbg_write_bf07aa79dd36a472: function(arg0, arg1) {
            const ret = arg0.write(arg1);
            return ret;
        },
        __wbindgen_generic_0000000000000001: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [Externref], shim_idx: 260, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___JsValue__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true_);
            return ret;
        },
        __wbindgen_generic_0000000000000002: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("Event")], shim_idx: 8, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___web_sys_bb7631f4d8aa1b67___features__gen_MessageEvent__MessageEvent______true_);
            return ret;
        },
        __wbindgen_generic_0000000000000003: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("MessageEvent")], shim_idx: 8, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___web_sys_bb7631f4d8aa1b67___features__gen_MessageEvent__MessageEvent______true__11);
            return ret;
        },
        __wbindgen_generic_0000000000000004: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("WebTransportBidirectionalStream")], shim_idx: 7, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true_);
            return ret;
        },
        __wbindgen_generic_0000000000000005: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [NamedExternref("undefined")], shim_idx: 7, ret: Result(Unit), inner_ret: Some(Result(Unit)) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true__13);
            return ret;
        },
        __wbindgen_generic_0000000000000006: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [U32, U32], shim_idx: 189, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___u32__u32______true_);
            return ret;
        },
        __wbindgen_generic_0000000000000007: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [U32, U32], shim_idx: 9, ret: I32, inner_ret: Some(I32) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___u32__u32__i32__true_);
            return ret;
        },
        __wbindgen_generic_0000000000000008: function(arg0, arg1) {
            // Cast intrinsic for `Closure(Closure { owned: true, function: Function { arguments: [], shim_idx: 253, ret: Unit, inner_ret: Some(Unit) }, mutable: true }) -> Externref`.
            const ret = makeMutClosure(arg0, arg1, wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke_______true_);
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

function wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke_______true_(arg0, arg1) {
    wasm.wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke_______true_(arg0, arg1);
}

function wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___web_sys_bb7631f4d8aa1b67___features__gen_MessageEvent__MessageEvent______true_(arg0, arg1, arg2) {
    wasm.wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___web_sys_bb7631f4d8aa1b67___features__gen_MessageEvent__MessageEvent______true_(arg0, arg1, arg2);
}

function wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___web_sys_bb7631f4d8aa1b67___features__gen_MessageEvent__MessageEvent______true__11(arg0, arg1, arg2) {
    wasm.wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___web_sys_bb7631f4d8aa1b67___features__gen_MessageEvent__MessageEvent______true__11(arg0, arg1, arg2);
}

function wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___JsValue__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true_(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___JsValue__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true_(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true_(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true_(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true__13(arg0, arg1, arg2) {
    const ret = wasm.wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___wasm_bindgen_4319d9ad64dc54c6___sys__Undefined__core_ed718c3d60ebd546___result__Result_____wasm_bindgen_4319d9ad64dc54c6___JsError___true__13(arg0, arg1, arg2);
    if (ret[1]) {
        throw takeFromExternrefTable0(ret[0]);
    }
}

function wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___js_sys_19e6b106fee1164f___Function_fn_wasm_bindgen_4319d9ad64dc54c6___JsValue_____wasm_bindgen_4319d9ad64dc54c6___sys__Undefined___js_sys_19e6b106fee1164f___Function_fn_wasm_bindgen_4319d9ad64dc54c6___JsValue_____wasm_bindgen_4319d9ad64dc54c6___sys__Undefined_______true_(arg0, arg1, arg2, arg3) {
    wasm.wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___js_sys_19e6b106fee1164f___Function_fn_wasm_bindgen_4319d9ad64dc54c6___JsValue_____wasm_bindgen_4319d9ad64dc54c6___sys__Undefined___js_sys_19e6b106fee1164f___Function_fn_wasm_bindgen_4319d9ad64dc54c6___JsValue_____wasm_bindgen_4319d9ad64dc54c6___sys__Undefined_______true_(arg0, arg1, arg2, arg3);
}

function wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___u32__u32______true_(arg0, arg1, arg2, arg3) {
    wasm.wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___u32__u32______true_(arg0, arg1, arg2, arg3);
}

function wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___u32__u32__i32__true_(arg0, arg1, arg2, arg3) {
    const ret = wasm.wasm_bindgen_4319d9ad64dc54c6___convert__closures_____invoke___u32__u32__i32__true_(arg0, arg1, arg2, arg3);
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
