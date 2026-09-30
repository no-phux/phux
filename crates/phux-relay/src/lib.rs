//! phux reference relay core (ADR-0051, ADR-0052): a byte relay between
//! outbound connector tunnels and inbound consumers.
//!
//! A phux server behind NAT dials OUT under the `phux-relay/1` ALPN and
//! registers a tunnel for a named route; consumers dial IN under
//! `phux-quic/1`, naming the route via TLS SNI. The relay splices each
//! admitted consumer onto a fresh stream over the route's tunnel.
//!
//! The relay **never parses phux frames**: its only parse is the connector's
//! stream-0 auth preamble; consumer bytes, including their bearer preamble,
//! cross opaquely (ADR-0051 invariants 1 and 5). The `phux relay` verb fronts
//! [`RelayRuntime`].

#![forbid(unsafe_code)]
#![deny(missing_docs)]
#![deny(rustdoc::private_intra_doc_links)]
#![allow(
    clippy::redundant_pub_crate,
    reason = "private modules keep pub(crate) items; conflicts with unreachable_pub"
)]

use std::path::PathBuf;

mod registry;
mod runtime;
mod splice;
mod tls;
mod tokens;

pub use runtime::{
    BoundRelay, DEFAULT_HANDSHAKES_PER_SOURCE, DEFAULT_MAX_CONNS, RelayConfig, RelayRuntime,
};
pub use tls::{cert_fingerprint, ensure_self_signed};
pub use tokens::{RouteTokenStore, mint_route_token, validate_route_name};

/// Application close code: bad, missing, or unknown tunnel token on the
/// connector leg; matches the server listener's auth refusal.
pub const AUTH_FAILED_CODE: u32 = 0x01;

/// Application close code: enrolled route, no live tunnel (`ROUTE_OFFLINE`).
/// Sent after the TLS handshake; unknown routes are refused at TLS instead.
pub const ROUTE_OFFLINE_CODE: u32 = 0x02;

/// Application close code: a newer claim on the same route superseded this
/// tunnel (`RECLAIMED`, last-writer-wins).
pub const RECLAIMED_CODE: u32 = 0x03;

/// Application close code: the connector sent bytes on reserved stream 0
/// after the auth preamble (ADR-0051 invariant 4).
pub const PROTOCOL_VIOLATION_CODE: u32 = 0x04;

/// Application close code: the relay is at its `--max-conns` cap
/// (`OVER_CAP`); existing connections are unaffected.
pub const OVER_CAP_CODE: u32 = 0x05;

/// phux's per-user state directory: `$XDG_STATE_HOME/phux`, else
/// `$HOME/.local/state/phux`. Duplicates `phux_server::telemetry::state_dir`
/// because the relay must not depend on the daemon (ADR-0051).
fn state_dir() -> PathBuf {
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
    base.join("phux")
}

/// Default relay certificate path: `<state-dir>/relay-cert.pem`.
#[must_use]
pub fn default_relay_cert_path() -> PathBuf {
    state_dir().join("relay-cert.pem")
}

/// Default relay private-key path: `<state-dir>/relay-key.pem`.
#[must_use]
pub fn default_relay_key_path() -> PathBuf {
    state_dir().join("relay-key.pem")
}

/// Default route-token store path: `<state-dir>/relay-tokens`.
#[must_use]
pub fn default_relay_tokens_path() -> PathBuf {
    state_dir().join("relay-tokens")
}

/// Errors surfaced by the relay library.
///
/// Only startup-fatal conditions reach this type (bind, token-store load,
/// certificate provisioning). Per-connection failures are logged and never
/// tear down the endpoint.
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
    /// ride SNI, so they are rejected, never normalized.
    #[error("invalid route name {name:?}: {reason}")]
    InvalidRouteName {
        /// The offending name, verbatim.
        name: String,
        /// Which grammar rule it broke.
        reason: &'static str,
    },
}

/// Maps `phux_dial`'s provisioning errors onto the relay's own vocabulary so
/// operator messages keep the relay's wording.
impl From<phux_dial::cert::CertError> for RelayError {
    fn from(err: phux_dial::cert::CertError) -> Self {
        use phux_dial::cert::CertError;
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
