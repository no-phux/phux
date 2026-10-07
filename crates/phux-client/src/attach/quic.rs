//! QUIC dialing via [`phux_dial`], with its errors mapped into [`AttachError`].

pub use phux_dial::CertTrust;
pub use phux_dial::quic::QuicDial;

use super::outcome::AttachError;

/// Decode a `phux pair` pairing token (hex) into its raw bytes.
///
/// # Errors
///
/// Returns [`AttachError::Connect`] when the token is not valid hex.
pub fn parse_token_hex(token: &str) -> Result<Vec<u8>, AttachError> {
    phux_dial::quic::parse_token_hex(token).map_err(AttachError::from)
}
