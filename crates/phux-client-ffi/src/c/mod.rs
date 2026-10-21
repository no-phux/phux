//! The C ABI encoder: today's `extern "C"` surface over [`crate::projection`].
//!
//! This lane owns `#[repr(C)]` types, borrowed views, pointer lifetimes and
//! the callback contract. Every product decision it encodes — which runtime
//! event is a terminal signal, what a lifecycle answer carries, how a grid
//! frame reads — comes from the shared projection layer, so the `UniFFI` lane
//! in `crate::uniffi` cannot disagree with it (ADR-0135).

#![allow(
    clippy::redundant_pub_crate,
    reason = "this private module is the crate root of the C lane; its `pub(crate)` helpers are shared with its own submodules"
)]

mod arrangement;
mod client;
mod connect;
mod directory;
mod draw_list;
mod error;
mod feed;
mod grid_metadata;
mod inbound;
mod input;
mod lifecycle;
mod log;
mod operations;
mod organization;
mod path;
mod pointer;
mod projection;
mod queries;
mod remote;
mod session_create;
mod session_query;
mod session_rename;
#[cfg(test)]
mod test_support;
mod types;
mod viewport;

#[cfg(test)]
use std::mem;
use std::panic::{AssertUnwindSafe, catch_unwind};
#[cfg(test)]
use std::ptr;

use client::{Client, Limits};
use error::{BridgeError, bytes_in, check_struct, outbound_bytes_in, terminal_id_in};
pub(crate) use inbound::poll_connected;
#[cfg(test)]
use phux_protocol::PROTOCOL_VERSION;
use phux_protocol::ResourceKind;
#[cfg(test)]
use phux_protocol::SessionId;
#[cfg(test)]
use phux_protocol::caps::BootstrapLimits;
use phux_protocol::input::InputEvent;
#[cfg(test)]
use phux_protocol::wire::frame::{AttachTarget, FrameKind, ViewportInfo};

pub use arrangement::*;
pub use connect::*;
pub use directory::*;
pub use draw_list::*;
pub use feed::*;
pub use grid_metadata::*;
pub use input::*;
pub use lifecycle::*;
pub use log::*;
pub use operations::*;
pub use organization::*;
pub use path::*;
pub use pointer::{
    PhuxSelectionGestureEvent, PhuxSelectionGestureResult, phux_client_selection_gesture,
    phux_client_terminal_mouse_mode,
};
pub use projection::*;
pub use queries::*;
pub use remote::registry::*;
pub use remote::*;
pub use session_create::*;
pub use session_query::*;
pub use session_rename::*;
pub use types::*;
pub use viewport::*;

#[repr(C)]
pub struct PhuxClient {
    inner: Client,
    _not_send_sync: std::marker::PhantomData<*mut ()>,
}

impl PhuxClient {
    pub(crate) const fn new(inner: Client) -> Self {
        Self {
            inner,
            _not_send_sync: std::marker::PhantomData,
        }
    }
}

impl std::fmt::Debug for PhuxClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PhuxClient")
            .field("state", &self.inner.state())
            .finish_non_exhaustive()
    }
}

pub(crate) fn with_client_mut(
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

/// A borrowed collection's length; zero for a null client or inside a callback.
fn borrowed_count(client: *const PhuxClient, len: impl FnOnce(&Client) -> usize) -> usize {
    unsafe { client.as_ref() }
        .filter(|client| !client.inner.in_callback)
        .map_or(0, |client| len(&client.inner))
}

/// Write one borrowed item into `out`: `check` validates the caller's struct
/// header, then `out` is reset (so a missing item reads as default) and filled.
fn borrowed_get<T: Default>(
    client: *const PhuxClient,
    out: *mut T,
    check: impl FnOnce(&T) -> Result<(), PhuxClientResult>,
    item: impl FnOnce(&Client) -> Option<T>,
) -> PhuxClientResult {
    match catch_unwind(AssertUnwindSafe(|| -> Result<(), PhuxClientResult> {
        let client = unsafe { client.as_ref() }.ok_or(PhuxClientResult::InvalidArgument)?;
        if client.inner.in_callback {
            return Err(PhuxClientResult::InvalidState);
        }
        let out = unsafe { out.as_mut() }.ok_or(PhuxClientResult::InvalidArgument)?;
        check(out)?;
        *out = T::default();
        *out = item(&client.inner).ok_or(PhuxClientResult::NoValue)?;
        Ok(())
    })) {
        Ok(Ok(())) => PhuxClientResult::Ok,
        Ok(Err(error)) => error,
        Err(_) => PhuxClientResult::Panic,
    }
}

fn invoke_failure(client: *mut PhuxClient, result: PhuxClientResult) -> PhuxClientResult {
    let (callback, userdata, message) = {
        let client_ref = unsafe { &*client };
        let Some(callback) = client_ref.inner.callbacks.on_failure else {
            return result;
        };
        let message = bytes_out(&client_ref.inner.last_error);
        (callback, client_ref.inner.callbacks.userdata, message)
    };
    let call = || unsafe { callback(userdata, result, message) };
    guarded_callback(
        client,
        call,
        "panic contained in phux-client failure callback",
    )
    .unwrap_or(result)
}

pub(crate) fn invoke_attached(client: *mut PhuxClient) -> PhuxClientResult {
    let (callback, userdata) = {
        let client_ref = unsafe { &mut *client };
        if client_ref.inner.attached_notified
            || !client_ref.inner.attached
            || client_ref.inner.state() != PhuxClientState::Attached
        {
            return PhuxClientResult::Ok;
        }
        client_ref.inner.attached_notified = true;
        let Some(callback) = client_ref.inner.callbacks.on_attached else {
            return PhuxClientResult::Ok;
        };
        (callback, client_ref.inner.callbacks.userdata)
    };
    let call = || unsafe { callback(userdata) };
    guarded_callback(
        client,
        call,
        "panic contained in phux-client attached callback",
    )
    .unwrap_or(PhuxClientResult::Ok)
}

/// Run an embedder callback with the reentry guard raised. A contained panic
/// records `panic_message` and yields `Some(Panic)`; success yields `None`.
fn guarded_callback(
    client: *mut PhuxClient,
    call: impl FnOnce(),
    panic_message: &str,
) -> Option<PhuxClientResult> {
    unsafe { (*client).inner.in_callback = true };
    let outcome = catch_unwind(AssertUnwindSafe(call));
    let client_ref = unsafe { &mut *client };
    client_ref.inner.in_callback = false;
    outcome.is_err().then(|| {
        client_ref.inner.set_error(panic_message);
        PhuxClientResult::Panic
    })
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
        InputEvent::Key(event) => client.control().send_key(terminal_id, event.clone()),
        InputEvent::Mouse(event) => client.control().send_mouse(terminal_id, *event),
        InputEvent::Focus(event) => client.control().send_focus(terminal_id, *event),
        InputEvent::Paste(event) => {
            client
                .control()
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

#[cfg(test)]
mod tests;
