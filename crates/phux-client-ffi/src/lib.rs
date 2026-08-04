//! Stable native C bridge for the current phux terminal projection.

#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[cfg(target_arch = "wasm32")]
compile_error!("phux-client-ffi is a native-only libghostty bridge");

mod client;
mod error;
mod types;

use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use client::Client;
use error::{BridgeError, bytes_in, check_struct, outbound_bytes_in, terminal_id_in};
use phux_protocol::caps::{ImageProtocolSet, OutputMode};
use phux_protocol::input::InputEvent;
use phux_protocol::input::focus::FocusEvent;
use phux_protocol::input::key::{KeyAction, KeyEvent, ModSet, PhysicalKey};
use phux_protocol::input::mouse::{MouseAction, MouseButton, MouseEvent};
use phux_protocol::input::paste::{PasteEvent, PasteTrust};
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};
use phux_protocol::{PROTOCOL_VERSION, SessionId};

pub use types::*;

#[repr(C)]
struct StructHeader {
    size: usize,
    version: u32,
}

unsafe fn checked_struct_ref<'a, T>(
    value: *const T,
    name: &'static str,
) -> Result<&'a T, BridgeError> {
    let header = unsafe { value.cast::<StructHeader>().as_ref() }
        .ok_or_else(|| BridgeError::invalid(format!("{name} is null")))?;
    check_struct(header.size, mem::size_of::<T>(), header.version)?;
    // SAFETY: the validated size guarantees the caller advertised storage for T.
    Ok(unsafe { &*value })
}

fn bool_in(value: u8, name: &'static str) -> Result<bool, BridgeError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(BridgeError::invalid(format!("{name} is not 0 or 1"))),
    }
}

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
            if !client_ref.inner.failed {
                client_ref.inner.last_error.clear();
            }
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
    terminal_id: phux_protocol::TerminalId,
    event: InputEvent,
) -> Result<(), BridgeError> {
    client.queue_input(terminal_id, event)
}

/// Creates a client owned by the calling thread.
///
/// # Safety
/// `options` must be readable and `out_client` writable for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_new(
    options: *const PhuxClientOptions,
    out_client: *mut *mut PhuxClient,
) -> PhuxClientResult {
    match catch_unwind(AssertUnwindSafe(|| -> Result<(), BridgeError> {
        let out = unsafe { out_client.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_client is null"))?;
        *out = ptr::null_mut();
        let _options = unsafe { checked_struct_ref::<PhuxClientOptions>(options, "options") }?;
        let client = Box::new(PhuxClient {
            inner: Client::new(),
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

/// Replaces lifecycle callbacks, or clears them when `callbacks` is null.
///
/// # Safety
/// Pointers must be valid for this call and callbacks must outlive their use.
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
        let callbacks =
            unsafe { checked_struct_ref::<PhuxClientCallbacks>(callbacks, "callbacks") }?;
        client.callbacks = *callbacks;
        Ok(())
    })
}

/// Destroys a client.
///
/// # Safety
/// `client` must be null or a uniquely owned pointer returned by `phux_client_new`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_free(client: *mut PhuxClient) {
    if unsafe { client.as_ref() }.is_some_and(|client| client.inner.in_callback) {
        return;
    }
    if !client.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| {
            drop(unsafe { Box::from_raw(client) });
        }));
    }
}

/// Returns the lifecycle state.
///
/// # Safety
/// `client` must be null or point to a live client on its owning thread.
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

/// Returns the latest borrowed error message.
///
/// # Safety
/// Pointers must be valid for this call. The returned span expires on mutation.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_last_error(
    client: *const PhuxClient,
    out_error: *mut PhuxBytes,
) -> PhuxClientResult {
    match catch_unwind(AssertUnwindSafe(|| {
        let client = unsafe { client.as_ref() }.ok_or(PhuxClientResult::InvalidArgument)?;
        if client.inner.in_callback {
            return Err(PhuxClientResult::InvalidState);
        }
        let out = unsafe { out_error.as_mut() }.ok_or(PhuxClientResult::InvalidArgument)?;
        *out = bytes_out(&client.inner.last_error);
        Ok(())
    })) {
        Ok(Ok(())) => PhuxClientResult::Ok,
        Ok(Err(error)) => error,
        Err(_) => PhuxClientResult::Panic,
    }
}

/// Queues the current-protocol HELLO frame.
///
/// # Safety
/// The client and client-name span must be valid for this call.
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
        let capabilities = phux_protocol::ClientCapabilities::new()
            .with_output_mode(OutputMode::StateSync)
            .with_image_protocols(ImageProtocolSet::new());
        client.queue_frame(&FrameKind::Hello {
            client_name: name.to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: capabilities,
        })?;
        client.hello_queued = true;
        Ok(())
    })
}

