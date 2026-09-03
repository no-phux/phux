//! Stable native C bridge for the synchronous phux session kernel.

#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[cfg(target_arch = "wasm32")]
compile_error!("phux-client-ffi is a native-only libghostty bridge");

mod client;
mod error;
mod types;

use std::collections::HashSet;
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use client::{Client, Limits, SessionSummary};
use error::{BridgeError, bytes_in, check_struct, outbound_bytes_in, terminal_id_in};
use phux_client_core::engine::CanonicalGeometry;
use phux_client_core::engine::ghostty::native_bootstrap_capabilities;
use phux_client_core::session::{KernelAction, KernelInput};
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use phux_protocol::{PROTOCOL_VERSION, SessionId};

pub use types::*;

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

fn apply_kernel_input(client: &mut Client, input: KernelInput<'_>) -> Result<(), BridgeError> {
    let update_result = client
        .session
        .update(input, &mut client.effects)
        .map_err(|error| BridgeError::state(error.to_string()));
    let effect_result = client.process_effects();
    update_result.and(effect_result)
}

fn apply_input(
    client: &mut Client,
    terminal_id: &phux_protocol::TerminalId,
    event: &InputEvent,
) -> Result<(), BridgeError> {
    client.ensure_attached()?;
    apply_kernel_input(
        client,
        KernelInput::Action(KernelAction::Input { terminal_id, event }),
    )
}

fn stream_profile_matches(
    selected: phux_protocol::BootstrapProfile,
    stream: phux_protocol::BootstrapStreamProfile,
) -> bool {
    match (selected, stream) {
        (
            phux_protocol::BootstrapProfile::NativeState {
                codec: selected, ..
            },
            phux_protocol::BootstrapStreamProfile::NativeState { codec: stream },
        ) => selected == stream,
        (
            phux_protocol::BootstrapProfile::SynthesizedVtRaw,
            phux_protocol::BootstrapStreamProfile::SynthesizedVtRaw,
        )
        | (
            phux_protocol::BootstrapProfile::SynthesizedVtStateSync,
            phux_protocol::BootstrapStreamProfile::SynthesizedVtStateSync,
        ) => true,
        _ => false,
    }
}

fn history_unavailable_reason(
    reason: phux_protocol::wire::frame::HistoryTombstoneReason,
) -> Result<phux_client_core::session::HistoryUnavailableReason, BridgeError> {
    use phux_client_core::session::HistoryUnavailableReason as Core;
    use phux_protocol::wire::frame::HistoryTombstoneReason as Wire;
    Ok(match reason {
        Wire::Stale => Core::Stale,
        Wire::Pruned => Core::Pruned,
        Wire::Reset => Core::Reset,
        Wire::Resize => Core::Resize,
        Wire::Expired => Core::Expired,
        Wire::Released => Core::Released,
        Wire::Limit => Core::Limit,
        Wire::CodecFailure => Core::CodecFailure,
        _ => {
            return Err(BridgeError::protocol(
                "unsupported history tombstone reason",
            ));
        }
    })
}

