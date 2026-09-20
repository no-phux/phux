//! Stable native C bridge for the synchronous phux session kernel.

#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[cfg(target_arch = "wasm32")]
compile_error!("phux-client-ffi is a native-only libghostty bridge");

mod client;
mod directory;
mod error;
mod grid_metadata;
mod log;
mod operations;
mod pointer;
mod projection;
mod remote;
mod session_create;
mod session_query;
mod session_rename;
mod types;
mod workspace;

use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use client::{Client, Limits};
use error::{BridgeError, bytes_in, check_struct, outbound_bytes_in, terminal_id_in};
#[cfg(test)]
use phux_client_core::engine::CanonicalGeometry;
#[cfg(test)]
use phux_client_core::session::KernelInput;
#[cfg(test)]
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::ResourceKind;
use phux_protocol::SessionId;
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};

pub use directory::*;
pub use grid_metadata::*;
pub use log::*;
pub use operations::*;
pub use pointer::{
    PhuxSelectionGestureEvent, PhuxSelectionGestureResult, phux_client_selection_gesture,
    phux_client_terminal_mouse_mode,
};
pub use projection::*;
pub use remote::registry::*;
pub use remote::*;
pub use session_create::*;
pub use session_query::*;
pub use session_rename::*;
pub use types::*;
pub use workspace::*;

#[repr(C)]
pub struct PhuxClient {
    inner: Client,
    _not_send_sync: std::marker::PhantomData<*mut ()>,
}

impl std::fmt::Debug for PhuxClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PhuxClient")
            .field("state", &self.inner.state())
            .finish_non_exhaustive()
    }
}

fn with_client_mut(
    client: *mut PhuxClient,
    f: impl FnOnce(&mut Client) -> Result<(), BridgeError>,
) -> PhuxClientResult {
    let Some(client_ref) = (unsafe { client.as_mut() }) else {
        return PhuxClientResult::InvalidArgument;
    };
    if client_ref.inner.in_callback {
        return PhuxClientResult::InvalidState;
    }
    let result = match catch_unwind(AssertUnwindSafe(|| {
        client_ref.inner.reset_borrows();
        f(&mut client_ref.inner)
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

fn with_client_ref(
    client: *const PhuxClient,
    f: impl FnOnce(&Client) -> Result<(), BridgeError>,
) -> PhuxClientResult {
    let Some(client_ref) = (unsafe { client.as_ref() }) else {
        return PhuxClientResult::InvalidArgument;
    };
    if client_ref.inner.in_callback {
        return PhuxClientResult::InvalidState;
    }
    match catch_unwind(AssertUnwindSafe(|| f(&client_ref.inner))) {
        Ok(Ok(())) => PhuxClientResult::Ok,
        Ok(Err(error)) => error.result,
        Err(_) => PhuxClientResult::Panic,
    }
}

fn invoke_failure(client: *mut PhuxClient, result: PhuxClientResult) -> PhuxClientResult {
    let invocation = {
        let client_ref = unsafe { &mut *client };
        let Some(callback) = client_ref.inner.callbacks.on_failure else {
            return result;
        };
        client_ref.inner.in_callback = true;
        (
            callback,
            client_ref.inner.callbacks.userdata,
            bytes_out(&client_ref.inner.last_error),
        )
    };
    let callback_result = catch_unwind(AssertUnwindSafe(|| unsafe {
        invocation.0(invocation.1, result, invocation.2);
    }));
    let client_ref = unsafe { &mut *client };
    client_ref.inner.in_callback = false;
    if callback_result.is_err() {
        client_ref
            .inner
            .set_error("panic contained in phux-client failure callback");
        PhuxClientResult::Panic
    } else {
        result
    }
}

fn invoke_attached(client: *mut PhuxClient) -> PhuxClientResult {
    let invocation = {
        let client_ref = unsafe { &mut *client };
        if client_ref.inner.attached_notified {
            return PhuxClientResult::Ok;
        }
        client_ref.inner.attached_notified = true;
        let Some(callback) = client_ref.inner.callbacks.on_attached else {
            return PhuxClientResult::Ok;
        };
        client_ref.inner.in_callback = true;
        (callback, client_ref.inner.callbacks.userdata)
    };
    let callback_result = catch_unwind(AssertUnwindSafe(|| unsafe {
        invocation.0(invocation.1);
    }));
    let client_ref = unsafe { &mut *client };
    client_ref.inner.in_callback = false;
    if callback_result.is_err() {
        client_ref
            .inner
            .set_error("panic contained in phux-client attached callback");
        PhuxClientResult::Panic
    } else {
        PhuxClientResult::Ok
    }
}

fn apply_input(
    client: &mut Client,
    terminal_id: &phux_protocol::ResourceId,
    event: &InputEvent,
) -> Result<(), BridgeError> {
    client.ensure_attached()?;
    if client.operations.detaching(terminal_id) {
        return Err(BridgeError::state("terminal detach is pending"));
    }
    let queued = match event {
        InputEvent::Key(event) => client.control.send_key(terminal_id, event.clone()),
        InputEvent::Mouse(event) => client.control.send_mouse(terminal_id, *event),
        InputEvent::Focus(event) => client.control.send_focus(terminal_id, *event),
        InputEvent::Paste(event) => {
            client
                .control
                .send_paste(terminal_id, event.data.clone(), event.trust)
        }
        _ => false,
    };
    if !queued {
        return Err(BridgeError::state(
            "terminal input is not currently eligible",
        ));
    }
    client.drain_outbound();
    Ok(())
}

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
        let client = Box::new(PhuxClient {
            inner: Client::new(client_limits(options)?),
            _not_send_sync: std::marker::PhantomData,
        });
        *out = Box::into_raw(client);
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
fn history_cache_cannot_retain_page(options: &PhuxClientOptions) -> bool {
    options.max_history_cache_bytes == 0
        || options.max_history_materialized_rows == 0
        || usize::try_from(options.max_history_page_bytes).is_err()
        || usize::try_from(options.max_history_page_bytes)
            .is_ok_and(|bytes| bytes > options.max_history_cache_bytes)
}

/// Resolves the bootstrap and history bounds a new client will enforce.
fn client_limits(options: &PhuxClientOptions) -> Result<Limits, BridgeError> {
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
        if !client.control.open_explicit(name.to_owned()) {
            return Err(BridgeError::state("HELLO was already queued or negotiated"));
        }
        client.hello_queued = true;
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
        #[cfg(test)]
        seed_legacy_test_lifecycle(client)?;
        ensure_attach_allowed(client)?;
        let name_bytes =
            unsafe { outbound_bytes_in(options.name.data, options.name.len, "attach name") }?;
        let target = attach_target(options, name_bytes)?;
        queue_attach_frame(client, options, target)
    })
}

/// Rejects an ATTACH the runtime lifecycle cannot accept.
fn ensure_attach_allowed(client: &Client) -> Result<(), BridgeError> {
    if client.control.status() != phux_client_runtime::control::Status::Negotiated
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
    if !client.control.attach_explicit(
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

/// Processes one complete server frame through the runtime control plane.
///
/// # Safety
/// `client` is exclusively owned for the call and `data` is readable for `len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_feed_frame(
    client: *mut PhuxClient,
    data: *const u8,
    len: usize,
) -> PhuxClientResult {
    let mut notify_attached = false;
    let result = with_client_mut(client, |client| {
        let data = unsafe { bytes_in(data, len) }?;
        let frame = decode_server_frame(client, data)?;
        notify_attached = apply_server_frame(client, frame)?;
        Ok(())
    });
    if result == PhuxClientResult::Ok && notify_attached {
        invoke_attached(client)
    } else {
        result
    }
}

fn decode_server_frame(client: &Client, data: &[u8]) -> Result<FrameKind, BridgeError> {
    let limits = client.control.decode_limits().unwrap_or_else(|| {
        BootstrapLimits::new(client.limits.bootstrap_chunk, client.limits.history_page)
            .unwrap_or_default()
    });
    let (frame, tail) = FrameKind::decode_with_limits(data, limits)
        .map_err(|error| BridgeError::protocol(error.to_string()))?;
    if !tail.is_empty() {
        return Err(BridgeError::protocol(
            "feed_frame accepts exactly one complete frame",
        ));
    }
    Ok(frame)
}

fn apply_server_frame(client: &mut Client, frame: FrameKind) -> Result<bool, BridgeError> {
    if consume_retired_close(client, &frame) {
        return Ok(false);
    }
    let extension_frame = frame.clone();
    let closed_terminal = match &frame {
        FrameKind::ResourceClosed { terminal_id, .. } => Some(terminal_id.clone()),
        _ => None,
    };
    #[cfg(test)]
    seed_legacy_test_lifecycle(client)?;
    validate_runtime_frame(client, &frame)?;
    let runtime_result = client.control.feed(frame).map_err(control_error);
    let extension_result = if runtime_result.is_ok() {
        observe_runtime_frame(client, &extension_frame);
        dispatch_extension_frame(client, extension_frame)
    } else {
        Ok(())
    };
    let attached = client.process_runtime_events()?;
    if let Some(terminal_id) = closed_terminal
        && !client.workspace.subscriptions.was_closed(&terminal_id)
    {
        client.finish_terminal_close(&terminal_id)?;
        client.publish_effects();
    }
    runtime_result?;
    extension_result?;
    if matches!(
        client.control.status(),
        phux_client_runtime::control::Status::Negotiated
    ) {
        session_rename::negotiated(client)?;
    }
    Ok(attached)
}

fn consume_retired_close(client: &mut Client, frame: &FrameKind) -> bool {
    let FrameKind::ResourceClosed { terminal_id, .. } = frame else {
        return false;
    };
    if !client.workspace.subscriptions.was_withdrawn(terminal_id)
        && !client.workspace.subscriptions.was_closed(terminal_id)
    {
        return false;
    }
    client.workspace.subscriptions.cancel(terminal_id);
    client.workspace.subscriptions.mark_closed(terminal_id);
    client
        .resources
        .retain(|resource| &resource.id != terminal_id);
    true
}

fn validate_runtime_frame(client: &Client, frame: &FrameKind) -> Result<(), BridgeError> {
    if client.detached {
        return Err(BridgeError::protocol("server frame arrived after DETACHED"));
    }
    let terminal = match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            profile,
            ..
        } => {
            let agent_profile = matches!(
                profile,
                phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1
            );
            if client.is_agent_stream(terminal_id) != agent_profile {
                return Err(BridgeError::protocol(
                    "bootstrap profile does not match the declared resource kind",
                ));
            }
            Some(terminal_id)
        }
        FrameKind::BootstrapChunk { terminal_id, .. }
        | FrameKind::BootstrapReady { terminal_id, .. }
        | FrameKind::BootstrapTombstone { terminal_id, .. }
        | FrameKind::ResourceOutput { terminal_id, .. }
        | FrameKind::HistoryPage { terminal_id, .. }
        | FrameKind::HistoryTombstone { terminal_id, .. }
        | FrameKind::HistoryRejected { terminal_id, .. }
        | FrameKind::ResourceClosed { terminal_id, .. } => Some(terminal_id),
        _ => None,
    };
    if let Some(terminal) = terminal {
        let retired_close = matches!(frame, FrameKind::ResourceClosed { .. })
            && (client.workspace.subscriptions.was_withdrawn(terminal)
                || client.workspace.subscriptions.was_closed(terminal));
        if !retired_close {
            client.ensure_participant(terminal)?;
        }
    }
    Ok(())
}

fn observe_runtime_frame(client: &mut Client, frame: &FrameKind) {
    if let FrameKind::BootstrapBegin {
        terminal_id,
        stream_id,
        bootstrap_id,
        profile: phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1,
        ..
    } = frame
    {
        client.open_agent_generation(terminal_id, *stream_id, *bootstrap_id);
    }
}

/// Feed runtime-owned values through the binding-specific extension projections.
fn dispatch_extension_frame(client: &mut Client, frame: FrameKind) -> Result<(), BridgeError> {
    let Some(frame) = directory::dispatch(client, frame) else {
        return Ok(());
    };
    let Some(frame) = session_create::dispatch(client, frame)? else {
        return Ok(());
    };
    let Some(frame) = session_query::dispatch(client, frame)? else {
        return Ok(());
    };
    let Some(frame) = session_rename::dispatch(client, frame)? else {
        return Ok(());
    };
    let Some(frame) = projection::dispatch(client, frame) else {
        return Ok(());
    };
    if workspace::resources::dispatch(client, &frame)? {
        return Ok(());
    }
    let Some(frame) = workspace::dispatch(client, frame) else {
        return Ok(());
    };
    let _ = operations::dispatch(client, frame)?;
    Ok(())
}

#[cfg(test)]
fn seed_legacy_test_lifecycle(client: &mut Client) -> Result<(), BridgeError> {
    if client.protocol_ready && !client.control.handshake_ready() {
        client.install_profile(
            client
                .selected_profile
                .unwrap_or(phux_protocol::BootstrapProfile::SynthesizedVtRaw),
            BootstrapLimits::new(client.limits.bootstrap_chunk, client.limits.history_page)
                .ok_or_else(|| BridgeError::state("invalid test bootstrap limits"))?,
        );
    }
    if client.attach_queued
        && let Some(attach_id) = client.expected_attach_id
    {
        let queued = client.control.attach_explicit(
            attach_id,
            AttachTarget::Last,
            ViewportInfo::new(80, 24),
            false,
            u32::try_from(client.limits.history_materialized_rows).unwrap_or(u32::MAX),
            None,
        );
        if queued {
            let _ = client.control.take_outbound();
        }
    }
    Ok(())
}

#[cfg(test)]
fn advertised_client_caps(client: &Client) -> phux_protocol::ClientCapabilities {
    client
        .offered_caps
        .unwrap_or_else(|| client.control.options().client_caps())
}

#[cfg(test)]
#[allow(
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    reason = "compatibility helper mirrors every borrowed KernelInput variant in tests"
)]
fn apply_kernel_input(client: &mut Client, input: KernelInput<'_>) -> Result<(), BridgeError> {
    use phux_client_runtime::engine::EngineEvent;
    if client.control.engine().is_none() {
        client.install_profile(
            client
                .selected_profile
                .unwrap_or(phux_protocol::BootstrapProfile::SynthesizedVtRaw),
            BootstrapLimits::new(client.limits.bootstrap_chunk, client.limits.history_page)
                .ok_or_else(|| BridgeError::state("invalid test bootstrap limits"))?,
        );
    }
    let event = match input {
        KernelInput::AttachStarted {
            attach_id,
            terminals,
        } => EngineEvent::AttachStarted {
            attach_id,
            terminals: terminals.to_vec(),
        },
        KernelInput::AttachReady { attach_id } => EngineEvent::AttachReady { attach_id },
        KernelInput::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            geometry,
            base_seq,
        } => EngineEvent::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            profile,
            cols: geometry.cols,
            rows: geometry.rows,
            base_seq,
        },
        KernelInput::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => EngineEvent::BootstrapChunk {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload: payload.to_vec(),
        },
        KernelInput::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => EngineEvent::BootstrapReady {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            history_cursor: history_cursor.map(<[u8]>::to_vec),
        },
        KernelInput::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            rows,
            payload,
            cursor,
            next_cursor,
        } => EngineEvent::HistoryPage {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            page_seq,
            rows,
            payload: payload.to_vec(),
            cursor: cursor.to_vec(),
            next_cursor: next_cursor.map(<[u8]>::to_vec),
        },
        KernelInput::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
        } => EngineEvent::HistoryTombstone {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: cursor.to_vec(),
            reason,
        },
        KernelInput::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
            required_bytes,
            required_rows,
        } => EngineEvent::HistoryRejected {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            cursor: cursor.to_vec(),
            reason,
            required_bytes,
            required_rows,
        },
        KernelInput::ResourceOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            payload,
        } => EngineEvent::Output {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            seq,
            bytes: payload.to_vec(),
        },
        KernelInput::Tombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        } => EngineEvent::Tombstone {
            terminal_id: terminal_id.clone(),
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        },
        KernelInput::ResourceClosed {
            terminal_id,
            exit_status,
            signal,
            reason,
        } => EngineEvent::Closed {
            terminal_id: terminal_id.clone(),
            exit_status,
            signal,
            reason,
        },
        KernelInput::Event { terminal_id, event } => EngineEvent::Agent {
            terminal_id: terminal_id.clone(),
            event: event.clone(),
        },
        KernelInput::AgentSessionDeclared(declaration) => EngineEvent::AgentSessionDeclared {
            terminal_id: declaration.terminal_id.clone(),
            parent: declaration.parent.cloned(),
            provider: declaration.provider.map(str::to_owned),
            native_id: declaration.native_id.map(str::to_owned),
            state: declaration.state.map(str::to_owned),
        },
        KernelInput::Action(_) => {
            return Err(BridgeError::state(
                "test helper does not apply input actions",
            ));
        }
    };
    client
        .control
        .apply_engine_event(event)
        .map_err(control_error)?;
    client.drain_outbound();
    for event in client.control.take_events() {
        let _ = client.process_runtime_event(event)?;
    }
    Ok(())
}

