//! Geometry and presentation: resize, scroll, anchors, history, selection, search.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use phux_protocol::wire::frame::ViewportInfo;

use super::{
    BridgeError, PhuxBytes, PhuxClient, PhuxClientResult, PhuxDocumentAnchor, PhuxDocumentPoint,
    PhuxResourceId, PhuxSearchResult, bytes_in, bytes_out, invoke_failure, terminal_id_in,
    with_client_mut,
};

/// Requests exact terminal geometry through the runtime's readiness and role checks.
/// Success means queued, not applied; authoritative grids are the readback.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_terminal_resize(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    cols: u16,
    rows: u16,
) -> PhuxClientResult {
    unsafe {
        phux_client_terminal_resize_with_cell_px(client, terminal_id, cols, rows, false, 0, 0)
    }
}

/// Same as [`phux_client_terminal_resize`]. When `has_cell_px` is set, the
/// queued `RESIZE_TERMINAL` carries that integer cell size.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_terminal_resize_with_cell_px(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    cols: u16,
    rows: u16,
    has_cell_px: bool,
    cell_width: u16,
    cell_height: u16,
) -> PhuxClientResult {
    use phux_client_runtime::control::TerminalResizeOutcome;
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let cell_px = if has_cell_px {
            if cell_width == 0 || cell_height == 0 {
                return Err(BridgeError::invalid(
                    "terminal cell geometry must be non-zero when present",
                ));
            }
            Some((cell_width, cell_height))
        } else if cell_width != 0 || cell_height != 0 {
            return Err(BridgeError::invalid(
                "terminal cell geometry is present without its discriminator",
            ));
        } else {
            None
        };
        if client.operations.subscription_pending(&terminal_id) {
            return Err(BridgeError::state(
                "terminal subscription change is pending",
            ));
        }
        let outcome = client.control().resize_terminal_with_cell(
            &terminal_id,
            u32::from(cols),
            u32::from(rows),
            cell_px,
        );
        match outcome {
            TerminalResizeOutcome::Queued => {
                client.drain_outbound();
                Ok(())
            }
            TerminalResizeOutcome::InvalidSize => Err(BridgeError::invalid(
                "terminal resize geometry must be non-zero",
            )),
            TerminalResizeOutcome::Observer => Err(BridgeError::state(
                "terminal resize is not allowed for an observe-only subscription",
            )),
            TerminalResizeOutcome::NotReady => Err(BridgeError::state(
                "terminal resize requires a live ready subscription",
            )),
        }
    })
}

/// Submits desired viewport geometry, retaining it until ATTACH is ready.
/// Success accepts intent, not application; session size policy remains authoritative.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_viewport_resize(
    client: *mut PhuxClient,
    cols: u16,
    rows: u16,
    has_pixel_size: bool,
    pixel_width: u16,
    pixel_height: u16,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if cols == 0 || rows == 0 {
            return Err(BridgeError::invalid(
                "viewport resize geometry must be non-zero",
            ));
        }
        if client.control().status().is_terminal() {
            return Err(BridgeError::state("viewport resize requires a live client"));
        }
        if has_pixel_size {
            if pixel_width == 0 || pixel_height == 0 {
                return Err(BridgeError::invalid(
                    "viewport pixel geometry must be non-zero when present",
                ));
            }
        } else if pixel_width != 0 || pixel_height != 0 {
            return Err(BridgeError::invalid(
                "viewport pixel geometry is present without its discriminator",
            ));
        }
        let pixels = has_pixel_size.then_some((pixel_width, pixel_height));
        client.control().resize_viewport_info(
            ViewportInfo::new(cols, rows)
                .with_pixels(pixels.map(|value| value.0), pixels.map(|value| value.1)),
        );
        client.drain_outbound();
        Ok(())
    })
}

/// Scrolls a terminal's history viewport.
///
/// `kind` 0 is the top, 1 the live tail, 2 `value` rows (negative toward history), 3 absolute row `value`,
/// and 4 the `value`th shell prompt (OSC 133) above (negative) or below the
/// viewport's top row, as Ghostty's `jump_to_prompt`.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_scroll_viewport(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    kind: u32,
    value: i64,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.scroll(&terminal_id, kind, value)
    })
}

/// Creates an engine-tracked document anchor.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable, and `out_anchor` must be
/// valid writable storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_anchor_create(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    point: PhuxDocumentPoint,
    out_anchor: *mut PhuxDocumentAnchor,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let out = unsafe { out_anchor.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_anchor is null"))?;
        *out = PhuxDocumentAnchor::default();
        *out = client.track_anchor(&terminal_id, point)?;
        Ok(())
    })
}

