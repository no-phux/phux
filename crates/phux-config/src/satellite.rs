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

    /// Enrolled client certificate PEM the hub presents when the satellite
    /// runs in `paired` mode (ADR-0116). Set together with
    /// [`Self::client_key`]; the same pair `[[remote]]` records.
    #[serde(
        default,
        rename = "client-cert",
        skip_serializing_if = "Option::is_none"
    )]
    pub client_cert: Option<PathBuf>,

    /// The private key for [`Self::client_cert`]: an owner-only file whose
    /// path is recorded here and whose bytes never are.
    #[serde(
        default,
        rename = "client-key",
        skip_serializing_if = "Option::is_none"
    )]
    pub client_key: Option<PathBuf>,

    /// Palette id overriding the satellite's automatic colour
    /// ([`crate::host_color`]); absent or `"auto"` keeps the hash of `name`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color: Option<String>,
}

impl SatelliteConfigEntry {
    /// The enrolled client identity as `(certificate, private_key)` paths,
    /// or `None` when the entry has none.
    ///
    /// # Errors
    ///
    /// Half an identity or a relative path, as for a `[[remote]]` entry.
    pub fn client_identity(&self) -> Result<Option<(PathBuf, PathBuf)>, String> {
        crate::remote::client_identity_paths(
            &self.name,
            self.client_cert.as_deref(),
            self.client_key.as_deref(),
        )
    }
}

const fn default_true() -> bool {
    true
}
