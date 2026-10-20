//! `HELLO` probes: the version and feature words a server advertised.

use phux_protocol::caps::ClientCapabilities;
use phux_protocol::wire::frame::FrameKind;

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// The protocol version triple the server selected in `HELLO_OK`. A
/// negotiated connection answers without a second `HELLO`; only a raw
/// transport performs the exchange here.
///
/// # Errors
///
/// Transport failure, a refusal (version incompatibility), or an unexpected
/// reply.
pub async fn probe_hello(conn: &mut Connection) -> Result<(u16, u16, u16), AttachError> {
    if conn.negotiated_bootstrap().is_some() {
        return Ok((
            phux_protocol::PROTOCOL_VERSION.major,
            phux_protocol::PROTOCOL_VERSION.minor,
            phux_protocol::PROTOCOL_VERSION.patch,
        ));
    }
    match raw_hello(conn).await? {
        FrameKind::HelloOk {
            protocol_major,
            protocol_minor,
            protocol_patch,
            ..
        } => Ok((protocol_major, protocol_minor, protocol_patch)),
        _ => unreachable!("raw_hello returns only HELLO_OK"),
    }
}

/// The `ServerFeatureSet` the server advertised, so a caller can observe a
/// feature bit before sending. A negotiated connection answers from its
/// kept `HELLO_OK`.
///
/// # Errors
///
/// As [`probe_hello`].
pub async fn probe_hello_features(
    conn: &mut Connection,
) -> Result<Option<phux_protocol::caps::ServerFeatureSet>, AttachError> {
    if let Some(negotiated) = conn.negotiated_bootstrap() {
        return Ok(Some(negotiated.server_features));
    }
    match raw_hello(conn).await? {
        FrameKind::HelloOk { server_caps, .. } => Ok(Some(server_caps.features)),
        _ => unreachable!("raw_hello returns only HELLO_OK"),
    }
}

/// The `features_ext` word the server advertised, like
/// [`probe_hello_features`]. A raw connection sends its one `HELLO` here, so
/// call one probe or the other, not both.
///
/// # Errors
///
/// As [`probe_hello`].
pub async fn probe_hello_features_ext(
    conn: &mut Connection,
) -> Result<phux_protocol::caps::ServerFeatureExtSet, AttachError> {
    if let Some(negotiated) = conn.negotiated_bootstrap() {
        return Ok(negotiated.server_features_ext);
    }
    match raw_hello(conn).await? {
        FrameKind::HelloOk { server_caps, .. } => Ok(server_caps.features_ext),
        _ => unreachable!("raw_hello returns only HELLO_OK"),
    }
}

/// Send `HELLO` on a raw connection; `Ok` only for a `HELLO_OK`.
async fn raw_hello(conn: &mut Connection) -> Result<FrameKind, AttachError> {
    conn.send(&FrameKind::Hello {
        client_name: format!("phux-cli/{}", env!("CARGO_PKG_VERSION")),
        protocol_major: phux_protocol::PROTOCOL_VERSION.major,
        protocol_minor: phux_protocol::PROTOCOL_VERSION.minor,
        protocol_patch: phux_protocol::PROTOCOL_VERSION.patch,
        client_caps: ClientCapabilities::default(),
    })
    .await?;
    match conn.recv().await? {
        ok @ FrameKind::HelloOk { .. } => Ok(ok),
        FrameKind::Error { message, .. } => Err(AttachError::Refused(message)),
        // Version skew, not a frame worth dumping.
        _ => Err(AttachError::Protocol(crate::explain::unexpected_reply(
            "HELLO",
        ))),
    }
}