fn history_rejection_reason(
    reason: phux_protocol::wire::frame::HistoryRejectionReason,
) -> Result<phux_client_core::session::HistoryRejectionReason, BridgeError> {
    use phux_client_core::session::HistoryRejectionReason as Core;
    use phux_protocol::wire::frame::HistoryRejectionReason as Wire;
    Ok(match reason {
        Wire::ZeroLimit => Core::ZeroLimit,
        Wire::TooSmall => Core::TooSmall,
        Wire::Busy => Core::Busy,
        _ => {
            return Err(BridgeError::protocol(
                "unsupported history rejection reason",
            ));
        }
    })
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

/// True when the requested cache bounds cannot retain one page of the
/// requested size.
fn history_cache_cannot_retain_page(options: &PhuxClientOptions) -> bool {
    options.max_history_cache_bytes == 0
        || options.max_history_materialized_rows == 0
        || usize::try_from(options.max_history_page_bytes).is_err()
        || usize::try_from(options.max_history_page_bytes)
            .is_ok_and(|bytes| bytes > options.max_history_cache_bytes)
        || usize::try_from(options.max_history_page_rows)
            .is_ok_and(|rows| rows > options.max_history_materialized_rows)
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

/// Replaces the client's lifecycle callbacks, or clears them when `callbacks` is null.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. `callbacks` may be null to clear callbacks;
/// otherwise it must be readable for the call. Configured callback functions
/// and their `userdata` must remain valid whenever the callbacks can run.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_set_callbacks(
    client: *mut PhuxClient,
    callbacks: *const PhuxClientCallbacks,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if callbacks.is_null() {
            client.callbacks = PhuxClientCallbacks::default();
            return Ok(());
        }
        let callbacks = unsafe { &*callbacks };
        check_struct(
            callbacks.size,
            mem::size_of::<PhuxClientCallbacks>(),
            callbacks.version,
        )?;
        client.callbacks = *callbacks;
        Ok(())
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
        if client.hello_queued || client.protocol_ready {
            return Err(BridgeError::state("HELLO was already queued or negotiated"));
        }
        let name = unsafe { outbound_bytes_in(client_name.data, client_name.len, "client name") }?;
        let name = std::str::from_utf8(name)
            .map_err(|_| BridgeError::invalid("client name is not UTF-8"))?;
        if name.is_empty() {
            return Err(BridgeError::invalid("client name is empty"));
        }
        let limits =
            BootstrapLimits::new(client.limits.bootstrap_chunk, client.limits.history_page)
                .ok_or_else(|| BridgeError::state("stored bootstrap limits are invalid"))?;
        let caps = phux_protocol::ClientCapabilities::new()
            .with_bootstrap(native_bootstrap_capabilities(limits));
        client.queue_frame(&FrameKind::Hello {
            client_name: name.to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: caps,
        })?;
        client.hello_queued = true;
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
        ensure_attach_allowed(client)?;
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        check_struct(
            options.size,
            mem::size_of::<PhuxAttachOptions>(),
            options.version,
        )?;
        validate_attach_options(options)?;
        let name_bytes =
            unsafe { outbound_bytes_in(options.name.data, options.name.len, "attach name") }?;
        let target = attach_target(options, name_bytes)?;
        queue_attach_frame(client, options, target)
    })
}

/// Rejects an ATTACH the client's lifecycle cannot accept.
fn ensure_attach_allowed(client: &Client) -> Result<(), BridgeError> {
    if !client.protocol_ready || client.attached || client.attach_queued || client.detached {
        return Err(BridgeError::state(
            "ATTACH is not valid in the current lifecycle state",
        ));
    }
    Ok(())
}

/// Rejects attach options whose identifier or geometry is unusable, including
/// pixel geometry that disagrees with its `has_pixel_size` discriminator.
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

/// Resolves the session an ATTACH addresses, rejecting an unknown target kind
/// and an empty name for the kinds that address a session by name.
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

/// Queues the ATTACH frame and records the attach the client now expects.
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
    client.queue_frame(&FrameKind::Attach {
        attach_id: options.attach_id,
        target,
        viewport,
        request_scrollback: options.request_scrollback,
        scrollback_limit_lines: options.scrollback_limit_lines,
    })?;
    client.attach_queued = true;
    client.expected_attach_id = Some(options.attach_id);
    Ok(())
}

/// Processes one complete server frame.
///
/// # Safety
///
/// When non-null, `client` must be a live client on its owning thread with
/// exclusive access for the call. When `len` is nonzero, `data` must be
/// readable for `len` bytes for the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_feed_frame(
    client: *mut PhuxClient,
    data: *const u8,
    len: usize,
) -> PhuxClientResult {
    let mut notify_attached = false;
    let result = with_client_mut(client, |client| {
        let data = unsafe { bytes_in(data, len) }?;
        let frame = decode_one_frame(client, data)?;
        ensure_frame_accepted(client, &frame)?;
        dispatch_frame(client, frame, &mut notify_attached)
    });
    if result == PhuxClientResult::Ok && notify_attached {
        invoke_attached(client)
    } else {
        result
    }
}

/// The bootstrap stream a terminal-scoped server frame belongs to.
#[allow(
    clippy::struct_field_names,
    reason = "the fields carry the wire frame's own field names, which is what makes the frame-to-KernelInput mapping checkable by eye"
)]
#[derive(Clone, Copy, Debug)]
struct StreamRef<'a> {
    terminal_id: &'a phux_protocol::TerminalId,
    stream_id: phux_protocol::StreamId,
    bootstrap_id: phux_protocol::BootstrapId,
}

impl<'a> StreamRef<'a> {
    const fn new(
        terminal_id: &'a phux_protocol::TerminalId,
        stream_id: phux_protocol::StreamId,
        bootstrap_id: phux_protocol::BootstrapId,
    ) -> Self {
        Self {
            terminal_id,
            stream_id,
            bootstrap_id,
        }
    }
}

