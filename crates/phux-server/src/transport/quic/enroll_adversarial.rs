//! The enrollment ALPN and the paired listener's post-handshake check, on a
//! real QUIC listener (`docs/spec/workload-auth.md` §3, §8.2, ADR-0154).
//! Every refusal has a positive control on the same listener.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "tests"
)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use phux_dial::enroll::{EnrollDial, check_issued_chain, enroll};
use phux_dial::{CertTrust, DialError};
use phux_protocol::enroll::Request;

use super::super::Incoming as _;
use super::QuicListener;
use crate::workload::{ReloadingWorkloadRegistry, WorkloadPaths, tickets};

struct Paired {
    _dir: tempfile::TempDir,
    paths: WorkloadPaths,
    cert: PathBuf,
    key: PathBuf,
}

impl Paired {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(
            dir.path(),
            <std::fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700),
        )
        .unwrap();
        let paths = WorkloadPaths {
            ca_cert: dir.path().join("workload-ca.pem"),
            ca_key: dir.path().join("workload-ca.key"),
            registry: dir.path().join("workload-keys"),
        };
        let cert = dir.path().join("remote-cert.pem");
        let key = dir.path().join("remote-key.pem");
        crate::transport::tls::ensure_server_identity(&cert, &key, &[], &paths).unwrap();
        Self {
            _dir: dir,
            paths,
            cert,
            key,
        }
    }

    /// The configured listener under `paired`: a client certificate mapped
    /// through the registry, and the enrollment ALPN when `enrollment`.
    fn listener(&self, enrollment: bool) -> (QuicListener, SocketAddr) {
        let ca = crate::workload::authority_certificate(&self.paths.ca_cert).unwrap();
        let registry =
            Arc::new(ReloadingWorkloadRegistry::load(self.paths.registry.clone()).unwrap());
        let listener = QuicListener::from_pem_with_client_ca_and_registry(
            "127.0.0.1:0".parse().unwrap(),
            &self.cert,
            &self.key,
            None,
            Some(&ca),
            Some(registry),
            enrollment.then(|| self.paths.clone()),
        )
        .unwrap();
        let addr = listener.local_addr().unwrap();
        (listener, addr)
    }

    fn trust(&self) -> CertTrust {
        CertTrust::Authority {
            ca: crate::workload::ca_fingerprint(&self.paths.ca_cert).unwrap(),
            leaf: None,
        }
    }

    fn ticket(&self) -> Vec<u8> {
        let minted = tickets::mint_ticket(
            &tickets::tickets_path(&self.paths.registry),
            vec!["observe@global".to_owned()],
            3600,
            600,
        )
        .unwrap();
        hex::decode(minted.secret_hex).unwrap()
    }
}

fn csr(key: &rcgen::KeyPair) -> Vec<u8> {
    rcgen::CertificateParams::new(Vec::<String>::new())
        .unwrap()
        .serialize_request(key)
        .unwrap()
        .der()
        .to_vec()
}

/// Enroll while driving the listener's accept loop, which never yields an
/// enrollment connection.
async fn enroll_on(
    listener: &QuicListener,
    addr: SocketAddr,
    trust: CertTrust,
    request: Request,
) -> Result<String, DialError> {
    let dial = EnrollDial {
        addr,
        server_name: "localhost".to_owned(),
        trust,
        inner: None,
    };
    tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            enrolled = enroll(&dial, &request) => enrolled,
            _ = listener.accept() => panic!("an enrollment is never a phux connection"),
        }
    })
    .await
    .expect("the enrollment settles")
}

