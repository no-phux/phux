//! Link-only host fixture: proves NAPI constructors in the FFI rlib register
//! into the host's runtime, and its native accessor sees that exact Client.
//! The desktop wrapper additionally links GPUIX and calls `napi_build::setup`.

use napi_derive::napi;
use phux_client_ffi::napi::{DesktopStatus, initialize};
use phux_client_ffi::projection::status;

#[napi]
#[allow(
    clippy::needless_pass_by_value,
    reason = "NAPI requires owned JS strings"
)]
pub fn native_client_status(handle: String) -> napi::Result<DesktopStatus> {
    let client = initialize()
        .client(&handle)
        .map_err(|error| napi::Error::from_reason(error.to_string()))?;
    Ok(status::connection(Some(client.status())).into())
}

/// A native painter's held lease, retained across a real transport restart.
#[napi]
#[derive(Debug)]
pub struct NativeClientLease {
    client: phux_client_runtime::Client,
}

#[napi]
impl NativeClientLease {
    #[napi(constructor)]
    #[allow(
        clippy::needless_pass_by_value,
        reason = "NAPI requires owned JS strings"
    )]
    pub fn new(handle: String) -> napi::Result<Self> {
        let client = initialize()
            .client(&handle)
            .map_err(|error| napi::Error::from_reason(error.to_string()))?;
        Ok(Self { client })
    }

    #[napi]
    #[must_use]
    pub fn connection_epoch(&self) -> String {
        self.client.connection_epoch().to_string()
    }

    #[napi]
    #[must_use]
    pub fn status(&self) -> DesktopStatus {
        status::connection(Some(self.client.status())).into()
    }
}