/// Decodes the one complete frame a `feed_frame` call must carry, under the
/// payload bounds this client negotiated.
fn decode_one_frame(client: &Client, data: &[u8]) -> Result<FrameKind, BridgeError> {
    let decode_limits =
        BootstrapLimits::new(client.limits.bootstrap_chunk, client.limits.history_page)
            .ok_or_else(|| BridgeError::state("stored bootstrap limits are invalid"))?;
    let (frame, remaining) = FrameKind::decode_with_limits(data, decode_limits)
        .map_err(|error| BridgeError::protocol(error.to_string()))?;
    if !remaining.is_empty() {
        return Err(BridgeError::protocol(
            "feed_frame accepts exactly one complete frame",
        ));
    }
    Ok(frame)
}

/// Rejects a frame that arrives outside the window the client's lifecycle
/// accepts it in: after DETACHED, or before `HELLO_OK` has been answered.
fn ensure_frame_accepted(client: &Client, frame: &FrameKind) -> Result<(), BridgeError> {
    if client.detached {
        return Err(BridgeError::protocol("server frame arrived after DETACHED"));
    }
    if !client.protocol_ready
        && !matches!(
            frame,
            FrameKind::HelloOk { .. } | FrameKind::Error { .. } | FrameKind::Detached { .. }
        )
    {
        return Err(BridgeError::state("server frame arrived before HELLO_OK"));
    }
    Ok(())
}

/// Applies a connection-lifecycle or notification frame, deferring every
/// terminal-stream frame to [`dispatch_bootstrap_frame`].
fn dispatch_frame(
    client: &mut Client,
    frame: FrameKind,
    notify_attached: &mut bool,
) -> Result<(), BridgeError> {
    match frame {
        FrameKind::HelloOk {
            protocol_major,
            protocol_minor,
            server_caps,
            selected_profile,
            bootstrap_limits,
            ..
        } => apply_hello_ok(
            client,
            protocol_major,
            protocol_minor,
            server_caps,
            selected_profile,
            bootstrap_limits,
        ),
        FrameKind::Ping { nonce } => client.queue_frame(&FrameKind::Pong { nonce }),
        FrameKind::Attached {
            attach_id,
            snapshot,
            ..
        } => apply_attached(client, attach_id, snapshot),
        FrameKind::AttachReady { attach_id } => {
            apply_attach_ready(client, attach_id)?;
            *notify_attached = true;
            Ok(())
        }
        FrameKind::Bell { terminal_id } => apply_bell(client, terminal_id),
        FrameKind::Error { code, message, .. } => {
            apply_error(client, code, &message);
            Ok(())
        }
        FrameKind::Detached { reason, message } => {
            apply_detached(client, reason, message);
            Ok(())
        }
        frame => dispatch_bootstrap_frame(client, frame),
    }
}

/// Applies a bootstrap-stream frame, deferring the remaining terminal-stream
/// frames to [`dispatch_history_frame`].
fn dispatch_bootstrap_frame(client: &mut Client, frame: FrameKind) -> Result<(), BridgeError> {
    match frame {
        FrameKind::BootstrapBegin {
            terminal_id,
            stream_id,
            bootstrap_id,
            profile,
            cols,
            rows,
            base_seq,
        } => apply_bootstrap_begin(
            client,
            StreamRef::new(&terminal_id, stream_id, bootstrap_id),
            profile,
            cols,
            rows,
            base_seq,
        ),
        FrameKind::BootstrapChunk {
            terminal_id,
            stream_id,
            bootstrap_id,
            chunk_seq,
            payload,
        } => apply_bootstrap_chunk(
            client,
            StreamRef::new(&terminal_id, stream_id, bootstrap_id),
            chunk_seq,
            payload.as_ref(),
        ),
        FrameKind::BootstrapReady {
            terminal_id,
            stream_id,
            bootstrap_id,
            history_cursor,
        } => apply_bootstrap_ready(
            client,
            StreamRef::new(&terminal_id, stream_id, bootstrap_id),
            history_cursor.as_deref(),
        ),
        FrameKind::BootstrapTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            reason,
            last_valid_seq,
        } => apply_bootstrap_tombstone(
            client,
            StreamRef::new(&terminal_id, stream_id, bootstrap_id),
            reason,
            last_valid_seq,
        ),
        frame => dispatch_history_frame(client, frame),
    }
}

