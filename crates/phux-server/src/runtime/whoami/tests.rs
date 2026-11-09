//! `phux.whoami/v1` record assembly and admission of the ssh origin.

#![allow(clippy::expect_used, reason = "tests")]

use chrono::Utc;
use phux_protocol::ids::ResourceId;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::{Scope, ServingUser, SshClient, WHOAMI_KEY, WhoamiRecord};
use phux_protocol::wire::ssh_origin::SshOrigin;

use super::{
    ServingHost, admit_ssh_origin, auth_route, build_record, is_whoami_key, may_announce_ssh,
    record_for,
};
use crate::auth::AuthenticatedCredential;
use crate::state::{ClientId, ServerState};

fn ssh_origin() -> SshOrigin {
    SshOrigin {
        client: "203.0.113.5:52144".parse().expect("endpoint"),
        server: Some("198.51.100.7:22".parse().expect("endpoint")),
    }
}

/// The whoami record `state` answers for `client`, parsed.
fn answered(state: &ServerState, client: ClientId) -> WhoamiRecord {
    let bytes = record_for(state, client).expect("an identity is stored");
    serde_json::from_slice(&bytes).expect("the documented record")
}

/// A state holding one Unix-socket client of `uid`, which then announces
/// an ssh origin in its HELLO.
fn announced_by(uid: u32) -> (ServerState, ClientId) {
    let mut state = ServerState::new();
    let client = ClientId(7);
    state.set_peer_identity(client, peer(TransportType::UnixSocket, uid));
    admit_ssh_origin(&mut state, client, Some(ssh_origin()));
    (state, client)
}

fn serving_uid() -> u32 {
    nix::unistd::geteuid().as_raw()
}

/// A delegated key reports who delegated it; any other connection
/// reports `null`.
#[test]
fn a_delegated_key_reports_on_behalf_of() {
    use crate::policy::ConnectionGrant;
    use phux_core::PrincipalId;
    use phux_protocol::scope::TerminalScopeSet;

    let mut state = ServerState::new();
    let client = ClientId(7);
    state.set_peer_identity(client, peer(TransportType::UnixSocket, serving_uid()));
    state.set_connection_grant(client, ConnectionGrant::owner());
    let read = |state: &ServerState| -> serde_json::Value {
        serde_json::from_slice(&record_for(state, client).expect("a record")).expect("json")
    };
    assert_eq!(read(&state)["on_behalf_of"], serde_json::Value::Null);

    let delegated = |by: PrincipalId| {
        ConnectionGrant::delegated(
            TerminalScopeSet::parse_all(&["observe@group:1"]).expect("scope"),
            PrincipalId::Credential("delegated:00".to_owned()),
            by,
            Utc::now() + chrono::Duration::hours(1),
        )
        .expect("grant")
    };
    state.set_connection_grant(client, delegated(PrincipalId::Owner));
    assert_eq!(read(&state)["on_behalf_of"], "owner");
    state.set_connection_grant(
        client,
        delegated(PrincipalId::Credential("sha256:abc".to_owned())),
    );
    assert_eq!(read(&state)["on_behalf_of"], "sha256:abc");
}

/// The bridge, a same-uid Unix-socket peer, relabels its route.
#[test]
fn a_same_uid_bridge_announcement_reports_ssh_stdio() {
    let (state, client) = announced_by(serving_uid());
    let record = answered(&state, client);
    assert_eq!(record.auth_route, "ssh-stdio");
    assert_eq!(
        record.ssh_client,
        Some(SshClient {
            addr: "203.0.113.5".to_owned(),
            port: 52144,
        })
    );
    assert_eq!(record.peer_uid, Some(serving_uid()), "the bridge's uid");
    assert_eq!(record.principal, None, "the announcement grants nothing");
}

/// Another uid on the socket (root, say) cannot relabel itself.
#[test]
fn an_announcement_from_another_uid_is_ignored() {
    let (state, client) = announced_by(serving_uid().wrapping_add(1));
    assert_eq!(state.ssh_origin(client), None);
    let record = answered(&state, client);
    assert_eq!(record.auth_route, "uds");
    assert_eq!(record.ssh_client, None);
}

/// No announcement, no change: the plain Unix-socket route.
#[test]
fn no_announcement_reports_uds() {
    let mut state = ServerState::new();
    let client = ClientId(7);
    state.set_peer_identity(client, peer(TransportType::UnixSocket, serving_uid()));
    admit_ssh_origin(&mut state, client, None);
    let record = answered(&state, client);
    assert_eq!(record.auth_route, "uds");
    assert_eq!(record.ssh_client, None);
}

/// Only a Unix-socket peer of the serving uid may announce. The
/// in-process transport and every network transport may not, whatever
/// uid they carry.
#[test]
fn only_a_same_uid_unix_socket_peer_may_announce() {
    let cases = [
        (TransportType::UnixSocket, 501, true),
        (TransportType::UnixSocket, 0, false),
        (TransportType::UnixSocket, 502, false),
        (TransportType::Localhost, 501, false),
        (TransportType::SshTunnel, 501, false),
        (TransportType::Quic, 501, false),
        (TransportType::WebSocket, 501, false),
        (TransportType::WebTransport, 501, false),
    ];
    for (transport, uid, want) in cases {
        assert_eq!(
            may_announce_ssh(&peer(transport, uid), 501),
            want,
            "{transport:?} uid {uid}"
        );
    }
}

/// The pure assembly never relabels a network route, even if handed an
/// origin admission would have refused.
#[test]
fn an_origin_on_a_network_route_is_not_reported() {
    let credential = credential();
    let record = build_record(
        &peer(TransportType::Quic, 0),
        Some(&credential),
        Some(ssh_origin()),
        &host(),
    );
    assert_eq!(record.auth_route, "bearer-quic");
    assert_eq!(record.ssh_client, None);
}

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
        registry_instance: None,
        subject: None,
        managed: None,
    }
}

/// A Unix-socket client reports the kernel's peer uid and no credential.
#[test]
fn a_uds_connection_reports_its_peer_uid() {
    let record = build_record(&peer(TransportType::UnixSocket, 501), None, None, &host());
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
            "ssh_client": null,
        })
    );
}

/// A bearer connection reports its principal and credential id, and no
/// peer uid: the transport's `uid: 0` is a placeholder, not root.
#[test]
fn a_bearer_connection_reports_its_principal_and_no_peer_uid() {
    let credential = credential();
    let record = build_record(
        &peer(TransportType::Quic, 0),
        Some(&credential),
        None,
        &host(),
    );
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
