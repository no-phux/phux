//! Client-side remote-server registry schema (ADR-0055).
//!
//! A `[[remote]]` entry names a phux server this machine attaches *to*, so
//! `phux attach mini` resolves an endpoint, a certificate pin, and a token
//! without the operator retyping two 64-hex strings.
//!
//! Deliberately a sibling of [`crate::satellite`] rather than a reuse of it.
//! The fields line up because both describe "a phux server reachable over a
//! pinned TLS transport," but the trust direction is opposite — a satellite
//! is a peer a *hub* dials on behalf of its users, a remote is a server *this
//! consumer* dials on behalf of itself. Collapsing them would let
//! `phux host add` edit federation topology.
//!
//! Two keys remember how the entry was made so a later attach can act on
//! it without the operator retyping anything: `ssh` is the destination
//! `phux host add` enrolled through, which is what a stopped server is
//! restarted over; `direct` is a paired `quic://` endpoint kept while
//! `endpoint` is `ssh://`, so an attach can try the direct route first and
//! promote it once it answers.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// A remote phux server declared in `config.toml`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RemoteConfigEntry {
    /// Local label. This is the name `phux attach <name>` resolves; it is a
    /// lookup key in one operator's config, not a routable identity and not
    /// ADR-0052's SNI route name.
    pub name: String,

    /// Transport endpoint URI: `quic://HOST:PORT`, `wss://HOST:PORT`, or
    /// `ssh://HOST` for the stdio bridge.
    pub endpoint: String,

    /// Path to a file holding the pairing bearer token minted by `phux pair`
    /// on the remote host: one hex token on one line, owner-only
    /// permissions. The token never appears in `config.toml`; this key only
    /// points at it.
    #[serde(
        default,
        rename = "token-file",
        skip_serializing_if = "Option::is_none"
    )]
    pub token_file: Option<PathBuf>,

    /// SHA-256 fingerprint pin of the remote's TLS leaf certificate, in the
    /// colon-or-bare hex shape `phux pair` prints. Not a secret — pinning it
    /// is what defeats a man-in-the-middle on a routable endpoint, and
    /// ADR-0031 refuses a non-loopback dial without one.
    #[serde(
        default,
        rename = "cert-fingerprint",
        skip_serializing_if = "Option::is_none"
    )]
    pub cert_fingerprint: Option<String>,

    /// Session to attach on arrival. Absent means the remote server's own
    /// last-attach memory decides, exactly as a local naked `phux` does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<String>,

    /// The ssh destination this entry was enrolled through (`me@mini`, or
    /// a `~/.ssh/config` alias), exactly as it is typed after `ssh`. An
    /// attach that finds the saved endpoint not answering restarts the
    /// server over it; absent, the entry's name is used as the destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ssh: Option<String>,

    /// A paired direct endpoint (`quic://HOST:PORT`) that did not answer
    /// when the entry was written, kept beside an `ssh://` endpoint so a
    /// later attach can try it first and promote it to `endpoint` once it
    /// does. Meaningless, and cleared, once `endpoint` is itself direct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub direct: Option<String>,
}
