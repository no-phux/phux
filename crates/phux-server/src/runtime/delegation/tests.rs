#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use std::os::fd::IntoRawFd as _;
use std::path::Path;
use std::time::{Duration, Instant};

use tokio::io::AsyncWriteExt as _;
use tokio::task::LocalSet;

use super::sockets::socket_path_for;
use super::*;
use crate::delegation::DelegationRequest;
use crate::policy::ConnectionGrant;
use crate::runtime::test_support::{encode, hello};
use crate::state::ClientId;
use crate::upgrade::blob::DelegationBlob;

const LAUNCHER: ClientId = ClientId(90);
const PATIENCE: Duration = Duration::from_secs(5);

/// A server with one session, `alpha`, its socket in `dir`, and the
/// owner connected as [`LAUNCHER`].
fn server(dir: &Path, root: &CancellationToken) -> (SharedState, SessionId) {
    let state = SharedState::new();
    let pane = super::super::commands::seed_session_with_actor(
        &state,
        "alpha",
        phux_config::ScrollbackLimits::default(),
        root,
    )
    .unwrap();
    let session = state.with_mut(|s| {
        s.set_server_socket_path(dir.join("phux.sock"));
        s.set_connection_grant(LAUNCHER, ConnectionGrant::owner());
        s.intern_upgrade_wires();
        s.registry().session_of(pane).unwrap()
    });
    (state, session)
}

/// The old image: mint a credential on `alpha` with its socket bound
/// but not served, and carry it. The old image's own descriptor is
/// closed, as the exec closes it; the carried duplicate keeps the
/// socket bound.
fn carried_by_old_image(dir: &Path) -> (DelegationBlob, PathBuf) {
    let root = CancellationToken::new();
    let (old, session) = server(dir, &root);
    let credential = old
        .with_mut(|s| {
            s.mint_delegation(LAUNCHER, session, DelegationRequest::default(), Utc::now())
        })
        .unwrap();
    let path = socket_path_for(&dir.join("phux.sock"), &credential.id);
    let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
    let fd = std::os::fd::AsRawFd::as_raw_fd(&listener);
    old.with_mut(|s| s.note_delegation_operation([7; 16], &credential.id));
    assert!(old.with_mut(|s| {
        s.attach_delegation_listener(&credential.id, path.clone(), fd, CancellationToken::new())
    }));
    let empty = old.with(|s| s.assemble_upgrade_blob(0, &crate::state::UpgradeHandoffs::default()));
    let (blob, duplicates) = carry_delegations(&old, empty).unwrap();
    drop(listener);
    root.cancel();
    assert_eq!(blob.delegations.len(), 1);
    for duplicate in duplicates {
        // Handed to the new image, which owns it from here.
        let _ = duplicate.into_raw_fd();
    }
    (blob.delegations[0].clone(), path)
}

#[tokio::test(flavor = "current_thread")]
async fn a_hot_upgrade_carries_a_live_credential_and_its_socket() {
    LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let (carried, path) = carried_by_old_image(dir.path());

            let root = CancellationToken::new();
            let (new, _) = server(dir.path(), &root);
            assert_eq!(
                resume_delegations(&new, std::slice::from_ref(&carried), None, &root),
                1
            );
            let credential = new
                .with(|s| s.delegation(&carried.id, Utc::now()).cloned())
                .expect("reinstated");
            assert_eq!(credential.on_behalf_of, phux_core::PrincipalId::Owner);
            assert_eq!(
                new.with(|s| {
                    s.delegation_minted_by(&[7; 16], Utc::now())
                        .map(|minted| minted.id.clone())
                }),
                Some(carried.id.clone()),
                "a retry of the minting operation still answers it"
            );
            assert_eq!(
                new.with(|s| s.delegation_socket(&carried.id).map(Path::to_path_buf)),
                Some(path.clone())
            );

            // The agent reconnects through the same path, as itself.
            let mut stream = tokio::net::UnixStream::connect(&path).await.unwrap();
            stream
                .write_all(&encode(&hello("delegated-agent")))
                .await
                .unwrap();
            let principal = credential.principal();
            let deadline = Instant::now() + PATIENCE;
            while !new.with(|s| {
                s.watched_connections()
                    .iter()
                    .any(|watched| watched.grant.principal() == &principal)
            }) {
                assert!(Instant::now() < deadline, "the agent never got its grant");
                tokio::time::sleep(Duration::from_millis(5)).await;
            }

            // The start-up sweep leaves a carried socket alone.
            sweep_stale_sockets(&new);
            assert!(path.exists());
            drop(stream);
            root.cancel();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_carried_credential_whose_session_is_gone_is_revoked_and_its_socket_removed() {
    LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let (mut carried, path) = carried_by_old_image(dir.path());
            carried.group += 100;

            let root = CancellationToken::new();
            let (new, _) = server(dir.path(), &root);
            assert_eq!(
                resume_delegations(&new, std::slice::from_ref(&carried), None, &root),
                0
            );
            assert!(new.with(|s| s.delegation(&carried.id, Utc::now()).is_none()));
            assert!(!path.exists(), "its socket file is removed");
            root.cancel();
        })
        .await;
}

#[tokio::test(flavor = "current_thread")]
async fn a_carried_credential_that_expired_meanwhile_is_revoked() {
    LocalSet::new()
        .run_until(async {
            let dir = tempfile::tempdir().unwrap();
            let (mut carried, path) = carried_by_old_image(dir.path());
            carried.expires_at_ms = Utc::now().timestamp_millis() - 1;

            let root = CancellationToken::new();
            let (new, _) = server(dir.path(), &root);
            assert_eq!(
                resume_delegations(&new, std::slice::from_ref(&carried), None, &root),
                0
            );
            assert!(!path.exists());
            root.cancel();
        })
        .await;
}

#[test]
fn the_start_up_sweep_removes_only_unserved_delegated_sockets() {
    let dir = tempfile::tempdir().unwrap();
    let unserved = dir.path().join("agent-0123456789abcdef.sock");
    drop(std::os::unix::net::UnixListener::bind(&unserved).unwrap());
    let live_path = dir.path().join("agent-fedcba9876543210.sock");
    let live = std::os::unix::net::UnixListener::bind(&live_path).unwrap();
    let unrelated = dir.path().join("agent-notes.sock");
    drop(std::os::unix::net::UnixListener::bind(&unrelated).unwrap());

    let state = SharedState::new();
    state.with_mut(|s| s.set_server_socket_path(dir.path().join("phux.sock")));
    sweep_stale_sockets(&state);
    assert!(!unserved.exists(), "a socket nothing serves is removed");
    assert!(live_path.exists(), "a served socket is kept");
    assert!(unrelated.exists(), "only delegated socket names are swept");
    drop(live);
}
