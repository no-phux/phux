//! Stable native C bridge for the synchronous phux session kernel.

#![cfg_attr(target_arch = "wasm32", allow(dead_code))]

#[cfg(target_arch = "wasm32")]
compile_error!("phux-client-ffi is a native-only libghostty bridge");

mod client;
mod error;
mod types;

use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::ptr;

use client::{Client, Limits};
use error::{BridgeError, bytes_in, check_struct, terminal_id_in};
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
        let limits = BootstrapLimits::new(
            options.max_bootstrap_chunk_bytes,
            options.max_history_page_bytes,
        )
        .ok_or_else(|| {
            BridgeError::invalid("bootstrap/history bounds are zero or exceed protocol limits")
        })?;
        if options.max_history_page_rows == 0
            || options.max_history_page_rows > phux_client_core::history::MAX_HISTORY_PAGE_ROWS
        {
            return Err(BridgeError::invalid(
                "history page row bound is zero or exceeds the protocol limit",
            ));
        }
        if options.max_history_cache_bytes == 0
            || options.max_history_materialized_rows == 0
            || usize::try_from(options.max_history_page_bytes).is_err()
            || usize::try_from(options.max_history_page_bytes)
                .is_ok_and(|bytes| bytes > options.max_history_cache_bytes)
            || usize::try_from(options.max_history_page_rows)
                .is_ok_and(|rows| rows > options.max_history_materialized_rows)
        {
            return Err(BridgeError::invalid(
                "history cache bounds cannot retain one requested page",
            ));
        }
        let client = Box::new(PhuxClient {
            inner: Client::new(Limits {
                bootstrap_chunk: limits.max_chunk_bytes(),
                history_page: limits.max_history_page_bytes(),
                history_page_rows: options.max_history_page_rows,
                history_cache_bytes: options.max_history_cache_bytes,
                history_materialized_rows: options.max_history_materialized_rows,
                history_prefetch_rows: options.history_prefetch_rows,
            }),
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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_queue_hello(
    client: *mut PhuxClient,
    client_name: PhuxBytes,
) -> PhuxClientResult {
    with_client_mut(client, |client| {
        if client.hello_queued || client.protocol_ready {
            return Err(BridgeError::state("HELLO was already queued or negotiated"));
        }
        let name = unsafe { bytes_in(client_name.data, client_name.len) }?;
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
        client.queue_frame(FrameKind::Hello {
            client_name: name.to_owned(),
            protocol_major: PROTOCOL_VERSION.major,
            protocol_minor: PROTOCOL_VERSION.minor,
            protocol_patch: PROTOCOL_VERSION.patch,
            client_caps: caps,
        });
        client.hello_queued = true;
        Ok(())
    })
}

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
        let options =
            unsafe { options.as_ref() }.ok_or_else(|| BridgeError::invalid("options is null"))?;
        check_struct(
            options.size,
            mem::size_of::<PhuxAttachOptions>(),
            options.version,
        )?;
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
        let name_bytes = unsafe { bytes_in(options.name.data, options.name.len) }?;
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
        let pixels = options
            .has_pixel_size
            .then_some((options.pixel_width, options.pixel_height));
        let viewport = ViewportInfo::new(options.cols, options.rows)
            .with_pixels(pixels.map(|value| value.0), pixels.map(|value| value.1));
        client.queue_frame(FrameKind::Attach {
            attach_id: options.attach_id,
            target,
            viewport,
            request_scrollback: options.request_scrollback,
            scrollback_limit_lines: options.scrollback_limit_lines,
        });
        client.attach_queued = true;
        client.expected_attach_id = Some(options.attach_id);
        Ok(())
    })
}

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_feed_frame(
    client: *mut PhuxClient,
    data: *const u8,
    len: usize,
) -> PhuxClientResult {
    let mut notify_attached = false;
    let result = with_client_mut(client, |client| {
        let data = unsafe { bytes_in(data, len) }?;
        let (frame, remaining) =
            FrameKind::decode(data).map_err(|error| BridgeError::protocol(error.to_string()))?;
        if !remaining.is_empty() {
            return Err(BridgeError::protocol(
                "feed_frame accepts exactly one complete frame",
            ));
        }
        if !client.protocol_ready && !matches!(&frame, FrameKind::HelloOk { .. }) {
            return Err(BridgeError::state("server frame arrived before HELLO_OK"));
        }
        match frame {
            FrameKind::HelloOk {
                protocol_major,
                protocol_minor,
                server_caps,
                selected_profile,
                bootstrap_limits,
                ..
            } => {
                if protocol_major != PROTOCOL_VERSION.major
                    || protocol_minor != PROTOCOL_VERSION.minor
                {
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
                let advertised = native_bootstrap_capabilities(bootstrap_limits);
                let profile_supported = match selected_profile {
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
                };
                if !profile_supported {
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
            }
            FrameKind::Ping { nonce } => client.queue_frame(FrameKind::Pong { nonce }),
            FrameKind::Attached {
                attach_id,
                snapshot,
                ..
            } => {
                if !client.attach_queued {
                    return Err(BridgeError::protocol("unsolicited ATTACHED"));
                }
                if client.expected_attach_id != Some(attach_id) {
                    return Err(BridgeError::protocol(
                        "ATTACHED attach_id does not match the request",
                    ));
                }
                let terminals: Vec<_> = snapshot.panes.into_iter().map(|pane| pane.id).collect();
                apply_kernel_input(
                    client,
                    KernelInput::AttachStarted {
                        attach_id,
                        terminals: &terminals,
                    },
                )?;
            }
            FrameKind::AttachReady { attach_id } => {
                if !client.attach_queued {
                    return Err(BridgeError::protocol("unsolicited ATTACH_READY"));
                }
                if client.expected_attach_id != Some(attach_id) {
                    return Err(BridgeError::protocol(
                        "ATTACH_READY attach_id does not match the request",
                    ));
                }
                apply_kernel_input(client, KernelInput::AttachReady { attach_id })?;
                client.attach_queued = false;
                client.attached = true;
                notify_attached = true;
            }
            FrameKind::BootstrapBegin {
                terminal_id,
                stream_id,
                bootstrap_id,
                profile,
                cols,
                rows,
                base_seq,
            } => {
                let selected_profile = client.selected_profile.ok_or_else(|| {
                    BridgeError::state("BOOTSTRAP_BEGIN arrived before profile negotiation")
                })?;
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
                        terminal_id: &terminal_id,
                        stream_id,
                        bootstrap_id,
                        profile,
                        geometry,
                        base_seq,
                    },
                )?;
            }
            FrameKind::BootstrapChunk {
                terminal_id,
                stream_id,
                bootstrap_id,
                chunk_seq,
                payload,
            } => {
                apply_kernel_input(
                    client,
                    KernelInput::BootstrapChunk {
                        terminal_id: &terminal_id,
                        stream_id,
                        bootstrap_id,
                        chunk_seq,
                        payload: payload.as_ref(),
                    },
                )?;
            }
            FrameKind::BootstrapReady {
                terminal_id,
                stream_id,
                bootstrap_id,
                history_cursor,
            } => {
                apply_kernel_input(
                    client,
                    KernelInput::BootstrapReady {
                        terminal_id: &terminal_id,
                        stream_id,
                        bootstrap_id,
                        history_cursor: history_cursor.as_deref(),
                    },
                )?;
                client.invalidate_terminal_handles(&terminal_id);
                client.bump_document_revision(&terminal_id)?;
            }
            FrameKind::HistoryPage {
                terminal_id,
                stream_id,
                bootstrap_id,
                page_seq,
                cursor,
                next_cursor,
                payload,
                rows,
            } => {
                let before = client.session.history_cache(&terminal_id).map(|cache| {
                    let status = cache.status();
                    (
                        status.loaded_pages,
                        status.loaded_bytes,
                        status.materialized_rows,
                    )
                });
                apply_kernel_input(
                    client,
                    KernelInput::HistoryPage {
                        terminal_id: &terminal_id,
                        stream_id,
                        bootstrap_id,
                        page_seq,
                        rows,
                        payload: payload.as_ref(),
                        cursor: cursor.as_ref(),
                        next_cursor: next_cursor.as_deref(),
                    },
                )?;
                let after = client.session.history_cache(&terminal_id).map(|cache| {
                    let status = cache.status();
                    (
                        status.loaded_pages,
                        status.loaded_bytes,
                        status.materialized_rows,
                    )
                });
                if before != after {
                    client.bump_document_revision(&terminal_id)?;
                }
            }
            FrameKind::HistoryTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
            } => {
                apply_kernel_input(
                    client,
                    KernelInput::HistoryTombstone {
                        terminal_id: &terminal_id,
                        stream_id,
                        bootstrap_id,
                        cursor: cursor.as_ref(),
                        reason: history_unavailable_reason(reason)?,
                    },
                )?;
            }
            FrameKind::HistoryRejected {
                terminal_id,
                stream_id,
                bootstrap_id,
                cursor,
                reason,
                required_bytes,
                required_rows,
            } => {
                apply_kernel_input(
                    client,
                    KernelInput::HistoryRejected {
                        terminal_id: &terminal_id,
                        stream_id,
                        bootstrap_id,
                        cursor: cursor.as_ref(),
                        reason: history_rejection_reason(reason)?,
                        required_bytes,
                        required_rows,
                    },
                )?;
            }
            FrameKind::TerminalOutput {
                terminal_id,
                stream_id,
                bootstrap_id,
                seq,
                bytes,
            } => {
                let before = client
                    .session
                    .published(&terminal_id)
                    .map(|published| published.last_seq());
                apply_kernel_input(
                    client,
                    KernelInput::TerminalOutput {
                        terminal_id: &terminal_id,
                        stream_id,
                        bootstrap_id,
                        seq,
                        payload: bytes.as_ref(),
                    },
                )?;
                let after = client
                    .session
                    .published(&terminal_id)
                    .map(|published| published.last_seq());
                if before != after {
                    client.bump_document_revision(&terminal_id)?;
                }
            }
            FrameKind::BootstrapTombstone {
                terminal_id,
                stream_id,
                bootstrap_id,
                reason,
                last_valid_seq,
            } => {
                apply_kernel_input(
                    client,
                    KernelInput::Tombstone {
                        terminal_id: &terminal_id,
                        stream_id,
                        bootstrap_id,
                        reason,
                        last_valid_seq,
                    },
                )?;
                client.render.remove(&terminal_id);
                client.document_revisions.remove(&terminal_id);
                client.invalidate_terminal_handles(&terminal_id);
            }
            FrameKind::TerminalClosed { terminal_id, .. } => {
                apply_kernel_input(
                    client,
                    KernelInput::TerminalClosed {
                        terminal_id: &terminal_id,
                    },
                )?;
                client.render.remove(&terminal_id);
                client.document_revisions.remove(&terminal_id);
                client.invalidate_terminal_handles(&terminal_id);
            }
            FrameKind::Bell { terminal_id } => {
                client
                    .owned_effects
                    .push(OwnedEffect::simple(2, 1, terminal_id));
                client.rebuild_effect_views();
            }
            FrameKind::Error { code, message, .. } => {
                let mut effect = OwnedEffect::simple(2, 4, phux_protocol::TerminalId::local(0));
                effect.bytes = format!("{code:?}: {message}").into_bytes();
                client.owned_effects.push(effect);
                client.rebuild_effect_views();
            }
            FrameKind::Detached => {
                client.detached = true;
                client.expected_attach_id = None;
                client.owned_effects.push(OwnedEffect::simple(
                    2,
                    5,
                    phux_protocol::TerminalId::local(0),
                ));
                client.rebuild_effect_views();
            }
            _ => {
                return Err(BridgeError::protocol(
                    "server sent a frame not accepted by the client session kernel",
                ));
            }
        }
        Ok(())
    });
    if result == PhuxClientResult::Ok && notify_attached {
        invoke_attached(client)
    } else {
        result
    }
}

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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_outgoing_clear(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.outgoing.clear();
        Ok(())
    })
}

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