/// Applies a history-stream frame, deferring the live terminal frames to
/// [`dispatch_terminal_frame`].
fn dispatch_history_frame(client: &mut Client, frame: FrameKind) -> Result<(), BridgeError> {
    match frame {
        FrameKind::HistoryPage {
            terminal_id,
            stream_id,
            bootstrap_id,
            page_seq,
            cursor,
            next_cursor,
            payload,
            rows,
        } => apply_history_page(
            client,
            StreamRef::new(&terminal_id, stream_id, bootstrap_id),
            page_seq,
            cursor.as_ref(),
            next_cursor.as_deref(),
            payload.as_ref(),
            rows,
        ),
        FrameKind::HistoryTombstone {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
        } => apply_history_tombstone(
            client,
            StreamRef::new(&terminal_id, stream_id, bootstrap_id),
            cursor.as_ref(),
            reason,
        ),
        FrameKind::HistoryRejected {
            terminal_id,
            stream_id,
            bootstrap_id,
            cursor,
            reason,
            required_bytes,
            required_rows,
        } => apply_history_rejected(
            client,
            StreamRef::new(&terminal_id, stream_id, bootstrap_id),
            cursor.as_ref(),
            reason,
            required_bytes,
            required_rows,
        ),
        frame => dispatch_terminal_frame(client, frame),
    }
}

/// Applies a live terminal frame, and rejects every frame the client session
/// kernel does not accept.
fn dispatch_terminal_frame(client: &mut Client, frame: FrameKind) -> Result<(), BridgeError> {
    match frame {
        FrameKind::TerminalOutput {
            terminal_id,
            stream_id,
            bootstrap_id,
            seq,
            bytes,
        } => apply_terminal_output(
            client,
            StreamRef::new(&terminal_id, stream_id, bootstrap_id),
            seq,
            bytes.as_ref(),
        ),
        FrameKind::TerminalClosed { terminal_id, .. } => {
            apply_terminal_closed(client, &terminal_id)
        }
        _ => Err(BridgeError::protocol(
            "server sent a frame not accepted by the client session kernel",
        )),
    }
}

/// Rejects a `HELLO_OK` that does not answer this client's handshake on the
/// terms it advertised.
fn ensure_hello_ok_accepted(
    client: &Client,
    protocol_major: u16,
    protocol_minor: u16,
    bootstrap_limits: BootstrapLimits,
) -> Result<(), BridgeError> {
    if protocol_major != PROTOCOL_VERSION.major || protocol_minor != PROTOCOL_VERSION.minor {
        return Err(BridgeError::protocol(
            "server selected an unsupported protocol version",
        ));
    }
    if bootstrap_limits.max_chunk_bytes() > client.limits.bootstrap_chunk
        || bootstrap_limits.max_history_page_bytes() > client.limits.history_page
    {
        return Err(BridgeError::protocol(
            "server selected payload limits above the client advertisement",
        ));
    }
    if !client.hello_queued || client.protocol_ready {
        return Err(BridgeError::protocol("unsolicited or duplicate HELLO_OK"));
    }
    Ok(())
}

/// True when the selected bootstrap profile is one this client advertised.
const fn profile_is_advertised(
    advertised: &phux_protocol::BootstrapCapabilities,
    selected_profile: phux_protocol::BootstrapProfile,
) -> bool {
    match selected_profile {
        phux_protocol::BootstrapProfile::NativeState { codec, features } => {
            advertised.native_codecs.contains(codec) && features.supports_native()
        }
        phux_protocol::BootstrapProfile::SynthesizedVtRaw => advertised
            .profiles
            .contains(phux_protocol::BootstrapProfileKind::SynthesizedVtRaw),
        phux_protocol::BootstrapProfile::SynthesizedVtStateSync => advertised
            .profiles
            .contains(phux_protocol::BootstrapProfileKind::SynthesizedVtStateSync),
        _ => false,
    }
}

