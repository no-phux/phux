//! The read-only `phux.whoami/v1` Global metadata key
//! (`docs/remote.md#who-can-do-what`, ADR-0106).
//!
//! `GET_METADATA` on the key is answered from the asking connection's own
//! identity, stamped by the accepting transport, rather than from the
//! metadata store: the principal and credential id of a bearer connection,
//! the kernel peer uid of a Unix-socket one, the route it authenticated by,
//! and the OS user and host this server runs as. Nothing is ever stored
//! under the key and a client write of it is refused (`client.rs`,
//! `is_server_owned_key`).
//!
//! The server never switches users, so the serving user is also the user
//! every pane runs as. Reporting it changes nothing.

use std::sync::OnceLock;

use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::scope::verb_name;
use phux_protocol::wire::frame::{
    AUTH_ROUTE_BEARER_QUIC, AUTH_ROUTE_BEARER_WEBTRANSPORT, AUTH_ROUTE_BEARER_WSS,
    AUTH_ROUTE_LOOPBACK_QUIC, AUTH_ROUTE_LOOPBACK_WEBTRANSPORT, AUTH_ROUTE_LOOPBACK_WS,
    AUTH_ROUTE_SSH_STDIO, AUTH_ROUTE_UDS, Scope, ServingUser, SshClient, WHOAMI_KEY,
    WHOAMI_SCHEMA_VERSION, WhoamiRecord,
};
use phux_protocol::wire::ssh_origin::SshOrigin;

use crate::auth::AuthenticatedCredential;
use crate::policy::Authority;
use crate::state::{ClientId, ServerState};

/// The server's release version, reported as `server_version`.
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Whether a `GET_METADATA` names the whoami key. Only the `Global` scope
/// carries it; the same key string under another scope is an ordinary key.
pub(super) fn is_whoami_key(scope: &Scope, key: &str) -> bool {
    matches!(scope, Scope::Global) && key == WHOAMI_KEY
}

/// The encoded record for `client_id`, or `None` when the transport stamped
/// no identity for it (every accept path does, so this is the defensive
/// "no answer", never a guess).
///
/// The record carries the asking connection's own grant and nobody else's
/// (`docs/remote.md#who-can-do-what`: the key answers only the asking connection).
pub(super) fn record_for(s: &ServerState, client_id: ClientId) -> Option<Vec<u8>> {
    let peer = s.peer_identity(client_id)?;
    let record = build_record(
        peer,
        s.authenticated_credential(client_id),
        s.ssh_origin(client_id),
        serving_host(),
    );
    let connection = s.connection_grant(client_id);
    let grant = connection.map_or_else(Vec::new, |grant| grant_entries(&grant.authority));
    let on_behalf_of = connection
        .and_then(crate::policy::ConnectionGrant::on_behalf_of)
        .map(|principal| crate::policy::principal_name(principal).to_owned());
    // C1 hook: add `on_behalf_of_subject` from that credential's registry `subject` here.
    serde_json::to_vec(&RecordWithGrant {
        record,
        grant,
        on_behalf_of,
    })
    .ok()
}

/// The record plus two additive schema-1 fields: `grant`, what this
/// connection may do in the registry grammar (`docs/remote.md#who-can-do-what`),
/// and `on_behalf_of`, who delegated a delegated key (`owner` or the
/// minting credential id), `null` otherwise.
#[derive(serde::Serialize)]
struct RecordWithGrant {
    #[serde(flatten)]
    record: WhoamiRecord,
    grant: Vec<GrantEntry>,
    on_behalf_of: Option<String>,
}

/// One grant: its verbs (lowercase, in bit order), its selector, and the
/// verbs it holds for approval (ADR-0128), absent when it holds none.
#[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
struct GrantEntry {
    verbs: Vec<String>,
    selector: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    held: Vec<String>,
}

fn grant_entries(authority: &Authority) -> Vec<GrantEntry> {
    authority
        .reported_grants()
        .iter()
        .map(|grant| GrantEntry {
            verbs: grant
                .verbs
                .iter()
                .map(|verb| verb_name(verb).to_owned())
                .collect(),
            selector: grant.selector.to_string(),
            held: grant
                .held
                .iter()
                .map(|verb| verb_name(verb).to_owned())
                .collect(),
        })
        .collect()
}

/// Record the ssh origin a HELLO carried, when its sender may make the claim.
///
/// `phux stdio-bridge` stamps the field, and the bridge is always a
/// Unix-socket client running as the serving user. The value is only what the
/// connecting side reported, so it is kept as a label and never trusted.
/// Anything else is ignored:
/// a network peer, another uid (root reaching the owner-only socket), or the
/// in-process transport. Accepting it changes only the route whoami reports.
/// Authentication and authorization have already run and never read it.
pub(super) fn admit_ssh_origin(
    s: &mut ServerState,
    client_id: ClientId,
    origin: Option<SshOrigin>,
) {
    let Some(origin) = origin else {
        return;
    };
    let admissible = s
        .peer_identity(client_id)
        .is_some_and(|peer| may_announce_ssh(peer, serving_host().user.uid));
    if !admissible {
        tracing::debug!(
            ?client_id,
            "HELLO ssh_origin ignored: sender is not a same-uid Unix-socket peer"
        );
        return;
    }
    tracing::info!(
        ?client_id,
        ssh_client = %origin.client,
        ssh_server = ?origin.server,
        "ssh-bridged client announced by stdio-bridge"
    );
    s.set_ssh_origin(client_id, origin);
}

