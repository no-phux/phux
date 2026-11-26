//! Shared outbound dialer and pure native route planning.
//!
//! Native adapters acquire addresses and credentials; [`QuicDial::plan`],
//! [`WsTarget::credentials`] and [`CertTrust`] decide route/pin/token trust
//! without DNS, files, prompts or environment access. Routable endpoints need
//! pinned TLS, with server authority trust kept inside a relayed QUIC session.
//! Loopback retains its development exception. This crate stops at the byte
//! stream; framing stays with callers. The
//! congestion-tracked QUIC send window ([`window`]) every phux writer shares,
//! and with `provision` the [`cert`] and [`secret_file`] modules, are
//! `quictun`'s, re-exported so phux names them in one place. With `relay`,
//! the `relay` module binds `quictun`'s relay to phux's wire for `phux relay`.

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]

#[cfg(feature = "provision")]
pub use quictun::cert;
pub mod enroll;
pub mod quic;
#[cfg(feature = "relay")]
pub mod relay;
#[cfg(feature = "provision")]
pub use quictun::secret_file;
#[cfg(any(test, feature = "testing"))]
pub mod testing;
pub mod tls;
pub use quictun::window;
pub mod ws;

pub use quic::QuicDial;
pub use tls::{AuthorityChange, CertTrust, HeldIdentity, TlsClientIdentity};
pub use window::{SendWindow, TrackedSend};
pub use ws::{WsDial, WsTarget};

/// A native route or credential that cannot safely be dialed.
///
/// Contains no token bytes. Platform adapters supply their own diagnostics and
/// remedies; planning never reads credentials, resolves names, or prompts.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PlanRefusal {
    /// A routable endpoint has no usable certificate pin.
    #[error("a routable endpoint needs a certificate pin")]
    Unpinned,
    /// A routable WebSocket would expose its session as plaintext.
    #[error("a routable WebSocket needs wss://")]
    Plaintext,
    /// A routable WebSocket has no bearer token.
    #[error("a routable WebSocket needs a pairing token")]
    NoToken,
    /// A bearer token is not hexadecimal.
    #[error("the pairing token is not valid hex")]
    BadToken,
    /// A relay would terminate a session without end-to-end server trust.
    #[error("a relay route needs the server's certificate authority pin")]
    UnpinnedRelay,
    /// Native WebSocket has no end-to-end relay session.
    #[error("a relay route needs a quic:// endpoint")]
    RelayRequiresQuic,
}

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

    /// The server did not present the certificate authority this client
    /// pinned (`docs/remote.md#who-can-connect` `authority_changed`, ADR-0153). Never
    /// retried: the remedy is re-pairing, after checking the change was the
    /// operator's rotation.
    #[error("{0}")]
    AuthorityChanged(tls::AuthorityChange),

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