/// Closes the handshake and installs the negotiated bootstrap profile.
fn apply_hello_ok(
    client: &mut Client,
    protocol_major: u16,
    protocol_minor: u16,
    server_caps: phux_protocol::caps::ServerCapabilities,
    selected_profile: phux_protocol::BootstrapProfile,
    bootstrap_limits: BootstrapLimits,
) -> Result<(), BridgeError> {
    ensure_hello_ok_accepted(client, protocol_major, protocol_minor, bootstrap_limits)?;
    let advertised = native_bootstrap_capabilities(bootstrap_limits);
    if !profile_is_advertised(&advertised, selected_profile) {
        return Err(BridgeError::protocol(
            "server selected a bootstrap profile the client did not advertise",
        ));
    }
    client.install_profile(selected_profile, bootstrap_limits);
    client.selected_profile = Some(selected_profile);
    client.terminal_reply = server_caps
        .features
        .contains(phux_protocol::ServerFeature::TerminalReply);
    client.protocol_ready = true;
    Ok(())
}

/// Rejects an attach-lifecycle frame that answers an attach this client never
/// requested, naming the frame in the error the way the caller knows it.
fn ensure_expected_attach(client: &Client, attach_id: u32, frame: &str) -> Result<(), BridgeError> {
    if !client.attach_queued {
        return Err(BridgeError::protocol(format!("unsolicited {frame}")));
    }
    if client.expected_attach_id != Some(attach_id) {
        return Err(BridgeError::protocol(format!(
            "{frame} attach_id does not match the request"
        )));
    }
    Ok(())
}

/// Records the session catalog the server attached this client to and starts
/// the attach in the session kernel.
fn apply_attached(
    client: &mut Client,
    attach_id: u32,
    snapshot: phux_protocol::wire::info::SessionSnapshot,
) -> Result<(), BridgeError> {
    ensure_expected_attach(client, attach_id, "ATTACHED")?;
    let focused_session = snapshot.focused_session;
    // ATTACHED describes the whole workspace so native clients can project a
    // session switcher, but the server bootstraps only the focused session.
    // Counting panes from other sessions leaves the attach barrier waiting for
    // bootstrap frames the server will never send.
    let focused_windows: HashSet<_> = snapshot
        .windows
        .iter()
        .filter(|window| window.session_id == focused_session)
        .map(|window| window.id)
        .collect();
    let terminals: Vec<_> = snapshot
        .panes
        .iter()
        .filter(|pane| focused_windows.contains(&pane.window_id))
        .map(|pane| pane.id.clone())
        .collect();
    client.sessions = snapshot
        .sessions
        .into_iter()
        .map(|session| SessionSummary {
            session_id: session.id.get(),
            name: session.name.into_bytes(),
            created_at_unix_secs: session.created_at_unix_secs,
            window_count: session.window_count,
            attached_client_count: session.attached_client_count,
            focused: session.id == focused_session,
        })
        .collect();
    apply_kernel_input(
        client,
        KernelInput::AttachStarted {
            attach_id,
            terminals: &terminals,
        },
    )
}

/// Completes the attach the client requested.
fn apply_attach_ready(client: &mut Client, attach_id: u32) -> Result<(), BridgeError> {
    ensure_expected_attach(client, attach_id, "ATTACH_READY")?;
    apply_kernel_input(client, KernelInput::AttachReady { attach_id })?;
    client.attach_queued = false;
    client.attached = true;
    Ok(())
}

/// Opens a bootstrap stream, holding the server to the profile `HELLO_OK` chose.
fn apply_bootstrap_begin(
    client: &mut Client,
    stream: StreamRef<'_>,
    profile: phux_protocol::BootstrapStreamProfile,
    cols: u16,
    rows: u16,
    base_seq: u64,
) -> Result<(), BridgeError> {
    client.ensure_participant(stream.terminal_id)?;
    let selected_profile = client
        .selected_profile
        .ok_or_else(|| BridgeError::state("BOOTSTRAP_BEGIN arrived before profile negotiation"))?;
    if !stream_profile_matches(selected_profile, profile) {
        return Err(BridgeError::protocol(
            "BOOTSTRAP_BEGIN profile differs from HELLO_OK selection",
        ));
    }
    let geometry = CanonicalGeometry::new(cols, rows)
        .ok_or_else(|| BridgeError::protocol("BOOTSTRAP_BEGIN geometry is zero"))?;
    apply_kernel_input(
        client,
        KernelInput::BootstrapBegin {
            terminal_id: stream.terminal_id,
            stream_id: stream.stream_id,
            bootstrap_id: stream.bootstrap_id,
            profile,
            geometry,
            base_seq,
        },
    )
}

