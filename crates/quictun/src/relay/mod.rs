//! The relay: one QUIC endpoint bridging services' outbound tunnels to
//! inbound consumers (feature `relay`).
//!
//! A service dials out under [`Protocol::tunnel_alpn`] and proves a route
//! token on stream 0 ([`crate::tunnel`]); consumers dial in under one of
//! [`Protocol::consumer_alpns`] and name the route in the TLS server name.
//! The relay splices each admitted consumer onto a fresh stream over the
//! route's tunnel.
//!
//! The relay **never reads what it forwards**: its only parse is the
//! tunnel's stream-0 preamble, and a consumer's bytes cross opaquely.
//! Consumer hellos whose server name is absent or not an enrolled route are
//! refused at the TLS layer, before any application byte flows.

use std::path::{Path, PathBuf};

mod ingress;
mod registry;
mod runtime;
mod shares;
mod tls;
mod tokens;

pub use ingress::TcpIngress;
pub use runtime::{
    BoundRelay, DEFAULT_HANDSHAKES_PER_SOURCE, DEFAULT_MAX_CONNS, RelayConfig, RelayRuntime,
};
pub use tls::{cert_fingerprint, ensure_self_signed};
pub use tokens::{RouteTokenStore, mint_route_token, validate_route_name};

use crate::Protocol;

/// The relay's certificate file name inside its state directory.
pub const CERT_FILE: &str = "relay-cert.pem";

/// The relay's private-key file name inside its state directory.
pub const KEY_FILE: &str = "relay-key.pem";

/// The route-token store's file name inside the relay's state directory.
pub const TOKENS_FILE: &str = "relay-tokens";

/// A per-user state directory for an application: `$XDG_STATE_HOME/<app>`,
/// else `$HOME/.local/state/<app>`.
#[must_use]
pub fn state_dir(app: &str) -> PathBuf {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map_or_else(
            || {
                let mut home = std::env::var_os("HOME").map_or_else(PathBuf::new, PathBuf::from);
                home.push(".local");
                home.push("state");
                home
            },
            PathBuf::from,
        );
    base.join(app)
}

impl RelayConfig {
    /// A config listening on `listen` for `protocol`, with its three files
    /// ([`CERT_FILE`], [`KEY_FILE`], [`TOKENS_FILE`]) in `dir` and the
    /// default connection cap.
    #[must_use]
    pub fn in_dir(listen: std::net::SocketAddr, dir: &Path, protocol: Protocol) -> Self {
        Self {
            listen,
            cert_path: dir.join(CERT_FILE),
            key_path: dir.join(KEY_FILE),
            tokens_path: dir.join(TOKENS_FILE),
            max_conns: DEFAULT_MAX_CONNS,
            protocol,
            max_conns_per_route: None,
            tcp_ingress: None,
        }
    }
}

/// Errors surfaced by the relay.
///
/// Only startup-fatal conditions reach this type (bind, token-store load,
/// certificate provisioning, an ambiguous protocol). Per-connection failures
/// are logged and never tear down the endpoint.
#[derive(Debug, thiserror::Error)]
pub enum RelayError {
    /// File or network I/O failed: state files, endpoint bind, or building
    /// the tokio runtime.
    #[error("relay io: {0}")]
    Io(#[from] std::io::Error),

    /// Generating the self-signed certificate failed.
    #[error("certificate generation: {0}")]
    Rcgen(#[from] rcgen::Error),

    /// A PEM certificate or key file could not be parsed.
    #[error("pem: {0}")]
    Pem(#[from] rustls::pki_types::pem::Error),

    /// rustls rejected the certificate/key material or the TLS config.
    #[error("rustls: {0}")]
    Rustls(#[from] rustls::Error),

    /// The certificate file held no certificates.
    #[error("no certificates in {0}")]
    NoCerts(String),

    /// Exactly one of the persisted cert/key pair exists; regenerating would
    /// rotate the pinned fingerprint, so the operator must delete it.
    #[error(
        "partial TLS pair: {present} exists but {missing} is missing — delete {present} to regenerate (this rotates the pinned fingerprint and breaks existing pins)"
    )]
    PartialTlsPair {
        /// Path of the file that still exists.
        present: String,
        /// Path of the file that is missing.
        missing: String,
    },

    /// A secret file (the TLS key or the route-token store) is owned by
    /// another account, or other accounts can replace it (or, for the key,
    /// read it). Carries the refusal and its fix.
    #[error("{0}")]
    InsecureFile(String),

    /// The OS random source failed while minting a token.
    #[error("os random source unavailable: {0}")]
    Random(#[from] getrandom::Error),

    /// A line in the route-token file was not `<64-char hex> <route>`.
    #[error(
        "malformed route-token line {line} (expected `<{hex_len}-char hex> <route>`, one entry per line)",
        hex_len = tokens::TOKEN_LEN * 2
    )]
    MalformedTokenLine {
        /// 1-based line number in the token file.
        line: usize,
    },

    /// A route name failed the lowercase RFC 1123 label grammar; route names
    /// ride the TLS server name, so they are rejected, never normalized.
    #[error("invalid route name {name:?}: {reason}")]
    InvalidRouteName {
        /// The offending name, verbatim.
        name: String,
        /// Which grammar rule it broke.
        reason: &'static str,
    },

    /// The [`Protocol`] cannot tell a tunnel from a consumer
    /// ([`Protocol::validate`]).
    #[error("relay protocol: {0}")]
    Protocol(&'static str),
}

/// Maps provisioning errors onto the relay's own vocabulary so operator
/// messages keep the relay's wording.
impl From<crate::cert::CertError> for RelayError {
    fn from(err: crate::cert::CertError) -> Self {
        use crate::cert::CertError;
        match err {
            CertError::Io(err) => Self::Io(err),
            CertError::Rcgen(err) => Self::Rcgen(err),
            CertError::Pem(err) => Self::Pem(err),
            CertError::NoCerts(path) => Self::NoCerts(path),
            CertError::InsecureKey(message) => Self::InsecureFile(message),
            CertError::PartialTlsPair { present, missing } => {
                Self::PartialTlsPair { present, missing }
            }
        }
    }
}
