//! The server identity the workload CA issues, pinned by a real client over a
//! real QUIC listener (ADR-0153, `docs/spec/workload-auth.md` §2).
//!
//! - A client pinning the CA connects; one pinning only the leaf learns the
//!   CA on that first authenticated connection.
//! - **CA swap.** After `phux workload authority --rotate` the server
//!   presents a leaf from a new CA: the client pinning the old one is
//!   refused with an authority change naming both fingerprints, never
//!   silently re-trusting, and a client pinning the old leaf is refused too.
//! - Re-pairing (pinning the new CA) is admitted: the positive control.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use phux_dial::{AuthorityLearner, CertTrust, DialError, QuicDial};

use super::super::Incoming as _;
use super::{QuicAdmission, QuicListener};
use crate::workload::WorkloadPaths;

/// A server state directory: the workload CA and the server's own pair.
struct State {
    _dir: tempfile::TempDir,
    paths: WorkloadPaths,
    cert: PathBuf,
    key: PathBuf,
}

impl State {
    fn provisioned() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            dir.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let state = Self {
            paths: WorkloadPaths {
                ca_cert: dir.path().join("workload-ca.pem"),
                ca_key: dir.path().join("workload-ca.key"),
                registry: dir.path().join("workload-keys"),
            },
            cert: dir.path().join("remote-cert.pem"),
            key: dir.path().join("remote-key.pem"),
            _dir: dir,
        };
        crate::transport::tls::ensure_server_identity(&state.cert, &state.key, &[], &state.paths)
            .unwrap();
        state
    }

    /// A listener presenting the pair as it is on disk now (a restart).
    fn listener(&self) -> (QuicListener, SocketAddr) {
        let listener = QuicListener::with_admission(
            "127.0.0.1:0".parse().unwrap(),
            &self.cert,
            &self.key,
            QuicAdmission::Open,
            None,
        )
        .unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, addr)
    }

    fn authority(&self) -> String {
        crate::transport::tls::presented_authority(&self.cert)
            .unwrap()
            .expect("the provisioned certificate chains to the workload CA")
    }

    fn leaf(&self) -> String {
        crate::transport::tls::cert_fingerprint(&self.cert).unwrap()
    }
}

/// Dial `addr` with `trust`, driving the listener's accept loop meanwhile.
async fn dial(
    listener: &QuicListener,
    addr: SocketAddr,
    trust: CertTrust,
) -> Result<(), DialError> {
    let plan = QuicDial {
        addr,
        server_name: "localhost".to_owned(),
        token: None,
        trust,
        identity: Some(phux_dial::TlsClientIdentity::None),
        inner: None,
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            dialed = phux_dial::quic::dial(&plan) => dialed.map(drop),
            _ = listener.accept() => panic!("nothing was written to admit"),
        }
    })
    .await
    .expect("the dial settles")
}

#[tokio::test]
async fn a_rotated_authority_is_refused_by_name_until_the_client_re_pairs() {
    let state = State::provisioned();
    let original = state.authority();
    assert_eq!(
        original,
        crate::workload::ca_fingerprint(&state.paths.ca_cert).unwrap(),
        "the server presents the CA `phux workload authority` prints"
    );
    let old_leaf = state.leaf();
    let pinned = CertTrust::Authority {
        ca: original.clone(),
        leaf: Some(old_leaf.clone()),
    };
    let (listener, addr) = state.listener();
    dial(&listener, addr, pinned.clone())
        .await
        .expect("control: the pinned authority's server");
    drop(listener);

    let rotation = crate::workload::rotate_authority(
        &state.paths,
        Some(crate::workload::ServerPair {
            cert: &state.cert,
            key: &state.key,
        }),
    )
    .unwrap();
    assert_eq!(rotation.previous, original);
    let rotated = state.authority();
    assert_eq!(rotation.fingerprint, rotated);

    let (listener, addr) = state.listener();
    match dial(&listener, addr, pinned).await {
        Err(DialError::AuthorityChanged(change)) => {
            assert_eq!(change.pinned, original);
            assert_eq!(change.presented.as_deref(), Some(rotated.as_str()));
        }
        other => panic!("a swapped authority must be refused by name, got {other:?}"),
    }
    assert!(
        dial(&listener, addr, CertTrust::Pinned(old_leaf))
            .await
            .is_err(),
        "a stale leaf pin is refused"
    );
    dial(
        &listener,
        addr,
        CertTrust::Authority {
            ca: rotated,
            leaf: None,
        },
    )
    .await
    .expect("re-paired: the new authority is admitted");
}

#[tokio::test]
async fn a_leaf_pinned_client_learns_the_authority_on_its_first_connection() {
    let state = State::provisioned();
    let (listener, addr) = state.listener();
    let learned = Arc::new(Mutex::new(None::<String>));
    let recorder = {
        let learned = Arc::clone(&learned);
        AuthorityLearner::new(move |authority| {
            *learned.lock().unwrap() = Some(authority.to_owned());
        })
    };
    dial(
        &listener,
        addr,
        CertTrust::PinnedLearning {
            leaf: state.leaf(),
            learn: recorder,
        },
    )
    .await
    .expect("the pinned leaf");
    assert_eq!(*learned.lock().unwrap(), Some(state.authority()));
}

/// A server whose pair predates ADR-0153 keeps its self-signed leaf: the
/// existing pin keeps working, and there is no authority to learn.
#[tokio::test]
async fn an_existing_self_signed_pair_is_never_re_issued() {
    let state = State::provisioned();
    std::fs::remove_file(&state.cert).unwrap();
    std::fs::remove_file(&state.key).unwrap();
    crate::transport::tls::ensure_self_signed(&state.cert, &state.key).unwrap();
    let legacy_leaf = state.leaf();
    crate::transport::tls::ensure_server_identity(&state.cert, &state.key, &[], &state.paths)
        .unwrap();
    assert_eq!(state.leaf(), legacy_leaf, "an existing pair is left alone");
    assert_eq!(
        crate::transport::tls::presented_authority(&state.cert).unwrap(),
        None
    );
    let (listener, addr) = state.listener();
    dial(&listener, addr, CertTrust::Pinned(legacy_leaf))
        .await
        .expect("the pin a device already holds");
}