fn forget_terminal(client: &mut Client, id: &phux_protocol::ResourceId) {
    client.render.remove(id);
    client.resources.retain(|resource| &resource.id != id);
}

fn control_error(error: phux_client_runtime::control::ControlError) -> BridgeError {
    use phux_client_runtime::control::ControlError;
    match error {
        ControlError::Protocol(message) | ControlError::Refused(message) => {
            BridgeError::protocol(message)
        }
        ControlError::InvalidState(message) => BridgeError::state(message),
        ControlError::Resync => BridgeError::state("a replica needs a fresh bootstrap"),
        ControlError::Closed => BridgeError::state("the session was detached"),
    }
}

/// Returns the number of sessions advertised by the latest accepted ATTACHED.
/// Zero means either no catalog has arrived or the client pointer is invalid.
///
/// # Safety
///
/// `client`, when non-null, must remain valid and unmodified for the call and
/// must be accessed only from its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_session_count(client: *const PhuxClient) -> usize {
    unsafe { client.as_ref() }.map_or(0, |client| {
        if client.inner.in_callback {
            0
        } else {
            client.inner.sessions.len()
        }
    })
}

/// Returns one borrowed server session summary from the latest ATTACHED.
///
/// # Safety
///
/// `client` must remain valid and unmodified for the call. `out_session` must
/// be writable. The returned name remains valid until the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_session_get(
    client: *const PhuxClient,
    index: usize,
    out_session: *mut PhuxSessionInfo,
) -> PhuxClientResult {
    match catch_unwind(AssertUnwindSafe(|| -> Result<(), PhuxClientResult> {
        let client = unsafe { client.as_ref() }.ok_or(PhuxClientResult::InvalidArgument)?;
        if client.inner.in_callback {
            return Err(PhuxClientResult::InvalidState);
        }
        let out = unsafe { out_session.as_mut() }.ok_or(PhuxClientResult::InvalidArgument)?;
        *out = PhuxSessionInfo::default();
        let session = client
            .inner
            .sessions
            .get(index)
            .ok_or(PhuxClientResult::NoValue)?;
        *out = PhuxSessionInfo {
            session_id: session.session_id,
            name: bytes_out(&session.name),
            created_at_unix_secs: session.created_at_unix_secs,
            window_count: session.window_count,
            attached_client_count: session.attached_client_count,
            focused: session.focused,
        };
        Ok(())
    })) {
        Ok(Ok(())) => PhuxClientResult::Ok,
        Ok(Err(error)) => error,
        Err(_) => PhuxClientResult::Panic,
    }
}

/// Returns the number of resources in the latest accepted inventory.
///
/// An ATTACHED or workspace `GET_STATE` snapshot replaces membership; a resource
/// closure removes an entry. Zero means either no
/// snapshot has arrived or the client pointer is invalid.
///
/// # Safety
///
/// `client`, when non-null, must remain valid and unmodified for the call and
/// must be accessed only from its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_resource_count(client: *const PhuxClient) -> usize {
    unsafe { client.as_ref() }.map_or(0, |client| {
        if client.inner.in_callback {
            0
        } else {
            client.inner.resources.len()
        }
    })
}

/// Returns one borrowed resource summary from the latest accepted inventory.
///
/// # Safety
///
/// `client` must remain valid and unmodified for the call. `out_resource` must
/// be writable with `size`/`version` initialised. Every span and the `parent`
/// pointer remain valid until the next mutable call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_resource_get(
    client: *const PhuxClient,
    index: usize,
    out_resource: *mut PhuxResourceInfo,
) -> PhuxClientResult {
    match catch_unwind(AssertUnwindSafe(|| -> Result<(), PhuxClientResult> {
        let client = unsafe { client.as_ref() }.ok_or(PhuxClientResult::InvalidArgument)?;
        if client.inner.in_callback {
            return Err(PhuxClientResult::InvalidState);
        }
        let out = unsafe { out_resource.as_mut() }.ok_or(PhuxClientResult::InvalidArgument)?;
        check_struct(out.size, mem::size_of::<PhuxResourceInfo>(), out.version)
            .map_err(|error| error.result)?;
        *out = PhuxResourceInfo::default();
        let resource = client
            .inner
            .resources
            .get(index)
            .ok_or(PhuxClientResult::NoValue)?;
        *out = PhuxResourceInfo {
            terminal_id: terminal_id_out(&resource.id),
            kind: resource.kind,
            parent: resource.parent_ptr(),
            provider: bytes_out(&resource.provider),
            native_id: bytes_out(&resource.native_id),
            state: bytes_out(&resource.state),
            ..PhuxResourceInfo::default()
        };
        Ok(())
    })) {
        Ok(Ok(())) => PhuxClientResult::Ok,
        Ok(Err(error)) => error,
        Err(_) => PhuxClientResult::Panic,
    }
}

/// Returns the number of queued outgoing frames.
///
/// # Safety
///
/// When non-null, `client` must point to a live client on its owning thread and
/// remain valid and unmodified for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_outgoing_count(client: *const PhuxClient) -> usize {
    unsafe { client.as_ref() }.map_or(0, |client| {
        if client.inner.in_callback {
            0
        } else {
            client.inner.outgoing.len()
        }
    })
}

/// Returns a borrowed queued outgoing frame.
///
/// # Safety
///
/// When non-null, `client` must point to a live client on its owning thread and
/// remain valid and unmodified for the call. When non-null, `out_frame` must be
/// valid writable storage. The returned bytes remain valid until the next
/// mutable call using `client`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_outgoing_get(
    client: *const PhuxClient,
    index: usize,
    out_frame: *mut PhuxBytes,
) -> PhuxClientResult {
    match catch_unwind(AssertUnwindSafe(|| -> Result<(), PhuxClientResult> {
        let client = unsafe { client.as_ref() }.ok_or(PhuxClientResult::InvalidArgument)?;
        if client.inner.in_callback {
            return Err(PhuxClientResult::InvalidState);
        }
        let out = unsafe { out_frame.as_mut() }.ok_or(PhuxClientResult::InvalidArgument)?;
        *out = PhuxBytes::default();
        let frame = client
            .inner
            .outgoing
            .get(index)
            .ok_or(PhuxClientResult::NoValue)?;
        *out = bytes_out(frame);
        Ok(())
    })) {
        Ok(Ok(())) => PhuxClientResult::Ok,
        Ok(Err(error)) => error,
        Err(_) => PhuxClientResult::Panic,
    }
}