/// Queues a current-protocol ATTACH frame.
///
/// # Safety
/// The client, options, and any option spans must be valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_attach(
    client: *mut PhuxClient,
    options: *const PhuxAttachOptions,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if !client.protocol_ready || client.attached || client.attach_queued || client.detached {
            return Err(BridgeError::state(
                "ATTACH is not valid in the current lifecycle state",
            ));
        }
        let options = unsafe { checked_struct_ref::<PhuxAttachOptions>(options, "options") }?;
        let has_pixel_size = bool_in(options.has_pixel_size, "has_pixel_size")?;
        let request_scrollback = bool_in(options.request_scrollback, "request_scrollback")?;
        if options.cols == 0 || options.rows == 0 {
            return Err(BridgeError::invalid("attach geometry must be non-zero"));
        }
        if has_pixel_size {
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
        let name =
            unsafe { outbound_bytes_in(options.name.data, options.name.len, "attach name") }?;
        let name = std::str::from_utf8(name)
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
        let pixels = has_pixel_size.then_some((options.pixel_width, options.pixel_height));
        client.max_scrollback = if request_scrollback {
            if options.scrollback_limit_lines == 0 {
                u32::MAX as usize
            } else {
                options.scrollback_limit_lines as usize
            }
        } else {
            0
        };
        client.queue_frame(&FrameKind::Attach {
            target,
            viewport: ViewportInfo::new(options.cols, options.rows)
                .with_pixels(pixels.map(|value| value.0), pixels.map(|value| value.1)),
            request_scrollback,
            scrollback_limit_lines: options.scrollback_limit_lines,
        })?;
        client.attach_queued = true;
        Ok(())
    })
}

/// Processes exactly one complete current-protocol server frame.
///
/// # Safety
/// The client and frame span must be valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_feed_frame(
    client: *mut PhuxClient,
    data: *const u8,
    len: usize,
) -> PhuxClientResult {
    let mut notify_attached = false;
    let mut notify_failed = false;
    let result = with_client_mut(client, |client| {
        let data = unsafe { bytes_in(data, len) }?;
        let (frame, remaining) =
            FrameKind::decode(data).map_err(|error| BridgeError::protocol(error.to_string()))?;
        if !remaining.is_empty() {
            return Err(BridgeError::protocol(
                "feed_frame accepts exactly one complete frame",
            ));
        }
        notify_attached = client.feed(frame)?;
        notify_failed = client.failed;
        Ok(())
    });
    if result == PhuxClientResult::Ok && notify_failed {
        let callback_result = invoke_failure(client, PhuxClientResult::ProtocolError);
        if callback_result == PhuxClientResult::Panic {
            callback_result
        } else {
            result
        }
    } else if result == PhuxClientResult::Ok && notify_attached {
        invoke_attached(client)
    } else {
        result
    }
}

/// Runs time-based projection maintenance such as synchronized-output expiry.
///
/// # Safety
/// `client` must point to a live client on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_maintenance(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.maintenance();
        Ok(())
    })
}

/// Reports whether time-based maintenance needs future owning-thread calls.
///
/// # Safety
/// `client` must point to a live client on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_maintenance_pending(client: *const PhuxClient) -> bool {
    unsafe { client.as_ref() }
        .is_some_and(|client| !client.inner.in_callback && client.inner.maintenance_pending())
}

/// Returns the queued outgoing-frame count.
///
/// # Safety
/// `client` must point to a live client on its owning thread.
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

/// Returns a borrowed outgoing frame.
///
/// # Safety
/// Pointers must be valid for this call. The returned span expires on mutation.
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

/// Clears queued outgoing frames.
///
/// # Safety
/// `client` must point to a live client on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_outgoing_clear(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.outgoing.clear();
        Ok(())
    })
}

/// Returns the staged effect count.
///
/// # Safety
/// `client` must point to a live client on its owning thread.
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
/// Pointers must be valid for this call. Borrowed spans expire on mutation.
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

/// Clears staged effects.
///
/// # Safety
/// `client` must point to a live client on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_effect_clear(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.owned_effects.clear();
        client.rebuild_effect_views();
        Ok(())
    })
}

/// Returns a borrowed dense terminal grid.
///
/// # Safety
/// Pointers must be valid for this call. Borrowed storage expires on mutation.
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
        *out = unsafe { *view };
        Ok(())
    })
}

/// Reports whether the terminal has an active DEC mouse-tracking mode.
///
/// # Safety
/// Pointers must be valid for this call.
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
        *out = client.mouse_tracking(&terminal_id)?;
        Ok(())
    })
}

