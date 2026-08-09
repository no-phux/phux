//! QUIC dialer shim over the shared [`phux_dial`] establishment stack.
//!
//! The QUIC-specific establishment (rustls client config with the phux ALPN,
//! fingerprint-pin or loopback-skip certificate verification, the bearer-token
//! stream preamble) moved to `phux-dial` with phux-v45.3 so the federation
//! hub's outbound dialer (`phux-server::hub`) reuses the identical tested
//! stack. This module keeps the established `crate::attach::quic` paths
//! resolving and maps [`phux_dial::DialError`] into [`AttachError`] at the
//! attach-loop boundary. Framing stays in [`super::connection`].

pub use phux_dial::CertTrust;
pub use phux_dial::quic::QuicDial;

use super::outcome::AttachError;

/// Decode a `phux pair` pairing token (hex) into the raw bytes the QUIC auth
/// preamble carries.
///
/// # Errors
///
/// Returns [`AttachError::Connect`] when the token is not valid hex.
pub fn parse_token_hex(token: &str) -> Result<Vec<u8>, AttachError> {
    phux_dial::quic::parse_token_hex(token).map_err(AttachError::from)
}

/// Connect to the QUIC listener and return the established bidi-stream halves,
/// the auth preamble already written. See [`phux_dial::quic::dial`].
pub(super) async fn dial(
    d: &QuicDial,
) -> Result<
    (
        quinn::Endpoint,
        quinn::Connection,
        quinn::SendStream,
        quinn::RecvStream,
    ),
    AttachError,
> {
    phux_dial::quic::dial(d).await.map_err(AttachError::from)
}
