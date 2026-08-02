use phux_protocol::{SatelliteHost, TerminalId};

use crate::types::{ABI_VERSION, PhuxClientResult, PhuxTerminalId};

#[derive(Debug)]
pub struct BridgeError {
    pub result: PhuxClientResult,
    pub message: String,
}

impl BridgeError {
    pub fn invalid(message: impl Into<String>) -> Self {
        Self { result: PhuxClientResult::InvalidArgument, message: message.into() }
    }

    pub fn state(message: impl Into<String>) -> Self {
        Self { result: PhuxClientResult::InvalidState, message: message.into() }
    }

    pub fn protocol(message: impl Into<String>) -> Self {
        Self { result: PhuxClientResult::ProtocolError, message: message.into() }
    }

    pub fn engine(message: impl Into<String>) -> Self {
        Self { result: PhuxClientResult::EngineError, message: message.into() }
    }

    pub fn ghostty(error: libghostty_vt::Error) -> Self {
        let result = if matches!(error, libghostty_vt::Error::OutOfMemory) {
            PhuxClientResult::OutOfMemory
        } else {
            PhuxClientResult::EngineError
        };
        Self { result, message: error.to_string() }
    }
}

pub fn check_struct(actual: usize, required: usize, version: u32) -> Result<(), BridgeError> {
    if version != ABI_VERSION {
        return Err(BridgeError::invalid("unsupported FFI struct version"));
    }
    if actual < required {
        return Err(BridgeError::invalid("FFI struct is smaller than required"));
    }
    Ok(())
}

pub unsafe fn bytes_in<'a>(data: *const u8, len: usize) -> Result<&'a [u8], BridgeError> {
    if len == 0 {
        return Ok(&[]);
    }
    if data.is_null() {
        return Err(BridgeError::invalid("non-empty byte span has a null pointer"));
    }
    // SAFETY: caller promises the non-null span is readable for `len` bytes.
    Ok(unsafe { std::slice::from_raw_parts(data, len) })
}

pub unsafe fn terminal_id_in(value: *const PhuxTerminalId) -> Result<TerminalId, BridgeError> {
    // SAFETY: pointer validity is checked before dereference.
    let value = unsafe { value.as_ref() }
        .ok_or_else(|| BridgeError::invalid("terminal_id is null"))?;
    match value.kind {
        0 => {
            if value.host.len != 0 {
                return Err(BridgeError::invalid("local terminal ID must not carry a host"));
            }
            Ok(TerminalId::local(value.id))
        }
        1 => {
            // SAFETY: span is validated by bytes_in.
            let host = unsafe { bytes_in(value.host.data, value.host.len) }?;
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