/// Clears all queued outgoing frames.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_outgoing_clear(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.outgoing.clear();
        Ok(())
    })
}

/// Returns the number of staged effects.
///
/// # Safety
///
/// When non-null, `client` must point to a live client on its owning thread and
/// remain valid and unmodified for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_effect_count(client: *const PhuxClient) -> usize {
    unsafe { client.as_ref() }.map_or(0, |client| {
        if client.inner.in_callback {
            0
        } else {
            client.inner.effect_count
        }
    })
}

/// Returns a borrowed staged effect.
///
/// # Safety
///
/// When non-null, `client` must point to a live client on its owning thread and
/// remain valid and unmodified for the call. When non-null, `out_effect` must
/// be valid writable storage. Pointers in the returned effect remain valid
/// until the next mutable call using `client`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_effect_get(
    client: *const PhuxClient,
    index: usize,
    out_effect: *mut PhuxClientEffect,
) -> PhuxClientResult {
    match catch_unwind(AssertUnwindSafe(|| -> Result<(), PhuxClientResult> {
        let client = unsafe { client.as_ref() }.ok_or(PhuxClientResult::InvalidArgument)?;
        if client.inner.in_callback {
            return Err(PhuxClientResult::InvalidState);
        }
        let out = unsafe { out_effect.as_mut() }.ok_or(PhuxClientResult::InvalidArgument)?;
        *out = PhuxClientEffect::default();
        *out = client
            .inner
            .effect_view(index)
            .ok_or(PhuxClientResult::NoValue)?;
        Ok(())
    })) {
        Ok(Ok(())) => PhuxClientResult::Ok,
        Ok(Err(error)) => error,
        Err(_) => PhuxClientResult::Panic,
    }
}

/// Clears all staged effects.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_effect_clear(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.owned_effects.clear();
        client.publish_effects();
        Ok(())
    })
}

/// Builds and returns the terminal's borrowed grid view.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable, and `out_view` must be valid
/// writable storage. Pointers in the returned view remain valid until the next
/// mutable call using `client`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_terminal_grid(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    out_view: *mut PhuxTerminalGridView,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let out =
            unsafe { out_view.as_mut() }.ok_or_else(|| BridgeError::invalid("out_view is null"))?;
        *out = PhuxTerminalGridView::default();
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let view = client.build_grid(&terminal_id)?;
        // SAFETY: build_grid returns a pointer to its bridge-owned cache, valid until mutation.
        *out = unsafe { *view };
        Ok(())
    })
}

/// Reports whether the terminal has a published DEC mouse-tracking mode.
///
/// # Safety
///
/// When non-null, `client` must point to a live client on its owning thread and
/// remain valid and unmodified for the call. When non-null, `terminal_id` and
/// any non-empty satellite host span must be readable, and `out_enabled` must
/// be valid writable storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_terminal_mouse_tracking(
    client: *const PhuxClient,
    terminal_id: *const PhuxResourceId,
    out_enabled: *mut bool,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        let out = unsafe { out_enabled.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_enabled is null"))?;
        client.ensure_attached()?;
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let enabled = client.mouse_tracking(&terminal_id)?;
        *out = enabled;
        Ok(())
    })
}

/// Sends a key event to a terminal.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and `event`
/// must be readable. Any non-empty satellite host span and any non-empty
/// `event.text` span selected by `event.has_text` must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_send_key(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    event: *const PhuxKeyEvent,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let event =
            unsafe { event.as_ref() }.ok_or_else(|| BridgeError::invalid("event is null"))?;
        check_struct(event.size, mem::size_of::<PhuxKeyEvent>(), event.version)?;
        let event = unsafe { key_input_event(event) }?;
        apply_input(client, &terminal_id, &event)
    })
}

/// True when the text carries a control codepoint, DEL, or a codepoint from
/// the private-use block platforms map their function keys into.
fn is_forbidden_key_text(text: &str) -> bool {
    text.chars()
        .any(|ch| ch <= '\u{1f}' || ch == '\u{7f}' || ('\u{f700}'..='\u{f8ff}').contains(&ch))
}

/// Reads the committed text a key event carries, if it carries any.
///
/// # Safety
///
/// Any non-empty `event.text` span selected by `event.has_text` must be
/// readable for the call.
unsafe fn key_event_text(event: &PhuxKeyEvent) -> Result<Option<String>, BridgeError> {
    if !event.has_text {
        if event.text.len != 0 {
            return Err(BridgeError::invalid(
                "key text bytes present when has_text is false",
            ));
        }
        return Ok(None);
    }
    let bytes = unsafe { outbound_bytes_in(event.text.data, event.text.len, "key text") }?;
    let text =
        std::str::from_utf8(bytes).map_err(|_| BridgeError::invalid("key text is not UTF-8"))?;
    if is_forbidden_key_text(text) {
        return Err(BridgeError::invalid(
            "key text contains forbidden control or platform function codepoints",
        ));
    }
    Ok(Some(text.to_owned()))
}

/// Resolves the held and consumed modifier sets, rejecting unknown bits and a
/// consumed set that is not a subset of the held one.
fn key_event_modifiers(event: &PhuxKeyEvent) -> Result<(ModSet, ModSet), BridgeError> {
    let mods = ModSet::from_bits(event.modifiers)
        .ok_or_else(|| BridgeError::invalid("unknown key modifier bits"))?;
    let consumed_mods = ModSet::from_bits(event.consumed_modifiers)
        .ok_or_else(|| BridgeError::invalid("unknown consumed modifier bits"))?;
    if !mods.contains(consumed_mods) {
        return Err(BridgeError::invalid(
            "consumed modifiers are not a subset of modifiers",
        ));
    }
    Ok((mods, consumed_mods))
}

/// Resolves the unshifted codepoint a key event carries, if it carries one.
fn key_event_unshifted_codepoint(event: &PhuxKeyEvent) -> Result<Option<u32>, BridgeError> {
    if !event.has_unshifted_codepoint {
        if event.unshifted_codepoint != 0 {
            return Err(BridgeError::invalid(
                "unshifted codepoint is present without its discriminator",
            ));
        }
        return Ok(None);
    }
    char::from_u32(event.unshifted_codepoint)
        .ok_or_else(|| BridgeError::invalid("unshifted codepoint is not a Unicode scalar"))?;
    Ok(Some(event.unshifted_codepoint))
}

/// Builds the key input event a validated `PhuxKeyEvent` describes.
///
/// # Safety
///
/// Any non-empty `event.text` span selected by `event.has_text` must be
/// readable for the call.
unsafe fn key_input_event(event: &PhuxKeyEvent) -> Result<InputEvent, BridgeError> {
    let text = unsafe { key_event_text(event) }?;
    let (mods, consumed_mods) = key_event_modifiers(event)?;
    let unshifted_codepoint = key_event_unshifted_codepoint(event)?;
    Ok(InputEvent::Key(KeyEvent {
        action: KeyAction::try_from(event.action)
            .map_err(|_| BridgeError::invalid("unknown key action"))?,
        key: PhysicalKey::try_from(event.key)
            .map_err(|_| BridgeError::invalid("unknown physical key"))?,
        mods,
        consumed_mods,
        composing: event.composing,
        text,
        unshifted_codepoint,
    }))
}

/// Sends a mouse event to a terminal.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id`, `event`, and
/// any non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_send_mouse(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    event: *const PhuxMouseEvent,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let event =
            unsafe { event.as_ref() }.ok_or_else(|| BridgeError::invalid("event is null"))?;
        check_struct(event.size, mem::size_of::<PhuxMouseEvent>(), event.version)?;
        if !event.x.is_finite() || !event.y.is_finite() || event.x < 0.0 || event.y < 0.0 {
            return Err(BridgeError::invalid(
                "mouse coordinates must be finite and non-negative",
            ));
        }
        let input = InputEvent::Mouse(MouseEvent {
            action: MouseAction::try_from(event.action)
                .map_err(|_| BridgeError::invalid("unknown mouse action"))?,
            button: MouseButton::try_from(event.button)
                .map_err(|_| BridgeError::invalid("unknown mouse button"))?,
            mods: ModSet::from_bits(event.modifiers)
                .ok_or_else(|| BridgeError::invalid("unknown mouse modifier bits"))?,
            x: event.x,
            y: event.y,
        });
        apply_input(client, &terminal_id, &input)
    })
}

/// Sends a focus event to a terminal.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_send_focus(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    focused: bool,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        apply_input(
            client,
            &terminal_id,
            &InputEvent::Focus(if focused {
                FocusEvent::Gained
            } else {
                FocusEvent::Lost
            }),
        )
    })
}

/// Sends pasted bytes to a terminal.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When non-null, `terminal_id` and any
/// non-empty satellite host span must be readable. When `len` is nonzero,
/// `data` must be readable for `len` bytes for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_send_paste(
    client: *mut PhuxClient,
    terminal_id: *const PhuxResourceId,
    data: *const u8,
    len: usize,
    trusted: bool,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let data = unsafe { outbound_bytes_in(data, len, "paste data") }?;
        apply_input(
            client,
            &terminal_id,
            &InputEvent::Paste(PasteEvent {
                trust: if trusted {
                    PasteTrust::Trusted
                } else {
                    PasteTrust::Untrusted
                },
                data: data.to_vec(),
            }),
        )
    })
}

/// Queues a terminal resize.
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
    with_client_mut(client, |client| {
        if cols == 0 || rows == 0 {
            return Err(BridgeError::invalid(
                "terminal resize geometry must be non-zero",
            ));
        }
        if !client.attached {
            return Err(BridgeError::state(
                "terminal resize requires an attached client",
            ));
        }
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let _ = client.terminal_key(&terminal_id)?;
        if client.operations.detaching(&terminal_id) {
            return Err(BridgeError::state("terminal detach is pending"));
        }
        client.queue_frame(&FrameKind::ResizeTerminal {
            terminal_id,
            cols,
            rows,
        })?;
        Ok(())
    })
}

/// Queues a viewport resize.
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
        if !client.attached {
            return Err(BridgeError::state(
                "viewport resize requires an attached client",
            ));
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
        client.queue_frame(&FrameKind::ViewportResize {
            viewport: ViewportInfo::new(cols, rows)
                .with_pixels(pixels.map(|value| value.0), pixels.map(|value| value.1)),
        })?;
        Ok(())
    })
}

/// Scrolls a terminal's history viewport.
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
/// for that terminal. Always on; counters since the client was created. The
/// bytes are borrowed from the client and valid until the next
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

#[cfg(test)]
mod tests {
    mod clear_presentation;
    mod resource_discovery;
    mod status_effects;

    use super::*;
    use phux_protocol::caps::ServerCapabilities;
    use phux_protocol::wire::frame::DetachReason;
    use std::ffi::c_void;

    struct CallbackContext {
        client: *mut PhuxClient,
        calls: usize,
        staged_before_call: bool,
        reentry_result: PhuxClientResult,
        failure_result: PhuxClientResult,
        failure_message: Vec<u8>,
    }

    unsafe extern "C-unwind" fn attached_callback(userdata: *mut c_void) {
        let context = unsafe { &mut *userdata.cast::<CallbackContext>() };
        context.calls += 1;
        context.staged_before_call = unsafe { (*context.client).inner.owned_effects.len() == 1 };
        context.reentry_result = unsafe { phux_client_outgoing_clear(context.client) };
    }

    unsafe extern "C-unwind" fn failure_callback(
        userdata: *mut c_void,
        result: PhuxClientResult,
        message: PhuxBytes,
    ) {
        let context = unsafe { &mut *userdata.cast::<CallbackContext>() };
        context.calls += 1;
        context.failure_result = result;
        context.failure_message = unsafe { bytes_in(message.data, message.len) }
            .expect("callback message span")
            .to_vec();
        context.reentry_result = unsafe { phux_client_outgoing_clear(context.client) };
    }

