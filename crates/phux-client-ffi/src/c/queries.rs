//! Borrowed reads: sessions, resources, queued frames and effects, and grids.

use std::mem;

use super::{
    BridgeError, PhuxBytes, PhuxClient, PhuxClientEffect, PhuxClientResult, PhuxResourceId,
    PhuxResourceInfo, PhuxSessionInfo, PhuxTerminalGridView, borrowed_count, borrowed_get,
    bytes_out, check_struct, terminal_id_in, terminal_id_out, with_client_mut, with_client_ref,
};

/// Returns the number of sessions advertised by the latest accepted ATTACHED.
/// Zero means either no catalog has arrived or the client pointer is invalid.
///
/// # Safety
///
/// `client`, when non-null, must remain valid and unmodified for the call and
/// must be accessed only from its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_session_count(client: *const PhuxClient) -> usize {
    borrowed_count(client, |client| client.sessions.len())
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
    borrowed_get(
        client,
        out_session,
        |_| Ok(()),
        |client| {
            let session = client.sessions.get(index)?;
            Some(PhuxSessionInfo {
                session_id: session.session_id,
                name: bytes_out(&session.name),
                created_at_unix_secs: session.created_at_unix_secs,
                window_count: session.window_count,
                attached_client_count: session.attached_client_count,
                focused: session.focused,
            })
        },
    )
}

/// Returns the number of resources in the latest accepted inventory.
///
/// An ATTACHED or arrangement `GET_STATE` snapshot replaces membership; a resource
/// closure removes an entry. Zero means either no
/// snapshot has arrived or the client pointer is invalid.
///
/// # Safety
///
/// `client`, when non-null, must remain valid and unmodified for the call and
/// must be accessed only from its owning thread.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_resource_count(client: *const PhuxClient) -> usize {
    borrowed_count(client, |client| client.resources.len())
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
    let check = |out: &PhuxResourceInfo| {
        check_struct(out.size, mem::size_of::<PhuxResourceInfo>(), out.version)
            .map_err(|error| error.result)
    };
    borrowed_get(client, out_resource, check, |client| {
        let resource = client.resources.get(index)?;
        Some(PhuxResourceInfo {
            terminal_id: terminal_id_out(&resource.id),
            kind: resource.kind,
            parent: resource.parent_ptr(),
            provider: bytes_out(&resource.provider),
            native_id: bytes_out(&resource.native_id),
            state: bytes_out(&resource.state),
            session_id: resource.session_id,
            cols: resource.cols,
            rows: resource.rows,
            key: resource.key,
            ..PhuxResourceInfo::default()
        })
    })
}

/// Returns the number of queued outgoing frames.
///
/// # Safety
///
/// When non-null, `client` must point to a live client on its owning thread and
/// remain valid and unmodified for the duration of the call.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_outgoing_count(client: *const PhuxClient) -> usize {
    borrowed_count(client, |client| client.outgoing.len())
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
    borrowed_get(
        client,
        out_frame,
        |_| Ok(()),
        |client| client.outgoing.get(index).map(|frame| bytes_out(frame)),
    )
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
    borrowed_count(client, |client| client.effect_count)
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
    borrowed_get(
        client,
        out_effect,
        |_| Ok(()),
        |client| client.effect_view(index),
    )
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
        client.accept_agent_publications();
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

/// Reports whether the terminal's published replica has DEC 2004 bracketed
/// paste on, the mode the server encodes a paste against.
///
/// # Safety
///
/// When non-null, `client` must point to a live client on its owning thread and
/// remain valid and unmodified for the call. When non-null, `terminal_id` and
/// any non-empty satellite host span must be readable, and `out_enabled` must
/// be valid writable storage.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phux_client_terminal_bracketed_paste(
    client: *const PhuxClient,
    terminal_id: *const PhuxResourceId,
    out_enabled: *mut bool,
) -> PhuxClientResult {
    with_client_ref(client, |client| {
        let out = unsafe { out_enabled.as_mut() }
            .ok_or_else(|| BridgeError::invalid("out_enabled is null"))?;
        client.ensure_attached()?;
        let terminal_id = unsafe { terminal_id_in(terminal_id) }?;
        *out = client.bracketed_paste(&terminal_id)?;
        Ok(())
    })
}
