//! WebSocket dialer shim over the shared [`phux_dial`] establishment stack.
//!
//! The WebSocket-specific establishment (TCP connect, optional TLS with the
//! shared trust policy, RFC 6455 upgrade with the `Authorization: Bearer`
//! pairing token) moved to `phux-dial` with phux-v45.3 so the federation
//! hub's outbound dialer (`phux-server::hub`) reuses the identical tested
//! stack. This module keeps the established `crate::attach::ws` paths
//! resolving and maps [`phux_dial::DialError`] into [`AttachError`] at the
//! attach-loop boundary. Framing stays in [`super::connection`].

pub use phux_dial::ws::{Ws, WsDial, WsReader, WsTarget, WsWriter};

use super::outcome::AttachError;

/// Connect to the WebSocket listener; see [`phux_dial::ws::dial`].
pub(super) async fn dial(d: &WsDial) -> Result<Ws, AttachError> {
    phux_dial::ws::dial(d).await.map_err(AttachError::from)
}