/// Queues structured key input.
///
/// # Safety
/// Pointers and selected spans must be valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_send_key(
    client: *mut PhuxClient,
    terminal_id: *const PhuxTerminalId,
    event: *const PhuxKeyEvent,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let event = unsafe { checked_struct_ref::<PhuxKeyEvent>(event, "event") }?;
        let has_text = bool_in(event.has_text, "has_text")?;
        let composing = bool_in(event.composing, "composing")?;
        let has_unshifted_codepoint =
            bool_in(event.has_unshifted_codepoint, "has_unshifted_codepoint")?;
        let text = if has_text {
            let bytes = unsafe { outbound_bytes_in(event.text.data, event.text.len, "key text") }?;
            let text = std::str::from_utf8(bytes)
                .map_err(|_| BridgeError::invalid("key text is not UTF-8"))?;
            if text.chars().any(|ch| {
                ch <= '\u{1f}' || ch == '\u{7f}' || ('\u{f700}'..='\u{f8ff}').contains(&ch)
            }) {
                return Err(BridgeError::invalid(
                    "key text contains forbidden control or platform function codepoints",
                ));
            }
            Some(text.to_owned())
        } else {
            if event.text.len != 0 {
                return Err(BridgeError::invalid(
                    "key text bytes present when has_text is false",
                ));
            }
            None
        };
        let mods = ModSet::from_bits(event.modifiers)
            .ok_or_else(|| BridgeError::invalid("unknown key modifier bits"))?;
        let consumed_mods = ModSet::from_bits(event.consumed_modifiers)
            .ok_or_else(|| BridgeError::invalid("unknown consumed modifier bits"))?;
        if !mods.contains(consumed_mods) {
            return Err(BridgeError::invalid(
                "consumed modifiers are not a subset of modifiers",
            ));
        }
        let unshifted_codepoint = if has_unshifted_codepoint {
            char::from_u32(event.unshifted_codepoint)
                .ok_or_else(|| BridgeError::invalid("unshifted codepoint is not Unicode"))?;
            Some(event.unshifted_codepoint)
        } else {
            if event.unshifted_codepoint != 0 {
                return Err(BridgeError::invalid(
                    "unshifted codepoint is present without its discriminator",
                ));
            }
            None
        };
        apply_input(
            client,
            terminal_id,
            InputEvent::Key(KeyEvent {
                action: KeyAction::try_from(event.action)
                    .map_err(|_| BridgeError::invalid("unknown key action"))?,
                key: PhysicalKey::try_from(event.key)
                    .map_err(|_| BridgeError::invalid("unknown physical key"))?,
                mods,
                consumed_mods,
                composing,
                text,
                unshifted_codepoint,
            }),
        )
    })
}

/// Queues structured mouse input.
///
/// # Safety
/// Pointers must be valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_send_mouse(
    client: *mut PhuxClient,
    terminal_id: *const PhuxTerminalId,
    event: *const PhuxMouseEvent,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        let event = unsafe { checked_struct_ref::<PhuxMouseEvent>(event, "event") }?;
        if !event.x.is_finite() || !event.y.is_finite() || event.x < 0.0 || event.y < 0.0 {
            return Err(BridgeError::invalid(
                "mouse coordinates must be finite and non-negative",
            ));
        }
        apply_input(
            client,
            terminal_id,
            InputEvent::Mouse(MouseEvent {
                action: MouseAction::try_from(event.action)
                    .map_err(|_| BridgeError::invalid("unknown mouse action"))?,
                button: MouseButton::try_from(event.button)
                    .map_err(|_| BridgeError::invalid("unknown mouse button"))?,
                mods: ModSet::from_bits(event.modifiers)
                    .ok_or_else(|| BridgeError::invalid("unknown mouse modifier bits"))?,
                x: event.x,
                y: event.y,
            }),
        )
    })
}

/// Queues focus input.
///
/// # Safety
/// Pointers must be valid for this call.
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
            terminal_id,
            InputEvent::Focus(if focused {
                FocusEvent::Gained
            } else {
                FocusEvent::Lost
            }),
        )
    })
}

