//! The read-only `phux.whoami/v1` Global metadata key (`docs/spec/L3.md`
//! §3.9, ADR-0106).
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
use phux_protocol::wire::frame::{
    AUTH_ROUTE_BEARER_QUIC, AUTH_ROUTE_BEARER_WEBTRANSPORT, AUTH_ROUTE_BEARER_WSS,
    AUTH_ROUTE_LOOPBACK_QUIC, AUTH_ROUTE_LOOPBACK_WEBTRANSPORT, AUTH_ROUTE_LOOPBACK_WS,
    AUTH_ROUTE_SSH_STDIO, AUTH_ROUTE_UDS, Scope, ServingUser, WHOAMI_KEY, WHOAMI_SCHEMA_VERSION,
    WhoamiRecord,
};

use crate::auth::AuthenticatedCredential;
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
pub(super) fn record_for(s: &ServerState, client_id: ClientId) -> Option<Vec<u8>> {
    let peer = s.peer_identity(client_id)?;
    let record = build_record(peer, s.authenticated_credential(client_id), serving_host());
    serde_json::to_vec(&record).ok()
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
fn build_record(
    peer: &PeerIdentity,
    credential: Option<&AuthenticatedCredential>,
    host: &ServingHost,
) -> WhoamiRecord {
    WhoamiRecord {
        schema_version: WHOAMI_SCHEMA_VERSION,
        principal: credential.map(|credential| credential.principal.clone()),
        credential_id: credential.map(|credential| credential.id.clone()),
        auth_route: auth_route(peer.transport, credential.is_some()).to_owned(),
        peer_uid: kernel_peer_uid(peer),
        serving_user: host.user.clone(),
        host: host.name.clone(),
        server_version: SERVER_VERSION.to_owned(),
    }
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
mod tests {
    #![allow(clippy::expect_used, reason = "tests")]

    use chrono::Utc;
    use phux_protocol::ids::ResourceId;
    use phux_protocol::policy::{PeerIdentity, TransportType};
    use phux_protocol::wire::frame::{Scope, ServingUser, WHOAMI_KEY, WhoamiRecord};

    use super::{ServingHost, auth_route, build_record, is_whoami_key};
    use crate::auth::AuthenticatedCredential;

    fn peer(transport: TransportType, uid: u32) -> PeerIdentity {
        PeerIdentity {
            uid,
            pid: None,
            exe_path: None,
            mcp_host_key: None,
            transport,
            source_addr: None,
        }
    }

    fn host() -> ServingHost {
        ServingHost {
            name: "mini".to_owned(),
            user: ServingUser {
                uid: 501,
                name: Some("me".to_owned()),
            },
        }
    }

    fn credential() -> AuthenticatedCredential {
        AuthenticatedCredential {
            id: "0123abcd".to_owned(),
            principal: "phone".to_owned(),
            scopes: vec!["terminal.control".to_owned()],
            issued_at: Utc::now(),
            expires_at: None,
            generation: 1,
        }
    }

    /// A Unix-socket client reports the kernel's peer uid and no credential.
    #[test]
    fn a_uds_connection_reports_its_peer_uid() {
        let record = build_record(&peer(TransportType::UnixSocket, 501), None, &host());
        let json = serde_json::to_value(&record).expect("serializes");
        assert_eq!(
            json,
            serde_json::json!({
                "schema_version": 1,
                "principal": null,
                "credential_id": null,
                "auth_route": "uds",
                "peer_uid": 501,
                "serving_user": { "uid": 501, "name": "me" },
                "host": "mini",
                "server_version": env!("CARGO_PKG_VERSION"),
            })
        );
    }

    /// A bearer connection reports its principal and credential id, and no
    /// peer uid: the transport's `uid: 0` is a placeholder, not root.
    #[test]
    fn a_bearer_connection_reports_its_principal_and_no_peer_uid() {
        let credential = credential();
        let record = build_record(&peer(TransportType::Quic, 0), Some(&credential), &host());
        assert_eq!(record.principal.as_deref(), Some("phone"));
        assert_eq!(record.credential_id.as_deref(), Some("0123abcd"));
        assert_eq!(record.auth_route, "bearer-quic");
        assert_eq!(record.peer_uid, None);
        assert_eq!(record.serving_user.uid, 501);
    }

    /// Every transport maps to one route word, split by whether a bearer
    /// credential admitted it.
    #[test]
    fn every_transport_has_a_route_word() {
        let cases = [
            (TransportType::UnixSocket, false, "uds"),
            (TransportType::Localhost, false, "uds"),
            (TransportType::SshTunnel, false, "ssh-stdio"),
            (TransportType::Quic, true, "bearer-quic"),
            (TransportType::Quic, false, "loopback-quic"),
            (TransportType::WebSocket, true, "bearer-wss"),
            (TransportType::WebSocket, false, "loopback-ws"),
            (TransportType::WebTransport, true, "bearer-webtransport"),
            (TransportType::WebTransport, false, "loopback-webtransport"),
        ];
        for (transport, bearer, want) in cases {
            assert_eq!(
                auth_route(transport, bearer),
                want,
                "{transport:?} {bearer}"
            );
        }
    }

    /// Only the Global scope carries the key.
    #[test]
    fn the_key_lives_in_the_global_scope_only() {
        assert!(is_whoami_key(&Scope::Global, WHOAMI_KEY));
        assert!(!is_whoami_key(
            &Scope::Resource(ResourceId::local(1)),
            WHOAMI_KEY
        ));
        assert!(!is_whoami_key(&Scope::Global, "phux.whoami/v2"));
    }

    /// A newer server's additive field does not break an older reader.
    #[test]
    fn a_reader_ignores_fields_it_does_not_know() {
        let json = r#"{"schema_version":1,"principal":"p","credential_id":"c",
            "auth_route":"bearer-quic","peer_uid":null,
            "serving_user":{"uid":0,"name":null},"host":"h",
            "server_version":"9","later":true}"#;
        let record: WhoamiRecord = serde_json::from_str(json).expect("parses");
        assert_eq!(record.principal.as_deref(), Some("p"));
        assert_eq!(record.serving_user.name, None);
    }
}
