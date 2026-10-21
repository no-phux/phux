//! Client lifetime: create, free, state, `HELLO`, and the attach request.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::{mem, ptr};

use phux_protocol::SessionId;
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::wire::frame::{AttachTarget, ViewportInfo};

use super::{
    BridgeError, Client, Limits, PhuxAttachOptions, PhuxBytes, PhuxClient, PhuxClientOptions,
    PhuxClientResult, PhuxClientState, bytes_out, check_struct, outbound_bytes_in, with_client_mut,
};

/// Creates a client owned by the calling thread.
///
/// # Safety
///
/// When non-null, `options` must be readable and `out_client` must be valid for
/// writes for the duration of the call. The two pointees must not overlap.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_new(
    options: *const PhuxClientOptions,
    out_client: *mut *mut PhuxClient,
) -> PhuxClientResult {
    match catch_unwind(AssertUnwindSafe(|| -> Result<(), BridgeError> {
        // SAFETY: checked before write.
        let out = unsafe { out_client.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_client is null"))?;
        *out = ptr::null_mut();
        // SAFETY: checked before dereference.
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        check_struct(
            options.size,
            mem::size_of::<PhuxClientOptions>(),
            options.version,
        )?;
        *out = Box::into_raw(Box::new(PhuxClient::new(Client::new(client_limits(
            options,
        )?))));
        Ok(())
    })) {
        Ok(Ok(())) => PhuxClientResult::Ok,
        Ok(Err(error)) => error.result,
        Err(_) => PhuxClientResult::Panic,
    }
}

/// True when the requested history page row bound is zero or above the
/// protocol limit.
const fn history_page_rows_out_of_range(rows: u32) -> bool {
    rows == 0 || rows > phux_client_core::history::MAX_HISTORY_PAGE_ROWS
}

/// True when the requested cache bounds cannot retain one opaque page.
pub(super) fn history_cache_cannot_retain_page(options: &PhuxClientOptions) -> bool {
    options.max_history_cache_bytes == 0
        || options.max_history_materialized_rows == 0
        || usize::try_from(options.max_history_page_bytes).is_err()
        || usize::try_from(options.max_history_page_bytes)
            .is_ok_and(|bytes| bytes > options.max_history_cache_bytes)
}

/// Resolves the bootstrap and history bounds a new client will enforce.
pub(crate) fn client_limits(options: &PhuxClientOptions) -> Result<Limits, BridgeError> {
    let limits = BootstrapLimits::new(
        options.max_bootstrap_chunk_bytes,
        options.max_history_page_bytes,
    )
    .ok_or_else(|| {
        BridgeError::invalid("bootstrap/history bounds are zero or exceed protocol limits")
    })?;
    if history_page_rows_out_of_range(options.max_history_page_rows) {
        return Err(BridgeError::invalid(
            "history page row bound is zero or exceeds the protocol limit",
        ));
    }
    if history_cache_cannot_retain_page(options) {
        return Err(BridgeError::invalid(
            "history cache bounds cannot retain one requested page",
        ));
    }
    Ok(Limits {
        bootstrap_chunk: limits.max_chunk_bytes(),
        history_page: limits.max_history_page_bytes(),
        history_page_rows: options.max_history_page_rows,
        history_cache_bytes: options.max_history_cache_bytes,
        history_materialized_rows: options.max_history_materialized_rows,
        history_prefetch_rows: options.history_prefetch_rows,
    })
}

/// Destroys a client.
///
/// # Safety
///
/// When non-null, `client` must be a live pointer returned by
/// `phux_client_new`, uniquely owned by the caller, on its owning thread, and
/// not previously freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_free(client: *mut PhuxClient) {
    if unsafe { client.as_ref() }.is_some_and(|client| client.inner.in_callback) {
        return;
    }
    if !client.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            // SAFETY: caller transfers the unique pointer returned by phux_client_new once.
            drop(unsafe { Box::from_raw(client) });
        }));
    }
}

/// Returns the client's lifecycle state.
///
/// # Safety
///
/// When non-null, `client` must point to a live client on its owning thread and
/// remain valid and unmodified for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_state(client: *const PhuxClient) -> PhuxClientState {
    unsafe { client.as_ref() }.map_or(PhuxClientState::Failed, |client| {
        if client.inner.in_callback {
            PhuxClientState::Failed
        } else {
            client.inner.state()
        }
    })
}

