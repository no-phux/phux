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

    /// The route name this connector's token is enrolled under at the relay
    /// (`phux relay pair --route NAME`). The connector itself never sends it
    /// — the tunnel token selects the route relay-side — but recording it
    /// lets `phux pair` print a complete relay-fronted connect link
    /// (endpoint, SNI, relay pin, consumer token) with nothing assembled by
    /// hand, and tells a future operator which route this entry claims.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connector_entry_uses_kebab_case_credentials() {
        let entry: ConnectorConfigEntry = toml::from_str(
            r#"
relay = "relay.example:4433"
token-file = "/run/secrets/phux-relay.token"
cert-fingerprint = "AB:CD"
route = "mini"
"#,
        )
        .expect("connector entry parses");

        assert_eq!(entry.relay, "relay.example:4433");
        assert_eq!(
            entry.token_file.as_deref(),
            Some(std::path::Path::new("/run/secrets/phux-relay.token"))
        );
        assert_eq!(entry.cert_fingerprint.as_deref(), Some("AB:CD"));
        assert_eq!(entry.route.as_deref(), Some("mini"));
    }

    #[test]
    fn route_is_optional() {
        let entry: ConnectorConfigEntry = toml::from_str(r#"relay = "localhost:4433""#)
            .expect("route-less connector entry parses");
        assert_eq!(entry.route, None);
    }

    #[test]
    fn connector_entry_rejects_unknown_fields() {
        let err = toml::from_str::<ConnectorConfigEntry>(
            r#"
relay = "localhost:4433"
tokne-file = "/tmp/token"
"#,
        )
        .expect_err("unknown connector keys fail closed");
        assert!(err.to_string().contains("tokne-file"), "{err}");
    }
}
