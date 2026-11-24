//! Public endpoint selection from the running server's actual listeners.

use std::net::SocketAddr;
use std::path::Path;

use super::{bound_or_refusal, query_live_listeners, resolve_quic_endpoint};

/// The `HOST:PORT` another machine dials this server's live QUIC listener at,
/// resolved as a pairing link's `quic` is (overlay address, `MagicDNS`
/// name). `Ok(None)` when no QUIC listener is bound or it binds loopback.
pub(crate) fn advertised_quic_endpoint(socket: &Path) -> Result<Option<String>, String> {
    let live = bound_or_refusal(socket, query_live_listeners(socket))?;
    let overlay = phux_config::overlay::detect();
    let magic_dns = phux_config::overlay::detect_magic_dns();
    Ok(
        resolve_quic_endpoint(&overlay, magic_dns.as_ref(), live.quic_addr())
            .map(|endpoint| endpoint.trim_start_matches("quic://").to_owned()),
    )
}

/// A conversation needs a live listener, not merely a configured address.
pub(crate) fn bound_quic_listener(socket: &Path) -> Result<Option<SocketAddr>, String> {
    bound_or_refusal(socket, query_live_listeners(socket)).map(|live| live.quic_addr())
}