/// Returns the client's most recent error message.
///
/// # Safety
///
/// When non-null, `client` must point to a live client on its owning thread and
/// remain valid and unmodified for the call. When non-null, `out_error` must be
/// valid writable storage. The returned bytes remain valid until the next
/// mutable call using `client`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_last_error(
    client: *const PhuxClient,
    out_error: *mut PhuxBytes,
) -> PhuxClientResult {
    let result = catch_unwind(AssertUnwindSafe(|| {
        let client = unsafe { client.as_ref() }.ok_or(PhuxClientResult::InvalidArgument)?;
        if client.inner.in_callback {
            return Err(PhuxClientResult::InvalidState);
        }
        let out = unsafe { out_error.as_mut() }.ok_or(PhuxClientResult::InvalidArgument)?;
        *out = bytes_out(&client.inner.last_error);
        Ok(())
    }));
    match result {
        Ok(Ok(())) => PhuxClientResult::Ok,
        Ok(Err(error)) => error,
        Err(_) => PhuxClientResult::Panic,
    }
}

/// Queues the initial protocol greeting.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When `client_name.len` is nonzero,
/// `client_name.data` must be readable for that many bytes for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_hello(
    client: *mut PhuxClient,
    client_name: PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let name = unsafe { outbound_bytes_in(client_name.data, client_name.len, "client name") }?;
        let name = std::str::from_utf8(name)
            .map_err(|_| BridgeError::invalid("client name is not UTF-8"))?;
        if name.is_empty() {
            return Err(BridgeError::invalid("client name is empty"));
        }
        if !client.control().open_explicit(name.to_owned()) {
            return Err(BridgeError::state("HELLO was already queued or negotiated"));
        }
        client.drain_outbound();
        Ok(())
    })
}

/// Queues an attach request.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. `options` may be null and is rejected;
/// otherwise it must be a readable, valid `PhuxAttachOptions`, and any
/// non-empty `options.name` span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_attach(
    client: *mut PhuxClient,
    options: *const PhuxAttachOptions,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        check_struct(
            options.size,
            mem::size_of::<PhuxAttachOptions>(),
            options.version,
        )?;
        validate_attach_options(options)?;
        ensure_attach_allowed(client)?;
        let name_bytes =
            unsafe { outbound_bytes_in(options.name.data, options.name.len, "attach name") }?;
        let target = attach_target(options, name_bytes)?;
        queue_attach_frame(client, options, target)
    })
}

/// Rejects an ATTACH the runtime lifecycle cannot accept.
fn ensure_attach_allowed(client: &Client) -> Result<(), BridgeError> {
    if client.control().status() != phux_client_runtime::control::Status::Negotiated
        || client.detached
    {
        return Err(BridgeError::state(
            "ATTACH is not valid in the current lifecycle state",
        ));
    }
    Ok(())
}

fn validate_attach_options(options: &PhuxAttachOptions) -> Result<(), BridgeError> {
    if options.attach_id == 0 {
        return Err(BridgeError::invalid("attach_id must be non-zero"));
    }
    if options.cols == 0 || options.rows == 0 {
        return Err(BridgeError::invalid("attach geometry must be non-zero"));
    }
    if options.has_pixel_size {
        if options.pixel_width == 0 || options.pixel_height == 0 {
            return Err(BridgeError::invalid(
                "attach pixel geometry must be non-zero when present",
            ));
        }
    } else if options.pixel_width != 0 || options.pixel_height != 0 {
        return Err(BridgeError::invalid(
            "attach pixel geometry is present without its discriminator",
        ));
    }
    Ok(())
}

fn attach_target(
    options: &PhuxAttachOptions,
    name_bytes: &[u8],
) -> Result<AttachTarget, BridgeError> {
    let name = std::str::from_utf8(name_bytes)
        .map_err(|_| BridgeError::invalid("attach name is not UTF-8"))?;
    let target = match options.target_kind {
        0 => AttachTarget::Last,
        1 => AttachTarget::ByName(name.to_owned()),
        2 => AttachTarget::ById(SessionId::new(options.session_id)),
        3 => AttachTarget::CreateIfMissing {
            name: name.to_owned(),
            command: None,
            cwd: None,
        },
        _ => return Err(BridgeError::invalid("unknown attach target kind")),
    };
    if matches!(options.target_kind, 1 | 3) && name.is_empty() {
        return Err(BridgeError::invalid("named attach target is empty"));
    }
    Ok(target)
}

fn queue_attach_frame(
    client: &mut Client,
    options: &PhuxAttachOptions,
    target: AttachTarget,
) -> Result<(), BridgeError> {
    let pixels = options
        .has_pixel_size
        .then_some((options.pixel_width, options.pixel_height));
    let viewport = ViewportInfo::new(options.cols, options.rows)
        .with_pixels(pixels.map(|value| value.0), pixels.map(|value| value.1));
    let role_policy = client.next_attach_role();
    if !client.control().attach_explicit(
        options.attach_id,
        target,
        viewport,
        options.request_scrollback,
        options.scrollback_limit_lines,
        role_policy,
    ) {
        return Err(BridgeError::state(
            "ATTACH is not valid in the current lifecycle state",
        ));
    }
    client.attach_queued = true;
    client.expected_attach_id = Some(options.attach_id);
    client.drain_outbound();
    Ok(())
}
