//! Shared outbound dialer for phux remote transports.
//!
//! Both remote *consumers* (`phux attach --quic/--ws`, the phux-client
//! attach loop) and the federation *hub* (`phux server --hub`, which dials
//! its satellites per ADR-0007/ADR-0038) establish connections the same
//! way: TLS 1.3 with the server's self-signed leaf certificate pinned by
//! SHA-256 fingerprint (or a loopback dev skip), plus an ADR-0031 bearer
//! token — a length-prefixed stream preamble on QUIC, an
//! `Authorization: Bearer` header on WebSocket.
//!
//! This crate is that shared establishment layer, factored out of
//! `phux-client::attach` so `phux-server`'s hub dialer (phux-v45.3) reuses
//! the identical, tested stack instead of duplicating security-sensitive
//! code. It deliberately stops at the byte stream: SPEC §5 framing and the
//! attach lifecycle stay with the callers (`phux-client::attach::connection`
//! and the server's hub link supervisor), preserving ADR-0007's
//! transport-trait shape — transports differ only below the framed
//! reader/writer seam.
//!
//! No wire bytes are defined here; the crate moves opaque streams and the
//! auth preamble ADR-0031 already specifies.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

pub mod quic;
pub mod tls;
pub mod ws;

pub use quic::QuicDial;
pub use tls::CertTrust;
pub use ws::{WsDial, WsTarget};

/// Errors surfaced while establishing a remote transport.
///
/// Callers wrap this into their own error vocabulary (the client's
/// `AttachError`, the server hub's link status) — the split mirrors the
/// two cases operators care about: a *local* I/O failure versus a
/// *transport establishment* failure (handshake, certificate pin, auth
/// preamble).
#[derive(Debug, thiserror::Error)]
pub enum DialError {
    /// Local I/O error — socket connect, read, or write.
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),

    /// The remote transport could not be established: QUIC/TLS handshake,
    /// certificate verification (a fingerprint that did not match the
    /// pin), or a refused/oversized auth preamble.
    #[error("transport connect error: {0}")]
    Connect(String),

    /// The remote host did not answer or its name could not be resolved:
    /// connection refused, no route to host, network unreachable or down,
    /// a handshake that timed out, or a name-resolution failure (on an
    /// overlay, `MagicDNS` being down looks exactly like this).
    /// Distinguished from [`Self::Connect`] so consumers can hint at
    /// reachability (an overlay that is down) rather than at credentials —
    /// certificate-pin and auth failures stay in [`Self::Connect`].
    #[error("transport connect error: {0}")]
    Unreachable(String),

    /// An established connection stopped answering: nothing inbound — not
    /// even a pong for our RFC 6455 keepalive ping — within the liveness
    /// timeout. This is what a laptop switching networks looks like on a
    /// TCP-based lane, where the old socket is never FIN'd or RST'd and a
    /// read would otherwise park forever. Callers treat it as a
    /// disconnection and reconnect; see [`ws::WsKeepalive`].
    #[error("transport stalled: {0}")]
    Stalled(String),
}

/// Whether an I/O error means the remote host never answered — the
/// reachability class that maps to [`DialError::Unreachable`] rather than
/// [`DialError::Connect`].
pub(crate) fn is_reachability_io(err: &std::io::Error) -> bool {
    matches!(
        err.kind(),
        std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::HostUnreachable
            | std::io::ErrorKind::NetworkUnreachable
            | std::io::ErrorKind::NetworkDown
            | std::io::ErrorKind::TimedOut
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reachability_io_matches_unanswered_hosts_only() {
        for kind in [
            std::io::ErrorKind::ConnectionRefused,
            std::io::ErrorKind::HostUnreachable,
            std::io::ErrorKind::NetworkUnreachable,
            std::io::ErrorKind::NetworkDown,
            std::io::ErrorKind::TimedOut,
        ] {
            assert!(
                is_reachability_io(&std::io::Error::from(kind)),
                "{kind:?} is a reachability failure"
            );
        }
        for kind in [
            std::io::ErrorKind::PermissionDenied,
            std::io::ErrorKind::NotFound,
            std::io::ErrorKind::InvalidData,
        ] {
            assert!(
                !is_reachability_io(&std::io::Error::from(kind)),
                "{kind:?} is not a reachability failure"
            );
        }
    }
}
