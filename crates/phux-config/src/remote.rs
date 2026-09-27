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
}
