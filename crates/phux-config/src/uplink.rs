//! Satellite uplink registry schema (ADR-0182).

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

    /// Joined for mail and claims only (`phux host join --mail-only`): the
    /// server never dials this hub, so the hub reaches no terminal here.
    #[serde(default, rename = "mail-only", skip_serializing_if = "is_false")]
    pub mail_only: bool,
}

impl UplinkConfigEntry {
    /// The hub's host and port from [`Self::hub`].
    ///
    /// # Errors
    ///
    /// As [`split_host_port`].
    pub fn hub_host_port(&self) -> Result<(String, u16), String> {
        split_host_port(&self.hub)
    }
}

/// Split a hub endpoint, `HOST:PORT` with IPv6 bracketed and no scheme, the
/// way a join code and an `[[uplink]]` entry read it.
///
/// # Errors
///
/// Wording naming the endpoint, for an empty host or a missing or zero port.
pub fn split_host_port(hub: &str) -> Result<(String, u16), String> {
    let malformed = || format!("{hub:?} is not HOST:PORT");
    let (host, port) = match hub.strip_prefix('[') {
        Some(rest) => rest.split_once("]:").ok_or_else(malformed)?,
        None => hub.rsplit_once(':').ok_or_else(malformed)?,
    };
    let port = port.parse::<u16>().ok().filter(|port| *port != 0);
    match port {
        Some(port) if !host.is_empty() && !host.contains(['/', '[', ']']) => {
            Ok((host.to_owned(), port))
        }
        _ => Err(malformed()),
    }
}

#[allow(
    clippy::trivially_copy_pass_by_ref,
    reason = "serde's skip_serializing_if signature"
)]
const fn is_false(value: &bool) -> bool {
    !*value
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