/// Feeds one bootstrap payload chunk to the session kernel.
fn apply_bootstrap_chunk(
    client: &mut Client,
    stream: StreamRef<'_>,
    chunk_seq: u32,
    payload: &[u8],
) -> Result<(), BridgeError> {
    client.ensure_participant(stream.terminal_id)?;
    apply_kernel_input(
        client,
        KernelInput::BootstrapChunk {
            terminal_id: stream.terminal_id,
            stream_id: stream.stream_id,
            bootstrap_id: stream.bootstrap_id,
            chunk_seq,
            payload,
        },
    )
}

/// Closes a bootstrap stream and republishes the terminal's document.
fn apply_bootstrap_ready(
    client: &mut Client,
    stream: StreamRef<'_>,
    history_cursor: Option<&[u8]>,
) -> Result<(), BridgeError> {
    client.ensure_participant(stream.terminal_id)?;
    apply_kernel_input(
        client,
        KernelInput::BootstrapReady {
            terminal_id: stream.terminal_id,
            stream_id: stream.stream_id,
            bootstrap_id: stream.bootstrap_id,
            history_cursor,
        },
    )?;
    client.invalidate_terminal_handles(stream.terminal_id);
    client.bump_document_revision(stream.terminal_id)
}

/// The history cache counters that decide whether a page changed the document.
fn history_cache_counters(
    client: &Client,
    terminal_id: &phux_protocol::TerminalId,
) -> Option<(usize, usize, usize)> {
    client.session.history_cache(terminal_id).map(|cache| {
        let status = cache.status();
        (
            status.loaded_pages,
            status.loaded_bytes,
            status.materialized_rows,
        )
    })
}

/// Admits one history page, republishing the document only when the page
/// actually moved the cache.
fn apply_history_page(
    client: &mut Client,
    stream: StreamRef<'_>,
    page_seq: u64,
    cursor: &[u8],
    next_cursor: Option<&[u8]>,
    payload: &[u8],
    rows: u32,
) -> Result<(), BridgeError> {
    client.ensure_participant(stream.terminal_id)?;
    let before = history_cache_counters(client, stream.terminal_id);
    apply_kernel_input(
        client,
        KernelInput::HistoryPage {
            terminal_id: stream.terminal_id,
            stream_id: stream.stream_id,
            bootstrap_id: stream.bootstrap_id,
            page_seq,
            rows,
            payload,
            cursor,
            next_cursor,
        },
    )?;
    let after = history_cache_counters(client, stream.terminal_id);
    if before != after {
        client.bump_document_revision(stream.terminal_id)?;
    }
    Ok(())
}

/// Records that a history range the client asked for is gone for good.
fn apply_history_tombstone(
    client: &mut Client,
    stream: StreamRef<'_>,
    cursor: &[u8],
    reason: phux_protocol::wire::frame::HistoryTombstoneReason,
) -> Result<(), BridgeError> {
    client.ensure_participant(stream.terminal_id)?;
    apply_kernel_input(
        client,
        KernelInput::HistoryTombstone {
            terminal_id: stream.terminal_id,
            stream_id: stream.stream_id,
            bootstrap_id: stream.bootstrap_id,
            cursor,
            reason: history_unavailable_reason(reason)?,
        },
    )
}

/// Records a history request the server declined, with the bounds it wanted.
fn apply_history_rejected(
    client: &mut Client,
    stream: StreamRef<'_>,
    cursor: &[u8],
    reason: phux_protocol::wire::frame::HistoryRejectionReason,
    required_bytes: u32,
    required_rows: u32,
) -> Result<(), BridgeError> {
    client.ensure_participant(stream.terminal_id)?;
    apply_kernel_input(
        client,
        KernelInput::HistoryRejected {
            terminal_id: stream.terminal_id,
            stream_id: stream.stream_id,
            bootstrap_id: stream.bootstrap_id,
            cursor,
            reason: history_rejection_reason(reason)?,
            required_bytes,
            required_rows,
        },
    )
}

/// Feeds live terminal output, republishing the document only when the applied
/// bytes advanced the published sequence.
fn apply_terminal_output(
    client: &mut Client,
    stream: StreamRef<'_>,
    seq: u64,
    payload: &[u8],
) -> Result<(), BridgeError> {
    client.ensure_participant(stream.terminal_id)?;
    let before = published_last_seq(client, stream.terminal_id);
    apply_kernel_input(
        client,
        KernelInput::TerminalOutput {
            terminal_id: stream.terminal_id,
            stream_id: stream.stream_id,
            bootstrap_id: stream.bootstrap_id,
            seq,
            payload,
        },
    )?;
    let after = published_last_seq(client, stream.terminal_id);
    if before != after {
        client.bump_document_revision(stream.terminal_id)?;
    }
    Ok(())
}

