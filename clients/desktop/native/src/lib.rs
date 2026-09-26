//! The single desktop addon: GPUIX and application exports share NAPI's registry.
//!
//! The optional phux-client-ffi encoder owns the only Client registry. Native
//! consumers resolve the same handle without taking its listener or event queue.

pub mod input;
#[cfg_attr(
    test,
    allow(
        dead_code,
        reason = "NAPI omits export registration in Rust unit tests"
    )
)]
mod probe;
pub mod terminal;

use gpuix_native::native_extensions;
use napi_derive::napi;

/// Explicit startup boundary; call before creating any GPUIX view/registry.
/// The probe verifies extension infrastructure, not terminal rendering.
#[napi]
pub fn initialize_desktop_host() -> napi::Result<()> {
    let _ = phux_client_ffi::napi::initialize();
    native_extensions::install(|registry| {
        probe::install(registry);
        terminal::install(registry);
    })
    .map_err(napi::Error::from_reason)
}

/// Resolve through the linked encoder, exactly as the terminal painter does.
/// This diagnostic never consumes events or replaces the client's listener.
#[napi]
pub fn native_client_status(handle: String) -> napi::Result<phux_client_ffi::napi::DesktopStatus> {
    let client = phux_client_ffi::napi::initialize()
        .client(&handle)
        .map_err(|error| napi::Error::from_reason(error.to_string()))?;
    Ok(phux_client_ffi::projection::status::connection(Some(client.status())).into())
}