    unsafe extern "C-unwind" fn panic_callback(_: *mut c_void) {
        panic!("callback panic");
    }

    fn boxed_client() -> *mut PhuxClient {
        Box::into_raw(Box::new(PhuxClient {
            inner: Client::new(Limits {
                bootstrap_chunk: 1024,
                history_page: 1024,
                history_page_rows: 128,
                history_cache_bytes: 4096,
                history_materialized_rows: 1024,
                history_prefetch_rows: 64,
            }),
            _not_send_sync: std::marker::PhantomData,
        }))
    }

    #[test]
    fn local_projection_budget_may_be_smaller_than_authenticated_page_rows() {
        let options = PhuxClientOptions {
            size: mem::size_of::<PhuxClientOptions>(),
            version: ABI_VERSION,
            max_bootstrap_chunk_bytes: 1024,
            max_history_page_bytes: 1024,
            max_history_page_rows: 1024,
            max_history_cache_bytes: 4096,
            max_history_materialized_rows: 1,
            history_prefetch_rows: 2,
        };
        assert!(!history_cache_cannot_retain_page(&options));
        assert!(client_limits(&options).is_ok());
    }

    #[test]
    fn search_result_release_consumes_the_borrowed_set_in_one_mutation() {
        let client = boxed_client();
        unsafe {
            (*client).inner.search_results.push(PhuxSearchResult {
                start: PhuxDocumentAnchor { opaque_id: 41 },
                end: PhuxDocumentAnchor { opaque_id: 42 },
            });
            assert_eq!(
                phux_client_search_results_release(client),
                PhuxClientResult::Ok
            );
            assert!((*client).inner.search_results.is_empty());
            assert_eq!(
                phux_client_search_results_release(client),
                PhuxClientResult::Ok
            );

            phux_client_free(client);
        }
    }

    #[test]
    fn attached_callback_runs_after_staging_once_and_rejects_reentry() {
        let client = boxed_client();
        let mut context = CallbackContext {
            client,
            calls: 0,
            staged_before_call: false,
            reentry_result: PhuxClientResult::Ok,
            failure_result: PhuxClientResult::Ok,
            failure_message: Vec::new(),
        };
        unsafe {
            (*client).inner.callbacks = PhuxClientCallbacks {
                userdata: ptr::from_mut(&mut context).cast(),
                on_attached: Some(attached_callback),
                ..PhuxClientCallbacks::default()
            };
            (*client).inner.owned_effects.push(OwnedEffect::simple(
                1,
                1,
                phux_protocol::ResourceId::local(7),
            ));
        }
        assert_eq!(invoke_attached(client), PhuxClientResult::Ok);
        assert_eq!(invoke_attached(client), PhuxClientResult::Ok);
        assert_eq!(context.calls, 1);
        assert!(context.staged_before_call);
        assert_eq!(context.reentry_result, PhuxClientResult::InvalidState);
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn failure_callback_observes_stable_error_and_rejects_reentry() {
        let client = boxed_client();
        let mut context = CallbackContext {
            client,
            calls: 0,
            staged_before_call: false,
            reentry_result: PhuxClientResult::Ok,
            failure_result: PhuxClientResult::Ok,
            failure_message: Vec::new(),
        };
        unsafe {
            (*client).inner.callbacks = PhuxClientCallbacks {
                userdata: ptr::from_mut(&mut context).cast(),
                on_failure: Some(failure_callback),
                ..PhuxClientCallbacks::default()
            };
        }
        let result = with_client_mut(client, |_| Err(BridgeError::invalid("exact failure")));
        assert_eq!(result, PhuxClientResult::InvalidArgument);
        assert_eq!(context.calls, 1);
        assert_eq!(context.failure_result, PhuxClientResult::InvalidArgument);
        assert_eq!(context.failure_message, b"exact failure");
        assert_eq!(context.reentry_result, PhuxClientResult::InvalidState);
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn callback_panic_is_contained_and_clears_reentry_guard() {
        const CHILD: &str = "PHUX_CLIENT_FFI_PANIC_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let status = std::process::Command::new(
                std::env::current_exe().expect("current test executable"),
            )
            .args([
                "--exact",
                "tests::callback_panic_is_contained_and_clears_reentry_guard",
            ])
            .env(CHILD, "1")
            .status()
            .expect("spawn panic-containment host process");
            assert!(status.success(), "panic-containment host process aborted");
            return;
        }

        let client = boxed_client();
        unsafe {
            (*client).inner.callbacks = PhuxClientCallbacks {
                on_attached: Some(panic_callback),
                ..PhuxClientCallbacks::default()
            };
        }
        assert_eq!(invoke_attached(client), PhuxClientResult::Panic);
        assert!(!unsafe { (*client).inner.in_callback });
        assert_eq!(
            unsafe { phux_client_outgoing_clear(client) },
            PhuxClientResult::Ok
        );
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn hello_ok_explicitly_gates_terminal_reply_frames() {
        fn feed_hello(
            client: *mut PhuxClient,
            server_caps: ServerCapabilities,
        ) -> PhuxClientResult {
            unsafe { (*client).inner.hello_queued = true };
            let mut encoded = bytes::BytesMut::new();
            FrameKind::HelloOk {
                protocol_major: PROTOCOL_VERSION.major,
                protocol_minor: PROTOCOL_VERSION.minor,
                protocol_patch: PROTOCOL_VERSION.patch,
                server_caps,
                server_id: b"server".to_vec(),
                selected_profile: phux_protocol::BootstrapProfile::SynthesizedVtRaw,
                bootstrap_limits: BootstrapLimits::new(1024, 1024).expect("valid test limits"),
            }
            .encode(&mut encoded);
            unsafe { phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) }
        }

        let old = boxed_client();
        assert_eq!(
            feed_hello(old, ServerCapabilities::new()),
            PhuxClientResult::Ok
        );
        assert!(!unsafe { (*old).inner.terminal_reply });
        unsafe { phux_client_free(old) };

        let new = boxed_client();
        let features =
            phux_protocol::ServerFeatureSet::with(&[phux_protocol::ServerFeature::TerminalReply]);
        assert_eq!(
            feed_hello(new, ServerCapabilities::new().with_features(features),),
            PhuxClientResult::Ok
        );
        assert!(unsafe { (*new).inner.terminal_reply });
        unsafe { phux_client_free(new) };
    }

    fn hello_ok(patch: u16, selected_profile: phux_protocol::BootstrapProfile) -> FrameKind {
        FrameKind::HelloOk {
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: patch,
            server_caps: ServerCapabilities::new(),
            server_id: b"server".to_vec(),
            selected_profile,
            bootstrap_limits: BootstrapLimits::new(1024, 1024).expect("valid test limits"),
        }
    }

    fn native_hello_ok_profile() -> phux_protocol::BootstrapProfile {
        phux_protocol::BootstrapProfile::NativeState {
            codec: phux_protocol::EngineCodec::LibghosttySnapshotV1,
            features: phux_protocol::EngineFeatureSet::required_native(),
        }
    }