/// The published sequence a terminal's replica has reached, when it has one.
fn published_last_seq(client: &Client, terminal_id: &phux_protocol::TerminalId) -> Option<u64> {
    client
        .session
        .published(terminal_id)
        .map(|published| published.last_seq())
}

/// Drops the client-side state of a terminal the bridge can no longer serve.
fn forget_terminal(client: &mut Client, terminal_id: &phux_protocol::TerminalId) {
    client.render.remove(terminal_id);
    client.document_revisions.remove(terminal_id);
    client.invalidate_terminal_handles(terminal_id);
}

/// Records a bootstrap stream the server invalidated and drops presentation
/// state only when that stream is the currently published generation.
fn apply_bootstrap_tombstone(
    client: &mut Client,
    stream: StreamRef<'_>,
    reason: phux_protocol::wire::frame::TombstoneReason,
    last_valid_seq: u64,
) -> Result<(), BridgeError> {
    client.ensure_participant(stream.terminal_id)?;
    let retires_published = client
        .session
        .published(stream.terminal_id)
        .is_some_and(|published| {
            let key = published.key();
            key.stream_id == stream.stream_id && key.bootstrap_id == stream.bootstrap_id
        });
    apply_kernel_input(
        client,
        KernelInput::Tombstone {
            terminal_id: stream.terminal_id,
            stream_id: stream.stream_id,
            bootstrap_id: stream.bootstrap_id,
            reason,
            last_valid_seq,
        },
    )?;
    if retires_published {
        forget_terminal(client, stream.terminal_id);
    }
    Ok(())
}

/// Records a terminal whose process exited and drops its state.
fn apply_terminal_closed(
    client: &mut Client,
    terminal_id: &phux_protocol::TerminalId,
) -> Result<(), BridgeError> {
    client.ensure_participant(terminal_id)?;
    apply_kernel_input(client, KernelInput::TerminalClosed { terminal_id })?;
    forget_terminal(client, terminal_id);
    Ok(())
}

/// Publishes a bell as an effect the embedder can observe.
fn apply_bell(
    client: &mut Client,
    terminal_id: phux_protocol::TerminalId,
) -> Result<(), BridgeError> {
    client.ensure_participant(&terminal_id)?;
    client
        .owned_effects
        .push(OwnedEffect::simple(2, 1, terminal_id));
    client.rebuild_effect_views();
    Ok(())
}

/// Publishes a server error as an effect the embedder can observe.
fn apply_error(client: &mut Client, code: phux_protocol::wire::frame::ErrorCode, message: &str) {
    let mut effect = OwnedEffect::simple(2, 4, phux_protocol::TerminalId::local(0));
    effect.bytes = format!("{code:?}: {message}").into_bytes();
    client.owned_effects.push(effect);
    client.rebuild_effect_views();
}

