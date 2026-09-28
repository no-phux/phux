//! Outbound relay connector registry schema (ADR-0052).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// One relay endpoint this server dials outbound.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ConnectorConfigEntry {
    /// Relay endpoint as `HOST:PORT`.
    pub relay: String,

    /// Path to the relay-enrollment token. The file contains one hex token and
    /// is re-read on every dial so rotation does not require a server restart.
    #[serde(
        default,
        rename = "token-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub token_file: Option<PathBuf>,

    /// SHA-256 fingerprint of the relay TLS leaf certificate. Required for a
    /// non-loopback relay; optional on loopback for local development.
    #[serde(
        default,
        rename = "cert-fingerprint",
        skip_serializing_if = "Option::is_none"
    )]
    pub cert_fingerprint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_entries_use_kebab_case_and_fail_closed() {
        let entry: ConnectorConfigEntry = toml::from_str(
            "relay = \"relay.example:4433\"\ntoken-file = \"/run/t\"\ncert-fingerprint = \"AB:CD\"\n",
        )
        .expect("connector entry parses");
        assert_eq!(
            entry.token_file.as_deref(),
            Some(std::path::Path::new("/run/t"))
        );
        assert_eq!(entry.cert_fingerprint.as_deref(), Some("AB:CD"));

        let err = toml::from_str::<ConnectorConfigEntry>("relay = \"x\"\ntokne-file = \"/t\"\n")
            .expect_err("unknown connector keys fail closed");
        assert!(err.to_string().contains("tokne-file"), "{err}");
    }
}