/// Releases an engine-tracked document anchor.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_anchor_release(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    anchor: PhuxDocumentAnchor,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.release_anchor(&terminal_id, anchor)
    })
}

/// Clear this client's active display and scrollback.
///
/// Preserves the live terminal and protocol sequence. The expected generation must match exactly.
/// All document anchors for the terminal are invalidated. No input is sent.
///
/// # Safety
///
/// `client` must be a live client on its owning thread with exclusive access.
/// `terminal_id` and its non-empty host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_clear_presentation(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    stream_id: u64,
    bootstrap_id: u64,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.clear_presentation(&terminal_id, stream_id, bootstrap_id)
    })
}

/// Pins the history viewport to an engine-tracked anchor.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_history_viewport_pin(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    anchor: PhuxDocumentAnchor,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.pin_viewport(&terminal_id, anchor)
    })
}

/// Returns the history viewport to the live terminal bottom.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_history_follow_live(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.follow_live(&terminal_id)
    })
}

/// Sets the terminal's selected document range.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_selection_set(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    start: PhuxDocumentAnchor,
    end: PhuxDocumentAnchor,
    rectangle: bool,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.set_selection(&terminal_id, start, end, rectangle)
    })
}

/// Clears the terminal's selected document range.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_selection_clear(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.clear_selection(&terminal_id)
    })
}

/// Returns the selected terminal text.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable, and `out_text` must be valid
/// writable storage. The returned bytes remain valid until the next mutable
/// call using `client`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_selection_text(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    out_text: *mut PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let out =
            unsafe { out_text.as_mut() }.ok_or_else(|| BridgeError::invalid("out_text is null"))?;
        *out = PhuxBytes::default();
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.selection_text(&terminal_id)?;
        *out = bytes_out(&client.selection_buf);
        Ok(())
    })
}

/// Snapshot the kernel's performance telemetry (ADR-0096) as a JSON `PerfReport`.
///
/// Frames applied and their bytes, engine apply time, and the echo round trip
/// from a key or paste leaving `phux_client_send_*` to the first output frame
/// for that terminal, then the runtime's owner-thread apply round trips and
/// grid publications (`runtime.*`). Always on; counters since the client was
/// created. The bytes are borrowed from the client and valid until the next
/// `phux_client_perf_json` call.
///
/// # Safety
///
/// `client` must be a live handle from `phux_client_new`; `out_json` must be
/// a valid, writable pointer.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_perf_json(
    client: *mut PhuxClient,
    out_json: *mut PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let out =
            unsafe { out_json.as_mut() }.ok_or_else(|| BridgeError::invalid("out_json is null"))?;
        *out = PhuxBytes::default();
        client.perf_json();
        *out = bytes_out(&client.perf_buf);
        Ok(())
    })
}

/// Searches the terminal document and returns borrowed results.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id`, any non-empty
/// satellite host span, and any non-empty `query_utf8` span must be readable.
/// When non-null, `out_results` and `out_count` must be valid writable storage.
/// The returned result array remains valid until the next mutable call using `client`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_search(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    query_utf8: PhuxBytes,
    case_sensitive: bool,
    out_results: *mut *const PhuxSearchResult,
    out_count: *mut usize,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let out_results = unsafe { out_results.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_results is null"))?;
        let out_count = unsafe { out_count.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_count is null"))?;
        *out_results = ptr::null();
        *out_count = 0;
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let query = unsafe { bytes_in(query_utf8.data, query_utf8.len) }?;
        client.search(&terminal_id, query, case_sensitive)?;
        *out_results = if client.search_results.is_empty() {
            ptr::null()
        } else {
            client.search_results.as_ptr()
        };
        *out_count = client.search_results.len();
        Ok(())
    })
}

/// Releases all anchors owned by the currently borrowed search result array.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_search_results_release(
    client: *mut PhuxClient,
) -> PhuxClientResult {
    let Some(client_ref) = (unsafe { client.as_mut() }) else {
        return PhuxClientResult::InvalidArgument;
    };
    if client_ref.inner.in_callback {
        return PhuxClientResult::InvalidState;
    }
    let result = match catch_unwind(AssertUnwindSafe(|| {
        client_ref.inner.release_search_results()
    })) {
        Ok(Ok(())) => {
            client_ref.inner.last_error.clear();
            PhuxClientResult::Ok
        }
        Ok(Err(error)) => {
            client_ref.inner.set_error(&error.message);
            error.result
        }
        Err(_) => {
            client_ref
                .inner
                .set_error("panic contained at phux-client FFI boundary");
            PhuxClientResult::Panic
        }
    };
    if result == PhuxClientResult::Ok {
        result
    } else {
        invoke_failure(client, result)
    }
}