/// Whether `peer` may announce an ssh origin: a Unix-socket peer running as
/// the serving uid, which a same-user `phux stdio-bridge` always is.
const fn may_announce_ssh(peer: &PeerIdentity, serving_uid: u32) -> bool {
    matches!(peer.transport, TransportType::UnixSocket) && peer.uid == serving_uid
}

/// What the server knows about itself: the same for every connection, so it
/// is read once.
struct ServingHost {
    name: String,
    user: ServingUser,
}

/// The serving host and user, probed on the first whoami read.
///
/// The password-database lookup can reach NSS, so it runs once per process
/// rather than on every read.
fn serving_host() -> &'static ServingHost {
    static HOST: OnceLock<ServingHost> = OnceLock::new();
    HOST.get_or_init(probe_serving_host)
}

fn probe_serving_host() -> ServingHost {
    // The effective uid: ADR-0106 names the serving user as the effective
    // user, which differs from the real uid only under a setuid install.
    let uid = nix::unistd::geteuid();
    let name = nix::unistd::User::from_uid(uid)
        .ok()
        .flatten()
        .map(|user| user.name);
    let host = nix::unistd::gethostname()
        .ok()
        .and_then(|name| name.into_string().ok())
        .unwrap_or_default();
    ServingHost {
        name: host,
        user: ServingUser {
            uid: uid.as_raw(),
            name,
        },
    }
}

/// Assemble the record for one connection. Pure, for tests.
///
/// An admitted `ssh_origin` relabels a Unix-socket route as `ssh-stdio` and
/// reports its client endpoint. It is ignored on any other transport, where
/// admission would already have refused it.
fn build_record(
    peer: &PeerIdentity,
    credential: Option<&AuthenticatedCredential>,
    ssh_origin: Option<SshOrigin>,
    host: &ServingHost,
) -> WhoamiRecord {
    let ssh_origin = ssh_origin.filter(|_| matches!(peer.transport, TransportType::UnixSocket));
    let route = route_word(peer, credential.is_some(), ssh_origin.is_some());
    WhoamiRecord {
        schema_version: WHOAMI_SCHEMA_VERSION,
        principal: credential.map(|credential| credential.principal.clone()),
        credential_id: credential.map(|credential| credential.id.clone()),
        auth_route: route.to_owned(),
        peer_uid: kernel_peer_uid(peer),
        serving_user: host.user.clone(),
        host: host.name.clone(),
        server_version: SERVER_VERSION.to_owned(),
        ssh_client: ssh_origin.map(|origin| SshClient {
            addr: origin.client.ip().to_string(),
            port: origin.client.port(),
        }),
    }
}

/// The `auth_route` word for a connection; an admitted ssh origin relabels
/// a Unix-socket route.
const fn route_word(peer: &PeerIdentity, bearer: bool, ssh: bool) -> &'static str {
    if ssh && matches!(peer.transport, TransportType::UnixSocket) {
        AUTH_ROUTE_SSH_STDIO
    } else {
        auth_route(peer.transport, bearer)
    }
}

/// The `auth_route` word whoami would report for `client_id`; `None` when
/// the transport stamped no identity.
#[allow(
    clippy::redundant_pub_crate,
    reason = "private module shared with the state layer; pub would violate unreachable_pub"
)]
pub(crate) fn route_for(s: &ServerState, client_id: ClientId) -> Option<&'static str> {
    let peer = s.peer_identity(client_id)?;
    Some(route_word(
        peer,
        s.authenticated_credential(client_id).is_some(),
        s.ssh_origin(client_id).is_some(),
    ))
}

/// The peer uid, when it came from the kernel's socket credentials. A
/// network transport stamps a placeholder `0`, which is not a fact about
/// anyone and is never reported.
fn kernel_peer_uid(peer: &PeerIdentity) -> Option<u32> {
    is_local_socket(peer.transport).then_some(peer.uid)
}

/// Whether the transport is the owner-only Unix socket (or in-process).
const fn is_local_socket(transport: TransportType) -> bool {
    matches!(
        transport,
        TransportType::UnixSocket | TransportType::Localhost
    )
}

/// The `auth_route` vocabulary word for a transport. A network transport
/// without a credential is a loopback listener: the server only runs one
/// without a token store on a loopback bind.
const fn auth_route(transport: TransportType, bearer: bool) -> &'static str {
    match transport {
        TransportType::UnixSocket | TransportType::Localhost => AUTH_ROUTE_UDS,
        TransportType::SshTunnel => AUTH_ROUTE_SSH_STDIO,
        TransportType::Quic => bearer_or(bearer, AUTH_ROUTE_BEARER_QUIC, AUTH_ROUTE_LOOPBACK_QUIC),
        TransportType::WebSocket => {
            bearer_or(bearer, AUTH_ROUTE_BEARER_WSS, AUTH_ROUTE_LOOPBACK_WS)
        }
        TransportType::WebTransport => bearer_or(
            bearer,
            AUTH_ROUTE_BEARER_WEBTRANSPORT,
            AUTH_ROUTE_LOOPBACK_WEBTRANSPORT,
        ),
    }
}

/// `with_bearer` when the connection presented a credential, else
/// `loopback`.
const fn bearer_or(
    bearer: bool,
    with_bearer: &'static str,
    loopback: &'static str,
) -> &'static str {
    if bearer { with_bearer } else { loopback }
}

#[cfg(test)]
mod tests;
