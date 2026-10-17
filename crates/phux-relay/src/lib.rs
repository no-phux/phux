//! phux's reference relay (ADR-0051, ADR-0052, ADR-0057): `quictun`'s relay
//! bound to phux's protocol and state directory.
//!
//! A phux server behind NAT dials OUT under the `phux-relay/1` ALPN and
//! registers a tunnel for a named route; consumers dial IN under
//! `phux-quic/1`, naming the route via TLS SNI. The relay splices each
//! admitted consumer onto a fresh stream over the route's tunnel, and every
//! consumer runs TLS end to end with the server inside it (ADR-0180).
//!
//! The relay itself is [`quictun`], which depends on no phux crate
//! (ADR-0181). This crate supplies only what makes it phux's: [`protocol`]
//! (the two ALPNs and the `phxT` connection-ID tag) and the files under
//! phux's state directory. The `phux relay` verb fronts it.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

use phux_protocol::policy::{QUIC_ALPN, QUIC_RELAY_ALPN};
pub use quictun::relay::{
    BoundRelay, DEFAULT_HANDSHAKES_PER_SOURCE, DEFAULT_MAX_CONNS, RelayConfig, RelayError,
    RelayRuntime, RouteTokenStore, cert_fingerprint, ensure_self_signed, mint_route_token,
    validate_route_name,
};
pub use quictun::tunnel::{
    AUTH_FAILED_CODE, OVER_CAP_CODE, PROTOCOL_VIOLATION_CODE, RECLAIMED_CODE, ROUTE_OFFLINE_CODE,
};

/// phux's relay protocol: connectors offer [`QUIC_RELAY_ALPN`], consumers
/// [`QUIC_ALPN`], and tunnels tag their connection ID with
/// [`phux_dial::quic::TUNNEL_CID_TAG`].
#[must_use]
pub fn protocol() -> quictun::Protocol {
    quictun::Protocol::new(QUIC_RELAY_ALPN, QUIC_ALPN, phux_dial::quic::TUNNEL_CID_TAG)
}

/// A phux relay config listening on `listen` with its files in `dir` and
/// the default connection cap.
#[must_use]
pub fn config_in(listen: SocketAddr, dir: &Path) -> RelayConfig {
    RelayConfig::in_dir(listen, dir, protocol())
}

/// A phux relay config listening on `listen` with its files in phux's state
/// directory and the default connection cap.
#[must_use]
pub fn config(listen: SocketAddr) -> RelayConfig {
    config_in(listen, &state_dir())
}

/// phux's per-user state directory: `$XDG_STATE_HOME/phux`, else
/// `$HOME/.local/state/phux`. Profile-free on purpose, unlike
/// `phux_config::instance::state_dir`: the relay is not a profiled instance
/// and must not depend on the daemon (ADR-0051).
fn state_dir() -> PathBuf {
    quictun::relay::state_dir("phux")
}

/// Default relay certificate path: `<state-dir>/relay-cert.pem`.
#[must_use]
pub fn default_relay_cert_path() -> PathBuf {
    state_dir().join(quictun::relay::CERT_FILE)
}

/// Default relay private-key path: `<state-dir>/relay-key.pem`.
#[must_use]
pub fn default_relay_key_path() -> PathBuf {
    state_dir().join(quictun::relay::KEY_FILE)
}

/// Default route-token store path: `<state-dir>/relay-tokens`.
#[must_use]
pub fn default_relay_tokens_path() -> PathBuf {
    state_dir().join(quictun::relay::TOKENS_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The relay serves phux's wire exactly: the ALPNs the server and the
    /// dialer use, and the tag the dialer puts on a tunnel.
    #[test]
    fn the_protocol_is_phux_wire() {
        let protocol = protocol();
        assert!(protocol.validate().is_ok());
        assert_eq!(protocol.tunnel_alpn, b"phux-relay/1");
        assert_eq!(protocol.consumer_alpns, vec![b"phux-quic/1".to_vec()]);
        assert_eq!(protocol.cid_tag.0, *b"phxT");
        let config = config_in("127.0.0.1:0".parse().unwrap(), Path::new("/state"));
        assert_eq!(config.tokens_path, Path::new("/state/relay-tokens"));
        assert_eq!(config.cert_path, Path::new("/state/relay-cert.pem"));
        assert_eq!(config.key_path, Path::new("/state/relay-key.pem"));
    }
}
