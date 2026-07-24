//! phux reference relay core (ADR-0051, ADR-0052): a byte relay between
//! outbound connector tunnels and inbound consumers.
//!
//! A phux server behind NAT dials OUT to a relay under the dedicated
//! `phux-relay/1` ALPN and registers a tunnel for a named route; remote
//! consumers dial IN under the production `phux-quic/1` ALPN, naming the
//! route via TLS SNI. The relay splices each admitted consumer connection
//! onto a fresh relay-initiated bidi stream over the route's tunnel.
//!
//! The relay **never parses phux frames**. Its only parse is the
//! connector's length-prefixed auth preamble on stream 0; everything else
//! — including each consumer's own bearer-token preamble — crosses as
//! opaque bytes (ADR-0051 invariants 1 and 5, held by construction: this
//! crate depends on `phux_protocol::policy` for the two ALPN constants and
//! nothing else from the wire crate).
//!
//! Single-tenant, single-process, no accounts, no persistence beyond a
//! route-bound token store and a self-signed keypair. The `phux relay`
//! verb in the `phux` binary fronts [`RelayRuntime`].

#![forbid(unsafe_code)]
#![deny(missing_docs)]

pub mod paths;
pub mod registry;
pub mod runtime;
pub mod splice;
pub mod tls;
pub mod tokens;

pub use runtime::{BoundRelay, DEFAULT_MAX_CONNS, RelayConfig, RelayRuntime};
pub use tls::{
    cert_fingerprint, default_relay_cert_path, default_relay_key_path, ensure_self_signed,
};
pub use tokens::{
    RouteTokenStore, TOKEN_LEN, default_relay_tokens_path, mint_route_token, validate_route_name,
};

/// Application close code: bad, missing, or unknown tunnel token on the
/// connector leg. Mirrors the server QUIC listener's auth-refusal code so
/// "unauthorized" reads the same on every phux transport.
pub const AUTH_FAILED_CODE: u32 = 0x01;

/// Application close code: enrolled route, no live tunnel (`ROUTE_OFFLINE`).
///
/// The TLS handshake completes before this close, distinguishing "server
/// down" from "unknown route" — the latter is refused at the TLS layer
/// and never reaches application close codes.
pub const ROUTE_OFFLINE_CODE: u32 = 0x02;

/// Application close code: this tunnel was superseded by a newer claim on
/// the same route (`RECLAIMED`, last-writer-wins). The warn log at the
/// relay is the operator's theft-detection surface.
pub const RECLAIMED_CODE: u32 = 0x03;

/// Application close code: the connector sent bytes on stream 0 after the
/// auth preamble. Stream 0 is reserved — richer relay dialogue requires an
/// ALPN bump, never in-band bytes (ADR-0051 invariant 4).
pub const PROTOCOL_VIOLATION_CODE: u32 = 0x04;

/// Application close code: the relay is at its connection cap
/// (`--max-conns`) and refused this connection after the handshake
/// (`OVER_CAP`). Existing tunnels and consumers are unaffected.
pub const OVER_CAP_CODE: u32 = 0x05;

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

    /// A route name failed the DNS-label grammar (lowercase RFC 1123
    /// label: `[a-z0-9-]`, 1-63 chars, no leading/trailing hyphen).
    /// Route names ride SNI, so the grammar is rejected — never
    /// normalized — at mint and at load.
    #[error("invalid route name {name:?}: {reason}")]
    InvalidRouteName {
        /// The offending name, verbatim.
        name: String,
        /// Which grammar rule it broke.
        reason: &'static str,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn close_codes_are_distinct() {
        let codes = [
            AUTH_FAILED_CODE,
            ROUTE_OFFLINE_CODE,
            RECLAIMED_CODE,
            PROTOCOL_VIOLATION_CODE,
            OVER_CAP_CODE,
        ];
        for (i, a) in codes.iter().enumerate() {
            for b in &codes[i + 1..] {
                assert_ne!(a, b, "close codes must be distinguishable");
            }
        }
    }
}
