use phux_protocol::{SatelliteHost, TerminalId};

use crate::types::{ABI_VERSION, PhuxClientResult, PhuxTerminalId};

#[allow(
    clippy::redundant_pub_crate,
    reason = "the private error module serves the crate-root C exports"
)]
pub(crate) const MAX_OUTBOUND_BYTES: usize =
    phux_protocol::wire::frame::MAX_INPUT_TERMINAL_REPLY_BYTES;

#[allow(
    clippy::redundant_pub_crate,
    reason = "the private error module serves the crate-root C exports"
)]
#[derive(Debug)]
pub(crate) struct BridgeError {
    pub result: PhuxClientResult,
    pub message: String,
}

impl BridgeError {
    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self {
            result: PhuxClientResult::InvalidArgument,
            message: message.into(),
        }
    }

    pub(crate) fn state(message: impl Into<String>) -> Self {
        Self {
            result: PhuxClientResult::InvalidState,
            message: message.into(),
        }
    }

    pub(crate) fn protocol(message: impl Into<String>) -> Self {
        Self {
            result: PhuxClientResult::ProtocolError,
            message: message.into(),
        }
    }

    pub(crate) fn engine(message: impl Into<String>) -> Self {
        Self {
            result: PhuxClientResult::EngineError,
            message: message.into(),
        }
    }

    pub(crate) fn ghostty(error: libghostty_vt::Error) -> Self {
        let result = if matches!(error, libghostty_vt::Error::OutOfMemory) {
            PhuxClientResult::OutOfMemory
        } else {
            PhuxClientResult::EngineError
        };
        Self {
            result,
            message: error.to_string(),
        }
    }
}

#[allow(
    clippy::redundant_pub_crate,
    reason = "the private error module serves the crate-root C exports"
)]
pub(crate) fn check_struct(
    actual: usize,
    required: usize,
    version: u32,
) -> Result<(), BridgeError> {
    if version != ABI_VERSION {
        return Err(BridgeError::invalid("unsupported FFI struct version"));
    }
    if actual < required {
        return Err(BridgeError::invalid("FFI struct is smaller than required"));
    }
    Ok(())
}

#[allow(
    clippy::redundant_pub_crate,
    reason = "the private error module serves the crate-root C exports"
)]
pub(crate) unsafe fn bytes_in<'a>(data: *const u8, len: usize) -> Result<&'a [u8], BridgeError> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err(BridgeError::invalid(
            "non-empty byte span has a null pointer",
        ));
    }
    // SAFETY: caller promises the non-null span is readable for `len` bytes.
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}

#[allow(
    clippy::redundant_pub_crate,
    reason = "the private error module serves the crate-root C exports"
)]
pub(crate) unsafe fn outbound_bytes_in<'a>(
    data: *const u8,
    len: usize,
    field: &'static str,
) -> Result<&'a [u8], BridgeError> {
    if len > MAX_OUTBOUND_BYTES {
        return Err(BridgeError::invalid(format!(
            "{field} exceeds the FFI outbound byte limit"
        )));
    }
    // SAFETY: forwards the caller's span contract after enforcing the outbound bound.
    unsafe { bytes_in(data, len) }
}

#[allow(
    clippy::redundant_pub_crate,
    reason = "the private error module serves the crate-root C exports"
)]
pub(crate) unsafe fn terminal_id_in(
    value: *const PhuxTerminalId,
) -> Result<TerminalId, BridgeError> {
    // SAFETY: pointer validity is checked before dereference.
    let value =
        unsafe { value.as_ref() }.ok_or_else(|| BridgeError::invalid("terminal_id is null"))?;
    match value.kind {
        0 => {
            if value.host.len != 0 {
                return Err(BridgeError::invalid(
                    "local terminal ID must not carry a host",
                ));
            }
            Ok(TerminalId::local(value.id))
        }
        1 => {
            // SAFETY: span is validated by bytes_in.
            let host =
                unsafe { outbound_bytes_in(value.host.data, value.host.len, "satellite host") }?;
            let host = std::str::from_utf8(host)
                .map_err(|_| BridgeError::invalid("satellite host is not UTF-8"))?;
            if host.is_empty() {
                return Err(BridgeError::invalid("satellite host is empty"));
            }
            Ok(TerminalId::satellite(SatelliteHost::new(host), value.id))
        }
        _ => Err(BridgeError::invalid("unknown terminal ID discriminant")),
    }
}
