//! Client-side remote-server registry schema (ADR-0055): `[[remote]]`
//! entries `phux attach <name>` resolves.
//!
//! A sibling of [`crate::satellite`], not a reuse: the trust direction is
//! opposite (a satellite is dialed by a hub for its users; a remote by this
//! client for itself).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A remote phux server declared in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfigEntry {
    /// Local label `phux attach <name>` resolves (not a routable identity).
    pub name: String,

    /// Transport endpoint URI: `quic://HOST:PORT`, `wss://HOST:PORT`, or
    /// `ssh://HOST` for the stdio bridge.
    pub endpoint: String,

    /// Path to the pairing token file `phux pair` minted; the token itself
    /// never appears in `config.toml`.
    #[serde(
        default,
        rename = "token-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub token_file: Option<PathBuf>,

    /// SHA-256 pin of the remote's TLS leaf certificate (required for a
    /// non-loopback dial, ADR-0031).
    #[serde(
        default,
        rename = "cert-fingerprint",
        skip_serializing_if = "Option::is_none"
    )]
    pub cert_fingerprint: Option<String>,

    /// TLS server name (SNI) a `quic://` or `wss://` dial offers instead of
    /// the endpoint's host. Set, it names a relay route: the endpoint is
    /// the relay, the pin is the relay's, and the relay splices the dial
    /// onto the server enrolled under this route (ADR-0052, ADR-0149).
    #[serde(
        default,
        rename = "tls-server-name",
        skip_serializing_if = "Option::is_none"
    )]
    pub tls_server_name: Option<String>,

    /// Session to attach on arrival; absent lets the server decide.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,

    /// The ssh destination the entry was enrolled through, used to restart a
    /// server that does not answer; absent, the name is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,

    /// A paired `quic://` endpoint kept beside an `ssh://` one, tried first
    /// and promoted to `endpoint` once it answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct: Option<String>,

    /// The workload client certificate chain (PEM, leaf first) this client
    /// presents to the remote over TLS, as `phux host add` enrolled it
    /// (ADR-0116). Set together with [`Self::client_key`].
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
}

impl RemoteConfigEntry {
    /// The enrolled client identity as `(certificate, private_key)` paths, or
    /// `None` when the entry has none.
    ///
    /// # Errors
    ///
    /// Half an identity, or a relative path: a dial that silently went
    /// without the certificate the operator enrolled would hide the mistake
    /// until a paired server refused it.
    pub fn client_identity(&self) -> Result<Option<(PathBuf, PathBuf)>, String> {
        client_identity_paths(
            &self.name,
            self.client_cert.as_deref(),
            self.client_key.as_deref(),
        )
    }
}

/// [`RemoteConfigEntry::client_identity`] over the two optional paths, for
/// callers that hold them outside the schema type.
///
/// # Errors
///
/// As [`RemoteConfigEntry::client_identity`].
pub fn client_identity_paths(
    name: &str,
    cert: Option<&std::path::Path>,
    key: Option<&std::path::Path>,
) -> Result<Option<(PathBuf, PathBuf)>, String> {
    match (cert, key) {
        (None, None) => Ok(None),
        (Some(cert), Some(key)) if cert.is_absolute() && key.is_absolute() => {
            Ok(Some((cert.to_path_buf(), key.to_path_buf())))
        }
        (Some(_), Some(_)) => Err(format!(
            "remote {name:?}: client-cert and client-key must be absolute paths"
        )),
        _ => Err(format!(
            "remote {name:?}: client-cert and client-key must be set together; \
             re-enroll with `phux host add {name}`"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_server_name_is_an_optional_kebab_case_key() {
        let routed: RemoteConfigEntry = toml::from_str(
            "name = \"mini\"\nendpoint = \"quic://relay.example:4433\"\n\
             cert-fingerprint = \"AB:CD\"\ntls-server-name = \"mini-route\"\n",
        )
        .expect("routed entry parses");
        assert_eq!(routed.tls_server_name.as_deref(), Some("mini-route"));
        let written = toml::to_string(&routed).expect("entry serializes");
        assert!(
            written.contains("tls-server-name = \"mini-route\""),
            "{written}"
        );

        let direct: RemoteConfigEntry =
            toml::from_str("name = \"mini\"\nendpoint = \"quic://mini:8788\"\n")
                .expect("an entry without the key still parses");
        assert_eq!(direct.tls_server_name, None);
        let written = toml::to_string(&direct).expect("entry serializes");
        assert!(!written.contains("tls-server-name"), "{written}");
    }
}
