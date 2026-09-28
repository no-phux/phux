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

    /// Path to the pairing token file `phux pair` minted (ADR-0038); the
    /// token itself never appears in `config.toml`.
    #[serde(
        default,
        rename = "token-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub token_file: Option<PathBuf>,

    /// SHA-256 pin of the satellite's TLS leaf certificate.
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
