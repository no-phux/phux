//! Satellite uplink registry schema (ADR-0181).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A hub this server dials out to and serves as a satellite, written by
/// `phux host join`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UplinkConfigEntry {
    /// The satellite name the hub enrolled this server as: its
    /// `ResourceId::Satellite.host` on the hub.
    pub name: String,

    /// The hub's QUIC listener as `HOST:PORT`.
    pub hub: String,

    /// The TLS server name offered to the hub, when it is not `hub`'s host.
    #[serde(
        default,
        rename = "server-name",
        skip_serializing_if = "Option::is_none"
    )]
    pub server_name: Option<String>,

    /// `sha256:` fingerprint of the hub's workload CA: the only trust the
    /// uplink extends to the hub's certificate.
    #[serde(rename = "ca-fingerprint")]
    pub ca_fingerprint: String,

    /// The enrolled satellite certificate chain (PEM, leaf then CA).
    #[serde(rename = "client-cert")]
    pub client_cert: PathBuf,

    /// Its private key: an owner-only file, never inline.
    #[serde(rename = "client-key")]
    pub client_key: PathBuf,

    /// Whether the server keeps this uplink up.
    #[serde(default = "default_true")]
    pub enabled: bool,
}

const fn default_true() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uplink_entries_use_kebab_case_and_fail_closed() {
        let entry: UplinkConfigEntry = toml::from_str(
            "name = \"sandbox\"\nhub = \"hub.example:8788\"\nca-fingerprint = \"sha256:ab\"\n\
             client-cert = \"/s/u.pem\"\nclient-key = \"/s/u.key\"\n",
        )
        .expect("uplink entry parses");
        assert!(entry.enabled);
        assert_eq!(entry.server_name, None);
        assert_eq!(entry.client_key, PathBuf::from("/s/u.key"));

        let err = toml::from_str::<UplinkConfigEntry>(
            "name = \"s\"\nhub = \"h:1\"\nca-fingerprint = \"x\"\nclient-cert = \"/c\"\n\
             client-key = \"/k\"\ntoken-file = \"/t\"\n",
        )
        .expect_err("unknown uplink keys fail closed");
        assert!(err.to_string().contains("token-file"), "{err}");
        assert!(
            toml::from_str::<UplinkConfigEntry>("name = \"s\"\nhub = \"h:1\"\n").is_err(),
            "the CA pin and identity are required"
        );
    }
}
