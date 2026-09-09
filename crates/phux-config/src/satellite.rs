//! Satellite registry schema.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A satellite declared in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SatelliteConfigEntry {
    /// Hub-local satellite name, used in `ResourceId::Satellite.host`.
    pub name: String,

    /// Transport endpoint URI for the satellite server.
    pub endpoint: String,

    /// Whether this satellite is active for hub routing.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Path to a file holding the pairing bearer token minted by `phux pair`
    /// on the satellite host (ADR-0038): one hex token on one line, owner-only
    /// permissions — the same shape as the server's token store. The token
    /// itself never appears in `config.toml`; this key only points at it.
    #[serde(
        default,
        rename = "token-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub token_file: Option<PathBuf>,

    /// SHA-256 fingerprint pin of the satellite's TLS leaf certificate, in
    /// the colon-or-bare hex shape `phux pair` prints. Not a secret — pinning
    /// it here is what defeats a man-in-the-middle on a routable endpoint.
    #[serde(
        default,
        rename = "cert-fingerprint",
        skip_serializing_if = "Option::is_none"
    )]
    pub cert_fingerprint: Option<String>,
}

const fn default_true() -> bool {
    true
}