/// Queues paste input.
///
/// # Safety
/// Pointers and the paste span must be valid for this call.
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
            terminal_id,
            InputEvent::Paste(PasteEvent {
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

/// Queues a per-terminal resize.
///
/// # Safety
/// Pointers must be valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_terminal_resize(
    client: *mut PhuxClient,
    terminal_id: *const PhuxTerminalId,
    cols: u16,
    rows: u16,
) -> PhuxClientResult {
    unsafe { phux_client_terminal_resize_with_pixels(client, terminal_id, cols, rows, 0, 0, 0) }
}

/// Queues a per-terminal resize with optional viewport pixel geometry.
///
/// # Safety
/// Pointers must be valid for this call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_terminal_resize_with_pixels(
    client: *mut PhuxClient,
    terminal_id: *const PhuxTerminalId,
    cols: u16,
    rows: u16,
    has_pixel_size: u8,
    pixel_width: u16,
    pixel_height: u16,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let has_pixel_size = bool_in(has_pixel_size, "has_pixel_size")?;
        if cols == 0 || rows == 0 {
            return Err(BridgeError::invalid(
                "terminal resize geometry must be non-zero",
            ));
        }
        if has_pixel_size {
            if pixel_width == 0 || pixel_height == 0 {
                return Err(BridgeError::invalid(
                    "terminal pixel geometry must be non-zero when present",
                ));
            }
        } else if pixel_width != 0 || pixel_height != 0 {
            return Err(BridgeError::invalid(
                "terminal pixel geometry is present without its discriminator",
            ));
        }
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        client.ensure_replica(&terminal_id)?;
        client.queue_frame(&FrameKind::TerminalResize {
            terminal_id,
            cols,
            rows,
            pixel_width: has_pixel_size.then_some(pixel_width),
            pixel_height: has_pixel_size.then_some(pixel_height),
        })
    })
}

/// Queues an outer viewport resize.
///
/// # Safety
/// `client` must point to a live client on its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_viewport_resize(
    client: *mut PhuxClient,
    cols: u16,
    rows: u16,
    has_pixel_size: u8,
    pixel_width: u16,
    pixel_height: u16,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        let has_pixel_size = bool_in(has_pixel_size, "has_pixel_size")?;
        if cols == 0 || rows == 0 {
            return Err(BridgeError::invalid(
                "viewport resize geometry must be non-zero",
            ));
        }
        client.ensure_attached()?;
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
        })
    })
}

/// Scrolls the local terminal viewport.
///
/// # Safety
/// Pointers must be valid for this call.
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

/// Creates a tracked libghostty document anchor.
///
/// # Safety
/// Pointers must be valid for this call.
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

/// Releases a tracked document anchor.
///
/// # Safety
/// Pointers must be valid for this call.
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

/// Pins the local viewport to a tracked anchor.
///
/// # Safety
/// Pointers must be valid for this call.
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

/// Returns the local viewport to the live bottom.
///
/// # Safety
/// Pointers must be valid for this call.
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

/// Sets the local terminal selection.
///
/// # Safety
/// Pointers must be valid for this call.
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

/// Clears the local terminal selection.
///
/// # Safety
/// Pointers must be valid for this call.
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

/// Returns borrowed selected text.
///
/// # Safety
/// Pointers must be valid for this call. The returned span expires on mutation.
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

/// Searches loaded terminal content when supported by the projection engine.
///
/// # Safety
/// Pointers and spans must be valid for this call. Results expire on mutation.
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