    #[test]
    fn hello_ok_accepts_matching_patch_and_native_features() {
        let client = boxed_client();
        unsafe { (*client).inner.hello_queued = true };
        let offered = advertised_client_caps(unsafe { &(*client).inner });
        let profile = if offered
            .bootstrap
            .profiles
            .contains(phux_protocol::BootstrapProfileKind::NativeState)
        {
            native_hello_ok_profile()
        } else {
            phux_protocol::BootstrapProfile::SynthesizedVtRaw
        };
        assert_eq!(
            feed_kind(client, &hello_ok(PROTOCOL_VERSION.patch, profile)),
            PhuxClientResult::Ok
        );
        assert!(unsafe { (*client).inner.protocol_ready });
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn hello_ok_refuses_protocol_patch_mismatch() {
        let client = boxed_client();
        unsafe { (*client).inner.hello_queued = true };
        assert_eq!(
            feed_kind(
                client,
                &hello_ok(
                    PROTOCOL_VERSION.patch.wrapping_add(1),
                    phux_protocol::BootstrapProfile::SynthesizedVtRaw,
                ),
            ),
            PhuxClientResult::ProtocolError
        );
        assert!(!unsafe { (*client).inner.protocol_ready });
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn hello_ok_refuses_native_profile_when_required_features_are_missing() {
        let client = boxed_client();
        unsafe {
            (*client).inner.hello_queued = true;
            (*client).inner.offered_caps = Some(
                phux_protocol::ClientCapabilities::new().with_bootstrap(
                    phux_protocol::BootstrapCapabilities::new()
                        .with_profiles(phux_protocol::BootstrapProfileSet::with(&[
                            phux_protocol::BootstrapProfileKind::NativeState,
                        ]))
                        .with_native_codecs(phux_protocol::EngineCodecSet::with(&[
                            phux_protocol::EngineCodec::LibghosttySnapshotV1,
                        ]))
                        .with_native_features(phux_protocol::EngineFeatureSet::with(&[
                            phux_protocol::EngineFeature::Continuation,
                        ])),
                ),
            );
        }
        let incomplete = phux_protocol::BootstrapProfile::NativeState {
            codec: phux_protocol::EngineCodec::LibghosttySnapshotV1,
            features: phux_protocol::EngineFeatureSet::with(&[
                phux_protocol::EngineFeature::Continuation,
            ]),
        };
        assert_eq!(
            feed_kind(client, &hello_ok(PROTOCOL_VERSION.patch, incomplete)),
            PhuxClientResult::ProtocolError
        );
        assert!(!unsafe { (*client).inner.protocol_ready });
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn attach_ready_must_match_the_queued_attach_id() {
        let client = boxed_client();
        unsafe {
            (*client).inner.protocol_ready = true;
            (*client).inner.attach_queued = true;
            (*client).inner.expected_attach_id = Some(7);
        }
        let mut encoded = bytes::BytesMut::new();
        FrameKind::AttachReady { attach_id: 8 }.encode(&mut encoded);
        assert_eq!(
            unsafe { phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) },
            PhuxClientResult::ProtocolError
        );
        assert!(!unsafe { (*client).inner.attached });
        assert!(unsafe { (*client).inner.attach_queued });
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn borrowed_getter_clears_output_before_no_value() {
        let client = boxed_client();
        let mut frame = PhuxBytes {
            data: ptr::dangling(),
            len: usize::MAX,
        };
        assert_eq!(
            unsafe { phux_client_outgoing_get(client, 0, &raw mut frame) },
            PhuxClientResult::NoValue
        );
        assert!(frame.data.is_null());
        assert_eq!(frame.len, 0);
        unsafe { phux_client_free(client) };
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn stale_tombstone_preserves_newer_grid_revision() {
        let terminal_id = phux_protocol::ResourceId::local(7);
        let c_terminal_id = PhuxResourceId {
            kind: 0,
            id: 7,
            host: PhuxBytes::default(),
        };
        let stream_id = phux_protocol::StreamId::new(1).expect("stream");
        let first_bootstrap = phux_protocol::BootstrapId::new(1).expect("bootstrap");
        let second_bootstrap = phux_protocol::BootstrapId::new(2).expect("bootstrap");
        let session_id = SessionId::new(1);
        let window_id = phux_protocol::WindowId::new(1);
        let snapshot = phux_protocol::wire::info::SessionSnapshot::new(
            session_id,
            window_id,
            terminal_id.clone(),
        )
        .with_sessions(vec![phux_protocol::wire::info::SessionInfo::new(
            session_id, "working",
        )])
        .with_windows(vec![phux_protocol::wire::info::WindowInfo::new(
            window_id, session_id, "working",
        )])
        .with_resources(vec![phux_protocol::wire::info::ResourceInfo::new(
            terminal_id.clone(),
            window_id,
            80,
            24,
        )]);
        let client = boxed_client();
        unsafe {
            (*client).inner.protocol_ready = true;
            (*client).inner.attach_queued = true;
            (*client).inner.expected_attach_id = Some(7);
            (*client).inner.selected_profile =
                Some(phux_protocol::BootstrapProfile::SynthesizedVtRaw);
        }
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::Attached {
                    attach_id: 7,
                    snapshot,
                    initial_client_id: phux_protocol::ClientId::new(9),
                },
            ),
            PhuxClientResult::Ok
        );
        for bootstrap_id in [first_bootstrap, second_bootstrap] {
            for frame in [
                FrameKind::BootstrapBegin {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
                    cols: 80,
                    rows: 24,
                    base_seq: 0,
                },
                FrameKind::BootstrapChunk {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    chunk_seq: 0,
                    payload: bytes::Bytes::from_static(b"$ "),
                },
                FrameKind::BootstrapReady {
                    terminal_id: terminal_id.clone(),
                    stream_id,
                    bootstrap_id,
                    history_cursor: None,
                },
            ] {
                assert_eq!(feed_kind(client, &frame), PhuxClientResult::Ok);
            }
        }
        assert_eq!(
            feed_kind(client, &FrameKind::AttachReady { attach_id: 7 }),
            PhuxClientResult::Ok
        );
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::BootstrapTombstone {
                    terminal_id,
                    stream_id,
                    bootstrap_id: first_bootstrap,
                    reason: phux_protocol::wire::frame::TombstoneReason::OutboundGap,
                    last_valid_seq: 0,
                },
            ),
            PhuxClientResult::Ok
        );

        client::RENDER_CACHE_BUILDS.set(0);
        let mut view = PhuxTerminalGridView::default();
        assert_eq!(
            unsafe { phux_client_terminal_grid(client, &raw const c_terminal_id, &raw mut view,) },
            PhuxClientResult::Ok
        );
        assert_eq!(view.stream_id, stream_id.get());
        assert_eq!(view.bootstrap_id, second_bootstrap.get());
        assert_eq!(client::RENDER_CACHE_BUILDS.get(), 1);
        for _ in 0..4 {
            assert_eq!(
                unsafe {
                    phux_client_anchor_release(client, &raw const c_terminal_id, view.top_anchor)
                },
                PhuxClientResult::Ok
            );
            assert_eq!(
                unsafe {
                    phux_client_terminal_grid(client, &raw const c_terminal_id, &raw mut view)
                },
                PhuxClientResult::Ok
            );
            assert_eq!(view.cell_count, 80 * 24);
        }
        assert_eq!(
            client::RENDER_CACHE_BUILDS.get(),
            1,
            "cache hits must not construct libghostty render state or iterators"
        );
        if view.top_anchor.opaque_id != 0 {
            assert_eq!(
                unsafe {
                    phux_client_anchor_release(client, &raw const c_terminal_id, view.top_anchor)
                },
                PhuxClientResult::Ok
            );
        }
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn attached_snapshot_scopes_participants_to_the_focused_session() {
        let client = boxed_client();
        unsafe {
            (*client).inner.protocol_ready = true;
            (*client).inner.attach_queued = true;
            (*client).inner.expected_attach_id = Some(7);
        }
        let focused_session = SessionId::new(2);
        let other_session = SessionId::new(1);
        let focused_window = phux_protocol::WindowId::new(20);
        let other_window = phux_protocol::WindowId::new(10);
        let focused_terminal = phux_protocol::ResourceId::local(30);
        let other_terminal = phux_protocol::ResourceId::local(40);
        let snapshot = phux_protocol::wire::info::SessionSnapshot::new(
            focused_session,
            focused_window,
            focused_terminal.clone(),
        )
        .with_sessions(vec![
            phux_protocol::wire::info::SessionInfo::new(other_session, "other"),
            phux_protocol::wire::info::SessionInfo::new(focused_session, "focused"),
        ])
        .with_windows(vec![
            phux_protocol::wire::info::WindowInfo::new(
                other_window,
                other_session,
                "other".to_owned(),
            ),
            phux_protocol::wire::info::WindowInfo::new(
                focused_window,
                focused_session,
                "focused".to_owned(),
            ),
        ])
        .with_resources(vec![
            phux_protocol::wire::info::ResourceInfo::new(
                other_terminal.clone(),
                other_window,
                80,
                24,
            ),
            phux_protocol::wire::info::ResourceInfo::new(
                focused_terminal.clone(),
                focused_window,
                80,
                24,
            ),
        ]);

        assert_eq!(
            feed_kind(
                client,
                &FrameKind::Attached {
                    attach_id: 7,
                    snapshot,
                    initial_client_id: phux_protocol::ClientId::new(9),
                },
            ),
            PhuxClientResult::Ok
        );
        assert!(unsafe { (*client).inner.active_attach_contains(&focused_terminal) });
        assert!(
            !unsafe { (*client).inner.active_attach_contains(&other_terminal) },
            "a terminal in another session is never bootstrapped by this attach"
        );
        unsafe { phux_client_free(client) };
    }

    fn three_pane_snapshot(
        seed: &phux_protocol::ResourceId,
        horizontal: &phux_protocol::ResourceId,
        vertical: &phux_protocol::ResourceId,
    ) -> phux_protocol::wire::info::SessionSnapshot {
        use phux_protocol::wire::info::{LayoutNode, SplitDir};

        let catalog_session = SessionId::new(1);
        let working_session = SessionId::new(2);
        let catalog_window = phux_protocol::WindowId::new(10);
        let working_window = phux_protocol::WindowId::new(20);
        let layout = LayoutNode::Split {
            dir: SplitDir::Horizontal,
            ratio: 0.5,
            left: Box::new(LayoutNode::Leaf(seed.clone())),
            right: Box::new(LayoutNode::Split {
                dir: SplitDir::Vertical,
                ratio: 0.5,
                left: Box::new(LayoutNode::Leaf(horizontal.clone())),
                right: Box::new(LayoutNode::Leaf(vertical.clone())),
            }),
        };
        phux_protocol::wire::info::SessionSnapshot::new(
            working_session,
            working_window,
            seed.clone(),
        )
        .with_sessions(vec![
            phux_protocol::wire::info::SessionInfo::new(catalog_session, "catalog"),
            phux_protocol::wire::info::SessionInfo::new(working_session, "working"),
        ])
        .with_windows(vec![
            phux_protocol::wire::info::WindowInfo::new(catalog_window, catalog_session, "catalog"),
            phux_protocol::wire::info::WindowInfo::new(working_window, working_session, "working")
                .with_active_resource(Some(seed.clone()))
                .with_layout(Some(layout)),
        ])
        .with_resources(vec![
            phux_protocol::wire::info::ResourceInfo::new(
                phux_protocol::ResourceId::local(1),
                catalog_window,
                80,
                24,
            ),
            phux_protocol::wire::info::ResourceInfo::new(seed.clone(), working_window, 80, 24),
            phux_protocol::wire::info::ResourceInfo::new(
                horizontal.clone(),
                working_window,
                40,
                24,
            ),
            phux_protocol::wire::info::ResourceInfo::new(vertical.clone(), working_window, 40, 12),
        ])
    }

    fn feed_complete_bootstrap(
        client: *mut PhuxClient,
        terminal_id: phux_protocol::ResourceId,
        payload: &'static [u8],
    ) {
        let stream_id = phux_protocol::StreamId::new(7).expect("stream");
        let bootstrap_id = phux_protocol::BootstrapId::new(1).expect("bootstrap");
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 40,
                rows: 12,
                base_seq: 0,
            },
            FrameKind::BootstrapChunk {
                terminal_id: terminal_id.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::copy_from_slice(payload),
            },
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        ] {
            assert_eq!(feed_kind(client, &frame), PhuxClientResult::Ok);
        }
    }

    fn client_with_searchable_scrollback() -> *mut PhuxClient {
        let client = boxed_client();
        // SAFETY: the fixture exclusively owns this live client on this thread.
        unsafe {
            (*client).inner.protocol_ready = true;
            (*client).inner.attach_queued = true;
            (*client).inner.expected_attach_id = Some(7);
            (*client).inner.selected_profile =
                Some(phux_protocol::BootstrapProfile::SynthesizedVtRaw);
        }
        let terminal = phux_protocol::ResourceId::local(1);
        let session = SessionId::new(1);
        let window = phux_protocol::WindowId::new(1);
        let snapshot =
            phux_protocol::wire::info::SessionSnapshot::new(session, window, terminal.clone())
                .with_windows(vec![phux_protocol::wire::info::WindowInfo::new(
                    window, session, "search",
                )])
                .with_resources(vec![phux_protocol::wire::info::ResourceInfo::new(
                    terminal.clone(),
                    window,
                    40,
                    12,
                )]);
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::Attached {
                    attach_id: 7,
                    snapshot,
                    initial_client_id: phux_protocol::ClientId::new(9),
                }
            ),
            PhuxClientResult::Ok,
        );
        feed_complete_bootstrap(client, terminal,
            b"older\r\nOFFSCREEN MATCH\r\n02\r\n03\r\n04\r\n05\r\n06\r\n07\r\n08\r\n09\r\n10\r\n11\r\n12\r\n13\r\n14\r\n15\r\n16\r\n17\r\n18\r\nLIVE TAIL");
        assert_eq!(
            feed_kind(client, &FrameKind::AttachReady { attach_id: 7 }),
            PhuxClientResult::Ok,
        );
        client
    }

    #[test]
    fn search_pin_moves_rendered_viewport_and_follow_live_restores_tail() {
        let client = client_with_searchable_scrollback();
        let terminal = PhuxResourceId {
            id: 1,
            ..PhuxResourceId::default()
        };
        let mut view = PhuxTerminalGridView::default();
        let mut results = ptr::null();
        let mut count = 0;
        // SAFETY: all spans and output pointers belong to this test; borrowed
        // grid/search data is consumed before the next mutable client call.
        unsafe {
            assert_eq!(
                phux_client_terminal_grid(client, &raw const terminal, &raw mut view),
                PhuxClientResult::Ok
            );
            let tail_offset = view.history_viewport_offset;
            assert!(tail_offset > 1);
            let text =
                std::str::from_utf8(bytes_in(view.utf8.data, view.utf8.len).unwrap()).unwrap();
            assert!(text.contains("LIVE TAIL"));
            assert!(!text.contains("OFFSCREEN MATCH"));
            assert_eq!(
                phux_client_anchor_release(client, &raw const terminal, view.top_anchor),
                PhuxClientResult::Ok
            );
            assert_eq!(
                phux_client_search(
                    client,
                    &raw const terminal,
                    bytes_out(b"offscreen match"),
                    false,
                    &raw mut results,
                    &raw mut count
                ),
                PhuxClientResult::Ok
            );
            assert_eq!(count, 1);
            let matched = *results;
            assert_eq!(
                phux_client_history_viewport_pin(client, &raw const terminal, matched.start),
                PhuxClientResult::Ok
            );
            // The viewport must own its pin independently of the transient results.
            assert_eq!(
                phux_client_search_results_release(client),
                PhuxClientResult::Ok
            );
            assert_eq!(
                phux_client_terminal_grid(client, &raw const terminal, &raw mut view),
                PhuxClientResult::Ok
            );
            assert_eq!(
                view.history_viewport_offset, 1,
                "search must move the actual rendered viewport"
            );
            let text =
                std::str::from_utf8(bytes_in(view.utf8.data, view.utf8.len).unwrap()).unwrap();
            assert!(text.starts_with("OFFSCREEN MATCH"));
            assert!(!text.contains("LIVE TAIL"));
            assert_eq!(
                phux_client_anchor_release(client, &raw const terminal, view.top_anchor),
                PhuxClientResult::Ok
            );
            assert_eq!(
                phux_client_history_follow_live(client, &raw const terminal),
                PhuxClientResult::Ok
            );
            assert_eq!(
                phux_client_terminal_grid(client, &raw const terminal, &raw mut view),
                PhuxClientResult::Ok
            );
            assert_eq!(
                view.history_viewport_offset, tail_offset,
                "follow-live must scroll the engine back to the tail"
            );
            let text =
                std::str::from_utf8(bytes_in(view.utf8.data, view.utf8.len).unwrap()).unwrap();
            assert!(text.contains("LIVE TAIL"));
            assert_eq!(
                phux_client_anchor_release(client, &raw const terminal, view.top_anchor),
                PhuxClientResult::Ok
            );
            phux_client_free(client);
        }
    }

    #[test]
    fn three_pane_attach_resolves_unbootstrapped_seed_by_closure() {
        let client = boxed_client();
        unsafe {
            (*client).inner.protocol_ready = true;
            (*client).inner.attach_queued = true;
            (*client).inner.expected_attach_id = Some(7);
            (*client).inner.selected_profile =
                Some(phux_protocol::BootstrapProfile::SynthesizedVtRaw);
        }
        let seed = phux_protocol::ResourceId::local(2);
        let horizontal = phux_protocol::ResourceId::local(3);
        let vertical = phux_protocol::ResourceId::local(4);
        let snapshot = three_pane_snapshot(&seed, &horizontal, &vertical);
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::Attached {
                    attach_id: 7,
                    snapshot,
                    initial_client_id: phux_protocol::ClientId::new(9),
                },
            ),
            PhuxClientResult::Ok,
        );
        feed_complete_bootstrap(client, horizontal.clone(), b"horizontal");
        feed_complete_bootstrap(client, vertical.clone(), b"vertical");
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::ResourceClosed {
                    terminal_id: seed.clone(),
                    exit_status: None,
                    reason: phux_protocol::wire::frame::CloseReason::Unknown,
                    signal: None,
                },
            ),
            PhuxClientResult::Ok,
        );
        assert_eq!(
            feed_kind(client, &FrameKind::AttachReady { attach_id: 7 }),
            PhuxClientResult::Ok,
        );
        assert!(unsafe { (*client).inner.attached });
        assert_eq!(unsafe { (*client).inner.sessions.len() }, 2);
        assert!(!unsafe { (*client).inner.active_attach_contains(&seed) });
        assert!(unsafe { (*client).inner.active_attach_contains(&horizontal) });
        assert!(unsafe { (*client).inner.active_attach_contains(&vertical) });
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn attached_snapshot_exposes_the_server_session_catalog() {
        let client = boxed_client();
        unsafe {
            (*client).inner.protocol_ready = true;
            (*client).inner.attach_queued = true;
            (*client).inner.expected_attach_id = Some(7);
        }
        let snapshot = phux_protocol::wire::info::SessionSnapshot::new(
            SessionId::new(2),
            phux_protocol::WindowId::new(20),
            phux_protocol::ResourceId::local(30),
        )
        .with_sessions(vec![
            phux_protocol::wire::info::SessionInfo::new(SessionId::new(1), "other")
                .with_created_at_unix_secs(100)
                .with_window_count(2),
            phux_protocol::wire::info::SessionInfo::new(SessionId::new(2), "focused")
                .with_created_at_unix_secs(200)
                .with_window_count(3)
                .with_attached_client_count(4),
        ]);
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::Attached {
                    attach_id: 7,
                    snapshot,
                    initial_client_id: phux_protocol::ClientId::new(9),
                },
            ),
            PhuxClientResult::Ok
        );
        assert_eq!(unsafe { phux_client_session_count(client) }, 2);

        let mut session = PhuxSessionInfo::default();
        assert_eq!(
            unsafe { phux_client_session_get(client, 1, &raw mut session) },
            PhuxClientResult::Ok
        );
        assert_eq!(session.session_id, 2);
        assert_eq!(
            unsafe { bytes_in(session.name.data, session.name.len) }.unwrap(),
            b"focused"
        );
        assert_eq!(session.created_at_unix_secs, 200);
        assert_eq!(session.window_count, 3);
        assert_eq!(session.attached_client_count, 4);
        assert!(session.focused);

        assert_eq!(
            unsafe { phux_client_session_get(client, 2, &raw mut session) },
            PhuxClientResult::NoValue
        );
        assert!(session.name.data.is_null());
        unsafe { phux_client_free(client) };
    }

    fn feed_kind(client: *mut PhuxClient, frame: &FrameKind) -> PhuxClientResult {
        let mut encoded = bytes::BytesMut::new();
        frame.encode(&mut encoded);
        unsafe { phux_client_feed_frame(client, encoded.as_ptr(), encoded.len()) }
    }

    #[test]
    fn queued_effects_are_projected_only_when_read() {
        let client = boxed_client();
        unsafe { (*client).inner.protocol_ready = true };
        client::EFFECT_VIEW_BUILDS.set(0);
        for index in 0..64 {
            assert_eq!(
                feed_kind(
                    client,
                    &FrameKind::Error {
                        code: phux_protocol::wire::frame::ErrorCode::InvalidCommand,
                        request_id: None,
                        message: format!("error {index}"),
                    }
                ),
                PhuxClientResult::Ok
            );
        }
        assert_eq!(unsafe { phux_client_effect_count(client) }, 64);
        assert_eq!(
            client::EFFECT_VIEW_BUILDS.get(),
            0,
            "feeding must not repeatedly project the growing effect backlog"
        );
        for index in 0..64 {
            let mut effect = PhuxClientEffect::default();
            assert_eq!(
                unsafe { phux_client_effect_get(client, index, &raw mut effect) },
                PhuxClientResult::Ok
            );
            assert_eq!((effect.kind, effect.detail), (2, 4));
            let message =
                unsafe { std::slice::from_raw_parts(effect.bytes.data, effect.bytes.len) };
            assert_eq!(message, format!("InvalidCommand: error {index}").as_bytes());
        }
        assert_eq!(client::EFFECT_VIEW_BUILDS.get(), 64);
        // Pending effects remain hidden until successful processing publishes them.
        let mut satellite =
            OwnedEffect::simple(2, 2, phux_protocol::ResourceId::satellite("peer", 9));
        satellite.bytes = b"satellite title".to_vec();
        satellite.stream_id = 11;
        satellite.bootstrap_id = 12;
        satellite.seq = 13;
        satellite.first_row = 3;
        satellite.last_row = 5;
        unsafe { (*client).inner.owned_effects.push(satellite) };
        let mut effect = PhuxClientEffect::default();
        assert_eq!(unsafe { phux_client_effect_count(client) }, 64);
        assert_eq!(
            unsafe { phux_client_effect_get(client, 64, &raw mut effect) },
            PhuxClientResult::NoValue
        );
        unsafe { (*client).inner.publish_effects() };
        assert_eq!(
            unsafe { phux_client_effect_get(client, 64, &raw mut effect) },
            PhuxClientResult::Ok
        );
        assert_eq!((effect.terminal_id.kind, effect.terminal_id.id), (1, 9));
        assert_eq!(
            unsafe {
                std::slice::from_raw_parts(
                    effect.terminal_id.host.data,
                    effect.terminal_id.host.len,
                )
            },
            b"peer"
        );
        assert_eq!(
            unsafe { std::slice::from_raw_parts(effect.bytes.data, effect.bytes.len) },
            b"satellite title"
        );
        assert_eq!(
            (effect.stream_id, effect.bootstrap_id, effect.seq),
            (11, 12, 13)
        );
        assert_eq!((effect.first_row, effect.last_row), (3, 5));
        assert_eq!(
            unsafe { phux_client_effect_clear(client) },
            PhuxClientResult::Ok
        );
        assert_eq!(unsafe { phux_client_effect_count(client) }, 0);
        let mut effect = PhuxClientEffect::default();
        assert_eq!(
            unsafe { phux_client_effect_get(client, 0, &raw mut effect) },
            PhuxClientResult::NoValue
        );
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn attach_id_zero_is_rejected_without_output() {
        let client = boxed_client();
        unsafe { (*client).inner.protocol_ready = true };
        let options = PhuxAttachOptions {
            size: mem::size_of::<PhuxAttachOptions>(),
            version: ABI_VERSION,
            attach_id: 0,
            target_kind: 0,
            session_id: 0,
            name: PhuxBytes::default(),
            cols: 80,
            rows: 24,
            has_pixel_size: false,
            pixel_width: 0,
            pixel_height: 0,
            request_scrollback: true,
            scrollback_limit_lines: 1_000,
        };
        assert_eq!(
            unsafe { phux_client_queue_attach(client, &raw const options) },
            PhuxClientResult::InvalidArgument
        );
        let client_ref = unsafe { &*client };
        assert!(client_ref.inner.outgoing.is_empty());
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn last_attach_queues_one_server_resolved_request_without_create_fallback() {
        let client = boxed_client();
        unsafe { (*client).inner.protocol_ready = true };
        let options = PhuxAttachOptions {
            size: mem::size_of::<PhuxAttachOptions>(),
            version: ABI_VERSION,
            attach_id: 7,
            target_kind: 0,
            session_id: 0,
            name: PhuxBytes::default(),
            cols: 80,
            rows: 24,
            has_pixel_size: false,
            pixel_width: 0,
            pixel_height: 0,
            request_scrollback: true,
            scrollback_limit_lines: 1_000,
        };

        assert_eq!(
            unsafe { phux_client_queue_attach(client, &raw const options) },
            PhuxClientResult::Ok,
        );
        let client_ref = unsafe { &*client };
        assert_eq!(
            client_ref.inner.outgoing.len(),
            1,
            "the bridge must not queue a client-derived fallback attempt",
        );
        let (decoded, remaining) =
            FrameKind::decode(&client_ref.inner.outgoing[0]).expect("ATTACH decodes");
        assert!(remaining.is_empty());
        assert!(matches!(
            decoded,
            FrameKind::Attach {
                attach_id: 7,
                target: AttachTarget::Last,
                ..
            }
        ));

        unsafe { phux_client_free(client) };
    }

    #[test]
    fn outbound_text_limit_rejects_overflow_and_accepts_boundary() {
        let too_large = vec![b'a'; crate::error::MAX_OUTBOUND_BYTES + 1];
        let rejected = boxed_client();
        assert_eq!(
            unsafe { phux_client_queue_hello(rejected, bytes_out(&too_large)) },
            PhuxClientResult::InvalidArgument
        );
        let rejected_client = unsafe { &*rejected };
        assert!(rejected_client.inner.outgoing.is_empty());
        unsafe { phux_client_free(rejected) };

        let boundary = vec![b'a'; crate::error::MAX_OUTBOUND_BYTES];
        let accepted = boxed_client();
        assert_eq!(
            unsafe { phux_client_queue_hello(accepted, bytes_out(&boundary)) },
            PhuxClientResult::Ok
        );
        let accepted_client = unsafe { &*accepted };
        let (decoded, remaining) =
            FrameKind::decode(&accepted_client.inner.outgoing[0]).expect("boundary HELLO decodes");
        assert!(remaining.is_empty());
        assert!(
            matches!(decoded, FrameKind::Hello { client_name, .. } if client_name.len() == boundary.len())
        );
        unsafe { phux_client_free(accepted) };
    }

    #[test]
    fn native_workspace_negotiates_metadata_before_issuing_layout_reads() {
        let client = boxed_client();
        assert_eq!(
            unsafe { phux_client_queue_hello(client, bytes_out(b"workspace")) },
            PhuxClientResult::Ok
        );
        let state = unsafe { &*client };
        let (frame, remaining) =
            FrameKind::decode(&state.inner.outgoing[0]).expect("outbound HELLO");
        assert!(remaining.is_empty());
        unsafe { phux_client_free(client) };
        let FrameKind::Hello { client_caps, .. } = frame else {
            panic!("expected HELLO");
        };
        assert!(client_caps.layers.contains(phux_protocol::Layer::L3));
    }

    #[test]
    fn feed_rejects_payload_above_current_limit_before_lifecycle_dispatch() {
        let client = boxed_client();
        let payload = vec![0_u8; 2 * 1024];
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::BootstrapChunk {
                    terminal_id: phux_protocol::ResourceId::local(7),
                    stream_id: phux_protocol::StreamId::new(1).expect("stream"),
                    bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
                    chunk_seq: 0,
                    payload: payload.into(),
                },
            ),
            PhuxClientResult::ProtocolError
        );
        assert!(!unsafe {
            (*client)
                .inner
                .active_attach_contains(&phux_protocol::ResourceId::local(7))
        });
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn terminal_state_frames_require_an_active_attach_participant() {
        let terminal_id = phux_protocol::ResourceId::local(7);
        let client = boxed_client();
        unsafe {
            (*client).inner.protocol_ready = true;
            (*client).inner.selected_profile =
                Some(phux_protocol::BootstrapProfile::SynthesizedVtRaw);
        }
        let begin = FrameKind::BootstrapBegin {
            terminal_id: terminal_id.clone(),
            stream_id: phux_protocol::StreamId::new(1).expect("stream"),
            bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
            profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
            cols: 80,
            rows: 24,
            base_seq: 0,
        };
        assert_eq!(feed_kind(client, &begin), PhuxClientResult::ProtocolError);
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::ResourceClosed {
                    terminal_id: terminal_id.clone(),
                    exit_status: None,
                    reason: phux_protocol::wire::frame::CloseReason::Unknown,
                    signal: None,
                },
            ),
            PhuxClientResult::ProtocolError
        );
        assert!(!unsafe { (*client).inner.active_attach_contains(&terminal_id) });
        unsafe {
            (*client).inner.attach_queued = true;
        }
        let authorized = [terminal_id.clone()];
        apply_kernel_input(
            unsafe { &mut (*client).inner },
            KernelInput::AttachStarted {
                attach_id: 7,
                terminals: &authorized,
            },
        )
        .expect("seed active ATTACH inventory");
        assert!(unsafe { (*client).inner.active_attach_contains(&terminal_id) });
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::BootstrapBegin {
                    terminal_id: phux_protocol::ResourceId::local(8),
                    stream_id: phux_protocol::StreamId::new(2).expect("stream"),
                    bootstrap_id: phux_protocol::BootstrapId::new(2).expect("bootstrap"),
                    profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
                    cols: 80,
                    rows: 24,
                    base_seq: 0,
                },
            ),
            PhuxClientResult::ProtocolError
        );
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn detached_before_attach_cleanly_ends_prehello_and_negotiated_connections() {
        for protocol_ready in [false, true] {
            let client = boxed_client();
            unsafe {
                (*client).inner.protocol_ready = protocol_ready;
            }
            assert_eq!(
                feed_kind(
                    client,
                    &FrameKind::Detached {
                        reason: Some(DetachReason::ProtocolError),
                        message: "connection refused".to_owned()
                    }
                ),
                PhuxClientResult::Ok
            );
            assert!(unsafe { (*client).inner.detached });
            assert_eq!(
                feed_kind(client, &FrameKind::Ping { nonce: 7 }),
                PhuxClientResult::ProtocolError,
                "nothing follows DETACHED"
            );
            unsafe { phux_client_free(client) };
        }
    }

    #[test]
    fn detached_releases_attach_and_rejects_subsequent_output_transactionally() {
        let terminal_id = phux_protocol::ResourceId::local(7);
        let authorized = [terminal_id.clone()];
        let client = boxed_client();
        unsafe {
            (*client).inner.protocol_ready = true;
            (*client).inner.attach_queued = true;
        }
        apply_kernel_input(
            unsafe { &mut (*client).inner },
            KernelInput::AttachStarted {
                attach_id: 7,
                terminals: &authorized,
            },
        )
        .expect("seed active ATTACH inventory");
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::Detached {
                    reason: None,
                    message: String::new()
                }
            ),
            PhuxClientResult::Ok
        );
        let client_ref = unsafe { &*client };
        assert!(!client_ref.inner.active_attach_contains(&terminal_id));
        let effects_before = client_ref.inner.owned_effects.len();
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::ResourceOutput {
                    terminal_id,
                    stream_id: phux_protocol::StreamId::new(1).expect("stream"),
                    bootstrap_id: phux_protocol::BootstrapId::new(1).expect("bootstrap"),
                    seq: 1,
                    bytes: bytes::Bytes::from_static(b"late"),
                },
            ),
            PhuxClientResult::ProtocolError
        );
        let client_ref = unsafe { &*client };
        assert_eq!(client_ref.inner.owned_effects.len(), effects_before);
        unsafe { phux_client_free(client) };
    }

    /// phux-l83x: the DETACHED status effect carries the reason as a stable
    /// wire value and the message verbatim, and an unstated reason is
    /// reported as UNSTATED rather than as `REQUESTED` (which is `0`, the
    /// value a zero-default would have produced).
    #[test]
    fn detached_status_effect_carries_the_reason_and_message() {
        for (reason, expected_code) in [
            (None, DETACH_REASON_UNSTATED),
            (Some(DetachReason::Requested), 0),
            (Some(DetachReason::ServerShutdown), 1),
            (Some(DetachReason::AuthenticationFailed), 5),
            (Some(DetachReason::AuthorizationRevoked), 6),
            (Some(DetachReason::AuthorizationExpired), 7),
            (Some(DetachReason::InternalError), 255),
        ] {
            let client = boxed_client();
            unsafe {
                (*client).inner.protocol_ready = true;
                (*client).inner.attach_queued = true;
            }
            assert_eq!(
                feed_kind(
                    client,
                    &FrameKind::Detached {
                        reason,
                        message: "server is stopping".to_owned(),
                    }
                ),
                PhuxClientResult::Ok
            );
            let client_ref = unsafe { &*client };
            let effect = client_ref
                .inner
                .owned_effects
                .last()
                .expect("DETACHED pushes a status effect");
            assert_eq!(effect.kind, 2);
            assert_eq!(effect.detail, 5);
            assert_eq!(effect.status_code, expected_code, "reason {reason:?}");
            assert_eq!(effect.bytes, b"server is stopping");
            unsafe { phux_client_free(client) };
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn mouse_tracking_getter_uses_published_dec_modes_and_preserves_borrows() {
        let terminal_id = phux_protocol::ResourceId::local(7);
        let c_terminal_id = PhuxResourceId {
            kind: 0,
            id: 7,
            host: PhuxBytes::default(),
        };
        let stream_id = phux_protocol::StreamId::new(1).expect("stream");
        let bootstrap_id = phux_protocol::BootstrapId::new(1).expect("bootstrap");
        let authorized = [terminal_id.clone()];
        let client = boxed_client();
        let inner = unsafe { &mut (*client).inner };
        inner.protocol_ready = true;
        inner.attach_queued = true;
        apply_kernel_input(
            inner,
            KernelInput::AttachStarted {
                attach_id: 7,
                terminals: &authorized,
            },
        )
        .expect("start attach");
        apply_kernel_input(
            inner,
            KernelInput::BootstrapBegin {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
                geometry: CanonicalGeometry::new(80, 24).expect("geometry"),
                base_seq: 0,
            },
        )
        .expect("begin bootstrap");
        apply_kernel_input(
            inner,
            KernelInput::BootstrapChunk {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: b"\x1b[?1000h",
            },
        )
        .expect("set DEC mouse mode");
        apply_kernel_input(
            inner,
            KernelInput::BootstrapReady {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
        )
        .expect("publish terminal");
        apply_kernel_input(inner, KernelInput::AttachReady { attach_id: 7 })
            .expect("release attach barrier");
        inner.attach_queued = false;
        inner.attached = true;
        inner.selection_buf.extend_from_slice(b"borrowed");
        let borrowed = inner.selection_buf.as_ptr();

        let mut enabled = false;
        assert_eq!(
            unsafe {
                phux_client_terminal_mouse_tracking(
                    client,
                    &raw const c_terminal_id,
                    &raw mut enabled,
                )
            },
            PhuxClientResult::Ok
        );
        assert!(enabled);
        assert_eq!(unsafe { &*client }.inner.selection_buf.as_ptr(), borrowed);

        apply_kernel_input(
            unsafe { &mut (*client).inner },
            KernelInput::ResourceOutput {
                terminal_id: &terminal_id,
                stream_id,
                bootstrap_id,
                seq: 1,
                payload: b"\x1b[?1000l",
            },
        )
        .expect("reset DEC mouse mode");
        assert_eq!(
            unsafe {
                phux_client_terminal_mouse_tracking(
                    client,
                    &raw const c_terminal_id,
                    &raw mut enabled,
                )
            },
            PhuxClientResult::Ok
        );
        assert!(!enabled);

        assert_eq!(
            unsafe {
                phux_client_terminal_mouse_tracking(
                    client,
                    &raw const c_terminal_id,
                    ptr::null_mut(),
                )
            },
            PhuxClientResult::InvalidArgument
        );
        assert_eq!(
            unsafe { phux_client_terminal_mouse_tracking(client, ptr::null(), &raw mut enabled) },
            PhuxClientResult::InvalidArgument
        );
        let unknown_id = PhuxResourceId {
            id: 8,
            ..c_terminal_id
        };
        assert_eq!(
            unsafe {
                phux_client_terminal_mouse_tracking(client, &raw const unknown_id, &raw mut enabled)
            },
            PhuxClientResult::InvalidState
        );
        assert_eq!(
            unsafe {
                phux_client_terminal_mouse_tracking(
                    ptr::null(),
                    &raw const c_terminal_id,
                    &raw mut enabled,
                )
            },
            PhuxClientResult::InvalidArgument
        );
        unsafe { &mut *client }.inner.in_callback = true;
        enabled = true;
        assert_eq!(
            unsafe {
                phux_client_terminal_mouse_tracking(
                    client,
                    &raw const c_terminal_id,
                    &raw mut enabled,
                )
            },
            PhuxClientResult::InvalidState
        );
        assert!(enabled);
        unsafe { &mut *client }.inner.in_callback = false;
        unsafe { &mut *client }.inner.detached = true;
        assert_eq!(
            unsafe {
                phux_client_terminal_mouse_tracking(
                    client,
                    &raw const c_terminal_id,
                    &raw mut enabled,
                )
            },
            PhuxClientResult::InvalidState
        );
        assert!(enabled);
        unsafe { phux_client_free(client) };
    }

    fn effect_bytes(effect: &PhuxClientEffect) -> &[u8] {
        if effect.bytes.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(effect.bytes.data, effect.bytes.len) }
        }
    }

    fn record(seq: u64, kind: &str, data: &str) -> String {
        format!(
            "{{\"seq\":{seq},\"ts_ms\":{},\"type\":\"{kind}\",\"data\":{data}}}\n",
            seq * 10
        )
    }

    fn effect_at(client: *mut PhuxClient, index: usize) -> PhuxClientEffect {
        let mut effect = PhuxClientEffect::default();
        assert_eq!(
            unsafe { phux_client_effect_get(client, index, &raw mut effect) },
            PhuxClientResult::Ok
        );
        effect
    }

    fn span_bytes(span: PhuxBytes) -> &'static [u8] {
        if span.len == 0 {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(span.data, span.len) }
        }
    }

    /// A focused session holding one terminal and one agent session bound to it.
    fn mixed_kind_snapshot(
        terminal: &phux_protocol::ResourceId,
        agent: &phux_protocol::ResourceId,
    ) -> phux_protocol::wire::info::SessionSnapshot {
        let session_id = SessionId::new(1);
        let window_id = phux_protocol::WindowId::new(10);
        phux_protocol::wire::info::SessionSnapshot::new(session_id, window_id, terminal.clone())
            .with_sessions(vec![phux_protocol::wire::info::SessionInfo::new(
                session_id, "working",
            )])
            .with_windows(vec![phux_protocol::wire::info::WindowInfo::new(
                window_id, session_id, "working",
            )])
            .with_resources(vec![
                phux_protocol::wire::info::ResourceInfo::new(terminal.clone(), window_id, 80, 24),
                phux_protocol::wire::info::ResourceInfo::new(
                    agent.clone(),
                    phux_protocol::WindowId::new(0),
                    0,
                    0,
                )
                .with_kind(ResourceKind::AgentSession)
                .with_parent(Some(terminal.clone()))
                .with_agent(Some(
                    phux_protocol::AgentFacet::new("claude", "working")
                        .with_native_id(Some("s-1".to_owned())),
                )),
            ])
    }

    const MIXED_TERMINAL: u32 = 30;
    const MIXED_AGENT: u32 = 31;

    /// A client attached to [`mixed_kind_snapshot`] with the terminal READY and
    /// the attach complete; the agent session is declared but has no stream yet.
    fn attached_mixed_client() -> *mut PhuxClient {
        let terminal = phux_protocol::ResourceId::local(MIXED_TERMINAL);
        let agent = phux_protocol::ResourceId::local(MIXED_AGENT);
        attached_resource_client(mixed_kind_snapshot(&terminal, &agent))
    }

    fn attached_resource_client(
        snapshot: phux_protocol::wire::info::SessionSnapshot,
    ) -> *mut PhuxClient {
        let terminal = snapshot.focused_resource.clone();
        let stream_id = phux_protocol::StreamId::new(1).expect("stream");
        let bootstrap_id = phux_protocol::BootstrapId::new(1).expect("bootstrap");
        let client = boxed_client();
        unsafe {
            (*client).inner.protocol_ready = true;
            (*client).inner.attach_queued = true;
            (*client).inner.expected_attach_id = Some(7);
            (*client).inner.selected_profile =
                Some(phux_protocol::BootstrapProfile::SynthesizedVtRaw);
        }
        for frame in [
            FrameKind::Attached {
                attach_id: 7,
                snapshot,
                initial_client_id: phux_protocol::ClientId::new(9),
            },
            FrameKind::BootstrapBegin {
                terminal_id: terminal.clone(),
                stream_id,
                bootstrap_id,
                profile: phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
                cols: 80,
                rows: 24,
                base_seq: 0,
            },
            FrameKind::BootstrapReady {
                terminal_id: terminal,
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            FrameKind::AttachReady { attach_id: 7 },
        ] {
            assert_eq!(feed_kind(client, &frame), PhuxClientResult::Ok);
        }
        assert_eq!(
            unsafe { phux_client_effect_clear(client) },
            PhuxClientResult::Ok
        );
        client
    }

    #[test]
    fn agent_sessions_are_catalogued_but_never_gate_the_barrier() {
        let terminal = phux_protocol::ResourceId::local(MIXED_TERMINAL);
        let agent = phux_protocol::ResourceId::local(MIXED_AGENT);
        let client = attached_mixed_client();
        let inner = unsafe { &(*client).inner };
        assert!(
            inner.attached,
            "the terminal alone completes an attach the agent never bootstrapped"
        );
        assert!(inner.active_attach_contains(&terminal));
        assert!(!inner.active_attach_contains(&agent));
        assert_eq!(
            inner.resource_kind(&agent),
            Some(ResourceKind::AgentSession)
        );
        assert!(inner.is_agent_stream(&agent));

        assert_eq!(unsafe { phux_client_resource_count(client) }, 2);
        let mut resource = PhuxResourceInfo::default();
        assert_eq!(
            unsafe { phux_client_resource_get(client, 0, &raw mut resource) },
            PhuxClientResult::Ok
        );
        assert_eq!(resource.kind, RESOURCE_KIND_TERMINAL);
        assert!(resource.parent.is_null());
        assert_eq!(
            resource.provider.len + resource.native_id.len + resource.state.len,
            0
        );
        assert_eq!(
            unsafe { phux_client_resource_get(client, 1, &raw mut resource) },
            PhuxClientResult::Ok
        );
        assert_eq!(resource.kind, RESOURCE_KIND_AGENT_SESSION);
        assert_eq!(
            (resource.terminal_id.kind, resource.terminal_id.id),
            (0, MIXED_AGENT)
        );
        let parent = unsafe { resource.parent.as_ref() }.expect("parent is present");
        assert_eq!((parent.kind, parent.id), (0, MIXED_TERMINAL));
        assert_eq!(span_bytes(resource.provider), b"claude");
        assert_eq!(span_bytes(resource.native_id), b"s-1");
        assert_eq!(span_bytes(resource.state), b"working");

        // A terminal profile on the agent stream, and the agent profile on a
        // terminal, are both protocol errors before the kernel sees them.
        for (terminal_id, profile) in [
            (
                agent,
                phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
            ),
            (
                terminal,
                phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1,
            ),
        ] {
            assert_eq!(
                feed_kind(
                    client,
                    &FrameKind::BootstrapBegin {
                        terminal_id,
                        stream_id: phux_protocol::StreamId::new(2).expect("stream"),
                        bootstrap_id: phux_protocol::BootstrapId::new(2).expect("bootstrap"),
                        profile,
                        cols: 0,
                        rows: 0,
                        base_seq: 0,
                    },
                ),
                PhuxClientResult::ProtocolError
            );
        }
        unsafe { phux_client_free(client) };
    }

    const AGENT_STREAM: u64 = 4;
    const AGENT_BOOTSTRAP: u64 = 5;

    /// Opens generation (4, 5) on the agent stream with two retained records
    /// and one live record: the retained backlog publishes once at READY,
    /// then live records follow.
    fn open_agent_stream(client: *mut PhuxClient) {
        let agent = phux_protocol::ResourceId::local(MIXED_AGENT);
        let stream_id = phux_protocol::StreamId::new(AGENT_STREAM).expect("stream");
        let bootstrap_id = phux_protocol::BootstrapId::new(AGENT_BOOTSTRAP).expect("bootstrap");
        let retained = format!(
            "{}{}",
            record(1, "session_start", r#"{"provider":"claude"}"#),
            record(2, "prompt", r#"{"length":3}"#)
        );
        for frame in [
            FrameKind::BootstrapBegin {
                terminal_id: agent.clone(),
                stream_id,
                bootstrap_id,
                profile: phux_protocol::BootstrapStreamProfile::AgentEventsJsonlV1,
                cols: 0,
                rows: 0,
                base_seq: 2,
            },
            FrameKind::BootstrapChunk {
                terminal_id: agent.clone(),
                stream_id,
                bootstrap_id,
                chunk_seq: 0,
                payload: bytes::Bytes::from(retained),
            },
            FrameKind::BootstrapReady {
                terminal_id: agent.clone(),
                stream_id,
                bootstrap_id,
                history_cursor: None,
            },
            FrameKind::ResourceOutput {
                terminal_id: agent.clone(),
                stream_id,
                bootstrap_id,
                seq: 3,
                bytes: bytes::Bytes::from(record(3, "ask", "{}")),
            },
        ] {
            assert_eq!(feed_kind(client, &frame), PhuxClientResult::Ok);
        }
        assert!(
            unsafe { (*client).inner.projection(&agent) }.is_none(),
            "an agent stream never publishes a terminal replica"
        );
    }

    #[test]
    fn agent_stream_frames_surface_as_records_effects() {
        let client = attached_mixed_client();
        open_agent_stream(client);
        assert_eq!(unsafe { phux_client_effect_count(client) }, 2);
        let effect = effect_at(client, 0);
        assert_eq!(
            (effect.kind, effect.detail, effect.seq),
            (EFFECT_AGENT_RECORDS, AGENT_RECORDS_RETAINED, 2)
        );
        assert_eq!((effect.stream_id, effect.bootstrap_id), (4, 5));
        assert_eq!(
            (effect.terminal_id.kind, effect.terminal_id.id),
            (0, MIXED_AGENT)
        );
        let lines: Vec<serde_json::Value> = effect_bytes(&effect)
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).expect("record line is JSON"))
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["type"], "session_start");
        assert_eq!(lines[0]["data"]["provider"], "claude");
        assert_eq!(lines[1]["seq"], 2);
        assert_eq!(lines[1]["ts_ms"], 20);
        let effect = effect_at(client, 1);
        assert_eq!(
            (effect.kind, effect.detail, effect.seq),
            (EFFECT_AGENT_RECORDS, AGENT_RECORDS_LIVE, 3)
        );
        let live: serde_json::Value =
            serde_json::from_slice(effect_bytes(&effect).trim_ascii()).expect("live record");
        assert_eq!(live["type"], "ask");
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn agent_stream_faults_retire_the_generation_and_closes_retire_the_resource() {
        let agent = phux_protocol::ResourceId::local(MIXED_AGENT);
        let stream_id = phux_protocol::StreamId::new(AGENT_STREAM).expect("stream");
        let bootstrap_id = phux_protocol::BootstrapId::new(AGENT_BOOTSTRAP).expect("bootstrap");
        let client = attached_mixed_client();
        open_agent_stream(client);
        assert_eq!(
            unsafe { phux_client_effect_clear(client) },
            PhuxClientResult::Ok
        );

        // History is a terminal facet: the kernel refuses it for an agent stream.
        assert_ne!(
            feed_kind(
                client,
                &FrameKind::HistoryRejected {
                    terminal_id: agent.clone(),
                    stream_id,
                    bootstrap_id,
                    cursor: bytes::Bytes::from_static(b"c"),
                    reason: phux_protocol::wire::frame::HistoryRejectionReason::Busy,
                    required_bytes: 1,
                    required_rows: 1,
                },
            ),
            PhuxClientResult::Ok
        );

        // A malformed payload retires the generation with a RESYNC_REQUIRED
        // status instead of handing the host a log with a hole in it.
        assert_ne!(
            feed_kind(
                client,
                &FrameKind::ResourceOutput {
                    terminal_id: agent.clone(),
                    stream_id,
                    bootstrap_id,
                    seq: 4,
                    bytes: bytes::Bytes::from_static(b"{not json\n"),
                },
            ),
            PhuxClientResult::Ok
        );
        let count = unsafe { phux_client_effect_count(client) };
        assert!(count >= 1);
        let effect = effect_at(client, count - 1);
        assert_eq!((effect.kind, effect.detail), (2, 3));
        assert_eq!(
            (effect.terminal_id.kind, effect.terminal_id.id),
            (0, MIXED_AGENT)
        );
        assert_eq!(
            unsafe { phux_client_effect_clear(client) },
            PhuxClientResult::Ok
        );

        // A close retires the resource with a CLOSED effect.
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::ResourceClosed {
                    terminal_id: agent.clone(),
                    exit_status: None,
                    reason: phux_protocol::wire::frame::CloseReason::ParentClosed,
                    signal: None,
                },
            ),
            PhuxClientResult::Ok
        );
        assert_eq!(unsafe { phux_client_resource_count(client) }, 1);
        assert_eq!(unsafe { phux_client_effect_count(client) }, 1);
        let effect = effect_at(client, 0);
        assert_eq!(
            (effect.kind, effect.detail),
            (EFFECT_AGENT_RECORDS, AGENT_RECORDS_CLOSED)
        );
        assert_eq!((effect.stream_id, effect.bootstrap_id), (4, 5));
        assert!(!unsafe { (*client).inner.is_agent_stream(&agent) });
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::ResourceOutput {
                    terminal_id: agent,
                    stream_id,
                    bootstrap_id,
                    seq: 5,
                    bytes: bytes::Bytes::from(record(5, "stop", "{}")),
                },
            ),
            PhuxClientResult::ProtocolError,
            "a closed agent resource is no longer a participant of any kind"
        );
        unsafe { phux_client_free(client) };
    }
}
