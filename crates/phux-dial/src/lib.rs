//! Shared outbound dialer for phux remote transports.
//!
//! Consumers and the federation hub establish QUIC and WebSocket connections
//! the same way: TLS 1.3 with the server's self-signed leaf pinned by SHA-256
//! fingerprint (or a loopback skip), plus an ADR-0031 bearer token. This crate
//! stops at the byte stream; framing stays with the callers. It also owns the
//! congestion-tracked QUIC send window ([`window`]) every phux writer shares.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

#[cfg(feature = "provision")]
pub mod cert;
pub mod quic;
#[cfg(feature = "provision")]
pub mod secret_file;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tls;
pub mod window;
pub mod ws;

pub use quic::QuicDial;
pub use tls::{CertTrust, TlsClientIdentity};
pub use window::{SendWindow, TrackedSend};
pub use ws::{WsDial, WsTarget};

/// Errors surfaced while establishing a remote transport.
#[derive(Debug, thiserror::Error)]
pub enum DialError {
    /// Local I/O error — socket connect, read, or write.
    #[error("io error: {0}")]
    Io(#[source] std::io::Error),

    /// The remote transport could not be established: handshake, certificate
    /// pin mismatch, or a malformed auth preamble.
    #[error("transport connect error: {0}")]
    Connect(String),

    /// The peer refused the pairing token (QUIC close `AUTH_FAILED`), so a
    /// revoked credential is not reported as a lost path.
    #[error("pairing token refused: {0}")]
    AuthRefused(String),

    /// The remote host did not answer or its name did not resolve (refused,
    /// no route, handshake timeout), so consumers can hint at reachability
    /// rather than credentials.
    #[error("transport connect error: {0}")]
    Unreachable(String),

    /// An established connection stopped answering within the liveness
    /// timeout (a half-open TCP socket); callers reconnect. See
    /// [`ws::WsKeepalive`].
    #[error("transport stalled: {0}")]
    Stalled(String),
}

/// Whether an I/O error means the remote host never answered.
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