/// Releases every anchor owned by the currently borrowed search result set.
///
/// This is one mutation by design: hosts must not iterate a borrowed result
/// array while calling per-anchor mutators, because the first mutation expires
/// that array.
///
/// # Safety
/// `client` must be a live client pointer.
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
    if catch_unwind(AssertUnwindSafe(|| {
        client_ref.inner.release_search_results();
        client_ref.inner.last_error.clear();
    }))
    .is_ok()
    {
        PhuxClientResult::Ok
    } else {
        client_ref
            .inner
            .set_error("panic contained at phux-client FFI boundary");
        PhuxClientResult::Panic
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn options() -> PhuxClientOptions {
        PhuxClientOptions {
            size: mem::size_of::<PhuxClientOptions>(),
            version: ABI_VERSION,
            max_bootstrap_chunk_bytes: 0,
            max_history_page_bytes: 0,
            max_history_page_rows: 0,
            max_history_cache_bytes: 0,
            max_history_materialized_rows: 0,
            history_prefetch_rows: 0,
        }
    }

    #[test]
    fn constructor_accepts_reserved_legacy_limits() {
        let mut client = ptr::null_mut();
        assert_eq!(
            unsafe { phux_client_new(&options(), &raw mut client) },
            PhuxClientResult::Ok
        );
        assert!(!client.is_null());
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn constructor_rejects_a_truncated_sized_struct_before_full_dereference() {
        let header = StructHeader {
            size: mem::size_of::<StructHeader>(),
            version: ABI_VERSION,
        };
        let mut client = ptr::null_mut();
        assert_eq!(
            unsafe {
                phux_client_new(
                    ptr::from_ref(&header).cast::<PhuxClientOptions>(),
                    &raw mut client,
                )
            },
            PhuxClientResult::InvalidArgument
        );
        assert!(client.is_null());
    }

    #[test]
    fn rust_abi_layout_matches_the_c_header_contract() {
        assert_eq!(mem::size_of::<PhuxBytes>(), 16);
        assert_eq!(mem::size_of::<PhuxTerminalId>(), 24);
        assert_eq!(mem::size_of::<PhuxClientOptions>(), 48);
        assert_eq!(mem::size_of::<PhuxClientCallbacks>(), 40);
        assert_eq!(mem::size_of::<PhuxAttachOptions>(), 56);
        assert_eq!(mem::size_of::<PhuxClientEffect>(), 88);
        assert_eq!(mem::size_of::<PhuxDocumentAnchor>(), 8);
        assert_eq!(mem::size_of::<PhuxDocumentPoint>(), 12);
        assert_eq!(mem::size_of::<PhuxTerminalCell>(), 36);
        assert_eq!(mem::size_of::<PhuxTerminalGridView>(), 176);
        assert_eq!(mem::size_of::<PhuxKeyEvent>(), 56);
        assert_eq!(mem::size_of::<PhuxMouseEvent>(), 40);
        assert_eq!(mem::size_of::<PhuxSearchResult>(), 16);
        assert_eq!(mem::offset_of!(PhuxAttachOptions, attach_id), 12);
        assert_eq!(mem::offset_of!(PhuxClientEffect, terminal_id), 16);
        assert_eq!(mem::offset_of!(PhuxTerminalGridView, cells), 64);
        assert_eq!(mem::offset_of!(PhuxTerminalGridView, top_anchor), 168);
        assert_eq!(mem::offset_of!(PhuxKeyEvent, text), 32);
    }

    #[test]
    fn hello_advertises_current_state_sync_protocol() {
        let mut client = ptr::null_mut();
        assert_eq!(
            unsafe { phux_client_new(&options(), &raw mut client) },
            PhuxClientResult::Ok
        );
        let name = b"ffi-test";
        assert_eq!(
            unsafe {
                phux_client_queue_hello(
                    client,
                    PhuxBytes {
                        data: name.as_ptr(),
                        len: name.len(),
                    },
                )
            },
            PhuxClientResult::Ok
        );
        let queued = unsafe { &(*client).inner.outgoing };
        let (frame, remaining) = FrameKind::decode(&queued[0]).expect("queued HELLO decodes");
        assert!(remaining.is_empty());
        assert!(matches!(
            frame,
            FrameKind::Hello {
                protocol_major,
                protocol_minor,
                client_caps,
                ..
            } if protocol_major == PROTOCOL_VERSION.major
                && protocol_minor == PROTOCOL_VERSION.minor
                && client_caps.output_mode == OutputMode::StateSync
        ));
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn legal_server_failure_preserves_its_callback_message() {
        let mut client = ptr::null_mut();
        assert_eq!(
            unsafe { phux_client_new(&options(), &raw mut client) },
            PhuxClientResult::Ok
        );
        let mut frame = bytes::BytesMut::new();
        FrameKind::Error {
            request_id: None,
            code: phux_protocol::wire::frame::ErrorCode::VersionIncompatible,
            message: "upgrade the native client".to_owned(),
        }
        .encode(&mut frame);
        assert_eq!(
            unsafe { phux_client_feed_frame(client, frame.as_ptr(), frame.len()) },
            PhuxClientResult::Ok
        );
        assert_eq!(
            unsafe { &(*client).inner.last_error },
            b"upgrade the native client"
        );
        unsafe { phux_client_free(client) };
    }

    #[test]
    fn caller_owned_boolean_discriminators_are_validated() {
        let mut client = ptr::null_mut();
        assert_eq!(
            unsafe { phux_client_new(&options(), &raw mut client) },
            PhuxClientResult::Ok
        );
        unsafe { (*client).inner.protocol_ready = true };
        let attach = PhuxAttachOptions {
            size: mem::size_of::<PhuxAttachOptions>(),
            version: ABI_VERSION,
            attach_id: 1,
            target_kind: 0,
            session_id: 0,
            name: PhuxBytes::default(),
            cols: 80,
            rows: 24,
            has_pixel_size: 2,
            pixel_width: 0,
            pixel_height: 0,
            request_scrollback: 0,
            scrollback_limit_lines: 0,
        };
        assert_eq!(
            unsafe { phux_client_queue_attach(client, &raw const attach) },
            PhuxClientResult::InvalidArgument
        );
        unsafe { phux_client_free(client) };
    }
}
