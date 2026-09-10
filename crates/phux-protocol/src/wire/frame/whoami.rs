//! The `phux.whoami/v1` record (`docs/spec/L3.md` §3.9, ADR-0106).
//!
//! A server-owned, read-only `Global` metadata key. `GET_METADATA` on it is
//! answered per connection: the value describes the connection that asked,
//! so two clients reading it at once can see two different records. Nothing
//! is stored under the key, a `SET_METADATA` or `DELETE_METADATA` of it is
//! refused, and `LIST_METADATA` does not enumerate it. The value is UTF-8
//! JSON; this module owns its shape so the server that writes it and the
//! clients that read it cannot drift. Gated on
//! [`ServerFeature::Whoami`](crate::caps::ServerFeature::Whoami).

use serde::{Deserialize, Serialize};

/// The read-only `Global` metadata key that reports the asking connection's
/// identity (ADR-0106).
pub const WHOAMI_KEY: &str = "phux.whoami/v1";

/// The `schema_version` a [`WhoamiRecord`] of this shape carries. Additive
/// fields do not bump it.
pub const WHOAMI_SCHEMA_VERSION: u32 = 1;

/// `auth_route` for a Unix-domain-socket client.
///
/// Trusted by the kernel's peer credentials. A client that arrived through
/// `phux stdio-bridge` is a UDS client of the serving host, so it reports
/// this route too.
pub const AUTH_ROUTE_UDS: &str = "uds";
/// `auth_route` reserved for a connection known to arrive over ssh stdio.
///
/// The bridge is byte-transparent, so the reference server cannot tell such
/// a client from a local one and reports [`AUTH_ROUTE_UDS`].
pub const AUTH_ROUTE_SSH_STDIO: &str = "ssh-stdio";
/// `auth_route` for a QUIC client admitted by a bearer credential, directly
/// or bridged through a relay.
pub const AUTH_ROUTE_BEARER_QUIC: &str = "bearer-quic";
/// `auth_route` for a TLS WebSocket client admitted by a bearer credential.
pub const AUTH_ROUTE_BEARER_WSS: &str = "bearer-wss";
/// `auth_route` for a WebTransport client admitted by a bearer credential.
pub const AUTH_ROUTE_BEARER_WEBTRANSPORT: &str = "bearer-webtransport";
/// `auth_route` for a QUIC client on a loopback listener, which carries no
/// credential.
pub const AUTH_ROUTE_LOOPBACK_QUIC: &str = "loopback-quic";
/// `auth_route` for a plaintext WebSocket client on a loopback listener,
/// which carries no credential.
pub const AUTH_ROUTE_LOOPBACK_WS: &str = "loopback-ws";
/// `auth_route` for a WebTransport client on a loopback listener, which
/// carries no credential.
pub const AUTH_ROUTE_LOOPBACK_WEBTRANSPORT: &str = "loopback-webtransport";

/// The OS user the server runs as. Every pane is a child of that user, and
/// the server never switches to another one (ADR-0106).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServingUser {
    /// The serving user's uid.
    pub uid: u32,
    /// The serving user's login name, or `None` when the uid has no
    /// password-database entry.
    pub name: Option<String>,
}

/// The value of [`WHOAMI_KEY`]: who this connection is, how it
/// authenticated, and whose server it reached.
///
/// Readers ignore fields they do not know, so the shape grows additively
/// under the same [`WHOAMI_SCHEMA_VERSION`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WhoamiRecord {
    /// [`WHOAMI_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// The authenticated principal of a bearer credential; `None` on a route
    /// with no credential.
    pub principal: Option<String>,
    /// The non-secret id of the bearer credential (the id `phux pair rotate`
    /// and `phux pair revoke` take); `None` on a route with no credential.
    pub credential_id: Option<String>,
    /// How the connection authenticated. An open vocabulary: the `AUTH_ROUTE_*`
    /// constants in this module are the values defined today, and a reader
    /// shows an unknown one as-is.
    pub auth_route: String,
    /// The peer process's uid from the kernel's socket credentials; `None`
    /// on a network route, where there is no such fact.
    pub peer_uid: Option<u32>,
    /// The OS user the server runs as.
    pub serving_user: ServingUser,
    /// The serving host's name.
    pub host: String,
    /// The server's release version.
    pub server_version: String,
}
