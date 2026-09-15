//! What is, and is not, a verdict (`docs/spec/workload-auth.md` §7).
//!
//! Benign pairing-token store events never cut a live paired session, while
//! a real revocation still does within the poll interval; a bearer revoked
//! since its upgrade is refused at HELLO; and the registry's edge cases: a
//! hand edit that keeps the generation is still judged, and a broken state
//! that recovers inside its grace window revokes nothing.

use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use phux_protocol::policy::{PeerIdentity, TransportType};
use phux_protocol::wire::frame::{DetachReason, ErrorCode, FrameKind};

use super::{
    Connection, Fixture, TEST_GRACE, hand_edit_registry, hello, local, settle, write_owner_only,
};
use crate::auth::{
    AuthenticatedCredential, BearerAdmission, ConnectionIdentity, ReloadingTokenStore, TOKEN_LEN,
};
use crate::policy::PermissivePolicy;
use crate::runtime::revocation::{POLL, spawn_revocation_watcher};

const SECRET: [u8; TOKEN_LEN] = [0x5a; TOKEN_LEN];

/// The id `write_test_credential` records.
const TOKEN_ID: &str = "test-credential";

/// The default engine: every connection holds the owner's grant, as with no
/// `[policy] mode`.
fn permissive() -> Fixture {
    Fixture::with_engine(
        tempfile::tempdir().unwrap(),
        PathBuf::new(),
        String::new(),
        Arc::new(PermissivePolicy),
    )
}

/// A pairing-token store holding one active credential.
fn token_store() -> (tempfile::TempDir, PathBuf, Arc<ReloadingTokenStore>) {
    let dir = tempfile::tempdir().unwrap();
    let tokens = dir.path().join("remote-tokens");
    crate::auth::write_test_credential(&tokens, &SECRET);
    let store = Arc::new(ReloadingTokenStore::load(tokens.clone()).unwrap());
    (dir, tokens, store)
}

/// A paired phone's WebSocket connection, admitted by a bearer from
/// `store`.
fn phone_identity(store: &Arc<ReloadingTokenStore>) -> ConnectionIdentity {
    let credential = AuthenticatedCredential {
        id: TOKEN_ID.to_owned(),
        principal: "test-principal".to_owned(),
        scopes: vec![crate::auth::TERMINAL_CONTROL_SCOPE.to_owned()],
        issued_at: Utc::now(),
        expires_at: None,
        generation: 1,
        registry_instance: None,
    };
    ConnectionIdentity {
        peer: PeerIdentity {
            uid: 0,
            pid: None,
            exe_path: None,
            mcp_host_key: Some(TOKEN_ID.to_owned()),
            transport: TransportType::WebSocket,
            source_addr: None,
        },
        bearer: Some(BearerAdmission::new(Arc::clone(store), &credential)),
        credential: Some(credential),
        ssh_origin: None,
    }
}

async fn phone(fx: &Fixture, store: &Arc<ReloadingTokenStore>) -> Connection {
    let connection = fx.connect(phone_identity(store), None);
    connection.hello().await;
    connection
}

/// Two watcher polls later, the live session is still up.
async fn assert_survives(phone: &Connection, what: &str) {
    tokio::time::sleep(POLL * 2).await;
    settle().await;
    assert!(
        phone.is_open(),
        "{what} ended a live paired session: {:?}",
        phone.frames()
    );
}

#[tokio::test(flavor = "current_thread")]
async fn benign_token_store_events_never_cut_live_bearer_sessions() {
    local(async {
        let fx = permissive();
        let (_dir, tokens, store) = token_store();
        spawn_revocation_watcher(&fx.state, &fx.root);
        let phone = phone(&fx, &store).await;

        write_owner_only(&tokens, b"");
        assert_survives(&phone, "an empty store").await;
        write_owner_only(&tokens, b"{\"version\":1,\"credentials\":[{\"id\":");
        assert_survives(&phone, "a truncated store").await;
        std::fs::write(&tokens, b"{\"version\":1").unwrap();
        assert_survives(&phone, "a store caught mid-write in place").await;
        std::fs::remove_file(&tokens).unwrap();
        assert_survives(&phone, "a missing store").await;
        crate::auth::write_test_credential(&tokens, &SECRET);
        std::fs::set_permissions(&tokens, std::fs::Permissions::from_mode(0o640)).unwrap();
        assert_survives(&phone, "a group-readable store").await;
        crate::auth::write_test_credential(&tokens, &SECRET);
        assert_survives(&phone, "the repaired store").await;

        // A real `phux pair revoke` still ends it within the poll interval.
        let revoked_at = Instant::now();
        crate::auth::revoke_credential(&tokens, TOKEN_ID).unwrap();
        phone
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        assert!(
            revoked_at.elapsed() < Duration::from_secs(2),
            "the revocation took {:?}",
            revoked_at.elapsed()
        );
        phone.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_bearer_revoked_since_its_upgrade_is_refused_at_hello() {
    local(async {
        let fx = permissive();
        let (_dir, tokens, store) = token_store();
        let admitted = phone(&fx, &store).await;

        crate::auth::revoke_credential(&tokens, TOKEN_ID).unwrap();
        let late = fx.connect(phone_identity(&store), None);
        late.send(&hello());
        late.wait_for("the HELLO refusal", |frame| {
            matches!(
                frame,
                FrameKind::Error {
                    code: ErrorCode::PermissionDenied,
                    ..
                }
            )
        })
        .await;
        assert!(
            !late
                .frames()
                .iter()
                .any(|frame| matches!(frame, FrameKind::HelloOk { .. })),
            "{:?}",
            late.frames()
        );
        assert!(
            fx.state.with(|s| s.connection_grant(late.client).is_none()),
            "no grant was minted"
        );
        admitted.finish().await;
        late.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_same_generation_hand_edit_is_still_judged() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let agent = fx.workload().await;
        fx.sweep();
        settle().await;
        assert!(agent.is_open(), "{:?}", agent.frames());

        // `revoked_at` written by hand, the generation untouched.
        hand_edit_registry(&fx.registry, |doc| {
            doc["credentials"][0]["revoked_at"] = Utc::now().timestamp().into();
        });
        fx.sweep();
        agent
            .assert_ended_with(DetachReason::AuthorizationRevoked)
            .await;
        agent.finish().await;
    })
    .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_broken_registry_that_recovers_inside_its_grace_window_revokes_nothing() {
    local(async {
        let fx = Fixture::paired(&["*@global"]);
        let agent = fx.workload().await;
        let good = std::fs::read(&fx.registry).unwrap();

        write_owner_only(&fx.registry, b"{ half-written");
        fx.sweep();
        write_owner_only(&fx.registry, &good);
        tokio::time::sleep(TEST_GRACE * 2).await;
        fx.sweep();
        settle().await;
        assert!(agent.is_open(), "{:?}", agent.frames());
        agent.finish().await;
    })
    .await;
}
