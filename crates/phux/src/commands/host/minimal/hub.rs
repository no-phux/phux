//! The address a box dials: this server's live QUIC listener, which a box
//! reaches as `host.min.internal`, the name Minimal gives this machine's
//! loopback inside a box (its NET-003).

use std::net::SocketAddr;
use std::path::Path;

use phux_protocol::wire::RemoteListenerTransport;

use super::super::super::json_err::{CliError, codes};

const HOST_NAME: &str = "host.min.internal";

/// `--hub`, else `host.min.internal` on the port of this server's live QUIC
/// listener, which must accept on loopback because that is where a box's
/// traffic to `host.min.internal` arrives.
pub(super) fn endpoint(explicit: Option<&str>) -> Result<String, CliError> {
    if let Some(hub) = explicit {
        return Ok(hub.to_owned());
    }
    let socket = phux_server::runtime::default_socket_path();
    let bind = live_quic_bind(&socket)
        .map_err(no_listener)?
        .ok_or_else(|| no_listener("this server has no QUIC listener".to_owned()))?;
    if !(bind.ip().is_unspecified() || bind.ip().is_loopback()) {
        return Err(no_listener(format!(
            "this server's QUIC listener is bound to {bind} only, and a box reaches this \
             machine on loopback; bind 0.0.0.0 or 127.0.0.1"
        )));
    }
    Ok(format!("{HOST_NAME}:{}", bind.port()))
}

/// The address the server on `socket` has its QUIC listener bound to.
fn live_quic_bind(socket: &Path) -> Result<Option<SocketAddr>, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("could not build a runtime to ask the server: {err}"))?;
    let view = runtime
        .block_on(phux_client::state::get_state(socket))
        .map_err(|err| format!("could not ask the server at {}: {err}", socket.display()))?;
    let snapshot = view.snapshot();
    Ok(snapshot.listeners().and_then(|report| {
        report
            .listeners
            .iter()
            .find(|slot| slot.transport == RemoteListenerTransport::Quic && slot.bound)
            .and_then(|slot| slot.addr.as_deref()?.parse().ok())
    }))
}

fn no_listener(detail: String) -> CliError {
    CliError::new(
        codes::NO_SERVER,
        detail,
        "run the hub with a QUIC listener on loopback (`phux server --hub --quic \
         127.0.0.1:8788`, or `0.0.0.0:8788`), or pass --hub HOST:PORT",
    )
}