/// Ends the session and publishes the ending as an effect.
fn apply_detached(
    client: &mut Client,
    reason: Option<phux_protocol::wire::frame::DetachReason>,
    message: String,
) {
    client.detach();
    let mut effect = OwnedEffect::simple(2, 5, phux_protocol::TerminalId::local(0));
    // phux-l83x: carry the ending's reason across the bridge as a
    // stable wire value, the way RESYNC_REQUIRED carries its
    // `TombstoneReason`. A consumer that only sees "detached"
    // cannot tell a requested detach from a server that died
    // under it. `DETACH_REASON_UNSTATED` is distinct from every
    // wire value precisely so absence stays legible: `REQUESTED`
    // is `0`, so a zero default would have claimed the user asked
    // for an ending they did not.
    effect.status_code =
        reason.map_or(DETACH_REASON_UNSTATED, |reason| u32::from(reason.as_wire()));
    effect.bytes = message.into_bytes();
    client.owned_effects.push(effect);
    client.rebuild_effect_views();
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
            client.inner.effect_views.len()
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
        *out = *client
            .inner
            .effect_views
            .get(index)
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
        client.rebuild_effect_views();
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
    out_enabled: *mut bool,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        let out = unsafe { out_enabled.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_enabled is null"))?;
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
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
        client.queue_frame(&FrameKind::TerminalResize {
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
    anchor: PhuxDocumentAnchor,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.release_anchor(&terminal_id, anchor)
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
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
    terminal_id: *const PhuxTerminalId,
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
                phux_protocol::TerminalId::local(7),
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
        let terminal_id = phux_protocol::TerminalId::local(7);
        let c_terminal_id = PhuxTerminalId {
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
        .with_panes(vec![phux_protocol::wire::info::TerminalInfo::new(
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

        let mut view = PhuxTerminalGridView::default();
        assert_eq!(
            unsafe { phux_client_terminal_grid(client, &raw const c_terminal_id, &raw mut view,) },
            PhuxClientResult::Ok
        );
        assert_eq!(view.stream_id, stream_id.get());
        assert_eq!(view.bootstrap_id, second_bootstrap.get());
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
        let focused_terminal = phux_protocol::TerminalId::local(30);
        let other_terminal = phux_protocol::TerminalId::local(40);
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
        .with_panes(vec![
            phux_protocol::wire::info::TerminalInfo::new(
                other_terminal.clone(),
                other_window,
                80,
                24,
            ),
            phux_protocol::wire::info::TerminalInfo::new(
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
        assert!(unsafe {
            (*client)
                .inner
                .session
                .active_attach_contains(&focused_terminal)
        });
        assert!(
            !unsafe {
                (*client)
                    .inner
                    .session
                    .active_attach_contains(&other_terminal)
            },
            "a terminal in another session is never bootstrapped by this attach"
        );
        unsafe { phux_client_free(client) };
    }

    fn three_pane_snapshot(
        seed: &phux_protocol::TerminalId,
        horizontal: &phux_protocol::TerminalId,
        vertical: &phux_protocol::TerminalId,
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
                .with_active_pane(Some(seed.clone()))
                .with_layout(Some(layout)),
        ])
        .with_panes(vec![
            phux_protocol::wire::info::TerminalInfo::new(
                phux_protocol::TerminalId::local(1),
                catalog_window,
                80,
                24,
            ),
            phux_protocol::wire::info::TerminalInfo::new(seed.clone(), working_window, 80, 24),
            phux_protocol::wire::info::TerminalInfo::new(
                horizontal.clone(),
                working_window,
                40,
                24,
            ),
            phux_protocol::wire::info::TerminalInfo::new(vertical.clone(), working_window, 40, 12),
        ])
    }

    fn feed_complete_bootstrap(
        client: *mut PhuxClient,
        terminal_id: phux_protocol::TerminalId,
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
        let seed = phux_protocol::TerminalId::local(2);
        let horizontal = phux_protocol::TerminalId::local(3);
        let vertical = phux_protocol::TerminalId::local(4);
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
                &FrameKind::TerminalClosed {
                    terminal_id: seed.clone(),
                    exit_status: None,
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
        assert!(!unsafe { (*client).inner.session.active_attach_contains(&seed) });
        assert!(unsafe { (*client).inner.session.active_attach_contains(&horizontal) });
        assert!(unsafe { (*client).inner.session.active_attach_contains(&vertical) });
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
            phux_protocol::TerminalId::local(30),
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
    fn feed_rejects_payload_above_current_limit_before_lifecycle_dispatch() {
        let client = boxed_client();
        let payload = vec![0_u8; 2 * 1024];
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::BootstrapChunk {
                    terminal_id: phux_protocol::TerminalId::local(7),
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
                .session
                .active_attach_contains(&phux_protocol::TerminalId::local(7))
        });
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn terminal_state_frames_require_an_active_attach_participant() {
        let terminal_id = phux_protocol::TerminalId::local(7);
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
                &FrameKind::TerminalClosed {
                    terminal_id: terminal_id.clone(),
                    exit_status: None,
                },
            ),
            PhuxClientResult::ProtocolError
        );
        assert!(!unsafe { (*client).inner.session.active_attach_contains(&terminal_id) });
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
        assert!(unsafe { (*client).inner.session.active_attach_contains(&terminal_id) });
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::BootstrapBegin {
                    terminal_id: phux_protocol::TerminalId::local(8),
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
        let terminal_id = phux_protocol::TerminalId::local(7);
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
        assert!(
            !client_ref
                .inner
                .session
                .active_attach_contains(&terminal_id)
        );
        let effects_before = client_ref.inner.owned_effects.len();
        assert_eq!(
            feed_kind(
                client,
                &FrameKind::TerminalOutput {
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
        let terminal_id = phux_protocol::TerminalId::local(7);
        let c_terminal_id = PhuxTerminalId {
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
            KernelInput::TerminalOutput {
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
        let unknown_id = PhuxTerminalId {
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
}