#[tokio::test]
async fn a_ticket_enrolls_one_key_and_the_chain_passes_the_client_checks() {
    let paired = Paired::new();
    let (listener, addr) = paired.listener(true);
    let key = rcgen::KeyPair::generate().unwrap();
    let ticket = paired.ticket();
    let chain = enroll_on(
        &listener,
        addr,
        paired.trust(),
        Request {
            ticket: ticket.clone(),
            csr: csr(&key),
        },
    )
    .await
    .expect("a live ticket enrolls");
    let pinned = crate::workload::ca_fingerprint(&paired.paths.ca_cert).unwrap();
    check_issued_chain(
        &chain,
        &rcgen::PublicKeyData::subject_public_key_info(&key),
        Some(&pinned),
    )
    .expect("the reply passes every client check");
    let registry = crate::workload::WorkloadRegistry::load(&paired.paths.registry).unwrap();
    assert_eq!(registry.credentials().len(), 1);

    // Replay with another key: refused, nothing enrolled.
    let replay = enroll_on(
        &listener,
        addr,
        paired.trust(),
        Request {
            ticket,
            csr: csr(&rcgen::KeyPair::generate().unwrap()),
        },
    )
    .await;
    assert!(
        matches!(replay, Err(DialError::AuthRefused(_))),
        "{replay:?}"
    );
    let registry = crate::workload::WorkloadRegistry::load(&paired.paths.registry).unwrap();
    assert_eq!(
        registry.credentials().len(),
        1,
        "the replay enrolled nothing"
    );
}

/// A request that fails before the ticket is checked leaves it unspent; a
/// wrong ticket and a listener that offers no enrollment are refused.
#[tokio::test]
async fn a_bad_request_spends_nothing_and_unknown_tickets_are_refused() {
    let paired = Paired::new();
    let (listener, addr) = paired.listener(true);
    let ticket = paired.ticket();
    let garbage = enroll_on(
        &listener,
        addr,
        paired.trust(),
        Request {
            ticket: ticket.clone(),
            csr: vec![0x30, 0x03, 0x01, 0x01, 0xff],
        },
    )
    .await;
    assert!(
        matches!(garbage, Err(DialError::AuthRefused(_))),
        "{garbage:?}"
    );
    let unknown = enroll_on(
        &listener,
        addr,
        paired.trust(),
        Request {
            ticket: vec![9; 32],
            csr: csr(&rcgen::KeyPair::generate().unwrap()),
        },
    )
    .await;
    assert!(
        matches!(unknown, Err(DialError::AuthRefused(_))),
        "{unknown:?}"
    );
    enroll_on(
        &listener,
        addr,
        paired.trust(),
        Request {
            ticket: ticket.clone(),
            csr: csr(&rcgen::KeyPair::generate().unwrap()),
        },
    )
    .await
    .expect("control: the ticket was not spent by the malformed request");

    let (bare, bare_addr) = paired.listener(false);
    let refused = enroll_on(
        &bare,
        bare_addr,
        paired.trust(),
        Request {
            ticket: paired.ticket(),
            csr: csr(&rcgen::KeyPair::generate().unwrap()),
        },
    )
    .await;
    assert!(
        refused.is_err(),
        "a listener without enrollment never issues"
    );
}

/// The paired listener completes a handshake without a certificate (so the
/// enrollment ALPN can answer) but refuses the terminal ALPN before any
/// stream byte: the dial's stream write, and so its bearer preamble, never
/// reaches the frame layer.
#[tokio::test]
async fn a_certificate_less_terminal_connection_is_refused_before_any_stream() {
    let paired = Paired::new();
    let (listener, addr) = paired.listener(true);
    let plan = phux_dial::QuicDial {
        addr,
        server_name: "localhost".to_owned(),
        token: Some(vec![1; 32]),
        trust: paired.trust(),
        identity: Some(phux_dial::TlsClientIdentity::None),
        inner: None,
    };
    let outcome = tokio::time::timeout(Duration::from_secs(10), async {
        tokio::select! {
            dialed = async {
                let (_endpoint, conn, _send, _recv) = phux_dial::quic::dial(&plan).await?;
                Ok::<_, DialError>(conn.closed().await)
            } => dialed.map(Some),
            _ = listener.accept() => Ok(None),
        }
    })
    .await
    .expect("settles");
    match outcome {
        Ok(Some(quinn::ConnectionError::ApplicationClosed(close))) => {
            assert_eq!(
                close.error_code,
                quinn::VarInt::from_u32(super::AUTH_FAILED_CODE)
            );
        }
        Err(DialError::AuthRefused(_) | DialError::Connect(_)) => {}
        other => panic!("a certificate-less peer must be refused, got {other:?}"),
    }
}