#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_effect_clear(client: *mut PhuxClient) -> PhuxClientResult {
    with_client_mut(client, |client| {
        client.owned_effects.clear();
        client.rebuild_effect_views();
        Ok(())
    })
}

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
        let text = if event.has_text {
            let bytes = unsafe { bytes_in(event.text.data, event.text.len) }?;
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
        let unshifted_codepoint = if event.has_unshifted_codepoint {
            char::from_u32(event.unshifted_codepoint).ok_or_else(|| {
                BridgeError::invalid("unshifted codepoint is not a Unicode scalar")
            })?;
            Some(event.unshifted_codepoint)
        } else {
            if event.unshifted_codepoint != 0 {
                return Err(BridgeError::invalid(
                    "unshifted codepoint is present without its discriminator",
                ));
            }
            None
        };
        let event = InputEvent::Key(KeyEvent {
            action: KeyAction::try_from(event.action)
                .map_err(|_| BridgeError::invalid("unknown key action"))?,
            key: PhysicalKey::try_from(event.key)
                .map_err(|_| BridgeError::invalid("unknown physical key"))?,
            mods,
            consumed_mods,
            composing: event.composing,
            text,
            unshifted_codepoint,
        });
        apply_input(client, &terminal_id, &event)
    })
}

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
        let data = unsafe { bytes_in(data, len) }?;
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
        client.queue_frame(FrameKind::TerminalResize {
            terminal_id,
            cols,
            rows,
        });
        Ok(())
    })
}

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
        client.queue_frame(FrameKind::ViewportResize {
            viewport: ViewportInfo::new(cols, rows)
                .with_pixels(pixels.map(|value| value.0), pixels.map(|value| value.1)),
        });
        Ok(())
    })
}

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

#[cfg(test)]
mod tests {
    use super::*;
    use phux_protocol::caps::ServerCapabilities;
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
            unsafe { phux_client_outgoing_get(client, 0, &mut frame) },
            PhuxClientResult::NoValue
        );
        assert!(frame.data.is_null());
        assert_eq!(frame.len, 0);
        unsafe { phux_client_free(client) };
    }
}
