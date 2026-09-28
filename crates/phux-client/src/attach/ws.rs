//! WebSocket dialing via [`phux_dial`], with its errors mapped into
//! [`AttachError`].

pub use phux_dial::ws::{Ws, WsDial, WsReader, WsTarget, WsWriter, recv_message_alive};

use super::outcome::AttachError;

/// Connect to the WebSocket listener; see [`phux_dial::ws::dial`].
pub(super) async fn dial(d: &WsDial) -> Result<Ws, AttachError> {
    phux_dial::ws::dial(d).await.map_err(AttachError::from)
}
